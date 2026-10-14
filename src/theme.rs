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

// v3 overlay + Windows

// Overlay
/// The overlay card's fills, as `u32` so `appearance::fill` can apply the background opacity: the card
/// (answers, toolbar) and its text box row.
pub const CARD: u32 = 0x0f1012;
pub const CARD_FIELD: u32 = 0x17191c;
/// Popovers on the card (the mode switcher) and the title-bar chrome.
pub fn chrome() -> Rgba { rgb(0x141518) }
/// A pressed control, a tooltip, the return and back buttons.
pub fn pressed() -> Rgba { rgb(0x23262b) }
/// The selected or keyboard-focused row (with `bubble_border`), and the Live waveform's button.
pub fn selected_row() -> Rgba { rgb(0x13203a) }
/// The mode chip's dashed edge while closed.
pub fn chip_edge() -> Rgba { rgb(0x3a3d42) }
/// Hover fills: a control, and a list row.
pub fn hover() -> Rgba { rgba(0xffffff0f) }
pub fn row_hover() -> Rgba { rgba(0xffffff08) }
/// Overlay radii, per platform (the lead's radius scale): Windows 11 is squarer.
#[cfg(not(target_os = "macos"))]
pub mod radius {
    /// The card, the mode switcher and the Sessions panel (which replaces the card body).
    pub const CARD: f32 = 8.0;
    pub const POPOVER: f32 = 8.0;
    /// Toolbar icon buttons, the mode chip, the tooltip, the Assist bubble, list rows and tiles.
    pub const ICON_BUTTON: f32 = 6.0;
    pub const CHIP: f32 = 6.0;
    pub const TOOLTIP: f32 = 6.0;
    pub const BUBBLE: f32 = 6.0;
    pub const ROW: f32 = 6.0;
    pub const SWITCHER_ROW: f32 = 6.0;
    pub const TILE: f32 = 6.0;
    pub const SMALL_TILE: f32 = 6.0;
    /// The return and back buttons, and keycaps.
    pub const BUTTON: f32 = 4.0;
    pub const KEYCAP: f32 = 4.0;
    /// The quick actions under an answer.
    pub const QUICK_ACTION: f32 = 6.0;
}
#[cfg(target_os = "macos")]
pub mod radius {
    pub const CARD: f32 = 12.0;
    pub const POPOVER: f32 = 12.0;
    pub const ICON_BUTTON: f32 = 8.0;
    pub const CHIP: f32 = 6.0;
    pub const TOOLTIP: f32 = 6.0;
    pub const BUBBLE: f32 = 12.0;
    pub const ROW: f32 = 12.0;
    pub const SWITCHER_ROW: f32 = 8.0;
    pub const TILE: f32 = 8.0;
    pub const SMALL_TILE: f32 = 6.0;
    pub const BUTTON: f32 = 8.0;
    pub const KEYCAP: f32 = 6.0;
    pub const QUICK_ACTION: f32 = 999.0;
}
/// Labels drawn at "medium" weight: Segoe UI has no 500, so Windows uses 600.
#[cfg(not(target_os = "macos"))]
pub const MEDIUM: gpui::FontWeight = gpui::FontWeight::SEMIBOLD;
#[cfg(target_os = "macos")]
pub const MEDIUM: gpui::FontWeight = gpui::FontWeight::MEDIUM;
// Windows Settings (Fluent): the Win11 Settings window (`settings_view::fluent`). Cards, fields and
// dividers reuse the tokens above (field, raised, hairline, divider, keycap_border, bubble_border).
/// Mica-dark window and content background.
pub fn fluent_window() -> Rgba { rgb(0x0f1012) }
/// Selected nav item and hovered rows.
pub fn fluent_selected() -> Rgba { rgb(0x23262b) }
/// The in-use / active SettingsCard.
pub fn fluent_in_use() -> Rgba { rgb(0x13203a) }
/// An expanded expander's body rows.
pub fn fluent_expander() -> Rgba { rgb(0x141518) }
/// The overlay scrollbar thumb: #9C9890 at 55%.
pub fn fluent_scrollbar() -> Rgba { rgba(0x9c98908c) }
/// The nav pane's app mark tile, top and bottom of its gradient.
pub const FLUENT_MARK_TOP: u32 = 0x24272c;
pub const FLUENT_MARK_BOTTOM: u32 = 0x0f1012;
