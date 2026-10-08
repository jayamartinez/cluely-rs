//! Everything that talks to the operating system directly: the overlay window's native
//! behaviour (capture exclusion, topmost, click-through shapes, focus, keyboard-driven movement)
//! and the system file manager. The rest of the crate is platform-independent and reaches the
//! OS only through this module.
//!
//! Each platform file provides the same items: `NativeWindow`, `PreviousFocus`, `Shape` and
//! the functions `overlay` and `sessions_window` call.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;
