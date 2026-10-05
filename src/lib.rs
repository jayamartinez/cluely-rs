//! CluelyRS: a native, keyboard-driven desktop assistant overlay for Windows built on GPUI.
//!
//! The binary (`main.rs`) only opens the overlay window; everything else lives here so each
//! module is testable on its own.

pub mod archive;
pub mod assets;
pub mod chat;
pub mod claude_cli;
pub mod hotkeys;
pub mod input;
pub mod markdown;
pub mod providers;
pub mod secrets;
pub mod settings;
pub mod theme;
pub mod ui;
pub mod win;
