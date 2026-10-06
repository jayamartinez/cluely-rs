//! Streaming speech-to-text. Providers implement [`StreamingAsr`]; everything downstream
//! consumes provider-independent [`TranscriptEvent`]s.

pub mod event;
pub mod provider;
pub mod scripted;
pub mod transcriber;

pub use event::{EventKind, Generation, TranscriptEvent};
pub use provider::{AsrError, Availability, Capabilities, EventSink, Locality, StreamingAsr, StreamingAsrSession};
pub use transcriber::Transcriber;
