//! One request at a time, each under its own generation. A new request or a stop cancels
//! what is running; replies carry their generation, so anything from a superseded request is
//! dropped before it can reach the screen.
//!
//! A speculative answer ([`ReasoningSession::speculate`]) runs beside that, into a hidden
//! buffer, under a generation that is not current. Pressing Assist [`claims`](ReasoningSession::claim)
//! it when the context still matches: its generation becomes current, the buffered text is
//! shown and the rest streams on. Otherwise it is cancelled and its replies stay stale.
//!
//! Speculation never touches the warm provider the asked-for answers use. It has its own
//! Codex thread and its own prepared Claude process, so a speculative turn that is cancelled
//! never leaves its text in the history of the thread answers run on. On a hit the two Codex
//! threads swap: the one that now holds the shown exchange carries the session on, and the
//! other is discarded and replaced by a fresh one. A speculation that ends any other way
//! discards its thread the same way.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::{UnboundedReceiver, unbounded};

use crate::answer::{self, Exchange, Target};
use crate::chat::Effort;
use crate::claude_cli::{ClaudeCli, Spare};
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

type Slot<T> = Arc<Mutex<Option<T>>>;

fn lock<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }

/// Whether a request's text is on screen yet, so "first visible words" is marked once.
#[derive(Default)]
struct Visibility {
    /// Someone is looking at this request's replies (asked for, or claimed).
    claimed: AtomicBool,
    shown: AtomicBool,
    /// The model has produced its first words.
    produced: AtomicBool,
}

/// A speculative answer running into a hidden buffer.
struct Speculation {
    key: u64,
    utterance: Option<UtteranceId>,
    provider: String,
    generation: Generation,
    cancel: Arc<AtomicBool>,
    visibility: Arc<Visibility>,
    replies: UnboundedReceiver<Reply>,
    /// Instructions its Codex thread was opened with, to open a fresh one afterwards.
    system: String,
}

/// A claimed speculative answer: what it has produced so far, and the rest as it arrives.
pub struct Claimed {
    pub generation: Generation,
    pub text: String,
    /// The answer had already finished.
    pub done: Option<Result<String, String>>,
    pub replies: UnboundedReceiver<Reply>,
}

pub struct ReasoningSession {
    codex: Arc<CodexClient>,
    /// The Codex thread for this session, opened on the first request and kept for all of them.
    thread: Slot<CodexThread>,
    /// A Claude Code process started ahead of the next answer (Claude subscription only).
    claude: Slot<Spare>,
    /// Speculation's own thread and process, so cancelled speculative turns never reach the
    /// history the asked-for answers build on.
    spec_thread: Slot<CodexThread>,
    spec_claude: Slot<Spare>,
    recorder: Option<LatencyRecorder>,
    generation: Generation,
    /// The last generation handed out, current or speculative.
    issued: u64,
    cancel: Option<Arc<AtomicBool>>,
    speculation: Option<Speculation>,
}

impl ReasoningSession {
    pub fn new(codex: Arc<CodexClient>, recorder: Option<LatencyRecorder>) -> Self {
        Self { codex, thread: Arc::default(), claude: Arc::default(), spec_thread: Arc::default(), spec_claude: Arc::default(),
            recorder, generation: Generation::default(), issued: 0, cancel: None, speculation: None }
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

    /// Free preparation when Live starts, in the background, so the first answer only pays for
    /// its turn. ChatGPT subscription: start the app-server and open the session's thread (a
    /// request that arrives meanwhile waits for it rather than opening a second one). Claude
    /// subscription: start a Claude Code process for the first answer. Nothing is sent either way.
    pub fn prewarm(&self, settings: &Settings) { self.prepare(settings, &self.thread, &self.claude, false); }

    /// The same preparation for speculative answers' own thread or process. Sends no model
    /// request; call when speculative answers are on and a question is coming.
    pub fn prewarm_speculation(&self, settings: &Settings) { self.prepare(settings, &self.spec_thread, &self.spec_claude, false); }

    /// Prepare `thread` / `claude` for the selected provider. With `fresh`, an open thread is
    /// discarded first (its history holds a speculative turn nobody saw).
    fn prepare(&self, settings: &Settings, thread: &Slot<CodexThread>, claude: &Slot<Spare>, fresh: bool) {
        if settings.provider == Provider::Claude {
            let (claude, system) = (claude.clone(), answer::system(settings));
            let (model, effort) = (settings.claude_model.id().to_string(), Effort::from_smart_mode(settings.smart_mode));
            let _ = std::thread::Builder::new().name("cluelyrs-answer-prewarm".into()).spawn(move || {
                if let Err(error) = ClaudeCli::prepare(&mut lock(&claude), &system, Some(&model), effort) { eprintln!("answer prewarm skipped: {error}"); }
            });
            return;
        }
        if settings.provider != Provider::Codex { return; }
        self.prepare_thread(thread, answer::system(settings), fresh);
    }

    fn prepare_thread(&self, thread: &Slot<CodexThread>, system: String, fresh: bool) {
        let (codex, thread) = (self.codex.clone(), thread.clone());
        let _ = std::thread::Builder::new().name("cluelyrs-answer-prewarm".into()).spawn(move || {
            let mut open = lock(&thread);
            if fresh { *open = None; }
            if let Err(error) = codex.prepare_thread(&mut open, &system) { eprintln!("answer prewarm skipped: {error}"); }
        });
    }

    /// Blocks until a preparation started by [`ReasoningSession::prewarm`] has finished.
    pub fn wait_prepared(&self) {
        for slot in [&self.thread, &self.spec_thread] { drop(lock(slot)); }
        for slot in [&self.claude, &self.spec_claude] { drop(lock(slot)); }
    }

    /// The user asked for an answer. Marks when they asked, before any screenshot is taken, so
    /// "asked → first words" is what they waited.
    pub fn note_requested(&self, utterance: Option<UtteranceId>) {
        if let Some(recorder) = &self.recorder { recorder.mark(Stage::AnswerRequested, utterance_context(utterance)); }
    }

    /// Start a request under a new generation, cancelling the previous one. Replies arrive on
    /// the returned channel from a worker thread; the provider blocks, the UI never does.
    pub fn ask(&mut self, settings: &Settings, request: Request) -> Result<(Generation, UnboundedReceiver<Reply>), String> {
        self.cancel();
        let target = answer::build(settings, &self.codex, &request.action, &request.question, &request.history, &request.conversation, request.screenshot)?;
        let generation = self.issue();
        self.generation = generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        let visibility = Arc::new(Visibility { claimed: AtomicBool::new(true), ..Visibility::default() });
        let context = utterance_context(request.utterance).provider(provider_id(settings));
        if let Some(recorder) = &self.recorder { recorder.mark(Stage::LlmRequestStarted, context.clone()); }
        let slots = (self.thread.clone(), self.claude.clone());
        let replies = self.spawn(target, generation, cancel, visibility, context, Stage::LlmFirstToken, slots)?;
        Ok((generation, replies))
    }

    /// Start a speculative answer (Settings, opt-in): the same real request an asked-for answer
    /// would send, into a hidden buffer, replacing any other speculation.
    /// `key` is the [`super::speculation::context_key`] it answers.
    pub fn speculate(&mut self, settings: &Settings, request: Request, key: u64) -> Result<(), String> {
        self.cancel_speculation();
        let target = answer::build(settings, &self.codex, &request.action, &request.question, &request.history, &request.conversation, request.screenshot)?;
        let generation = self.issue();
        let cancel = Arc::new(AtomicBool::new(false));
        let visibility = Arc::new(Visibility::default());
        let provider = provider_id(settings);
        let context = utterance_context(request.utterance).provider(provider.clone());
        if let Some(recorder) = &self.recorder { recorder.mark(Stage::SpeculationStarted, context.clone()); }
        let slots = (self.spec_thread.clone(), self.spec_claude.clone());
        let replies = self.spawn(target, generation, cancel.clone(), visibility.clone(), context, Stage::SpeculationFirstToken, slots)?;
        self.speculation = Some(Speculation { key, utterance: request.utterance, provider, generation, cancel, visibility, replies, system: answer::system(settings) });
        Ok(())
    }

    /// Whether a speculative answer is running or ready, and for which context.
    pub fn speculating(&self) -> Option<u64> { self.speculation.as_ref().map(|speculation| speculation.key) }

    /// The speculative answer now answers `key` (its question was committed after it started).
    pub fn rekey(&mut self, key: u64) {
        if let Some(speculation) = &mut self.speculation { speculation.key = key; }
    }

    /// The context a speculative answer has started writing for, if any: it can be shown now.
    pub fn speculation_ready(&self) -> Option<u64> {
        self.speculation.as_ref().filter(|speculation| speculation.visibility.produced.load(Ordering::Relaxed)).map(|speculation| speculation.key)
    }

    /// The user asked for the answer to `key`. If the speculative answer is for exactly that
    /// context, it becomes the current request (cancelling any other) and is returned with
    /// what it has produced so far. Otherwise it is cancelled as a miss and `None` returned.
    pub fn claim(&mut self, key: u64) -> Option<Claimed> { self.take_speculation(key, true) }

    /// Show the speculative answer for `key` without anyone asking (Settings → Show answers
    /// automatically). Like [`ReasoningSession::claim`], but not counted as a request.
    pub fn claim_automatically(&mut self, key: u64) -> Option<Claimed> { self.take_speculation(key, false) }

    fn take_speculation(&mut self, key: u64, requested: bool) -> Option<Claimed> {
        let speculation = self.speculation.take()?;
        let context = utterance_context(speculation.utterance).provider(speculation.provider.clone());
        if speculation.key != key || speculation.cancel.load(Ordering::Relaxed) {
            if let Some(recorder) = &self.recorder { recorder.mark(Stage::SpeculationMissed, context); }
            let system = speculation.system.clone();
            self.end_speculation(speculation, Some(system));
            return None;
        }
        self.cancel();
        if requested && let Some(recorder) = &self.recorder { recorder.mark(Stage::SpeculationHit, context.clone()); }
        let Speculation { generation, cancel, visibility, mut replies, provider, system, .. } = speculation;
        let (mut text, mut done) = (String::new(), None);
        while let Ok(reply) = replies.try_recv() {
            match reply.event {
                Event::Delta(delta) => text.push_str(&delta),
                Event::Done(result) => done = Some(result),
            }
        }
        // From here the worker marks the first words itself if none were buffered yet.
        visibility.claimed.store(true, Ordering::SeqCst);
        if requested && !text.is_empty() && !visibility.shown.swap(true, Ordering::SeqCst)
            && let Some(recorder) = &self.recorder { recorder.mark(Stage::AnswerShown, context); }
        // Shown without a request: nothing was waited for, so no "first visible words" either.
        if !requested { visibility.shown.store(true, Ordering::SeqCst); }
        self.generation = generation;
        self.cancel = Some(cancel);
        // The speculative thread now holds the shown exchange: answers continue on it, and the
        // previous thread (without that exchange) becomes the spare that is opened afresh.
        std::mem::swap(&mut self.thread, &mut self.spec_thread);
        if provider == "codex" { self.prepare_thread(&self.spec_thread, system, true); }
        Some(Claimed { generation, text, done, replies })
    }

    /// Stop the speculative answer, if any (a newer question, Live ending).
    pub fn cancel_speculation(&mut self) {
        if let Some(speculation) = self.speculation.take() {
            let system = speculation.system.clone();
            self.end_speculation(speculation, Some(system));
        }
    }

    /// The instructions changed (another mode): a speculative answer prepared under the old ones
    /// is dropped, and the provider is prepared with the new ones, for answers and, when one was
    /// running, for the speculation that replaces it.
    pub fn change_instructions(&mut self, settings: &Settings) {
        if let Some(speculation) = self.speculation.take() { self.end_speculation(speculation, Some(answer::system(settings))); }
        self.prewarm(settings);
    }

    /// With `replace`, a fresh speculation thread is opened in place of its own, under those instructions.
    fn end_speculation(&mut self, speculation: Speculation, replace: Option<String>) {
        speculation.cancel.store(true, Ordering::Relaxed);
        if let Some(recorder) = &self.recorder {
            recorder.mark(Stage::SpeculationCancelled, utterance_context(speculation.utterance).provider(speculation.provider.clone()));
        }
        // Its thread may now hold a turn nobody saw: replace it rather than reuse it. The reset
        // waits for the cancelled turn to let go of the thread.
        if let Some(system) = replace.filter(|_| speculation.provider == "codex") { self.prepare_thread(&self.spec_thread, system, true); }
    }

    fn issue(&mut self) -> Generation {
        self.issued += 1;
        Generation(self.issued)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn(&self, target: Target, generation: Generation, cancel: Arc<AtomicBool>, visibility: Arc<Visibility>, context: Context,
        first_token: Stage, (thread, claude): (Slot<CodexThread>, Slot<Spare>)) -> Result<UnboundedReceiver<Reply>, String> {
        let (sender, replies) = unbounded();
        let recorder = self.recorder.clone();
        let codex = self.codex.clone();
        std::thread::Builder::new().name(format!("cluelyrs-answer-{}", generation.0)).spawn(move || {
            let deltas = sender.clone();
            let mut first = true;
            let mut on_delta = |delta: &str| {
                visibility.produced.store(true, Ordering::Relaxed);
                if let Some(recorder) = &recorder {
                    if first { recorder.mark(first_token, context.clone()); }
                    if visibility.claimed.load(Ordering::SeqCst) && !visibility.shown.swap(true, Ordering::SeqCst) {
                        recorder.mark(Stage::AnswerShown, context.clone());
                    }
                }
                first = false;
                let _ = deltas.unbounded_send(Reply { generation, event: Event::Delta(delta.to_string()) });
            };
            let result = match &target {
                // The warm thread: only the new message travels; the thread holds the rest.
                Target::Codex(req, _) => codex.stream_turn(&mut lock(&thread), req, &cancel, &mut on_delta),
                // A process started ahead of time, and a fresh one ready for the next answer.
                Target::Claude(req) => ClaudeCli::stream_warm(&mut lock(&claude), true, req, &cancel, &mut on_delta),
                other => answer::run(other, &cancel, &mut on_delta),
            };
            if result.is_ok() && let Some(recorder) = &recorder { recorder.mark(Stage::ResponseCommitted, context.clone()); }
            let _ = sender.unbounded_send(Reply { generation, event: Event::Done(result) });
        }).map_err(|error| format!("Couldn't start the request: {error}"))?;
        Ok(replies)
    }
}

impl Drop for ReasoningSession {
    fn drop(&mut self) {
        // Live is over: nothing is opened in place of the speculation's thread.
        if let Some(speculation) = self.speculation.take() { self.end_speculation(speculation, None); }
        self.cancel();
    }
}

fn utterance_context(utterance: Option<UtteranceId>) -> Context {
    let context = Context::default();
    match utterance { Some(id) => context.utterance(id), None => context }
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

    fn stages(recorder: &LatencyRecorder) -> Vec<Stage> { recorder.snapshot().iter().map(|m| m.stage).collect() }

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
        let stages = stages(&recorder);
        assert!(stages.starts_with(&[Stage::LlmRequestStarted, Stage::SpeculationCancelled, Stage::LlmRequestStarted, Stage::SpeculationCancelled]), "{stages:?}");
        assert!(recorder.snapshot().iter().all(|m| m.utterance == Some(3) || m.stage == Stage::SpeculationCancelled));
    }

    #[test]
    fn a_speculative_answer_is_hidden_until_claimed_with_the_same_context() {
        let recorder = LatencyRecorder::new(Instant::now());
        let mut session = ReasoningSession::new(CodexClient::new(), Some(recorder.clone()));
        let settings = settings();
        let (asked, _) = session.ask(&settings, request()).unwrap();
        session.speculate(&settings, request(), 42).unwrap();
        // Speculating never changes what is current: the asked-for answer keeps the screen.
        assert!(session.is_current(asked));
        assert_eq!(session.speculating(), Some(42));
        session.note_requested(Some(3));
        let claimed = session.claim(42).expect("same context: a hit");
        assert!(claimed.generation > asked);
        assert!(session.is_current(claimed.generation) && !session.is_current(asked));
        assert_eq!(session.speculating(), None);
        // Its replies (here the failure from the unreachable server) carry the claimed generation.
        let mut replies = claimed.replies;
        let done = match claimed.done {
            Some(result) => result,
            None => loop {
                let reply = futures::executor::block_on(replies.next()).unwrap();
                assert_eq!(reply.generation, claimed.generation);
                if let Event::Done(result) = reply.event { break result; }
            },
        };
        assert!(done.is_err());
        let stages = stages(&recorder);
        assert!(stages.starts_with(&[Stage::LlmRequestStarted, Stage::SpeculationStarted, Stage::AnswerRequested, Stage::SpeculationCancelled, Stage::SpeculationHit]), "{stages:?}");
    }

    fn finish(mut replies: UnboundedReceiver<Reply>) -> Result<String, String> {
        loop {
            if let Event::Done(result) = futures::executor::block_on(replies.next()).unwrap().event { return result; }
        }
    }

    /// (thread, number of input items) of every turn sent to the app-server.
    fn turns(received: &Mutex<Vec<serde_json::Value>>) -> Vec<(String, usize)> {
        lock(received).iter().filter(|m| m["method"] == "turn/start")
            .map(|m| (m["params"]["threadId"].as_str().unwrap().to_string(), m["params"]["input"].as_array().unwrap().len())).collect()
    }

    fn wait_for(received: &Mutex<Vec<serde_json::Value>>, method: &str, count: usize) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while lock(received).iter().filter(|m| m["method"] == method).count() < count {
            assert!(Instant::now() < deadline, "waited for {count} × {method}");
            std::thread::yield_now();
        }
    }

    /// With the ChatGPT subscription (mocked app-server): speculation runs on its own thread, a
    /// speculation that isn't shown is thrown away with its thread, and a shown one carries the
    /// session on, so the thread answers build on never holds a turn nobody saw.
    #[test]
    fn speculative_codex_turns_never_reach_the_thread_answers_continue_on() {
        let (codex, received) = crate::codex::tests::scripted_client();
        let mut session = ReasoningSession::new(codex, None);
        let settings = Settings { provider: Provider::Codex, ..Settings::default() };
        let (_, replies) = session.ask(&settings, request()).unwrap();
        assert_eq!(finish(replies).unwrap(), "answer 1");

        // Speculated, then the user asked about something else: a miss.
        session.speculate(&settings, request(), 1).unwrap();
        wait_for(&received, "turn/start", 2);
        assert!(session.claim(2).is_none());
        wait_for(&received, "thread/unsubscribe", 1);
        wait_for(&received, "thread/start", 3);
        let (_, replies) = session.ask(&settings, request()).unwrap();
        finish(replies).unwrap();
        assert_eq!(turns(&received), [("thr_1".into(), 1), ("thr_2".into(), 1), ("thr_1".into(), 1)]);

        // Speculated on the fresh thread and asked for in the same context: a hit, shown, and
        // the next answer continues on the thread that holds it.
        session.speculate(&settings, request(), 7).unwrap();
        let claimed = session.claim(7).unwrap();
        let text = match claimed.done { Some(result) => result.unwrap(), None => format!("{}{}", claimed.text, finish(claimed.replies).unwrap_or_default()) };
        assert!(text.starts_with("answer 4"), "{text}");
        let (_, replies) = session.ask(&settings, request()).unwrap();
        finish(replies).unwrap();
        assert_eq!(turns(&received)[3..], [("thr_3".into(), 1), ("thr_3".into(), 1)]);
        // The previous thread, without the shown exchange, was let go and a fresh spare opened.
        wait_for(&received, "thread/start", 4);
        wait_for(&received, "thread/unsubscribe", 2);
    }

    /// Switching modes mid-session (ChatGPT subscription, mocked app-server): the next answer is
    /// a turn on the same thread, which holds every earlier turn, under the new instructions; a
    /// running speculation is replaced by a thread opened with the new instructions, not the old.
    #[test]
    fn a_mode_switch_keeps_the_session_thread_and_prepares_speculation_with_the_new_mode() {
        let (codex, received) = crate::codex::tests::scripted_client();
        let mut session = ReasoningSession::new(codex, None);
        let general = Settings { provider: Provider::Codex, ..Settings::default() };
        for _ in 0..2 {
            let (_, replies) = session.ask(&general, request()).unwrap();
            finish(replies).unwrap();
        }
        session.speculate(&general, request(), 1).unwrap();
        wait_for(&received, "turn/start", 3);
        let mode = crate::modes::Active { name: "Interview".into(), context: "I'm interviewing for a backend role.".into(), files: Vec::new() };
        let interview = Settings { mode: Some(Arc::new(mode)), ..general.clone() };
        session.change_instructions(&interview);
        assert_eq!(session.speculating(), None);
        let (_, replies) = session.ask(&interview, request()).unwrap();
        finish(replies).unwrap();
        wait_for(&received, "thread/start", 3);
        // Answers: thr_1 throughout, its last turn carrying only the new message.
        let turns = turns(&received);
        assert_eq!(turns[..2], [("thr_1".into(), 1), ("thr_1".into(), 1)]);
        assert_eq!(turns.last().unwrap(), &("thr_1".to_string(), 1));
        let messages = lock(&received).clone();
        let injected: Vec<&serde_json::Value> = messages.iter().filter(|m| m["method"] == "thread/inject_items").collect();
        assert_eq!(injected.len(), 1);
        assert_eq!(injected[0]["params"]["threadId"], "thr_1");
        assert!(injected[0]["params"]["items"][0]["content"][0]["text"].as_str().unwrap().ends_with(&answer::system(&interview)));
        // Speculation: its thread (thr_2) is replaced by one opened under the new instructions.
        let started: Vec<&serde_json::Value> = messages.iter().filter(|m| m["method"] == "thread/start").collect();
        assert_eq!(started[1]["params"]["baseInstructions"], answer::system(&general));
        assert_eq!(started[2]["params"]["baseInstructions"], answer::system(&interview));
    }

    /// Stopping Live and starting it again starts a fresh model context: the new session opens
    /// its own Codex thread instead of continuing the previous session's.
    #[test]
    fn a_new_live_session_never_continues_the_previous_sessions_thread() {
        let (codex, received) = crate::codex::tests::scripted_client();
        let settings = Settings { provider: Provider::Codex, ..Settings::default() };
        let mut first = ReasoningSession::new(codex.clone(), None);
        for _ in 0..2 { let (_, replies) = first.ask(&settings, request()).unwrap(); finish(replies).unwrap(); }
        drop(first);
        let mut second = ReasoningSession::new(codex, None);
        let (_, replies) = second.ask(&settings, request()).unwrap();
        finish(replies).unwrap();
        assert_eq!(turns(&received), [("thr_1".into(), 1), ("thr_1".into(), 1), ("thr_2".into(), 1)]);
        wait_for(&received, "thread/unsubscribe", 1);
    }

    #[test]
    fn a_speculative_answer_for_another_context_is_cancelled_and_never_becomes_current() {
        let recorder = LatencyRecorder::new(Instant::now());
        let mut session = ReasoningSession::new(CodexClient::new(), Some(recorder.clone()));
        let settings = settings();
        session.speculate(&settings, request(), 1).unwrap();
        assert!(session.claim(2).is_none());
        assert_eq!(session.speculating(), None);
        assert!(session.claim(1).is_none(), "a missed speculation is gone");
        assert!(!session.is_current(Generation(1)));
        // A newer question replaces the one in flight; stopping Live cancels what is left.
        session.speculate(&settings, request(), 3).unwrap();
        session.speculate(&settings, request(), 4).unwrap();
        drop(session);
        let stages = stages(&recorder);
        assert_eq!(stages.iter().filter(|s| **s == Stage::SpeculationStarted).count(), 3);
        assert_eq!(stages.iter().filter(|s| **s == Stage::SpeculationCancelled).count(), 3);
        assert_eq!(stages.iter().filter(|s| **s == Stage::SpeculationMissed).count(), 1);
        assert!(!stages.contains(&Stage::SpeculationHit));
    }
}
