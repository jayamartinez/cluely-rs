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

// v3 macOS settings + onboarding
/// The window's own background, under the glow of setup's windows.
pub fn window_bg() -> Rgba { rgb(0x0f1012) }
/// The Settings title bar and tabs, and the Listening tab's speech model panel.
pub fn title_bar() -> Rgba { rgb(0x141518) }
/// Cards on setup's pages, the Modes sidebar and its footer.
pub fn panel() -> Rgba { rgb(0x131416) }
/// A selected tab, segment or menu row.
pub fn selected_fill() -> Rgba { rgb(0x23262b) }
/// The chosen provider, mode or setup choice.
pub fn chosen() -> Rgba { rgb(0x13203a) }
/// Monochrome logos on their tiles.
pub fn logo_ink() -> Rgba { rgb(0xe8e4dc) }
/// The overlay scrollbar's thumb (Settings, the Modes sidebar).
pub fn scroll_thumb() -> Rgba { rgba(0x9c989080) }
/// A permission or key that failed, and Delete in menus.
pub fn danger() -> Rgba { rgb(0xffb4a8) }

#[cfg(not(target_os = "macos"))]
pub const FONT: &str = "Segoe UI";
#[cfg(not(target_os = "macos"))]
pub const MONO: &str = "Cascadia Mono";
// SF Pro, and Menlo for monospace (SF Mono isn't installed as a font family apps can name).
// Segoe UI and Cascadia Mono are Windows fonts.
#[cfg(target_os = "macos")]
pub const FONT: &str = ".SystemUIFont";
#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo";
