//! A deterministic provider that replays a script against audio time. Used by tests and for
//! exercising the pipeline without a model.

use super::event::EventKind;
use super::provider::{AsrError, Availability, Capabilities, EventSink, Locality, StreamingAsr, StreamingAsrSession};
use crate::audio::{AudioChunk, Source};

#[derive(Clone, Debug)]
pub struct Step {
    pub source: Source,
    /// Emitted once the session has received audio up to this time.
    pub at_ms: f64,
    pub kind: EventKind,
}

impl Step {
    pub fn partial(source: Source, at_ms: f64, text: &str) -> Self {
        Self { source, at_ms, kind: EventKind::Partial { text: text.into(), stable_hint: None } }
    }
    pub fn eou(source: Source, at_ms: f64, text: &str) -> Self {
        Self { source, at_ms, kind: EventKind::EndOfUtterance { text: text.into() } }
    }
    pub fn final_text(source: Source, at_ms: f64, text: &str) -> Self {
        Self { source, at_ms, kind: EventKind::Final { text: text.into() } }
    }
}

pub struct ScriptedAsr {
    script: Vec<Step>,
    availability: Availability,
    fail_start: bool,
}

impl ScriptedAsr {
    pub fn new(script: Vec<Step>) -> Self { Self { script, availability: Availability::Ready, fail_start: false } }
    pub fn with_availability(mut self, availability: Availability) -> Self { self.availability = availability; self }
    pub fn failing_start(mut self) -> Self { self.fail_start = true; self }
}

impl StreamingAsr for ScriptedAsr {
    fn capabilities(&self) -> Capabilities {
        Capabilities { id: "scripted", label: "Scripted", summary: "Replays a fixed script (testing)", locality: Locality::OnDevice,
            requires_api_key: false, emits_end_of_utterance: true, languages: &["en"] }
    }

    fn availability(&self) -> Availability { self.availability.clone() }

    fn start_session(&self, sink: EventSink) -> Result<Box<dyn StreamingAsrSession>, AsrError> {
        if self.fail_start { return Err(AsrError::Failed("Scripted start failure.".into())); }
        let mut steps: Vec<Step> = self.script.iter().filter(|s| s.source == sink.source).cloned().collect();
        steps.sort_by(|a, b| a.at_ms.total_cmp(&b.at_ms));
        Ok(Box::new(ScriptedSession { sink, steps, next: 0, utterance_start: None }))
    }
}

struct ScriptedSession {
    sink: EventSink,
    steps: Vec<Step>,
    next: usize,
    utterance_start: Option<f64>,
}

impl StreamingAsrSession for ScriptedSession {
    fn push(&mut self, chunk: &AudioChunk) -> Result<(), AsrError> {
        let start = *self.utterance_start.get_or_insert(chunk.start_ms);
        while let Some(step) = self.steps.get(self.next).filter(|s| s.at_ms <= chunk.end_ms()) {
            self.sink.emit(start, step.at_ms, step.kind.clone());
            if matches!(step.kind, EventKind::EndOfUtterance { .. } | EventKind::Final { .. }) { self.utterance_start = None; }
            self.next += 1;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), AsrError> { Ok(()) }
}
