//! Win32 window behaviour GPUI does not expose: capture exclusion, topmost,
//! keyboard-driven movement and hide/show without destroying the window.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui::{Pixels, Size, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMNCRP_DISABLED, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, RGN_OR, SetWindowRgn,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_LBUTTON, VK_LEFT, VK_MENU, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::Foundation::{COLORREF, POINT};
use super::{FADE, POP_DISTANCE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetForegroundWindow, GetLayeredWindowAttributes, GetWindowLongPtrW, SetForegroundWindow, LWA_ALPHA, SetLayeredWindowAttributes,
    SetWindowLongPtrW, WS_EX_LAYERED, WS_EX_TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowRect, HWND_TOPMOST, IsWindowVisible, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE,
    SetWindowDisplayAffinity, SetWindowPos, ShowWindow, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
};

/// The overlay's own top-level window.
pub type NativeWindow = HWND;
/// The window that had the keyboard before the overlay took it.
pub type PreviousFocus = HWND;

pub fn native_window(window: &Window) -> Option<NativeWindow> {
    // Window has an inherent `window_handle()` returning GPUI's own handle type.
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut _)),
        _ => None,
    }
}

/// Keep the overlay out of screen shares, recordings and screenshots (Windows 10 2004+).
/// Older systems ignore the flag and show the window as a black rectangle instead.
pub fn set_capture_hidden(hwnd: HWND, hidden: bool) -> windows::core::Result<()> {
    unsafe { SetWindowDisplayAffinity(hwnd, if hidden { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE }) }
}

/// DWM draws a border, rounded corners and a drop shadow around every top-level window.
/// The overlay's transparent rectangle must be invisible, so remove all three: with
/// non-client rendering disabled DWM no longer paints the square shadow.
pub fn remove_frame(hwnd: HWND) {
    unsafe {
        let none = DWMWA_COLOR_NONE;
        let square = DWMWCP_DONOTROUND;
        let disabled = DWMNCRP_DISABLED;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_NCRENDERING_POLICY, &disabled as *const _ as *const _, size_of_val(&disabled) as u32);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &none as *const _ as *const _, size_of_val(&none) as u32);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &square as *const _ as *const _, size_of_val(&square) as u32);
    }
}

pub fn set_topmost(hwnd: HWND) -> windows::core::Result<()> {
    unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE) }
}

/// Resize the window to `size`, keeping its top-left corner.
pub fn resize(window: &mut Window, _hwnd: Option<HWND>, size: Size<Pixels>) { window.resize(size); }

/// Change the width to `width` (logical pixels) keeping the top edge and the horizontal centre of the
/// client area, within the work area of the window's monitor (`super::centred_left`). Like GPUI's
/// resize it doesn't run inside the current update: a short-lived thread asks for it, and the window's
/// own thread applies it once the update is done.
pub fn set_width_centred(hwnd: HWND, width: f32) {
    // HWND isn't Send; the handle stays valid for the app's lifetime.
    let raw = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("cluelyrs-width".into()).spawn(move || unsafe {
        let hwnd = HWND(raw as *mut _);
        let (mut outer, mut client, mut origin) = (RECT::default(), RECT::default(), POINT::default());
        if GetWindowRect(hwnd, &mut outer).is_err() || GetClientRect(hwnd, &mut client).is_err() || !ClientToScreen(hwnd, &mut origin).as_bool() { return; }
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let work = if GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() { info.rcWork } else { outer };
        let new_width = (f64::from(width) * f64::from(GetDpiForWindow(hwnd)) / 96.0).round();
        let old_width = f64::from(client.right - client.left);
        let left = super::centred_left(f64::from(origin.x), old_width, new_width, f64::from(work.left), f64::from(work.right)).round() as i32;
        // The outer frame keeps its invisible borders around the client area.
        let (border, extra) = (origin.x - outer.left, (outer.right - outer.left) - (client.right - client.left));
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), left - border, outer.top, new_width as i32 + extra, outer.bottom - outer.top, SWP_NOACTIVATE);
    });
}

/// Move by a physical-pixel delta, clamped to the work area of the window's monitor. Below
/// `content_bottom` (client pixels; the bottom of what is drawn) the window is transparent, so that
/// part may go past the work area's bottom edge.
pub fn move_by(hwnd: HWND, dx: i32, dy: i32, content_bottom: Option<i32>) -> windows::core::Result<()> {
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect)?;
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let work = if GetMonitorInfoW(monitor, &mut info).as_bool() { info.rcWork } else { rect };
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        // The outer frame keeps invisible borders above the client area (see `set_shape`).
        let mut client = POINT::default();
        let visible = match content_bottom {
            Some(bottom) if ClientToScreen(hwnd, &mut client).as_bool() => (client.y - rect.top + bottom).min(height),
            _ => height,
        };
        let x = (rect.left + dx).clamp(work.left, (work.right - width).max(work.left));
        let y = (rect.top + dy).clamp(work.top, (work.bottom - visible).max(work.top));
        SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE)
    }
}

fn down(key: VIRTUAL_KEY) -> bool { unsafe { GetAsyncKeyState(key.0 as i32) as u16 & 0x8000 != 0 } }

/// Arrow direction currently held with Ctrl+Alt (and Shift exactly when `with_shift`), or
/// `None` once the chord is released. Global hotkeys fire once per press, so held
/// movement polls the live keyboard state instead of waiting for repeats.
pub fn held_direction(with_shift: bool) -> Option<(i32, i32)> {
    if !down(VK_CONTROL) || !down(VK_MENU) || down(VK_SHIFT) != with_shift { return None; }
    let axis = |negative, positive| down(positive) as i32 - down(negative) as i32;
    let direction = (axis(VK_LEFT, VK_RIGHT), axis(VK_UP, VK_DOWN));
    (direction != (0, 0)).then_some(direction)
}

/// Whether the left mouse button is held right now, wherever the pointer is.
pub fn left_button_down() -> bool { down(VK_LBUTTON) }

/// A rounded rectangle in physical pixels relative to the window: left, top, right, bottom, radius.
pub type Shape = (i32, i32, i32, i32, i32);

/// Limit the window to `shapes`. Outside them Windows treats the overlay as absent, so
/// clicks and hover go straight to whatever is underneath.
pub fn set_shape(hwnd: HWND, shapes: &[Shape]) {
    unsafe {
        // Shapes are in client pixels, but a window region is relative to the window's outer
        // frame, which keeps invisible resize borders (8 px left, 1 px top here) even without a
        // caption. Without this offset the region sat left of the content and clipped its right edge.
        let mut frame = RECT::default();
        let _ = GetWindowRect(hwnd, &mut frame);
        let mut client = POINT::default();
        let _ = ClientToScreen(hwnd, &mut client);
        let (dx, dy) = (client.x - frame.left, client.y - frame.top);
        let region = CreateRectRgn(0, 0, 0, 0);
        for &(left, top, right, bottom, radius) in shapes {
            let part = CreateRoundRectRgn(left + dx, top + dy, right + dx + 1, bottom + dy + 1, radius * 2, radius * 2);
            CombineRgn(Some(region), Some(region), Some(part), RGN_OR);
            let _ = DeleteObject(part.into());
        }
        // The system owns the region after a successful call.
        if SetWindowRgn(hwnd, Some(region), true) == 0 { let _ = DeleteObject(region.into()); }
    }
}

/// Screen-space centre of the window, used to pick the monitor to capture.
pub fn center(hwnd: HWND) -> Option<(i32, i32)> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    Some(((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2))
}

/// Make the window layered once (fully opaque), so `set_mouse_passthrough` can toggle
/// WS_EX_TRANSPARENT; Windows only honours that flag for hit-testing on layered windows.
pub fn enable_passthrough(hwnd: HWND) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED.0 as isize);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
    }
}

/// While on, mouse input goes to whatever is underneath the overlay.
pub fn set_mouse_passthrough(hwnd: HWND, on: bool) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let next = if on { style | WS_EX_TRANSPARENT.0 as isize } else { style & !(WS_EX_TRANSPARENT.0 as isize) };
        if next != style { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next); }
    }
}

/// Cursor position relative to the window's top-left, in physical pixels.
pub fn cursor_in_window(hwnd: HWND) -> Option<(i32, i32)> {
    let mut point = POINT::default();
    let mut rect = RECT::default();
    unsafe { GetCursorPos(&mut point).ok()?; GetWindowRect(hwnd, &mut rect).ok()?; }
    Some((point.x - rect.left, point.y - rect.top))
}

fn foreground() -> Option<HWND> {
    let window = unsafe { GetForegroundWindow() };
    (!window.is_invalid()).then_some(window)
}

/// Bring a window to the foreground. Allowed here because it follows a hotkey press or a click.
fn activate(hwnd: HWND) { unsafe { let _ = SetForegroundWindow(hwnd); } }

/// Bring the overlay to the foreground, returning the window that had the keyboard if it was
/// another one.
pub fn take_focus(hwnd: HWND) -> Option<PreviousFocus> {
    let previous = foreground().filter(|previous| *previous != hwnd);
    activate(hwnd);
    previous
}

/// Hand the keyboard back to the window `take_focus` returned.
pub fn return_focus(previous: PreviousFocus) { activate(previous); }

/// Whether the overlay was last shown rather than hidden. A hide keeps the window on screen until its
/// fade ends, so the window's own visibility would lag a quick second toggle (as on macOS).
static SHOWN: AtomicBool = AtomicBool::new(true);
/// Bumped by every show and hide, so a fade that a newer one has overtaken stops.
static VISIBILITY_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Time between fade steps (about 120 per second; each is one layered-alpha update).
const FADE_STEP: Duration = Duration::from_millis(8);

pub fn is_visible(hwnd: HWND) -> bool { (unsafe { IsWindowVisible(hwnd).as_bool() }) && SHOWN.load(Ordering::Relaxed) }

/// Show without stealing focus from the app the user is working in. As on macOS, showing pops in (a
/// fade while settling `POP_DISTANCE` down into place) and hiding fades out, over `FADE`. The steps run
/// on a short-lived thread, so the UI thread never waits for them.
pub fn set_visible(hwnd: HWND, visible: bool) {
    let generation = VISIBILITY_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    SHOWN.store(visible, Ordering::Relaxed);
    // HWND isn't Send; the handle stays valid for the app's lifetime.
    let raw = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("cluelyrs-fade".into()).spawn(move || fade(HWND(raw as *mut _), visible, generation));
}

fn fade(hwnd: HWND, visible: bool, generation: u64) {
    let overtaken = || VISIBILITY_GENERATION.load(Ordering::Relaxed) != generation;
    unsafe {
        let was_visible = IsWindowVisible(hwnd).as_bool();
        if !visible && !was_visible { return; }
        // A fade that was overtaken continues from where it stopped.
        let mut alpha = 255u8;
        let from = if was_visible && GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), None).is_ok() { alpha as f32 / 255.0 } else { 0.0 };
        let to = if visible { 1.0 } else { 0.0 };
        let mut rest = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rest);
        let pop = if visible && !was_visible { (POP_DISTANCE * GetDpiForWindow(hwnd) as f64 / 96.0).round() as i32 } else { 0 };
        let place = |offset: i32| { let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), rest.left, rest.top - offset, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE); };
        if visible && !was_visible {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
            place(pop);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        let started = Instant::now();
        loop {
            if overtaken() {
                // The newer fade starts from wherever this one is; only the position must be at rest.
                if pop != 0 { place(0); }
                return;
            }
            let progress = ease_in_out((started.elapsed().as_secs_f32() / FADE.as_secs_f32()).min(1.0));
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), ((from + (to - from) * progress) * 255.0).round() as u8, LWA_ALPHA);
            if pop != 0 { place((pop as f32 * (1.0 - progress)).round() as i32); }
            if progress >= 1.0 { break; }
            std::thread::sleep(FADE_STEP);
        }
        if !visible && !overtaken() {
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        }
    }
}

/// Slow in, slow out, like the macOS animation's default timing.
fn ease_in_out(t: f32) -> f32 { if t < 0.5 { 2.0 * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(2) / 2.0 } }

/// Open a folder in File Explorer.
pub fn open_folder(path: &Path) -> std::io::Result<()> { Command::new("explorer.exe").arg(path).spawn().map(drop) }

/// Open File Explorer at a file's folder with the file selected.
pub fn reveal_file(path: &Path) -> std::io::Result<()> { Command::new("explorer.exe").arg("/select,").arg(path).spawn().map(drop) }
