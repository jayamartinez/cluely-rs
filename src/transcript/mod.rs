//! Live transcript: stability tracking, intent heuristics, committed/provisional state and
//! endpointing. Provider-independent: everything here consumes `stt::TranscriptEvent`s.

pub mod stability;
pub mod intent;
