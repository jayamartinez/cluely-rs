//! CluelyRS: a native, keyboard-driven desktop assistant overlay for Windows built on GPUI.
//!
//! The binary (`main.rs`) only opens the overlay window; everything else lives here so each
//! module is testable on its own.

pub mod assets;
pub mod theme;
pub mod win;
