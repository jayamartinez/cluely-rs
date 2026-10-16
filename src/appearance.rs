//! Settings › Window › Appearance: how see-through the overlay's card is, how large its reading text
//! is and how wide it is. Only the overlay follows these; the Settings, Sessions and Modes windows keep
//! their own look.

use gpui::{Rgba, rgba};
use serde::{Deserialize, Serialize};

/// Background opacity, in percent, unless the user picks another: see-through enough to keep the
/// app underneath in view, solid enough for the answer to read cleanly.
pub const DEFAULT_OPACITY: u8 = 75;
/// The opacity slider moves in steps of this many percent.
pub const OPACITY_STEP: u8 = 5;

/// `rgb` (0xRRGGBB) at `percent` opacity (values above 100 count as 100). Only the card's own fill
/// takes the setting; text, buttons and the hairline border stay solid at every value.
pub fn fill(rgb: u32, percent: u8) -> Rgba { rgba((rgb << 8) | alpha(percent)) }

/// A dim text colour (`theme::muted`, `theme::placeholder`) for text on the card at `percent`
/// opacity. At the default and above it stays as designed; below it, it moves toward
/// `theme::text` (all the way at `LEGIBLE_FLOOR`), so it still reads when a light window shows
/// through the card.
pub fn legible(dim: Rgba, percent: u8) -> Rgba {
    let lift = f32::from(DEFAULT_OPACITY.saturating_sub(percent).min(DEFAULT_OPACITY - LEGIBLE_FLOOR)) / f32::from(DEFAULT_OPACITY - LEGIBLE_FLOOR);
    let text = crate::theme::text();
    let mix = |from: f32, to: f32| from + (to - from) * lift;
    Rgba { r: mix(dim.r, text.r), g: mix(dim.g, text.g), b: mix(dim.b, text.b), a: dim.a }
}

/// The opacity at and below which dim text is as bright as the answer.
const LEGIBLE_FLOOR: u8 = 30;

/// `percent` as an 8-bit alpha, rounded: 80 % is 0xcc.
fn alpha(percent: u8) -> u32 { (u32::from(percent.min(100)) * 255 + 50) / 100 }

/// The opacity at `fraction` (0 to 1) of the slider's track, to the nearest step.
pub fn opacity_at(fraction: f32) -> u8 {
    let steps = f32::from(100 / OPACITY_STEP);
    (fraction.clamp(0.0, 1.0) * steps).round() as u8 * OPACITY_STEP
}

/// The size of the overlay's reading text: the answers and the transcript strip. The text box and
/// the toolbar keep theirs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextSize { Small, #[default] Standard, Large }

impl TextSize {
    pub const ALL: [(Self, &'static str); 3] = [(Self::Small, "Small"), (Self::Standard, "Default"), (Self::Large, "Large")];

    /// Answers' body text; headings and code scale from it.
    pub fn answer(self) -> f32 { match self { Self::Small => 13.0, Self::Standard => 14.0, Self::Large => 16.0 } }

    /// The transcript strip's text size and line height.
    pub fn transcript(self) -> (f32, f32) { match self { Self::Small => (12.0, 17.0), Self::Standard => (13.0, 18.0), Self::Large => (15.0, 21.0) } }
}

/// The overlay card's width. The window is a fixed margin wider, and the Windows Settings panel
/// under the card matches it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardWidth { Narrow, #[default] Standard, Wide }

impl CardWidth {
    pub const ALL: [(Self, &'static str); 3] = [(Self::Narrow, "Narrow"), (Self::Standard, "Default"), (Self::Wide, "Wide")];

    pub fn card(self) -> f32 { match self { Self::Narrow => 560.0, Self::Standard => crate::overlay::CARD_WIDTH, Self::Wide => 760.0 } }

    /// The overlay window: the card plus room either side.
    pub fn window(self) -> f32 { self.card() + crate::overlay::WIDTH - crate::overlay::CARD_WIDTH }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_opacity_is_three_quarters() {
        assert_eq!(fill(0x0f1012, DEFAULT_OPACITY), rgba(0x0f1012bf));
        assert_eq!(fill(0x121315, 80), rgba(0x121315cc));
    }

    #[test]
    fn opacity_covers_the_whole_range_and_clamps_above_it() {
        assert_eq!(fill(0x121315, 0), rgba(0x12131500), "0 % leaves only text, buttons and the border");
        assert_eq!(fill(0x121315, 100), rgba(0x121315ff));
        assert_eq!(fill(0x121315, 250), rgba(0x121315ff), "a hand-edited value past 100 counts as 100");
        assert_eq!(alpha(50), 128);
    }

    #[test]
    fn dim_text_brightens_only_below_the_default_opacity() {
        let dim = crate::theme::placeholder();
        assert_eq!(legible(dim, 100), dim);
        assert_eq!(legible(dim, DEFAULT_OPACITY), dim);
        assert_eq!(legible(dim, LEGIBLE_FLOOR), crate::theme::text());
        assert_eq!(legible(dim, 0), crate::theme::text());
        let halfway = legible(dim, 50);
        assert!(dim.r < halfway.r && halfway.r < crate::theme::text().r);
    }

    #[test]
    fn the_slider_snaps_to_five_percent_steps() {
        assert_eq!(opacity_at(0.0), 0);
        assert_eq!(opacity_at(1.0), 100);
        assert_eq!(opacity_at(0.8), 80);
        assert_eq!(opacity_at(0.42), 40);
        assert_eq!(opacity_at(0.43), 45);
        assert_eq!(opacity_at(-3.0), 0, "dragged past the left end");
        assert_eq!(opacity_at(7.0), 100, "dragged past the right end");
        assert!((0..=20).map(|step| opacity_at(step as f32 / 20.0)).all(|value| value % OPACITY_STEP == 0));
    }

    #[test]
    fn default_sizes_are_the_ones_the_overlay_always_had() {
        assert_eq!(TextSize::default().answer(), 14.0);
        assert_eq!(TextSize::default().transcript(), (13.0, 18.0));
        assert_eq!(CardWidth::default().card(), crate::overlay::CARD_WIDTH);
        assert_eq!(CardWidth::default().window(), crate::overlay::WIDTH);
        assert!(CardWidth::Narrow.card() < CardWidth::Standard.card() && CardWidth::Standard.card() < CardWidth::Wide.card());
        assert!(TextSize::Small.answer() < TextSize::Standard.answer() && TextSize::Standard.answer() < TextSize::Large.answer());
    }
}
