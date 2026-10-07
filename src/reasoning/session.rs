//! One request at a time, each under its own generation. A new request or a stop cancels
//! what is running; replies carry their generation, so anything from a superseded request is
//! dropped before it can reach the screen.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::{UnboundedReceiver, unbounded};

use crate::answer::{self, Exchange, Target};
use crate::codex::{CodexClient, CodexThread};
use crate::metrics::{Context, LatencyRecorder, Stage};
use crate::settings::{Provider, Settings};
use crate::transcript::state::UtteranceId;

/// Identifies one request. Later generations supersede earlier ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(pub u64);

pub struct Request {
    pub action: String,
    pub question: String,
    /// Finished exchanges of this session, oldest first (replayed for stateless providers).
    pub history: Vec<Exchange>,
    /// The conversation heard so far, already rendered (see `reasoning::context`).
    pub conversation: String,
    pub screenshot: Option<Vec<u8>>,
    /// The utterance this request answers (the latest committed line from the other side),
    /// so latency marks can be paired with its transcript marks.
    pub utterance: Option<UtteranceId>,
}

#[derive(Debug)]
pub enum Event { Delta(String), Done(Result<String, String>) }

#[derive(Debug)]
pub struct Reply { pub generation: Generation, pub event: Event }

pub struct ReasoningSession {
    codex: Arc<CodexClient>,
    /// The Codex thread for this session, opened on the first request and kept for all of them.
    thread: Arc<Mutex<Option<CodexThread>>>,
    recorder: Option<LatencyRecorder>,
    generation: Generation,
    cancel: Option<Arc<AtomicBool>>,
}

impl ReasoningSession {
    pub fn new(codex: Arc<CodexClient>, recorder: Option<LatencyRecorder>) -> Self {
        Self { codex, thread: Arc::new(Mutex::new(None)), recorder, generation: Generation::default(), cancel: None }
    }

    pub fn generation(&self) -> Generation { self.generation }

    /// Whether a reply belongs to the request that is current now.
    pub fn is_current(&self, generation: Generation) -> bool { generation == self.generation && self.cancel.is_some() }

    /// Stop the running request, if any. Its remaining replies are stale from here on.
    pub fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
            if let Some(recorder) = &self.recorder { recorder.mark(Stage::SpeculationCancelled, Context::default()); }
        }
    }

    /// Free preparation when Live starts: for the ChatGPT subscription, start the app-server and
    /// open the session's thread in the background, so the first answer only pays for its turn.
    /// A request that arrives meanwhile waits for the thread rather than opening a second one.
    pub fn prewarm(&self, settings: &Settings) {
        if settings.provider != Provider::Codex { return; }
        let (codex, thread, system) = (self.codex.clone(), self.thread.clone(), answer::system(settings));
        let _ = std::thread::Builder::new().name("cluelyrs-answer-prewarm".into()).spawn(move || {
            let mut open = thread.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Err(error) = codex.prepare_thread(&mut open, &system) { eprintln!("answer prewarm skipped: {error}"); }
        });
    }

    /// Blocks until a preparation started by [`ReasoningSession::prewarm`] has finished.
    pub fn wait_prepared(&self) { drop(self.thread.lock()); }

    /// Start a request under a new generation, cancelling the previous one. Replies arrive on
    /// the returned channel from a worker thread; the provider blocks, the UI never does.
    pub fn ask(&mut self, settings: &Settings, request: Request) -> Result<(Generation, UnboundedReceiver<Reply>), String> {
        self.cancel();
        let target = answer::build(settings, &self.codex, &request.action, &request.question, &request.history, &request.conversation, request.screenshot)?;
        self.generation = Generation(self.generation.0 + 1);
        let generation = self.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        let (sender, replies) = unbounded();
        let provider = provider_id(settings);
        let mut context = Context::default().provider(provider);
        if let Some(id) = request.utterance { context = context.utterance(id); }
        if let Some(recorder) = &self.recorder { recorder.mark(Stage::LlmRequestStarted, context.clone()); }
        let recorder = self.recorder.clone();
        let thread = self.thread.clone();
        let codex = self.codex.clone();
        std::thread::Builder::new().name(format!("cluelyrs-answer-{}", generation.0)).spawn(move || {
            let deltas = sender.clone();
            let mut first = true;
            let mut on_delta = |delta: &str| {
                if first {
                    first = false;
                    if let Some(recorder) = &recorder { recorder.mark(Stage::LlmFirstToken, context.clone()); }
                }
                let _ = deltas.unbounded_send(Reply { generation, event: Event::Delta(delta.to_string()) });
            };
            let result = match &target {
                // The warm thread: only the new message travels; the thread holds the rest.
                Target::Codex(req, _) => {
                    let mut open = thread.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    codex.stream_turn(&mut open, req, &cancel, &mut on_delta)
                }
                other => answer::run(other, &cancel, &mut on_delta),
            };
            if result.is_ok() && let Some(recorder) = &recorder { recorder.mark(Stage::ResponseCommitted, context.clone()); }
            let _ = sender.unbounded_send(Reply { generation, event: Event::Done(result) });
        }).map_err(|error| format!("Couldn't start the request: {error}"))?;
        Ok((generation, replies))
    }
}

impl Drop for ReasoningSession {
    fn drop(&mut self) { self.cancel(); }
}

fn provider_id(settings: &Settings) -> String {
    match settings.provider {
        Provider::Codex => "codex".into(),
        Provider::Claude => "claude".into(),
        Provider::ApiKey => settings.api_provider.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use futures::StreamExt;

    use super::*;

    fn request() -> Request {
        Request { action: "Ask".into(), question: "hello".into(), history: Vec::new(), conversation: String::new(), screenshot: None, utterance: Some(3) }
    }

    /// The custom OpenAI-compatible preset pointed at loopback port 9 (discard; nothing ever
    /// listens there), so the request fails fast on the worker on any machine: generations,
    /// cancellation and reply tagging are exercised without network or sign-in.
    fn settings() -> Settings {
        Settings { provider: Provider::ApiKey, api_provider: "custom".into(), custom_base_url: "http://127.0.0.1:9/v1".into(),
            api_models: [("custom".to_string(), "test-model".to_string())].into(), ..Settings::default() }
    }

    #[test]
    fn each_request_gets_a_new_generation_and_supersedes_the_previous_one() {
        let recorder = LatencyRecorder::new(Instant::now());
        let mut session = ReasoningSession::new(CodexClient::new(), Some(recorder.clone()));
        let keyless = Settings { api_provider: "anthropic".into(), ..settings() };
        let Err(reason) = session.ask(&keyless, request()) else { panic!("a missing key must be reported when building") };
        assert!(reason.contains("API key"), "{reason}");
        assert_eq!(session.generation(), Generation(0));

        let settings = settings();
        let (first, mut replies) = session.ask(&settings, request()).unwrap();
        assert_eq!(first, Generation(1));
        assert!(session.is_current(first));
        let (second, _) = session.ask(&settings, request()).unwrap();
        assert_eq!(second, Generation(2));
        assert!(!session.is_current(first) && session.is_current(second));
        // The superseded request still finishes, but under its own stale generation.
        let reply = futures::executor::block_on(replies.next()).unwrap();
        assert_eq!(reply.generation, first);
        assert!(matches!(reply.event, Event::Done(Err(_))));
        session.cancel();
        assert!(!session.is_current(second));
        let stages: Vec<Stage> = recorder.snapshot().iter().map(|m| m.stage).collect();
        assert!(stages.starts_with(&[Stage::LlmRequestStarted, Stage::SpeculationCancelled, Stage::LlmRequestStarted, Stage::SpeculationCancelled]), "{stages:?}");
        assert!(recorder.snapshot().iter().all(|m| m.utterance == Some(3) || m.stage == Stage::SpeculationCancelled));
    }
}
