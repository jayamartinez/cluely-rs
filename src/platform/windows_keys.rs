//! The Windows side of `shortcut_keys`: low-level keyboard and mouse hooks on a thread of their own.
//! Held-back keys and clicks are sent again with `SendInput`, marked so the hooks let them through.
//!
//! Windows doesn't call the hooks while an elevated window is in front, so `RegisterHotKey` stays
//! registered underneath and the shortcuts still work there, as they did before.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use global_hotkey::hotkey::Code;
use gpui::{Keystroke, Modifiers};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    MOUSE_EVENT_FLAGS, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEINPUT, MapVirtualKeyW, MAPVK_VK_TO_CHAR, SendInput, ToUnicodeEx, VIRTUAL_KEY, VK_CAPITAL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GA_ROOT, GetAncestor, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_UP, MSG,
    MSLLHOOKSTRUCT, SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_XBUTTONDOWN, WindowFromPoint,
};

use crate::shortcut_keys::{self, ALT, CONTROL, Chords, Fired, SHIFT, SUPER, Shortcut, Typed, Verdict};

/// Marks the keys and clicks CluelyRS sends again, so the hooks let them through untouched.
const REPLAYED: usize = 0x434C_5253;

const VK_TAB: u16 = 0x09;
const VK_ESCAPE: u16 = 0x1B;
const VK_PACKET: u16 = 0xE7;
const LOCKS: [u16; 3] = [0x14, 0x90, 0x91];

/// A key press or release as the hook saw it, kept to send again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key { vk: u16, scan: u16, extended: bool, up: bool }

fn modifier(vk: u16) -> Option<u8> {
    match vk {
        0xA2 | 0xA3 => Some(CONTROL),
        0xA4 | 0xA5 => Some(ALT),
        0xA0 | 0xA1 => Some(SHIFT),
        0x5B | 0x5C => Some(SUPER),
        _ => None,
    }
}

struct Hook {
    chords: Chords<Key>,
    fire: Box<dyn Fn(Fired) + Send>,
}

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);
/// Whether anything is held back or typed; lets mouse movement skip the lock.
static WATCHING_MOUSE: AtomicBool = AtomicBool::new(false);

/// Start hiding `shortcuts` (none claimed until `set_shortcut_active`). `fire` runs on the hook's
/// thread for a shortcut the hook completed.
pub fn hide_shortcuts(shortcuts: Vec<Shortcut>, fire: impl Fn(Fired) + Send + 'static) -> anyhow::Result<()> {
    *lock() = Some(Hook { chords: Chords::new(shortcuts, modifier), fire: Box::new(fire) });
    let (started, result) = std::sync::mpsc::channel();
    // Its own thread and message loop: keyboard input everywhere waits on a low-level hook, so it
    // must never sit behind a busy UI thread.
    std::thread::Builder::new().name("shortcut-keys".into()).spawn(move || unsafe {
        let module = GetModuleHandleW(None).ok().map(Into::into);
        let hooks = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0)
            .and_then(|_| SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0));
        let failed = hooks.is_err();
        let _ = started.send(hooks.map(drop));
        if failed { return; }
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {}
    })?;
    let started = result.recv()?;
    if started.is_err() { *lock() = None; }
    Ok(started?)
}

fn lock() -> std::sync::MutexGuard<'static, Option<Hook>> { HOOK.lock().unwrap_or_else(|poison| poison.into_inner()) }

fn with_chords<T>(f: impl FnOnce(&mut Chords<Key>) -> T) -> Option<T> { lock().as_mut().map(|hook| f(&mut hook.chords)) }

pub fn set_shortcut_active(index: usize, active: bool) { with_chords(|chords| chords.set_active(index, active)); }

/// Whether `vk` is down, counting keys the hook kept from the system (which `GetAsyncKeyState`
/// misses). VK_CONTROL, VK_MENU and VK_SHIFT count either side.
pub(super) fn key_held(vk: u16) -> bool {
    let sides: &[u16] = match vk { 0x11 => &[0xA2, 0xA3], 0x12 => &[0xA4, 0xA5], 0x10 => &[0xA0, 0xA1], _ => &[vk] };
    let now = Instant::now();
    with_chords(|chords| sides.iter().any(|side| chords.held(*side, now))).unwrap_or(false)
}

/// Send the overlay's typing to `overlay` without making it the foreground window, so the app in
/// front keeps focus. False if the hooks aren't running.
pub(super) fn start_typing(overlay: HWND) -> bool {
    let started = with_chords(|chords| chords.set_typing(true)).is_some();
    if started {
        shortcut_keys::set_typing_into(overlay.0 as usize);
        WATCHING_MOUSE.store(true, Ordering::Relaxed);
    }
    started
}

pub(super) fn stop_typing() {
    with_chords(|chords| chords.set_typing(false));
    shortcut_keys::set_typing_into(0);
}

pub(super) fn typing_into(window: HWND) -> bool { shortcut_keys::typing_into() == window.0 as usize && window.0 as usize != 0 }

/// Typing ends without the overlay asking: tell it, so it stops showing a caret.
fn typing_lost(chords: &mut Chords<Key>) {
    chords.set_typing(false);
    shortcut_keys::set_typing_into(0);
    shortcut_keys::send_typed(Typed::Lost);
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    // VK_PACKET carries a character another program typed, never a key of a shortcut.
    if code < 0 || info.dwExtraInfo == REPLAYED || info.vkCode as u16 == VK_PACKET { return unsafe { CallNextHookEx(None, code, wparam, lparam) }; }
    let key = Key { vk: info.vkCode as u16, scan: info.scanCode as u16, extended: info.flags.contains(LLKHF_EXTENDED), up: info.flags.contains(LLKHF_UP) };
    let (verdict, typed) = {
        let mut hook = lock();
        let Some(hook) = hook.as_mut() else { return unsafe { CallNextHookEx(None, code, wparam, lparam) } };
        let chords = &mut hook.chords;
        // Caps Lock and Num Lock keep working for every app; the overlay reads their state.
        if chords.typing() && LOCKS.contains(&key.vk) { return unsafe { CallNextHookEx(None, code, wparam, lparam) }; }
        // Switching apps (Alt+Tab, the Windows key, Ctrl+Esc) ends typing and works as usual.
        if chords.typing() && !key.up && leaves_typing(key.vk, chords.held_mods()) { typing_lost(chords); }
        let (verdict, fired) = chords.key(key.vk, key.up, key, Instant::now());
        if let Some(fired) = fired { (hook.fire)(fired); }
        let typed = matches!(verdict, Verdict::Type).then(|| keystroke(key, &hook.chords));
        WATCHING_MOUSE.store(hook.chords.hiding() || hook.chords.typing(), Ordering::Relaxed);
        (verdict, typed)
    };
    if let Some(keystroke) = typed.flatten() { shortcut_keys::send_typed(Typed::Key(keystroke)); }
    match verdict {
        Verdict::Pass => unsafe { CallNextHookEx(None, code, wparam, lparam) },
        Verdict::Swallow | Verdict::Type => LRESULT(1),
        Verdict::Replay(keys) => {
            send(keys.iter().chain([&key]).map(keyboard_input).collect());
            LRESULT(1)
        }
    }
}

fn leaves_typing(vk: u16, mods: u8) -> bool {
    modifier(vk) == Some(SUPER) || mods & SUPER != 0 || (vk == VK_TAB && mods & ALT != 0) || (vk == VK_ESCAPE && mods & (ALT | CONTROL) != 0)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    if code < 0 || info.dwExtraInfo == REPLAYED || !WATCHING_MOUSE.load(Ordering::Relaxed) { return unsafe { CallNextHookEx(None, code, wparam, lparam) }; }
    let Some(press) = mouse_press(wparam.0 as u32, info.mouseData) else {
        with_chords(|chords| chords.mouse_move(Instant::now()));
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    };
    let verdict = with_chords(|chords| {
        // A click in another window ends typing into the overlay; clicks on the overlay keep it.
        if chords.typing() {
            let target = unsafe { GetAncestor(WindowFromPoint(info.pt), GA_ROOT) };
            if target.0 as usize != shortcut_keys::typing_into() { typing_lost(chords); }
        }
        let verdict = chords.mouse_press(Instant::now());
        WATCHING_MOUSE.store(chords.typing(), Ordering::Relaxed);
        verdict
    });
    match verdict {
        Some(Verdict::Replay(keys)) => {
            let mut inputs: Vec<INPUT> = keys.iter().map(keyboard_input).collect();
            inputs.push(press);
            send(inputs);
            LRESULT(1)
        }
        _ => unsafe { CallNextHookEx(None, code, wparam, lparam) },
    }
}

/// A key typed into the overlay, as GPUI's own Windows backend would describe it: named keys
/// ("enter", "left") carry no text, the rest are named by their unshifted character and carry the
/// text this keyboard layout types with the modifiers held.
fn keystroke(key: Key, chords: &Chords<Key>) -> Option<Keystroke> {
    let mods = chords.held_mods();
    let mut modifiers = Modifiers { control: mods & CONTROL != 0, alt: mods & ALT != 0, shift: mods & SHIFT != 0, platform: mods & SUPER != 0, function: false };
    if let Some(name) = named_key(key.vk) { return Some(Keystroke { modifiers, key: name.into(), key_char: None }); }
    let layout = unsafe { GetKeyboardLayout(GetWindowThreadProcessId(GetForegroundWindow(), None)) };
    let mut state = [0u8; 256];
    let now = Instant::now();
    for vk in 0..256u16 {
        if chords.held(vk, now) { state[vk as usize] = 0x80; }
    }
    for (generic, sides) in [(0x10, [0xA0, 0xA1]), (0x11, [0xA2, 0xA3]), (0x12, [0xA4, 0xA5])] {
        if sides.iter().any(|side| state[*side] != 0) { state[generic] = 0x80; }
    }
    if unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0 { state[VK_CAPITAL.0 as usize] |= 1; }
    let mut buffer = [0u16; 8];
    // Flag 4 leaves the layout's dead-key state alone, so the app in front isn't affected.
    let length = unsafe { ToUnicodeEx(key.vk as u32, key.scan as u32, &state, &mut buffer, 4, Some(layout)) };
    let text = (length > 0).then(|| String::from_utf16_lossy(&buffer[..length as usize])).filter(|text| !text.chars().any(char::is_control));
    // AltGr is Ctrl+Alt on Windows: a character typed with it is plain text, not a chord.
    if text.is_some() && modifiers.control && modifiers.alt { modifiers.control = false; modifiers.alt = false; }
    let name = char::from_u32(unsafe { MapVirtualKeyW(key.vk as u32, MAPVK_VK_TO_CHAR) } & 0xFFFF).filter(|name| *name != '\0')?;
    let key_char = text.filter(|_| !modifiers.control && !modifiers.platform);
    Some(Keystroke { modifiers, key: name.to_ascii_lowercase().to_string(), key_char })
}

fn named_key(vk: u16) -> Option<&'static str> {
    Some(match vk {
        0x20 => "space", 0x08 => "backspace", 0x0D => "enter", 0x09 => "tab", 0x1B => "escape",
        0x26 => "up", 0x28 => "down", 0x27 => "right", 0x25 => "left", 0x24 => "home", 0x23 => "end", 0x21 => "pageup", 0x22 => "pagedown",
        0x2D => "insert", 0x2E => "delete", 0x5D => "menu",
        0x70 => "f1", 0x71 => "f2", 0x72 => "f3", 0x73 => "f4", 0x74 => "f5", 0x75 => "f6", 0x76 => "f7", 0x77 => "f8", 0x78 => "f9", 0x79 => "f10",
        0x7A => "f11", 0x7B => "f12",
        _ => return None,
    })
}

/// A button press or wheel turn as input to send again; `None` for movement and releases.
fn mouse_press(message: u32, data: u32) -> Option<INPUT> {
    let high = data >> 16;
    let (flags, data) = match message {
        WM_LBUTTONDOWN => (MOUSEEVENTF_LEFTDOWN, 0),
        WM_RBUTTONDOWN => (MOUSEEVENTF_RIGHTDOWN, 0),
        WM_MBUTTONDOWN => (MOUSEEVENTF_MIDDLEDOWN, 0),
        WM_XBUTTONDOWN => (MOUSEEVENTF_XDOWN, high),
        // The wheel's delta is signed, in the high word.
        WM_MOUSEWHEEL => (MOUSEEVENTF_WHEEL, high as u16 as i16 as i32 as u32),
        WM_MOUSEHWHEEL => (MOUSEEVENTF_HWHEEL, high as u16 as i16 as i32 as u32),
        _ => return None,
    };
    Some(mouse_input(flags, data))
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, data: u32) -> INPUT {
    INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: REPLAYED } } }
}

fn keyboard_input(key: &Key) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if key.extended { flags |= KEYEVENTF_EXTENDEDKEY; }
    if key.up { flags |= KEYEVENTF_KEYUP; }
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(key.vk), wScan: key.scan, dwFlags: flags, time: 0, dwExtraInfo: REPLAYED } } }
}

fn send(inputs: Vec<INPUT>) {
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
}

/// The virtual key `RegisterHotKey` is given for `code`, as `global-hotkey` maps it.
pub fn shortcut_key(code: Code) -> Option<u16> {
    use Code::*;
    const LETTERS: [Code; 26] = [KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT,
        KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ];
    const DIGITS: [Code; 10] = [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9];
    const NUMPAD: [Code; 10] = [Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9];
    const FUNCTION: [Code; 24] = [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23, F24];
    let at = |table: &[Code], first: u16| table.iter().position(|each| *each == code).map(|index| first + index as u16);
    if let Some(vk) = at(&LETTERS, 0x41).or_else(|| at(&DIGITS, 0x30)).or_else(|| at(&NUMPAD, 0x60)).or_else(|| at(&FUNCTION, 0x70)) { return Some(vk); }
    Some(match code {
        Equal => 0xBB, Comma => 0xBC, Minus => 0xBD, Period => 0xBE, Semicolon => 0xBA, Slash => 0xBF, Backquote => 0xC0,
        BracketLeft => 0xDB, Backslash => 0xDC, BracketRight => 0xDD, Quote => 0xDE,
        Backspace => 0x08, Tab => 0x09, Space => 0x20, Enter | NumpadEnter => 0x0D, Escape => 0x1B,
        PageUp => 0x21, PageDown => 0x22, End => 0x23, Home => 0x24, ArrowLeft => 0x25, ArrowUp => 0x26, ArrowRight => 0x27, ArrowDown => 0x28,
        Insert => 0x2D, Delete => 0x2E, PrintScreen => 0x2C, Pause => 0x13,
        NumpadAdd => 0x6B, NumpadSubtract => 0x6D, NumpadMultiply => 0x6A, NumpadDecimal => 0x6E, NumpadDivide => 0x6F,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_map_to_the_virtual_keys_registered_for_them() {
        assert_eq!(shortcut_key(Code::Enter), Some(0x0D));
        assert_eq!(shortcut_key(Code::Quote), Some(0xDE), "VK_OEM_7, the key right of ; on a US layout");
        assert_eq!(shortcut_key(Code::Backslash), Some(0xDC));
        assert_eq!(shortcut_key(Code::KeyQ), Some(0x51));
        assert_eq!(shortcut_key(Code::F12), Some(0x7B));
    }

    #[test]
    fn switching_apps_ends_typing() {
        assert!(leaves_typing(VK_TAB, ALT));
        assert!(leaves_typing(0x5B, 0));
        assert!(leaves_typing(VK_ESCAPE, CONTROL | SHIFT));
        assert!(!leaves_typing(VK_TAB, SHIFT), "Shift+Tab is typing");
        assert!(!leaves_typing(VK_ESCAPE, 0), "Esc is the overlay's own");
    }
}
