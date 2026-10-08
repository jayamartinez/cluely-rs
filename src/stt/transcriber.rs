//! Runs one recognition session per audio source, each on its own worker thread, and owns
//! provider switching. Consumers keep only events from the current generation.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use super::event::{EventKind, Generation, TranscriptEvent};
use super::provider::{AsrError, Availability, EventSink, StreamingAsr};
use crate::audio::{AudioChunk, Source};

enum Message { Audio(AudioChunk), Finish }

struct Worker {
    audio: Sender<Message>,
    thread: JoinHandle<()>,
    /// Audio time (f64 bits) the session has processed up to.
    processed: Arc<AtomicU64>,
}

pub struct Transcriber {
    events: Sender<TranscriptEvent>,
    generation: Generation,
    workers: HashMap<Source, Worker>,
}

impl Transcriber {
    pub fn new(events: Sender<TranscriptEvent>) -> Self {
        Self { events, generation: Generation::default(), workers: HashMap::new() }
    }

    pub fn generation(&self) -> Generation { self.generation }

    /// Whether an event belongs to the provider configuration that is running now.
    pub fn is_current(&self, event: &TranscriptEvent) -> bool { event.generation == self.generation }

    /// Stop whatever is running and start `provider` for `sources` under a new generation.
    /// Returns immediately; model loading happens on the workers, and a session that fails to
    /// start reports a fatal `Error` event instead of blocking the caller.
    pub fn start(&mut self, provider: Arc<dyn StreamingAsr>, sources: &[Source]) -> Result<Generation, AsrError> {
        match provider.availability() {
            Availability::Ready => {}
            other => return Err(AsrError::NotReady(other)),
        }
        self.stop();
        self.generation = Generation(self.generation.0 + 1);
        for &source in sources {
            let (audio, inbox) = channel();
            let sink = EventSink::new(source, self.generation, self.events.clone());
            let provider = provider.clone();
            let processed = Arc::new(AtomicU64::new(0f64.to_bits()));
            let progress = processed.clone();
            let thread = std::thread::Builder::new().name(format!("cluelyrs-asr-{}", source.label()))
                .spawn(move || run(provider, sink, inbox, &progress))
                .map_err(|error| AsrError::Failed(format!("Couldn't start transcription: {error}")))?;
            self.workers.insert(source, Worker { audio, thread, processed });
        }
        Ok(self.generation)
    }

    /// Route a chunk to its source's session. Chunks for sources that aren't running are dropped.
    pub fn feed(&self, chunk: AudioChunk) {
        if let Some(worker) = self.workers.get(&chunk.source) { let _ = worker.audio.send(Message::Audio(chunk)); }
    }

    /// Audio time `source`'s session has processed up to. Behind the audio fed so far when the
    /// recognizer can't keep up (a saturated CPU). Every event from that audio has been sent
    /// before the position is published, so draining events after reading it sees them all.
    pub fn processed_until(&self, source: Source) -> Option<f64> {
        self.workers.get(&source).map(|worker| f64::from_bits(worker.processed.load(Ordering::Acquire)))
    }

    /// Flush and end every session, waiting for their final events.
    pub fn stop(&mut self) {
        for (_, worker) in self.workers.drain() {
            let _ = worker.audio.send(Message::Finish);
            let _ = worker.thread.join();
        }
    }

    pub fn running_sources(&self) -> Vec<Source> { self.workers.keys().copied().collect() }
}

impl Drop for Transcriber {
    fn drop(&mut self) { self.stop(); }
}

fn run(provider: Arc<dyn StreamingAsr>, sink: EventSink, inbox: Receiver<Message>, processed: &AtomicU64) {
    let mut session = match provider.start_session(sink.clone()) {
        Ok(session) => session,
        Err(error) => { sink.emit(0.0, 0.0, EventKind::Error { message: error.to_string(), fatal: true }); return; }
    };
    let mut last_end = 0.0;
    loop {
        match inbox.recv() {
            Ok(Message::Audio(chunk)) => {
                last_end = chunk.end_ms();
                if let Err(error) = session.push(&chunk) {
                    sink.emit(chunk.start_ms, last_end, EventKind::Error { message: error.to_string(), fatal: true });
                    return;
                }
                processed.store(last_end.to_bits(), Ordering::Release);
            }
            Ok(Message::Finish) | Err(_) => {
                if let Err(error) = session.finish() {
                    sink.emit(last_end, last_end, EventKind::Error { message: error.to_string(), fatal: true });
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::stt::scripted::{ScriptedAsr, Step};

    fn chunk(source: Source, start_ms: f64, ms: usize) -> AudioChunk {
        AudioChunk { source, start_ms, samples: vec![0.0; ms * 16] }
    }

    fn drain(events: &Receiver<TranscriptEvent>) -> Vec<TranscriptEvent> {
        let mut out = Vec::new();
        while let Ok(event) = events.recv_timeout(Duration::from_millis(200)) { out.push(event); }
        out
    }

    #[test]
    fn each_source_gets_its_own_session_and_events_keep_source_and_audio_time() {
        let provider = Arc::new(ScriptedAsr::new(vec![
            Step::partial(Source::Them, 100.0, "how would you"),
            Step::partial(Source::Them, 200.0, "how would you design"),
            Step::eou(Source::Them, 300.0, "how would you design it"),
            Step::partial(Source::Me, 150.0, "hmm"),
        ]));
        let (sender, events) = channel();
        let mut transcriber = Transcriber::new(sender);
        let generation = transcriber.start(provider, &Source::ALL).unwrap();
        for t in (0..400).step_by(50) {
            transcriber.feed(chunk(Source::Them, t as f64, 50));
            transcriber.feed(chunk(Source::Me, t as f64, 50));
        }
        transcriber.stop();
        let events = drain(&events);
        let them: Vec<_> = events.iter().filter(|e| e.source == Source::Them).map(|e| e.text().unwrap().to_string()).collect();
        assert_eq!(them, ["how would you", "how would you design", "how would you design it"]);
        assert!(events.iter().any(|e| e.source == Source::Me && e.text() == Some("hmm")));
        let eou = events.iter().find(|e| matches!(e.kind, EventKind::EndOfUtterance { .. })).unwrap();
        assert_eq!((eou.start_ms, eou.end_ms), (0.0, 300.0));
        assert!(events.iter().all(|e| e.generation == generation));
    }

    #[test]
    fn the_position_each_session_has_processed_is_reported() {
        let (sender, _events) = channel();
        let mut transcriber = Transcriber::new(sender);
        transcriber.start(Arc::new(ScriptedAsr::new(vec![])), &[Source::Them]).unwrap();
        assert_eq!(transcriber.processed_until(Source::Them), Some(0.0));
        assert_eq!(transcriber.processed_until(Source::Me), None);
        for t in (0..400).step_by(50) { transcriber.feed(chunk(Source::Them, t as f64, 50)); }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while transcriber.processed_until(Source::Them) != Some(400.0) && std::time::Instant::now() < deadline { std::thread::yield_now(); }
        assert_eq!(transcriber.processed_until(Source::Them), Some(400.0));
    }

    #[test]
    fn switching_provider_starts_a_new_generation_and_old_events_are_not_current() {
        let (sender, events) = channel();
        let mut transcriber = Transcriber::new(sender);
        let first = transcriber.start(Arc::new(ScriptedAsr::new(vec![Step::partial(Source::Them, 10.0, "old")])), &[Source::Them]).unwrap();
        transcriber.feed(chunk(Source::Them, 0.0, 20));
        let second = transcriber.start(Arc::new(ScriptedAsr::new(vec![Step::partial(Source::Them, 10.0, "new")])), &[Source::Them]).unwrap();
        assert!(second > first);
        transcriber.feed(chunk(Source::Them, 0.0, 20));
        transcriber.stop();
        let events = drain(&events);
        let current: Vec<_> = events.iter().filter(|e| transcriber.is_current(e)).map(|e| e.text().unwrap()).collect();
        assert_eq!(current, ["new"]);
        assert!(events.iter().any(|e| e.generation == first && e.text() == Some("old")));
    }

    #[test]
    fn unavailable_providers_refuse_to_start_and_failed_sessions_report_fatal_errors() {
        let (sender, events) = channel();
        let mut transcriber = Transcriber::new(sender);
        let missing = ScriptedAsr::new(vec![]).with_availability(Availability::NeedsModel { download_bytes: 1 });
        assert!(matches!(transcriber.start(Arc::new(missing), &[Source::Them]), Err(AsrError::NotReady(Availability::NeedsModel { .. }))));
        assert_eq!(transcriber.generation(), Generation(0));

        transcriber.start(Arc::new(ScriptedAsr::new(vec![]).failing_start()), &[Source::Me]).unwrap();
        transcriber.stop();
        let events = drain(&events);
        assert!(matches!(&events[..], [TranscriptEvent { kind: EventKind::Error { fatal: true, .. }, source: Source::Me, .. }]));
    }

    #[test]
    fn audio_for_sources_that_are_not_running_is_ignored() {
        let (sender, events) = channel();
        let mut transcriber = Transcriber::new(sender);
        transcriber.start(Arc::new(ScriptedAsr::new(vec![Step::partial(Source::Me, 0.0, "me")])), &[Source::Them]).unwrap();
        transcriber.feed(chunk(Source::Me, 0.0, 20));
        assert_eq!(transcriber.running_sources(), [Source::Them]);
        transcriber.stop();
        assert!(drain(&events).is_empty());
    }
}
