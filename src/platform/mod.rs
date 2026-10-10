//! Everything that talks to the operating system directly: the overlay window's native
//! behaviour (capture exclusion, topmost, click-through shapes, focus, keyboard-driven movement)
//! and the system file manager. The rest of the crate is platform-independent and reaches the
//! OS only through this module.
//!
//! Each platform file provides the same items: `NativeWindow`, `PreviousFocus`, `Shape` and
//! the functions `overlay` and `sessions_window` call, though macOS's `take_focus` calls back once
//! CluelyRS is the active app rather than returning at once. macOS also provides system audio capture
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

/// Where a window's left edge goes when its width changes from `old_width` to `width`, keeping its
/// horizontal centre where it was (a centred overlay stays centred) but within the screen's visible
/// area `work_left..work_right`. A window wider than that area starts at its left edge. Any one unit:
/// macOS passes points, Windows physical pixels.
pub(crate) fn centred_left(left: f64, old_width: f64, width: f64, work_left: f64, work_right: f64) -> f64 {
    (left + (old_width - width) / 2.0).min(work_right - width).max(work_left)
}

#[cfg(test)]
mod tests {
    use super::centred_left;

    #[test]
    fn a_width_change_keeps_the_centre_within_the_screen_at_any_scale() {
        for scale in [1.0, 1.5, 2.0] {
            let (work_left, work_right) = (0.0, 1920.0 * scale);
            let (narrow, standard, wide) = (600.0 * scale, 680.0 * scale, 800.0 * scale);
            // Centred: stays centred, growing and shrinking evenly.
            let centred = (work_right - standard) / 2.0;
            assert_eq!(centred_left(centred, standard, wide, work_left, work_right), (work_right - wide) / 2.0);
            assert_eq!(centred_left(centred, standard, narrow, work_left, work_right), (work_right - narrow) / 2.0);
            // Off-centre: its own centre stays put.
            let left = 300.0 * scale;
            let wider = centred_left(left, standard, wide, work_left, work_right);
            assert_eq!(wider + wide / 2.0, left + standard / 2.0, "scale {scale}");
            // Back to the first width returns to the same place.
            assert_eq!(centred_left(wider, wide, standard, work_left, work_right), left);
            // Near an edge: kept on screen instead of spilling past it.
            assert_eq!(centred_left(work_left, standard, wide, work_left, work_right), work_left);
            assert_eq!(centred_left(work_right - standard, standard, wide, work_left, work_right), work_right - wide);
        }
    }

    #[test]
    fn the_visible_area_need_not_start_at_zero() {
        // A second screen to the left, or a Dock on the left of the main one.
        assert_eq!(centred_left(-1900.0, 680.0, 800.0, -1920.0, 0.0), -1920.0);
        assert_eq!(centred_left(100.0, 680.0, 800.0, 80.0, 1440.0), 80.0);
        // Wider than the visible area: starts at its left edge.
        assert_eq!(centred_left(500.0, 680.0, 2000.0, 0.0, 1440.0), 0.0);
    }
}
