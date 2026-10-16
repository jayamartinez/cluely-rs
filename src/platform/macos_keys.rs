//! The macOS side of `shortcut_keys`: an event tap on a thread of its own. Held-back keys and clicks
//! are posted again from the tap, which sends them on past it, so it doesn't see them twice.
//!
//! macOS creates the tap only with the Accessibility permission (Privacy & Security › Accessibility).
//! Without it the shortcuts and the text box behave as before: the Carbon hot keys still work, and
//! typing activates CluelyRS.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, Ordering};
use std::time::Instant;

use global_hotkey::hotkey::Code;
use gpui::{Keystroke, Modifiers, Window};
use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::NativeWindow;
use crate::shortcut_keys::{self, ALT, CONTROL, Chords, Fired, SHIFT, SUPER, Shortcut, Typed, Verdict};

/// Marks the events CluelyRS posts again (in the event's user data field).
const REPLAYED: i64 = 0x434C_5253;

const TAB: u16 = 0x30;
const SPACE: u16 = 0x31;
const GRAVE: u16 = 0x32;
const CAPS_LOCK: u16 = 0x39;

/// A key event kept to post again. Core Graphics events are reference counted and safe to hand
/// between threads; only the tap's thread uses these.
struct Event(CFRetained<CGEvent>);
unsafe impl Send for Event {}

/// Which modifier a key code is, and its bit in the event's device-dependent flags, which tell
/// the left and right keys apart.
fn modifier_and_flag(code: u16) -> Option<(u8, u64)> {
    Some(match code {
        0x3B => (CONTROL, 0x0001), 0x3E => (CONTROL, 0x2000),
        0x38 => (SHIFT, 0x0002), 0x3C => (SHIFT, 0x0004),
        0x37 => (SUPER, 0x0008), 0x36 => (SUPER, 0x0010),
        0x3A => (ALT, 0x0020), 0x3D => (ALT, 0x0040),
        _ => return None,
    })
}

fn modifier(code: u16) -> Option<u8> { modifier_and_flag(code).map(|(bit, _)| bit) }

struct Hook {
    chords: Chords<Event>,
    fire: Box<dyn Fn(Fired) + Send>,
}

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);
/// The tap, to switch back on when macOS switches it off for being slow.
static TAP: AtomicPtr<CFMachPort> = AtomicPtr::new(std::ptr::null_mut());
static RUNNING: AtomicBool = AtomicBool::new(false);
/// The window server's number for the overlay while it is typed into; a click elsewhere ends typing.
static TYPING_WINDOW_NUMBER: AtomicIsize = AtomicIsize::new(0);

fn lock() -> std::sync::MutexGuard<'static, Option<Hook>> { HOOK.lock().unwrap_or_else(|poison| poison.into_inner()) }

fn with_chords<T>(f: impl FnOnce(&mut Chords<Event>) -> T) -> Option<T> { lock().as_mut().map(|hook| f(&mut hook.chords)) }

/// Start hiding `shortcuts` (none claimed until `set_shortcut_active`). `fire` runs on the tap's
/// thread for a shortcut the tap completed. Fails without the Accessibility permission.
pub fn hide_shortcuts(shortcuts: Vec<Shortcut>, fire: impl Fn(Fired) + Send + 'static) -> anyhow::Result<()> {
    *lock() = Some(Hook { chords: Chords::new(shortcuts, modifier), fire: Box::new(fire) });
    let (started, result) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("shortcut-keys".into()).spawn(move || {
        let kinds = [CGEventType::KeyDown, CGEventType::KeyUp, CGEventType::FlagsChanged, CGEventType::LeftMouseDown, CGEventType::RightMouseDown,
            CGEventType::OtherMouseDown, CGEventType::ScrollWheel, CGEventType::MouseMoved];
        let mask = kinds.iter().fold(0 as CGEventMask, |mask, kind| mask | 1 << kind.0);
        let tap = unsafe {
            CGEvent::tap_create(CGEventTapLocation::SessionEventTap, CGEventTapPlacement::HeadInsertEventTap, CGEventTapOptions::Default, mask, Some(tap),
                std::ptr::null_mut())
        };
        let Some(tap) = tap else {
            let _ = started.send(Err(anyhow::anyhow!("macOS refused the event tap; allow CluelyRS under Privacy & Security › Accessibility")));
            return;
        };
        let Some(source) = CFMachPort::new_run_loop_source(None, Some(&*tap), 0) else {
            let _ = started.send(Err(anyhow::anyhow!("no run loop source for the event tap")));
            return;
        };
        let Some(run_loop) = CFRunLoop::current() else { return };
        run_loop.add_source(Some(&*source), unsafe { kCFRunLoopCommonModes });
        CGEvent::tap_enable(&tap, true);
        TAP.store(CFRetained::as_ptr(&tap).as_ptr(), Ordering::Relaxed);
        RUNNING.store(true, Ordering::Relaxed);
        let _ = started.send(Ok(()));
        CFRunLoop::run();
    })?;
    let started = result.recv()?;
    if started.is_err() { *lock() = None; }
    started
}

pub fn set_shortcut_active(index: usize, active: bool) { with_chords(|chords| chords.set_active(index, active)); }

/// The modifiers held as the tap saw them, or `None` when it isn't running. Held-back modifiers
/// may never reach AppKit's own modifier state.
pub(super) fn held_mods() -> Option<u8> {
    if !RUNNING.load(Ordering::Relaxed) { return None; }
    with_chords(|chords| chords.held_mods())
}

/// The value `typing_into` compares: GPUI's view, read without retaining its window.
fn view_of(window: &Window) -> Option<usize> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::AppKit(handle) => Some(handle.ns_view.as_ptr() as usize),
        _ => None,
    }
}

/// Send the overlay's typing to `overlay` without activating CluelyRS, so the app in front keeps
/// focus. False if the tap isn't running. Main thread.
pub(super) fn start_typing(overlay: NativeWindow) -> bool {
    if !RUNNING.load(Ordering::Relaxed) || with_chords(|chords| chords.set_typing(true)).is_none() { return false; }
    TYPING_WINDOW_NUMBER.store(overlay.get().windowNumber(), Ordering::Relaxed);
    shortcut_keys::set_typing_into(overlay.view_pointer());
    true
}

pub(super) fn stop_typing() {
    with_chords(|chords| chords.set_typing(false));
    shortcut_keys::set_typing_into(0);
}

pub fn typing_into(window: &Window) -> bool { view_of(window).is_some_and(|view| view != 0 && view == shortcut_keys::typing_into()) }

/// Typing ends without the overlay asking: tell it, so it stops showing a caret.
fn typing_lost(chords: &mut Chords<Event>) {
    chords.set_typing(false);
    shortcut_keys::set_typing_into(0);
    shortcut_keys::send_typed(Typed::Lost);
}

unsafe extern "C-unwind" fn tap(_proxy: CGEventTapProxy, kind: CGEventType, event: NonNull<CGEvent>, _: *mut c_void) -> *mut CGEvent {
    let pass = event.as_ptr();
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        if let Some(tap) = NonNull::new(TAP.load(Ordering::Relaxed)) { CGEvent::tap_enable(unsafe { tap.as_ref() }, true); }
        return pass;
    }
    let event = unsafe { event.as_ref() };
    if CGEvent::integer_value_field(Some(event), CGEventField::EventSourceUserData) == REPLAYED { return pass; }
    match kind {
        CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged => key(kind, event),
        CGEventType::MouseMoved => {
            with_chords(|chords| chords.mouse_move(Instant::now()));
            pass
        }
        _ => mouse_press(event),
    }
}

fn key(kind: CGEventType, event: &CGEvent) -> *mut CGEvent {
    let pass = event as *const CGEvent as *mut CGEvent;
    let code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode) as u16;
    let up = match kind {
        CGEventType::KeyUp => true,
        // A modifier's press and release both arrive as a change of flags: it's down if its flag is set.
        CGEventType::FlagsChanged => match modifier_and_flag(code) {
            Some((_, flag)) => CGEvent::flags(Some(event)).0 & flag == 0,
            None => return pass,
        },
        _ => false,
    };
    let (verdict, typed) = {
        let mut hook = lock();
        let Some(hook) = hook.as_mut() else { return pass };
        let chords = &mut hook.chords;
        // Caps Lock keeps working everywhere; the overlay reads it from the event's flags.
        if chords.typing() && code == CAPS_LOCK { return pass; }
        // Switching apps or opening Spotlight ends typing and works as usual.
        if chords.typing() && !up && leaves_typing(code, chords.held_mods()) { typing_lost(chords); }
        let Some(copy) = CGEvent::new_copy(Some(event)) else { return pass };
        let (verdict, fired) = chords.key(code, up, Event(copy), Instant::now());
        if let Some(fired) = fired { (hook.fire)(fired); }
        let typed = matches!(verdict, Verdict::Type).then(|| keystroke(code, event, hook.chords.held_mods()));
        (verdict, typed)
    };
    if let Some(keystroke) = typed.flatten() { shortcut_keys::send_typed(Typed::Key(keystroke)); }
    match verdict {
        Verdict::Pass => pass,
        Verdict::Swallow | Verdict::Type => std::ptr::null_mut(),
        Verdict::Replay(events) => {
            for held in events { post(&held.0); }
            if let Some(again) = CGEvent::new_copy(Some(event)) { post(&again); }
            std::ptr::null_mut()
        }
    }
}

fn leaves_typing(code: u16, mods: u8) -> bool { mods & SUPER != 0 && matches!(code, TAB | GRAVE | SPACE) }

fn mouse_press(event: &CGEvent) -> *mut CGEvent {
    let pass = event as *const CGEvent as *mut CGEvent;
    let verdict = with_chords(|chords| {
        // A click in another window ends typing into the overlay; clicks on the overlay keep it.
        if chords.typing() {
            let under = CGEvent::integer_value_field(Some(event), CGEventField::MouseEventWindowUnderMousePointer) as isize;
            if under != TYPING_WINDOW_NUMBER.load(Ordering::Relaxed) { typing_lost(chords); }
        }
        chords.mouse_press(Instant::now())
    });
    match verdict {
        Some(Verdict::Replay(events)) => {
            for held in events { post(&held.0); }
            if let Some(again) = CGEvent::new_copy(Some(event)) { post(&again); }
            std::ptr::null_mut()
        }
        _ => pass,
    }
}

/// Post an event on past the tap, as if it had come from the keyboard or mouse.
fn post(event: &CGEvent) {
    CGEvent::set_integer_value_field(Some(event), CGEventField::EventSourceUserData, REPLAYED);
    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(event));
}

/// A key typed into the overlay, as GPUI's own macOS backend would describe it: named keys
/// ("enter", "left") carry no text; the rest are named by the character typed without modifiers
/// and carry the character typed with them, except with ⌘ or ⌃ held.
fn keystroke(code: u16, event: &CGEvent, mods: u8) -> Option<Keystroke> {
    let flags = CGEvent::flags(Some(event));
    let modifiers = Modifiers { control: mods & CONTROL != 0, alt: mods & ALT != 0, shift: mods & SHIFT != 0, platform: mods & SUPER != 0,
        function: flags.contains(CGEventFlags::MaskSecondaryFn) && named_key(code).is_none() };
    if let Some(name) = named_key(code) { return Some(Keystroke { modifiers, key: name.into(), key_char: None }); }
    let typed = characters(event);
    let plain = CGEvent::new_copy(Some(event)).and_then(|plain| {
        CGEvent::set_flags(Some(&*plain), CGEventFlags::empty());
        characters(&plain)
    })?;
    let key_char = typed.filter(|_| !modifiers.platform && !modifiers.control);
    Some(Keystroke { modifiers, key: plain.to_lowercase(), key_char })
}

fn characters(event: &CGEvent) -> Option<String> {
    let mut buffer = [0u16; 8];
    let mut length = 0;
    unsafe { CGEvent::keyboard_get_unicode_string(Some(event), buffer.len() as _, &mut length, buffer.as_mut_ptr()) };
    let text = String::from_utf16_lossy(&buffer[..length as usize]);
    (!text.is_empty() && !text.chars().any(char::is_control)).then_some(text)
}

fn named_key(code: u16) -> Option<&'static str> {
    Some(match code {
        0x24 | 0x4C => "enter", 0x30 => "tab", 0x31 => "space", 0x33 => "backspace", 0x75 => "delete", 0x35 => "escape",
        0x7B => "left", 0x7C => "right", 0x7D => "down", 0x7E => "up", 0x73 => "home", 0x77 => "end", 0x74 => "pageup", 0x79 => "pagedown",
        0x7A => "f1", 0x78 => "f2", 0x63 => "f3", 0x76 => "f4", 0x60 => "f5", 0x61 => "f6", 0x62 => "f7", 0x64 => "f8", 0x65 => "f9", 0x6D => "f10",
        0x67 => "f11", 0x6F => "f12",
        _ => return None,
    })
}

/// The virtual key code (kVK_*) of `code` on macOS, where Carbon's hot keys name keys by position.
pub fn shortcut_key(code: Code) -> Option<u16> {
    use Code::*;
    Some(match code {
        KeyA => 0x00, KeyS => 0x01, KeyD => 0x02, KeyF => 0x03, KeyH => 0x04, KeyG => 0x05, KeyZ => 0x06, KeyX => 0x07, KeyC => 0x08, KeyV => 0x09,
        KeyB => 0x0B, KeyQ => 0x0C, KeyW => 0x0D, KeyE => 0x0E, KeyR => 0x0F, KeyY => 0x10, KeyT => 0x11, Digit1 => 0x12, Digit2 => 0x13,
        Digit3 => 0x14, Digit4 => 0x15, Digit6 => 0x16, Digit5 => 0x17, Equal => 0x18, Digit9 => 0x19, Digit7 => 0x1A, Minus => 0x1B,
        Digit8 => 0x1C, Digit0 => 0x1D, BracketRight => 0x1E, KeyO => 0x1F, KeyU => 0x20, BracketLeft => 0x21, KeyI => 0x22, KeyP => 0x23,
        Enter => 0x24, KeyL => 0x25, KeyJ => 0x26, Quote => 0x27, KeyK => 0x28, Semicolon => 0x29, Backslash => 0x2A, Comma => 0x2B, Slash => 0x2C,
        KeyN => 0x2D, KeyM => 0x2E, Period => 0x2F, Tab => 0x30, Space => 0x31, Backquote => 0x32, Backspace => 0x33, Escape => 0x35,
        NumpadEnter => 0x4C, Home => 0x73, PageUp => 0x74, Delete => 0x75, End => 0x77, PageDown => 0x79,
        ArrowLeft => 0x7B, ArrowRight => 0x7C, ArrowDown => 0x7D, ArrowUp => 0x7E,
        F1 => 0x7A, F2 => 0x78, F3 => 0x63, F4 => 0x76, F5 => 0x60, F6 => 0x61, F7 => 0x62, F8 => 0x64, F9 => 0x65, F10 => 0x6D, F11 => 0x67,
        F12 => 0x6F,
        _ => return None,
    })
}
