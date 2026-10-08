//! Ties the transcript pieces together for a running session: feeds audio levels and provider
//! events in, decides commits per source, filters stale provider generations, and records
//! latency marks with utterance ids.

use std::collections::HashSet;

use super::endpoint::{Decision, EndpointConfig, Reason, Signals, SilenceTracker, decide};
use super::intent::assess;
use super::state::{Change, Committed, TranscriptState, UtteranceId};
use crate::audio::{AudioChunk, Source};
use crate::metrics::{Context, LatencyRecorder, Stage};
use crate::stt::{EventKind, Generation, TranscriptEvent};

/// The question score at which an utterance counts as a question for metrics and listeners.
pub const QUESTION_THRESHOLD: f32 = 0.5;

/// Default for how far behind the audio a recognizer's words arrive while someone is still
/// talking (Parakeet measured 160–640 ms between partials); providers state their own.
const DEFAULT_TEXT_LAG_MS: f64 = 400.0;

#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Provisional { source: Source, id: UtteranceId, stable: String, unstable: String },
    /// An utterance in progress now looks like a question (fires once per utterance).
    QuestionLikely { source: Source, id: UtteranceId, score: f32 },
    Committed { utterance: Committed, reason: Reason },
    Error { source: Source, message: String, fatal: bool },
}

pub struct LiveTranscript {
    state: TranscriptState,
    silence: [SilenceTracker; 2],
    config: EndpointConfig,
    generation: Option<Generation>,
    provider: String,
    recorder: Option<LatencyRecorder>,
    questions: HashSet<UtteranceId>,
    /// Audio time observed when each source's recognizer last sent anything (Me, Them).
    last_event_ms: [f64; 2],
    text_lag_ms: f64,
}

fn slot(source: Source) -> usize { match source { Source::Me => 0, Source::Them => 1 } }

impl LiveTranscript {
    pub fn new(config: EndpointConfig, recorder: Option<LatencyRecorder>) -> Self {
        Self { state: TranscriptState::new(), silence: Default::default(), config, generation: None, provider: String::new(),
            recorder, questions: HashSet::new(), last_event_ms: [0.0; 2], text_lag_ms: DEFAULT_TEXT_LAG_MS }
    }

    pub fn state(&self) -> &TranscriptState { &self.state }

    /// The running provider's `Capabilities::text_lag_ms`.
    pub fn set_text_lag(&mut self, ms: f64) { self.text_lag_ms = ms; }

    /// A provider (re)started: only its generation's events count, and in-progress text from
    /// the previous one is dropped.
    pub fn set_generation(&mut self, generation: Generation, provider: impl Into<String>) {
        self.generation = Some(generation);
        self.provider = provider.into();
        for source in Source::ALL { self.state.discard_provisional(source); }
    }

    /// Track speech energy and re-check endpointing as silence grows.
    pub fn on_audio(&mut self, chunk: &AudioChunk) -> Vec<Update> {
        self.silence[slot(chunk.source)].observe(chunk);
        self.evaluate(chunk.source).into_iter().collect()
    }

    pub fn on_event(&mut self, event: &TranscriptEvent) -> Vec<Update> {
        if self.generation != Some(event.generation) { return Vec::new(); }
        let source = event.source;
        self.last_event_ms[slot(source)] = self.silence[slot(source)].latest_end_ms();
        let mut updates = Vec::new();
        match self.state.apply(event) {
            None => {}
            Some(Change::Error { source, message, fatal }) => updates.push(Update::Error { source, message, fatal }),
            Some(change) => {
                let id = match change {
                    Change::Provisional { id, first, .. } => { if first { self.mark(Stage::AsrPartial, source, id, Some(event.end_ms)); } id }
                    Change::EndOfUtterance { id, .. } => { self.mark(Stage::AsrEndOfUtterance, source, id, Some(event.end_ms)); id }
                    Change::ProviderFinal { id, .. } => { self.mark(Stage::AsrFinal, source, id, Some(event.end_ms)); id }
                    Change::Error { .. } => unreachable!(),
                };
                if let Some(provisional) = self.state.provisional(source) {
                    updates.push(Update::Provisional { source, id, stable: provisional.split.stable().to_string(), unstable: provisional.split.unstable().to_string() });
                    let score = assess(provisional.text()).question;
                    if score >= QUESTION_THRESHOLD && self.questions.insert(id) {
                        self.mark(Stage::IntentDetected, source, id, None);
                        updates.push(Update::QuestionLikely { source, id, score });
                    }
                }
            }
        }
        if !matches!(event.kind, EventKind::Error { .. }) { updates.extend(self.evaluate(source)); }
        updates
    }

    /// Listening ended: whatever is still in progress is committed as it stands.
    pub fn finish(&mut self) -> Vec<Update> {
        Source::ALL.into_iter().filter_map(|source| {
            let utterance = self.state.commit(source)?;
            self.mark(Stage::UtteranceCommitted, source, utterance.id, Some(utterance.end_ms));
            self.questions.remove(&utterance.id);
            Some(Update::Committed { utterance, reason: Reason::Stopped })
        }).collect()
    }

    fn evaluate(&mut self, source: Source) -> Option<Update> {
        let provisional = self.state.provisional(source)?;
        let signals = Signals {
            has_text: !provisional.text().trim().is_empty(),
            end_of_utterance: provisional.end_of_utterance_ms.is_some(),
            silence_ms: self.silence_for(source, provisional.end_ms, provisional.end_of_utterance_ms.is_some()),
            assessment: assess(provisional.text()),
        };
        let Decision::Commit(reason) = decide(&signals, &self.config) else { return None };
        let utterance = self.state.commit(source)?;
        self.mark(Stage::UtteranceCommitted, source, utterance.id, Some(utterance.end_ms));
        self.questions.remove(&utterance.id);
        Some(Update::Committed { utterance, reason })
    }

    /// How long the speaker has been quiet, from two independent signals: audio energy since
    /// the later of the last voice and the last text (so words still arriving aren't cut off),
    /// and the recognizer producing nothing new. The second one allows for its normal lag,
    /// and covers microphones whose noise bed hides the energy gaps.
    fn silence_for(&self, source: Source, text_end_ms: f64, end_of_utterance: bool) -> f64 {
        let tracker = &self.silence[slot(source)];
        // Until the recognizer has been quiet for its own lag, more words for this stretch may
        // still be on the way (Deepgram's last segment arrives ~0.6 s after speech stops); an
        // end-of-utterance says it has finished, so that path isn't held back.
        let since_last_event = (tracker.latest_end_ms() - self.last_event_ms[slot(source)]).max(0.0);
        let energy = tracker.silence_since(text_end_ms);
        // After an end-of-utterance the recognizer has said it is done with this stretch, so the
        // time since that signal counts in full. This is what endpoints desktop audio with
        // music or another voice underneath, where energy never reads as silence.
        if end_of_utterance { return energy.max(since_last_event); }
        if since_last_event < self.text_lag_ms { return 0.0; }
        // Measured from when the last event arrived, not the audio it covered: cloud recognizers
        // report a window that ends well before the audio they have already consumed.
        let no_new_text = (tracker.latest_end_ms() - text_end_ms.max(self.last_event_ms[slot(source)]) - self.text_lag_ms).max(0.0);
        energy.max(no_new_text)
    }

    fn mark(&self, stage: Stage, source: Source, id: UtteranceId, audio_ms: Option<f64>) {
        let Some(recorder) = &self.recorder else { return };
        let mut context = Context::source(source).utterance(id);
        // ASR stages belong to the provider; later stages are shared across providers.
        if matches!(stage, Stage::AsrPartial | Stage::AsrEndOfUtterance | Stage::AsrFinal) { context = context.provider(self.provider.clone()); }
        if let Some(ms) = audio_ms { context = context.audio_ms(ms); }
        recorder.mark(stage, context);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    const GEN: Generation = Generation(7);

    fn event(source: Source, end_ms: f64, kind: EventKind) -> TranscriptEvent {
        TranscriptEvent { source, generation: GEN, start_ms: 0.0, end_ms, kind }
    }
    fn partial(source: Source, end_ms: f64, text: &str) -> TranscriptEvent {
        event(source, end_ms, EventKind::Partial { text: text.into(), stable_hint: Some(usize::MAX) })
    }
    /// 10 ms chunks at `level`. Speech-like levels get the short dips between syllables that
    /// real speech has (the noise floor is learned from them), so 20 ms of every 100 ms is quiet.
    fn audio(live: &mut LiveTranscript, source: Source, from_ms: f64, to_ms: f64, level: f32) -> Vec<Update> {
        let mut updates = Vec::new();
        let mut t = from_ms;
        while t < to_ms {
            let dip = level >= 0.01 && (t / 10.0).round() as i64 % 10 >= 8;
            let chunk_level = if dip { level * 0.1 } else { level };
            updates.extend(live.on_audio(&AudioChunk { source, start_ms: t, samples: vec![chunk_level; 160] }));
            t += 10.0;
        }
        updates
    }
    /// A constant level with no dips: a microphone's noise bed.
    fn noise(live: &mut LiveTranscript, source: Source, from_ms: f64, to_ms: f64, level: f32) -> Vec<Update> {
        let mut updates = Vec::new();
        let mut t = from_ms;
        while t < to_ms { updates.extend(live.on_audio(&AudioChunk { source, start_ms: t, samples: vec![level; 160] })); t += 10.0; }
        updates
    }
    fn committed(updates: &[Update]) -> Vec<(&str, Reason)> {
        updates.iter().filter_map(|u| match u { Update::Committed { utterance, reason } => Some((utterance.text.as_str(), *reason)), _ => None }).collect()
    }

    #[test]
    fn a_question_commits_after_eou_and_silence_not_on_eou_alone() {
        let recorder = LatencyRecorder::new(Instant::now());
        let mut live = LiveTranscript::new(EndpointConfig::default(), Some(recorder.clone()));
        live.set_generation(GEN, "parakeet-realtime");
        audio(&mut live, Source::Them, 0.0, 1500.0, 0.2);
        let updates = live.on_event(&partial(Source::Them, 1400.0, "so how would you design a distributed cache"));
        assert!(updates.iter().any(|u| matches!(u, Update::QuestionLikely { .. })));
        assert!(committed(&live.on_event(&event(Source::Them, 1500.0, EventKind::EndOfUtterance { text: "so how would you design a distributed cache".into() }))).is_empty());
        let updates = audio(&mut live, Source::Them, 1500.0, 1800.0, 0.001);
        assert_eq!(committed(&updates), [("so how would you design a distributed cache", Reason::EndOfUtterance)]);
        let stages: Vec<Stage> = recorder.snapshot().iter().map(|m| m.stage).collect();
        assert_eq!(stages, [Stage::AsrPartial, Stage::IntentDetected, Stage::AsrEndOfUtterance, Stage::UtteranceCommitted]);
        assert!(recorder.snapshot().iter().all(|m| m.utterance == Some(1)));
    }

    #[test]
    fn a_trailing_conjunction_keeps_listening_through_a_short_pause() {
        let mut live = LiveTranscript::new(EndpointConfig::default(), None);
        live.set_generation(GEN, "test");
        audio(&mut live, Source::Them, 0.0, 1000.0, 0.2);
        live.on_event(&partial(Source::Them, 1000.0, "would you use redis here or"));
        live.on_event(&event(Source::Them, 1000.0, EventKind::EndOfUtterance { text: "would you use redis here or".into() }));
        assert!(committed(&audio(&mut live, Source::Them, 1000.0, 1600.0, 0.001)).is_empty());
        audio(&mut live, Source::Them, 1600.0, 1900.0, 0.2);
        live.on_event(&partial(Source::Them, 1900.0, "would you use redis here or avoid"));
        audio(&mut live, Source::Them, 1900.0, 2400.0, 0.2);
        live.on_event(&partial(Source::Them, 2400.0, "would you use redis here or avoid caching entirely"));
        let updates = audio(&mut live, Source::Them, 2400.0, 3200.0, 0.001);
        assert_eq!(committed(&updates), [("would you use redis here or avoid caching entirely", Reason::Silence)]);
    }

    #[test]
    fn me_and_them_endpoint_independently() {
        let mut live = LiveTranscript::new(EndpointConfig::default(), None);
        live.set_generation(GEN, "test");
        audio(&mut live, Source::Me, 0.0, 500.0, 0.2);
        audio(&mut live, Source::Them, 0.0, 500.0, 0.2);
        live.on_event(&partial(Source::Me, 500.0, "let me think about that"));
        live.on_event(&partial(Source::Them, 500.0, "what is a mutex"));
        let them = audio(&mut live, Source::Them, 500.0, 1100.0, 0.001);
        assert_eq!(committed(&them), [("what is a mutex", Reason::Silence)]);
        assert!(live.state().provisional(Source::Me).is_some());
    }

    /// Seen with Parakeet: speech ends, the endpointer sees silence, but the recognizer is
    /// still emitting the last words. They must land in the same utterance, and the late
    /// end-of-utterance that repeats the whole sentence must not commit it again.
    #[test]
    fn words_that_arrive_after_the_speech_stopped_stay_in_the_same_utterance() {
        let mut live = LiveTranscript::new(EndpointConfig::default(), None);
        live.set_generation(GEN, "parakeet-realtime");
        audio(&mut live, Source::Them, 0.0, 2000.0, 0.2);
        live.on_event(&partial(Source::Them, 1600.0, "walk me through what happens when two writers"));
        // Words keep arriving ~400 ms behind the audio; the request reads finished each time.
        assert!(committed(&audio(&mut live, Source::Them, 2000.0, 2400.0, 0.001)).is_empty());
        live.on_event(&partial(Source::Them, 2400.0, "walk me through what happens when two writers update the same key"));
        let updates = audio(&mut live, Source::Them, 2400.0, 3200.0, 0.001);
        assert_eq!(committed(&updates), [("walk me through what happens when two writers update the same key", Reason::Silence)]);
        let late = live.on_event(&event(Source::Them, 3400.0, EventKind::EndOfUtterance { text: "walk me through what happens when two writers update the same key".into() }));
        assert!(late.is_empty(), "{late:?}");
        assert!(committed(&audio(&mut live, Source::Them, 3400.0, 5400.0, 0.001)).is_empty());
        assert_eq!(live.state().committed(Source::Them).len(), 1);
    }

    /// Seen on a USB microphone: a constant noise bed kept every chunk "voiced", so Me never
    /// endpointed and one line grew forever. Quiet is also inferred from the recognizer.
    #[test]
    fn a_microphone_that_never_reads_as_silent_still_endpoints_when_words_stop() {
        let mut live = LiveTranscript::new(EndpointConfig::default(), None);
        live.set_generation(GEN, "parakeet-realtime");
        // Speech at 0.05 over a 0.03 noise bed never clears the voice threshold (3× the floor).
        noise(&mut live, Source::Me, 0.0, 3000.0, 0.03);
        noise(&mut live, Source::Me, 3000.0, 4500.0, 0.05);
        live.on_event(&partial(Source::Me, 4400.0, "i would start with a write through cache"));
        assert!(live.silence_for(Source::Me, 4400.0, false) < 1.0);
        let updates = noise(&mut live, Source::Me, 4500.0, 5600.0, 0.03);
        assert_eq!(committed(&updates), [("i would start with a write through cache", Reason::Silence)]);
        // With an EOU the commit follows the recognizer going quiet too.
        noise(&mut live, Source::Me, 5600.0, 6600.0, 0.05);
        live.on_event(&partial(Source::Me, 6500.0, "then i would add invalidation on every write"));
        live.on_event(&event(Source::Me, 6600.0, EventKind::EndOfUtterance { text: "then i would add invalidation on every write".into() }));
        let updates = noise(&mut live, Source::Me, 6600.0, 7400.0, 0.03);
        assert_eq!(committed(&updates), [("then i would add invalidation on every write", Reason::EndOfUtterance)]);
    }

    /// Seen with Deepgram: an interim result arrives a second or more after the audio it covers.
    /// Quiet is counted from when the recognizer last said anything, not from that window.
    #[test]
    fn a_recognizer_that_reports_late_windows_is_not_mistaken_for_quiet() {
        let mut live = LiveTranscript::new(EndpointConfig::default(), None);
        live.set_generation(GEN, "deepgram");
        audio(&mut live, Source::Them, 0.0, 3000.0, 0.2);
        // Arrives at 3.0 s but covers only the first second of a sentence still being spoken.
        let early = TranscriptEvent { start_ms: 0.0, ..partial(Source::Them, 1000.0, "walk me through what happens") };
        assert!(committed(&live.on_event(&early)).is_empty());
        assert!(committed(&audio(&mut live, Source::Them, 3000.0, 3400.0, 0.2)).is_empty());
        let full = TranscriptEvent { start_ms: 0.0, ..partial(Source::Them, 3000.0, "walk me through what happens when two writers update the same key") };
        live.on_event(&full);
        let updates = audio(&mut live, Source::Them, 3400.0, 4400.0, 0.001);
        assert_eq!(committed(&updates), [("walk me through what happens when two writers update the same key", Reason::Silence)]);
    }

    #[test]
    fn finishing_commits_whatever_is_in_progress_on_both_sources() {
        let mut live = LiveTranscript::new(EndpointConfig::default(), None);
        live.set_generation(GEN, "test");
        live.on_event(&partial(Source::Me, 300.0, "i was about to"));
        live.on_event(&partial(Source::Them, 400.0, "and"));
        let updates = live.finish();
        assert_eq!(committed(&updates), [("i was about to", Reason::Stopped), ("and", Reason::Stopped)]);
        assert!(live.finish().is_empty());
        assert_eq!(live.state().conversation().len(), 2);
    }

    #[test]
    fn events_from_another_generation_are_ignored() {
        let mut live = LiveTranscript::new(EndpointConfig::default(), None);
        live.set_generation(GEN, "test");
        let stale = TranscriptEvent { generation: Generation(6), ..partial(Source::Them, 100.0, "old provider") };
        assert!(live.on_event(&stale).is_empty());
        assert!(live.state().provisional(Source::Them).is_none());
    }
}
