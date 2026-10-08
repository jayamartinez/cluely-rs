//! Everything that talks to the operating system directly: the overlay window's native
//! behaviour (capture exclusion, topmost, click-through shapes, focus, keyboard-driven movement)
//! and the system file manager. The rest of the crate is platform-independent and reaches the
//! OS only through this module.
//!
//! Each platform file provides the same items: `NativeWindow`, `PreviousFocus`, `Shape` and
//! the functions `overlay` and `sessions_window` call. macOS also provides system audio capture
//! (`SystemAudio`), which `audio::capture` uses where Windows has WASAPI loopback.

/// Showing and hiding the overlay fades it over this long; showing also settles it `POP_DISTANCE`
/// (logical pixels) down into place. The same on every platform.
pub(crate) const FADE: std::time::Duration = std::time::Duration::from_millis(140);
pub(crate) const POP_DISTANCE: f64 = 8.0;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;
#[cfg(target_os = "macos")]
mod macos_system_audio;
#[cfg(target_os = "macos")]
pub use self::macos_system_audio::{SystemAudio, SystemAudioBuffer, SystemAudioEvent};
