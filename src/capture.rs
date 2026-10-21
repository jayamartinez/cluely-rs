//! Screenshot-on-send. CluelyRS's own windows never appear in these captures, whatever "Hide from
//! screen capture" says: while one is taken, every window of the app is excluded from capture, then
//! each goes back to the exclusion its setting asks for. The image shows only what's underneath.

use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;

use gpui::BackgroundExecutor;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};

use crate::platform::{self, NativeWindow};

/// Long edge cap: enough to read code and slides, small enough to keep requests fast.
const MAX_EDGE: u32 = 1600;
const QUALITY: u8 = 80;

/// Capture the monitor containing the screen point (x, y) as JPEG bytes, with CluelyRS's own windows
/// left out. Await it on the main thread: the windows' exclusion is changed there.
pub async fn screenshot(executor: &BackgroundExecutor, (x, y): (i32, i32)) -> Option<Vec<u8>> {
    let _hidden = OwnWindowsHidden::begin();
    executor.spawn(async move { screen_jpeg(x, y).ok() }).await
}

/// Keep `window` out of screen shares, recordings and screenshots, or let them show it. A screenshot
/// being taken keeps it out until the last one is done.
pub fn set_window_hidden(window: NativeWindow, hidden: bool) {
    let hidden = OWN_WINDOWS.with_borrow_mut(|own| own.apply(platform::window_key(window), hidden));
    if let Err(error) = platform::set_capture_hidden(window, hidden) { eprintln!("capture exclusion unavailable: {error}"); }
}

thread_local! {
    /// Main thread only, like the windows themselves.
    static OWN_WINDOWS: RefCell<OwnWindows> = RefCell::default();
}

/// The screenshots being taken, and the exclusion each window they hid goes back to after the last.
#[derive(Default)]
struct OwnWindows { captures: usize, restore: HashMap<usize, bool> }

impl OwnWindows {
    /// A screenshot starts: `set_all` hides every window. Those shown until now are shown again after
    /// the last screenshot.
    fn begin(&mut self, set_all: impl FnOnce(&mut dyn FnMut(usize, bool) -> bool)) {
        self.captures += 1;
        set_all(&mut |key, hidden| {
            if !hidden { self.restore.entry(key).or_insert(false); }
            true
        });
    }

    /// A screenshot is done. After the last one, `set_all` gives each window it hid the exclusion its
    /// setting asks for.
    fn end(&mut self, set_all: impl FnOnce(&mut dyn FnMut(usize, bool) -> bool)) {
        self.captures -= 1;
        if self.captures > 0 { return; }
        let restore = std::mem::take(&mut self.restore);
        set_all(&mut |key, hidden| restore.get(&key).copied().unwrap_or(hidden));
    }

    /// The exclusion a window's setting asks for, to set now. During a screenshot it is recorded for
    /// afterwards and the window stays hidden, including one opened since the screenshot began.
    fn apply(&mut self, key: usize, hidden: bool) -> bool {
        if self.captures == 0 { return hidden; }
        self.restore.insert(key, hidden);
        true
    }
}

/// Every window of the app hidden from capture while alive. Dropped on the main thread it began on
/// (it isn't `Send`), the last one gives each window back the exclusion its setting asks for.
struct OwnWindowsHidden(PhantomData<*const ()>);

impl OwnWindowsHidden {
    fn begin() -> Self {
        OWN_WINDOWS.with_borrow_mut(|own| own.begin(|decide| platform::set_own_windows_capture_hidden(decide)));
        Self(PhantomData)
    }
}

impl Drop for OwnWindowsHidden {
    fn drop(&mut self) {
        OWN_WINDOWS.with_borrow_mut(|own| own.end(|decide| platform::set_own_windows_capture_hidden(decide)));
    }
}

/// Capture the monitor containing the screen point (x, y) as JPEG bytes, exactly as it is shown.
fn screen_jpeg(x: i32, y: i32) -> anyhow::Result<Vec<u8>> {
    let monitor = xcap::Monitor::from_point(x, y).or_else(|_| {
        xcap::Monitor::all()?.into_iter().find(|m| m.is_primary().unwrap_or(false)).ok_or(xcap::XCapError::new("no monitor"))
    })?;
    encode(monitor.capture_image()?)
}

fn encode(image: RgbaImage) -> anyhow::Result<Vec<u8>> { jpeg(DynamicImage::ImageRgba8(image)) }

/// A picture as the JPEG sent to the model: its long edge at most `MAX_EDGE`. Screenshots and
/// attached images go the same way.
pub(crate) fn jpeg(mut picture: DynamicImage) -> anyhow::Result<Vec<u8>> {
    let (width, height) = (picture.width(), picture.height());
    let scale = (MAX_EDGE as f32 / width.max(height) as f32).min(1.0);
    if scale < 1.0 {
        picture = picture.resize(((width as f32 * scale) as u32).max(1), ((height as f32 * scale) as u32).max(1), FilterType::Triangle);
    }
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, QUALITY).encode_image(&picture.to_rgb8())?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_captures_are_scaled_to_the_long_edge_and_encoded_as_jpeg() {
        let bytes = encode(RgbaImage::from_pixel(3200, 1800, image::Rgba([30, 60, 90, 255]))).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1600, 900));
    }

    /// Windows by key and whether each is hidden from capture, changed as the platform would be.
    struct Screen(Vec<(usize, bool)>);

    impl Screen {
        fn set_all(&mut self) -> impl FnOnce(&mut dyn FnMut(usize, bool) -> bool) + '_ {
            |decide| for (key, hidden) in &mut self.0 { *hidden = decide(*key, *hidden); }
        }

        fn begin(&mut self, own: &mut OwnWindows) { own.begin(self.set_all()); }

        fn end(&mut self, own: &mut OwnWindows) { own.end(self.set_all()); }

        fn set(&mut self, own: &mut OwnWindows, key: usize, hidden: bool) {
            let applied = own.apply(key, hidden);
            match self.0.iter_mut().find(|(k, _)| *k == key) {
                Some((_, now)) => *now = applied,
                None => self.0.push((key, applied)),
            }
        }

        fn hidden(&self) -> Vec<bool> { self.0.iter().map(|(_, hidden)| *hidden).collect() }
    }

    #[test]
    fn a_screenshot_hides_every_window_then_restores_each_one() {
        let (mut own, mut screen) = (OwnWindows::default(), Screen(vec![(1, false), (2, true)]));
        screen.begin(&mut own);
        assert_eq!(screen.hidden(), [true, true]);
        screen.end(&mut own);
        assert_eq!(screen.hidden(), [false, true]);
    }

    #[test]
    fn overlapping_screenshots_restore_only_after_the_last() {
        let (mut own, mut screen) = (OwnWindows::default(), Screen(vec![(1, false)]));
        screen.begin(&mut own);
        screen.begin(&mut own);
        screen.end(&mut own);
        assert_eq!(screen.hidden(), [true]);
        screen.end(&mut own);
        assert_eq!(screen.hidden(), [false]);
    }

    #[test]
    fn a_setting_changed_during_a_screenshot_applies_after_it() {
        let (mut own, mut screen) = (OwnWindows::default(), Screen(vec![(1, false), (2, true)]));
        screen.begin(&mut own);
        screen.set(&mut own, 1, true);
        screen.set(&mut own, 2, false);
        assert_eq!(screen.hidden(), [true, true]);
        screen.end(&mut own);
        assert_eq!(screen.hidden(), [true, false]);
    }

    #[test]
    fn a_window_opened_during_a_screenshot_stays_out_of_it() {
        let (mut own, mut screen) = (OwnWindows::default(), Screen(vec![(1, true)]));
        screen.begin(&mut own);
        screen.set(&mut own, 3, false);
        assert_eq!(screen.hidden(), [true, true]);
        screen.end(&mut own);
        assert_eq!(screen.hidden(), [true, false]);
    }

    #[test]
    fn without_a_screenshot_the_setting_applies_at_once() {
        let (mut own, mut screen) = (OwnWindows::default(), Screen(vec![(1, true)]));
        screen.set(&mut own, 1, false);
        assert_eq!(screen.hidden(), [false]);
    }
}
