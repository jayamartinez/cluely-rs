//! What every streaming speech provider emits, independent of which provider it is.

use crate::audio::Source;

/// Identifies one provider configuration. Switching provider (or restarting it) starts a new
/// generation, and anything still in flight from an older one is discarded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub enum EventKind {
    /// Provisional hypothesis for the utterance in progress: the full text so far, which may
    /// still change. `stable_hint` is a provider's own claim of how many leading characters
    /// won't change, when it has one.
    Partial { text: String, stable_hint: Option<usize> },
    /// The provider's endpoint signal (Parakeet's `<EOU>` token, Deepgram's speech_final).
    /// One input to endpointing, not a commit by itself.
    EndOfUtterance { text: String },
    /// Text the provider won't revise any more for this span.
    Final { text: String },
    /// The session hit a problem; `fatal` sessions produce no further events.
    Error { message: String, fatal: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptEvent {
    pub source: Source,
    pub generation: Generation,
    /// Audio-time span the event covers, in session milliseconds (same clock as `AudioChunk`).
    pub start_ms: f64,
    pub end_ms: f64,
    pub kind: EventKind,
}

impl TranscriptEvent {
    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            EventKind::Partial { text, .. } | EventKind::EndOfUtterance { text } | EventKind::Final { text } => Some(text),
            EventKind::Error { .. } => None,
        }
    }
}
