//! Audio capture and normalization. Capture knows about devices; everything downstream only
//! sees canonical mono 16 kHz [`AudioChunk`]s tagged with their [`Source`].

pub mod capture;
pub mod frame;
pub mod normalize;

pub use capture::{AudioCapture, SourceInfo};
pub use frame::{AudioChunk, SAMPLE_RATE, Source};
pub use normalize::Normalizer;
