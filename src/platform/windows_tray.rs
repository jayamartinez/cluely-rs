//! Windows: the notification-area (tray) icon, through `Shell_NotifyIconW`. A hidden window of its
//! own receives the icon's clicks; it is created on the GPUI main thread, so GPUI's message loop
//! serves it as it does the global shortcuts' (`hotkeys`), and clicks reach the overlay through a
//! channel. The icon is the app mark as a one-colour glyph that follows the taskbar's light or dark
//! theme, with a blue badge while Live is on (Paper "Tray · Windows · 1 Icon in the taskbar").
//! Everything here runs on the main thread.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFY_ICON_MESSAGE, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, GetCursorPos, HICON, LR_DEFAULTCOLOR,
    LookupIconIdFromDirectoryEx, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CONTEXTMENU,
    WM_DPICHANGED, WM_SETTINGCHANGE, WNDCLASSW,
};
use windows::core::w;

/// The icons, by taskbar theme, each with frames for 100–300 % scale.
const IDLE_DARK: &[u8] = include_bytes!("../../assets/tray/idle-dark.ico");
const LIVE_DARK: &[u8] = include_bytes!("../../assets/tray/live-dark.ico");
const IDLE_LIGHT: &[u8] = include_bytes!("../../assets/tray/idle-light.ico");
const LIVE_LIGHT: &[u8] = include_bytes!("../../assets/tray/live-light.ico");

/// The message the shell sends the hidden window about the icon.
const WM_TRAY: u32 = WM_APP + 1;
/// Enter or Space on the icon while it has the keyboard (Win+B): `NIN_SELECT | NINF_KEY`.
const NIN_KEYSELECT: u32 = NIN_SELECT + 1;

/// What happened to the icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// A left click, or Enter on the icon.
    Click,
    /// A right click, or Shift+F10 / the Menu key on the icon: open the menu at this point
    /// (physical screen pixels).
    Menu(i32, i32),
}

struct Tray {
    hwnd: HWND,
    shown: bool,
    live: bool,
    tip: String,
    icon: Option<HICON>,
    events: UnboundedSender<TrayEvent>,
    /// The last click, so the second click of a double click is ignored (it would hide what the first showed).
    last_click: Option<Instant>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

/// Sent to every top-level window when Explorer (and the taskbar) starts again; the icon must be added again.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// The tray icon. Hidden until `set_shown(true)`; removed when dropped.
pub struct TrayIcon(());

impl TrayIcon {
    /// Create the icon's window and the channel its clicks arrive on. `None` if Windows refuses.
    pub fn new() -> Option<(Self, UnboundedReceiver<TrayEvent>)> {
        let instance = unsafe { GetModuleHandleW(None) }.ok()?;
        let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: w!("CluelyRS.Tray"), ..Default::default() };
        // Fails harmlessly when the class is already registered.
        unsafe { RegisterClassW(&class) };
        TASKBAR_CREATED.store(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }, Ordering::Relaxed);
        // A top-level window that is never shown (a message-only one wouldn't hear TaskbarCreated
        // or theme changes).
        let hwnd = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("CluelyRS.Tray"), w!("CluelyRS"), WINDOW_STYLE(0), 0, 0, 0, 0, None, None, Some(instance.into()), None) }.ok()?;
        let (events, receiver) = unbounded();
        TRAY.with_borrow_mut(|tray| *tray = Some(Tray { hwnd, shown: false, live: false, tip: "CluelyRS".into(), icon: None, events, last_click: None }));
        Some((Self(()), receiver))
    }

    /// Put the icon in the notification area, or take it away.
    pub fn set_shown(&mut self, shown: bool) {
        with_tray(|tray| {
            if tray.shown == shown { return; }
            tray.shown = shown;
            if shown { add(tray); } else { notify(NIM_DELETE, tray); }
        });
    }

    /// While Live is on, the icon has a blue badge.
    pub fn set_live(&mut self, live: bool) {
        with_tray(|tray| {
            if tray.live == live { return; }
            tray.live = live;
            refresh_icon(tray);
        });
    }

    /// What hovering over the icon shows (at most 127 characters).
    pub fn set_tooltip(&mut self, tip: &str) {
        with_tray(|tray| {
            if tray.tip == tip { return; }
            tray.tip = tip.to_string();
            if tray.shown { notify(NIM_MODIFY, tray); }
        });
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        let Some(tray) = TRAY.with_borrow_mut(Option::take) else { return };
        if tray.shown { notify(NIM_DELETE, &tray); }
        unsafe {
            if let Some(icon) = tray.icon { let _ = DestroyIcon(icon); }
            let _ = DestroyWindow(tray.hwnd);
        }
    }
}

fn with_tray(f: impl FnOnce(&mut Tray)) {
    // The window procedure may run while the shell is being told something; it only reads then.
    TRAY.with(|tray| if let Ok(mut tray) = tray.try_borrow_mut() && let Some(tray) = tray.as_mut() { f(tray) });
}

fn add(tray: &mut Tray) {
    if tray.icon.is_none() { tray.icon = load_icon(tray.hwnd, tray.live); }
    notify(NIM_ADD, tray);
    // Version 4: clicks come as NIN_SELECT and WM_CONTEXTMENU, with the point in `wparam`.
    let mut data = data(tray);
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    unsafe { let _ = Shell_NotifyIconW(NIM_SETVERSION, &data); }
}

/// Pick the icon again: Live changed, or the taskbar's theme or scale did.
fn refresh_icon(tray: &mut Tray) {
    let old = std::mem::replace(&mut tray.icon, load_icon(tray.hwnd, tray.live));
    if tray.shown { notify(NIM_MODIFY, tray); }
    if let Some(old) = old { unsafe { let _ = DestroyIcon(old); } }
}

fn notify(message: NOTIFY_ICON_MESSAGE, tray: &Tray) { unsafe { let _ = Shell_NotifyIconW(message, &data(tray)); } }

fn data(tray: &Tray) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: tray.hwnd, uID: 1, uFlags: NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY, ..Default::default() };
    if let Some(icon) = tray.icon { data.hIcon = icon; data.uFlags |= NIF_ICON; }
    for (slot, unit) in data.szTip.iter_mut().zip(tray.tip.encode_utf16().take(127)) { *slot = unit; }
    data
}

/// The icon for the taskbar's theme at the small-icon size of the window's screen.
fn load_icon(hwnd: HWND, live: bool) -> Option<HICON> {
    let ico = match (taskbar_is_light(), live) { (false, false) => IDLE_DARK, (false, true) => LIVE_DARK, (true, false) => IDLE_LIGHT, (true, true) => LIVE_LIGHT };
    unsafe {
        let size = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(hwnd).max(96));
        // The best frame for that size, by its offset in the file.
        let offset = usize::try_from(LookupIconIdFromDirectoryEx(ico.as_ptr(), true, size, size, LR_DEFAULTCOLOR)).ok().filter(|&offset| offset > 0 && offset < ico.len())?;
        CreateIconFromResourceEx(&ico[offset..], true, 0x0003_0000, size, size, LR_DEFAULTCOLOR).ok()
    }
}

/// Whether the taskbar is light (Settings › Personalization › Colors › "Choose your default Windows
/// mode"), which isn't always the apps' mode.
fn taskbar_is_light() -> bool {
    let (mut value, mut size) = (0u32, size_of::<u32>() as u32);
    let read = unsafe { RegGetValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), w!("SystemUsesLightTheme"),
        RRF_RT_REG_DWORD, None, Some((&raw mut value).cast()), Some(&mut size)) };
    read.is_ok() && value == 1
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_TRAY => {
            let event = match (lparam.0 & 0xffff) as u32 {
                NIN_SELECT | NIN_KEYSELECT => Some(TrayEvent::Click),
                // The point where the menu goes, in `wparam` (version 4).
                WM_CONTEXTMENU => Some(TrayEvent::Menu((wparam.0 & 0xffff) as u16 as i16 as i32, ((wparam.0 >> 16) & 0xffff) as u16 as i16 as i32)),
                _ => None,
            };
            if let Some(event) = event { send(event); }
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_DPICHANGED => {
            with_tray(refresh_icon);
            if message == WM_DPICHANGED { LRESULT(0) } else { unsafe { DefWindowProcW(hwnd, message, wparam, lparam) } }
        }
        _ if message != 0 && message == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            with_tray(|tray| if tray.shown { add(tray) });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn send(event: TrayEvent) {
    with_tray(|tray| {
        if event == TrayEvent::Click {
            let double = Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }));
            if tray.last_click.is_some_and(|last| last.elapsed() < double) { return; }
            tray.last_click = Some(Instant::now());
        }
        let _ = tray.events.unbounded_send(event);
    });
}

/// A screen, in physical pixels: the point asked about, the screen's work area (without the
/// taskbar) as left, top, right, bottom, and its scale (1.0 at 100 %).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerScreen { pub point: (i32, i32), pub work: (i32, i32, i32, i32), pub scale: f32 }

/// The screen with `point` on it, or with the pointer when `None`.
pub fn pointer_screen(point: Option<(i32, i32)>) -> Option<PointerScreen> {
    let point = match point {
        Some((x, y)) => POINT { x, y },
        None => { let mut cursor = POINT::default(); unsafe { GetCursorPos(&mut cursor) }.ok()?; cursor }
    };
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() { return None; }
        let (mut dpi, mut _y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut _y);
        let work = info.rcWork;
        Some(PointerScreen { point: (point.x, point.y), work: (work.left, work.top, work.right, work.bottom), scale: dpi as f32 / 96.0 })
    }
}
