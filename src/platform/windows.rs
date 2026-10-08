//! Win32 window behaviour GPUI does not expose: capture exclusion, topmost,
//! keyboard-driven movement and hide/show without destroying the window.

use std::path::Path;
use std::process::Command;

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
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, SetForegroundWindow, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED,
    WS_EX_TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, HWND_TOPMOST, IsWindowVisible, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE,
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

/// Move by a physical-pixel delta, clamped to the work area of the window's monitor.
pub fn move_by(hwnd: HWND, dx: i32, dy: i32) -> windows::core::Result<()> {
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect)?;
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let work = if GetMonitorInfoW(monitor, &mut info).as_bool() { info.rcWork } else { rect };
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let x = (rect.left + dx).clamp(work.left, (work.right - width).max(work.left));
        let y = (rect.top + dy).clamp(work.top, (work.bottom - height).max(work.top));
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

pub fn is_visible(hwnd: HWND) -> bool { unsafe { IsWindowVisible(hwnd).as_bool() } }

/// Show without stealing focus from the app the user is working in.
pub fn set_visible(hwnd: HWND, visible: bool) {
    unsafe { let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE }); }
}

/// Open a folder in File Explorer.
pub fn open_folder(path: &Path) -> std::io::Result<()> { Command::new("explorer.exe").arg(path).spawn().map(drop) }

/// Open File Explorer at a file's folder with the file selected.
pub fn reveal_file(path: &Path) -> std::io::Result<()> { Command::new("explorer.exe").arg("/select,").arg(path).spawn().map(drop) }
