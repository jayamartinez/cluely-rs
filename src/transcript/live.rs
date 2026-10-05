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
}

fn slot(source: Source) -> usize { match source { Source::Me => 0, Source::Them => 1 } }

impl LiveTranscript {
    pub fn new(config: EndpointConfig, recorder: Option<LatencyRecorder>) -> Self {
        Self { state: TranscriptState::new(), silence: Default::default(), config, generation: None, provider: String::new(),
            recorder, questions: HashSet::new() }
    }

    pub fn state(&self) -> &TranscriptState { &self.state }

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

    fn evaluate(&mut self, source: Source) -> Option<Update> {
        let provisional = self.state.provisional(source)?;
        let signals = Signals {
            has_text: !provisional.text().trim().is_empty(),
            end_of_utterance: provisional.end_of_utterance_ms.is_some(),
            silence_ms: self.silence[slot(source)].silence_ms(),
            assessment: assess(provisional.text()),
        };
        let Decision::Commit(reason) = decide(&signals, &self.config) else { return None };
        let utterance = self.state.commit(source)?;
        self.mark(Stage::UtteranceCommitted, source, utterance.id, Some(utterance.end_ms));
        self.questions.remove(&utterance.id);
        Some(Update::Committed { utterance, reason })
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
    fn audio(live: &mut LiveTranscript, source: Source, from_ms: f64, to_ms: f64, level: f32) -> Vec<Update> {
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
        audio(&mut live, Source::Them, 1600.0, 2400.0, 0.2);
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

    #[test]
    fn events_from_another_generation_are_ignored() {
        let mut live = LiveTranscript::new(EndpointConfig::default(), None);
        live.set_generation(GEN, "test");
        let stale = TranscriptEvent { generation: Generation(6), ..partial(Source::Them, 100.0, "old provider") };
        assert!(live.on_event(&stale).is_empty());
        assert!(live.state().provisional(Source::Them).is_none());
    }
}
