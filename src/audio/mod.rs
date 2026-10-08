//! Audio capture and normalization. Capture knows about devices; everything downstream only
//! sees canonical mono 16 kHz [`AudioChunk`]s tagged with their [`Source`].

pub mod capture;
pub mod frame;
#[cfg(any(target_os = "macos", test))]
pub mod gaps;
pub mod normalize;

pub use capture::{AudioCapture, DeviceList, Devices, SourceInfo, list_devices};
pub use frame::{AudioChunk, SAMPLE_RATE, Source};
pub use normalize::Normalizer;
