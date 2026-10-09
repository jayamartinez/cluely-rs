//! CluelyRS: a native, keyboard-driven desktop assistant overlay for Windows built on GPUI.
//!
//! The binary (`main.rs`) only opens the overlay window; everything else lives here so each
//! module is testable on its own.

pub mod answer;
pub mod archive;
pub mod assets;
pub mod audio;
pub mod capture;
pub mod chat;
pub mod claude_cli;
pub mod codex;
pub mod hotkeys;
pub mod input;
pub mod listening;
pub mod markdown;
pub mod metrics;
pub mod models;
pub mod modes;
pub mod modes_view;
pub mod notes;
pub mod overlay;
pub mod platform;
pub mod providers;
pub mod reasoning;
pub mod secrets;
pub mod sessions_window;
pub mod settings;
pub mod settings_view;
#[cfg(target_os = "macos")]
pub mod settings_window;
pub mod stt;
pub mod text_area;
pub mod theme;
pub mod toggles;
pub mod transcript;
pub mod transcript_view;
pub mod ui;
