//! The provider abstraction. The rest of CluelyRS only talks to these traits, never to a
//! specific model's API.

use std::sync::mpsc::Sender;

use super::event::{Generation, TranscriptEvent};
use crate::audio::{AudioChunk, Source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locality {
    /// Runs entirely on this computer.
    OnDevice,
    /// Audio is streamed to a service.
    Cloud,
}

/// What a provider can do, so settings and the pipeline can adapt without naming models.
#[derive(Clone, Debug, PartialEq)]
pub struct Capabilities {
    /// Stable identifier used in settings, e.g. "parakeet-realtime".
    pub id: &'static str,
    pub label: &'static str,
    pub summary: &'static str,
    pub locality: Locality,
    pub requires_api_key: bool,
    /// Emits `EndOfUtterance` events (otherwise endpointing relies on silence and text only).
    pub emits_end_of_utterance: bool,
    /// BCP-47 language tags; empty means unspecified/multilingual.
    pub languages: &'static [&'static str],
    /// How far behind the audio the provider's words can arrive while someone is still talking.
    /// Endpointing counts quiet from the recognizer only beyond this.
    pub text_lag_ms: f64,
}

/// Whether a provider can start right now, and if not, what the user needs to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Availability {
    Ready,
    /// A local model needs downloading first.
    NeedsModel { download_bytes: u64 },
    /// A cloud provider has no API key configured.
    NeedsApiKey,
    Unavailable { reason: String },
}

#[derive(Debug)]
pub enum AsrError {
    NotReady(Availability),
    Failed(String),
}

impl std::fmt::Display for AsrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AsrError::NotReady(Availability::NeedsModel { .. }) => f.write_str("The transcription model isn't installed yet."),
            AsrError::NotReady(Availability::NeedsApiKey) => f.write_str("Add an API key for this transcription provider."),
            AsrError::NotReady(Availability::Unavailable { reason }) => f.write_str(reason),
            AsrError::NotReady(Availability::Ready) => f.write_str("The provider isn't ready."),
            AsrError::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AsrError {}

/// Where a session delivers its events. Cloneable so providers can emit from their own threads.
#[derive(Clone)]
pub struct EventSink {
    pub source: Source,
    pub generation: Generation,
    sender: Sender<TranscriptEvent>,
}

impl EventSink {
    pub fn new(source: Source, generation: Generation, sender: Sender<TranscriptEvent>) -> Self { Self { source, generation, sender } }

    /// Returns false once nobody is listening, so sessions can stop early.
    pub fn emit(&self, start_ms: f64, end_ms: f64, kind: super::EventKind) -> bool {
        self.sender.send(TranscriptEvent { source: self.source, generation: self.generation, start_ms, end_ms, kind }).is_ok()
    }
}

pub trait StreamingAsr: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    fn availability(&self) -> Availability;
    /// Start a session for one source. Events go to `sink`, possibly from other threads.
    fn start_session(&self, sink: EventSink) -> Result<Box<dyn StreamingAsrSession>, AsrError>;
}

/// One audio source's recognition stream. `push` may run inference synchronously; callers
/// run each session on its own worker thread, never on the UI thread.
pub trait StreamingAsrSession: Send {
    fn push(&mut self, chunk: &AudioChunk) -> Result<(), AsrError>;
    /// No more audio: flush anything pending (a final hypothesis, a closing EOU).
    fn finish(&mut self) -> Result<(), AsrError>;
}
