//! A small, always-on latency recorder. Stages push timestamped marks into a bounded buffer;
//! a session can be exported as JSON Lines for offline comparison (see `report`).

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::audio::Source;

/// Marks beyond this are dropped oldest-first, so a long session can't grow without bound.
pub const DEFAULT_CAPACITY: usize = 50_000;

/// Pipeline stages, in roughly the order they happen for one utterance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    AudioReceived,
    AsrPartial,
    AsrEndOfUtterance,
    AsrFinal,
    /// Endpointing decided the utterance is over and committed it.
    UtteranceCommitted,
    IntentDetected,
    ContextStarted,
    ContextCompleted,
    LlmRequestStarted,
    LlmFirstToken,
    ResponseCommitted,
    /// A speculative answer that wasn't shown was stopped (newer question, a miss, Live ended).
    SpeculationCancelled,
    /// A speculative answer started for a question nobody has asked for yet.
    SpeculationStarted,
    SpeculationFirstToken,
    /// The user asked for an answer (Assist, What do I say?, a typed question).
    AnswerRequested,
    /// The asked-for answer was ready and shown at once.
    SpeculationHit,
    /// A speculative answer was running but the conversation no longer matched what was asked.
    SpeculationMissed,
    /// The first words of the asked-for answer reached the screen.
    AnswerShown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub stage: Stage,
    /// Wall time in milliseconds since the session started (same origin as audio timestamps).
    pub at_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceTag>,
    /// Groups marks belonging to one utterance (assigned by transcript state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utterance: Option<u64>,
    /// Which ASR or reasoning provider produced this mark, for side-by-side comparisons.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Audio time the mark refers to (e.g. end of the audio a partial covers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_ms: Option<f64>,
}

/// Serializable mirror of [`Source`] (kept separate so audio types stay serde-free).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTag { Me, Them }

impl From<Source> for SourceTag {
    fn from(source: Source) -> Self { match source { Source::Me => SourceTag::Me, Source::Them => SourceTag::Them } }
}

/// Optional context attached to a mark.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub source: Option<Source>,
    pub utterance: Option<u64>,
    pub provider: Option<String>,
    pub audio_ms: Option<f64>,
}

impl Context {
    pub fn source(source: Source) -> Self { Self { source: Some(source), ..Self::default() } }
    pub fn utterance(mut self, id: u64) -> Self { self.utterance = Some(id); self }
    pub fn provider(mut self, id: impl Into<String>) -> Self { self.provider = Some(id.into()); self }
    pub fn audio_ms(mut self, ms: f64) -> Self { self.audio_ms = Some(ms); self }
}

struct Inner {
    origin: Instant,
    capacity: usize,
    marks: Mutex<VecDeque<Mark>>,
}

/// Cheap to clone and share across threads.
#[derive(Clone)]
pub struct LatencyRecorder(Arc<Inner>);

impl LatencyRecorder {
    /// `origin` should be the same instant the audio capture session started from.
    pub fn new(origin: Instant) -> Self { Self::with_capacity(origin, DEFAULT_CAPACITY) }

    pub fn with_capacity(origin: Instant, capacity: usize) -> Self {
        Self(Arc::new(Inner { origin, capacity: capacity.max(1), marks: Mutex::new(VecDeque::new()) }))
    }

    pub fn mark(&self, stage: Stage, context: Context) {
        let at_ms = self.0.origin.elapsed().as_secs_f64() * 1000.0;
        self.push(Mark { stage, at_ms, source: context.source.map(Into::into), utterance: context.utterance,
            provider: context.provider, audio_ms: context.audio_ms });
    }

    /// Record a mark with an explicit time (replaying logs, tests).
    pub fn push(&self, mark: Mark) {
        let mut marks = self.0.marks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if marks.len() == self.0.capacity { marks.pop_front(); }
        marks.push_back(mark);
    }

    pub fn snapshot(&self) -> Vec<Mark> {
        self.0.marks.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().cloned().collect()
    }

    /// Write every mark as one JSON object per line.
    pub fn write_jsonl(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
        let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
        for mark in self.snapshot() {
            serde_json::to_writer(&mut out, &mark).map_err(std::io::Error::other)?;
            out.write_all(b"\n")?;
        }
        out.flush()
    }
}

/// Read marks back from a JSON Lines file, skipping blank lines.
pub fn read_jsonl(text: &str) -> Result<Vec<Mark>, serde_json::Error> {
    text.lines().filter(|line| !line.trim().is_empty()).map(serde_json::from_str).collect()
}

/// Whether to write metrics files, from `CLUELYRS_METRICS=1`.
pub fn export_enabled() -> bool { std::env::var("CLUELYRS_METRICS").is_ok_and(|value| value == "1") }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_carry_context_on_the_session_clock() {
        let recorder = LatencyRecorder::new(Instant::now());
        recorder.mark(Stage::AsrPartial, Context::source(Source::Them).utterance(3).provider("parakeet-realtime").audio_ms(1200.0));
        let marks = recorder.snapshot();
        assert_eq!(marks.len(), 1);
        let mark = &marks[0];
        assert_eq!((mark.stage, mark.source, mark.utterance, mark.audio_ms), (Stage::AsrPartial, Some(SourceTag::Them), Some(3), Some(1200.0)));
        assert_eq!(mark.provider.as_deref(), Some("parakeet-realtime"));
        assert!(mark.at_ms >= 0.0 && mark.at_ms < 1000.0);
    }

    #[test]
    fn the_buffer_is_bounded_and_drops_the_oldest_marks() {
        let recorder = LatencyRecorder::with_capacity(Instant::now(), 3);
        for i in 0..5 {
            recorder.push(Mark { stage: Stage::AudioReceived, at_ms: i as f64, source: None, utterance: None, provider: None, audio_ms: None });
        }
        assert_eq!(recorder.snapshot().iter().map(|m| m.at_ms).collect::<Vec<_>>(), [2.0, 3.0, 4.0]);
    }

    #[test]
    fn jsonl_round_trips_and_omits_empty_fields() {
        let recorder = LatencyRecorder::new(Instant::now());
        recorder.push(Mark { stage: Stage::LlmFirstToken, at_ms: 812.5, source: None, utterance: Some(1), provider: Some("claude".into()), audio_ms: None });
        recorder.push(Mark { stage: Stage::ResponseCommitted, at_ms: 1500.0, source: Some(SourceTag::Me), utterance: Some(1), provider: None, audio_ms: None });
        let path = std::env::temp_dir().join(format!("cluelyrs-metrics-{}.jsonl", std::process::id()));
        recorder.write_jsonl(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(text.lines().next().unwrap().contains(r#""stage":"llm_first_token""#));
        assert!(!text.contains("audio_ms"));
        assert_eq!(read_jsonl(&text).unwrap(), recorder.snapshot());
    }
}
