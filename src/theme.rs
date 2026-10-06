//! Color and type tokens from the Paper "Redesign" page (instrument-black glass, blue accent).

use gpui::{Rgba, rgb, rgba};

// Translucent glass, as in the Paper design; the desktop shows faintly through the overlay.
pub fn glass() -> Rgba { rgba(0x0f1012eb) }
pub fn raised() -> Rgba { rgb(0x1b1d20) }
pub fn field() -> Rgba { rgb(0x17191c) }
pub fn hairline() -> Rgba { rgb(0x2c2f33) }
pub fn divider() -> Rgba { rgb(0x22252a) }
pub fn keycap_border() -> Rgba { rgb(0x34373c) }
pub fn text() -> Rgba { rgb(0xf2efe8) }
pub fn body() -> Rgba { rgb(0xc9c5bc) }
pub fn muted() -> Rgba { rgb(0x9c9890) }
pub fn placeholder() -> Rgba { rgb(0x8a867e) }
pub fn accent() -> Rgba { rgb(0x4c8dff) }
pub fn accent_soft() -> Rgba { rgb(0x7aaeff) }
pub fn accent_ink() -> Rgba { rgb(0x061230) }
pub fn bubble_border() -> Rgba { rgb(0x2d4c85) }
pub fn bubble() -> Rgba { rgb(0x0e1a33) }
pub fn ok() -> Rgba { rgb(0x7dd3a0) }

pub const FONT: &str = "Segoe UI";
pub const MONO: &str = "Cascadia Mono";
