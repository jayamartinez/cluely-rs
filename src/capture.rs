//! Screenshot-on-send. The overlay sets WDA_EXCLUDEFROMCAPTURE, so it never appears in
//! these captures itself; the image shows only what's underneath.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, RgbaImage};

/// Long edge cap: enough to read code and slides, small enough to keep requests fast.
const MAX_EDGE: u32 = 1600;
const QUALITY: u8 = 80;

/// Capture the monitor containing the screen point (x, y) as JPEG bytes.
pub fn screen_jpeg(x: i32, y: i32) -> anyhow::Result<Vec<u8>> {
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
}
