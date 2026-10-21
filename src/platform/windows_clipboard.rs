//! What the clipboard holds besides text, for pasting into the overlay's text box: the files
//! Explorer copied, and a bitmap. GPUI reads neither in a usable form: it joins copied file names
//! into one string without separators, and only reads images put there as PNG, JPEG or GIF, while
//! Print Screen, Snipping Tool and most apps' copy-image put a device-independent bitmap (CF_DIB).

use std::path::PathBuf;

use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

/// Standard clipboard formats (winuser.h).
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;
/// More files than a question takes are never needed; the rest are ignored.
const MAX_PATHS: u32 = 64;
/// A DIB larger than this (an 8K screen at 32 bits is about 130 MB) isn't read.
const MAX_DIB_BYTES: usize = 256 * 1024 * 1024;

/// The clipboard, open for this thread until dropped.
struct Clipboard;

impl Clipboard {
    fn open() -> Option<Self> { unsafe { OpenClipboard(None) }.ok().map(|_| Self) }

    fn data(&self, format: u32) -> Option<HANDLE> {
        unsafe { IsClipboardFormatAvailable(format) }.ok()?;
        unsafe { GetClipboardData(format) }.ok()
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) { let _ = unsafe { CloseClipboard() }; }
}

/// The files Explorer (or another app) copied, in order; empty when there are none.
pub fn pasted_files() -> Vec<PathBuf> {
    let Some(clipboard) = Clipboard::open() else { return Vec::new() };
    let Some(handle) = clipboard.data(CF_HDROP) else { return Vec::new() };
    let drop = HDROP(handle.0);
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) }.min(MAX_PATHS);
    (0..count).filter_map(|index| {
        let length = unsafe { DragQueryFileW(drop, index, None) } as usize;
        let mut name = vec![0u16; length + 1];
        let copied = unsafe { DragQueryFileW(drop, index, Some(&mut name)) } as usize;
        (copied > 0).then(|| PathBuf::from(String::from_utf16_lossy(&name[..copied])))
    }).collect()
}

/// The clipboard's bitmap as the bytes of a .bmp file, which the `image` crate decodes.
pub fn pasted_bitmap() -> Option<Vec<u8>> {
    let clipboard = Clipboard::open()?;
    let global = HGLOBAL(clipboard.data(CF_DIB)?.0);
    let size = unsafe { GlobalSize(global) };
    if size == 0 || size > MAX_DIB_BYTES { return None; }
    let data = unsafe { GlobalLock(global) };
    if data.is_null() { return None; }
    let dib = unsafe { std::slice::from_raw_parts(data as *const u8, size) }.to_vec();
    let _ = unsafe { GlobalUnlock(global) };
    drop(clipboard);
    bmp_file(&dib)
}

/// A packed DIB (BITMAPINFOHEADER or later, the colour table and the pixels) with the 14-byte
/// file header a .bmp file starts with. `None` when the header doesn't fit the data.
pub(crate) fn bmp_file(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |at: usize| dib.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u16_at = |at: usize| dib.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let header = u32_at(0)? as usize;
    if !(40..=124).contains(&header) { return None; }
    let (bits, compression, used) = (u16_at(14)?, u32_at(16)?, u32_at(32)? as usize);
    let palette = if used > 0 { used } else if bits <= 8 { 1 << bits } else { 0 };
    // BI_BITFIELDS with the original 40-byte header keeps its three masks after it.
    let masks = if header == 40 && compression == 3 { 12 } else { 0 };
    let pixels = 14 + header + masks + palette * 4;
    if pixels > 14 + dib.len() { return None; }
    let mut file = Vec::with_capacity(14 + dib.len());
    file.extend(b"BM");
    file.extend(u32::try_from(14 + dib.len()).ok()?.to_le_bytes());
    file.extend([0; 4]);
    file.extend(u32::try_from(pixels).ok()?.to_le_bytes());
    file.extend(dib);
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::bmp_file;

    /// A 2×1 24-bit DIB as the clipboard holds it, turned into a .bmp the attachment reader decodes.
    #[test]
    fn a_clipboard_dib_becomes_a_bmp_file() {
        let mut dib = Vec::new();
        dib.extend(40u32.to_le_bytes());
        dib.extend(2i32.to_le_bytes());
        dib.extend(1i32.to_le_bytes());
        dib.extend(1u16.to_le_bytes());
        dib.extend(24u16.to_le_bytes());
        dib.extend([0; 24]); // compression, size, resolution, colours
        dib.extend([0, 0, 255, 0, 255, 0, 0, 0]); // red, green, padded to 4 bytes
        let file = bmp_file(&dib).unwrap();
        assert_eq!(&file[..2], b"BM");
        let decoded = image::load_from_memory(&file).unwrap().to_rgb8();
        assert_eq!((decoded.width(), decoded.height()), (2, 1));
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0]);
        assert!(bmp_file(&[1, 2, 3]).is_none() && bmp_file(&dib[..30]).is_none());
    }
}
