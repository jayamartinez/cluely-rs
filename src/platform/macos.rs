//! macOS window behaviour GPUI does not expose: capture exclusion, a borderless panel, click-through,
//! keyboard-driven movement and focus hand-off. GPUI already opens the overlay (`WindowKind::PopUp`)
//! as a non-activating NSPanel at the pop-up menu level that joins every Space, including full-screen
//! apps; this adds the rest of what the Windows overlay does. Everything here runs on the main thread,
//! where GPUI runs the overlay.

use std::path::Path;
use std::process::Command;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use gpui::{App, Pixels, Size, Window};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSRunningApplication, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowSharingType, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The overlay's own window. `native_window` keeps a strong reference for the rest of the
/// process, so the pointer stays valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeWindow { window: NonNull<NSWindow>, view: NonNull<NSView> }

impl NativeWindow {
    fn get(&self) -> &NSWindow { unsafe { self.window.as_ref() } }

    /// GPUI's view, which must be the first responder for key presses to reach GPUI.
    fn view(&self) -> &NSView { unsafe { self.view.as_ref() } }
}

/// The app that was frontmost before the overlay took the keyboard, and the overlay itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviousFocus { pid: i32, overlay: NativeWindow }

/// A rounded rectangle in physical pixels relative to the window: left, top, right, bottom, radius.
pub type Shape = (i32, i32, i32, i32, i32);

pub fn native_window(window: &Window) -> Option<NativeWindow> {
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
    // GPUI's view, alive as long as its window, which is retained here for the rest of the process.
    let view = handle.ns_view.cast::<NSView>();
    let window = NonNull::new(Retained::into_raw(unsafe { view.as_ref() }.window()?))?;
    Some(NativeWindow { window, view })
}

/// Keep the overlay out of screen shares, recordings and screenshots: the window server leaves
/// windows that don't allow sharing out of captures (see README for what that covers).
pub fn set_capture_hidden(window: NativeWindow, hidden: bool) -> std::io::Result<()> {
    window.get().setSharingType(if hidden { NSWindowSharingType::None } else { NSWindowSharingType::ReadOnly });
    Ok(())
}

/// GPUI's panel is titled, so macOS draws a hairline border and a shadow around its transparent
/// rectangle. Make it borderless (still non-activating) and drop the shadow.
pub fn remove_frame(window: NativeWindow) {
    let native = window.get();
    native.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
    native.setHasShadow(false);
    // Changing the style mask rebuilds the frame and leaves the window itself as first responder,
    // so key presses would never reach GPUI (the text box would show a caret but take no typing).
    native.makeFirstResponder(Some(window.view()));
}

/// GPUI already floats the panel above other apps' windows. Also keep it in place through Mission
/// Control and out of ⌘` window cycling, as the Windows overlay stays out of Alt+Tab.
pub fn set_topmost(window: NativeWindow) -> std::io::Result<()> {
    let window = window.get();
    window.setCollectionBehavior(window.collectionBehavior() | NSWindowCollectionBehavior::Stationary | NSWindowCollectionBehavior::IgnoresCycle);
    Ok(())
}

/// Resize to `size` (logical pixels, which are points here) keeping the top edge in place, as on
/// Windows. GPUI's resize keeps AppKit's bottom-left origin, so a taller overlay would grow upward
/// off the screen. Like GPUI's, the resize runs on the next turn of the main queue, so AppKit's
/// resize notifications never re-enter GPUI mid-update.
pub fn resize(window: &mut Window, native: Option<NativeWindow>, size: Size<Pixels>) {
    let Some(native) = native else { window.resize(size); return };
    struct OnMainQueue(NativeWindow);
    // The main queue runs on the main thread, where the window is used.
    unsafe impl Send for OnMainQueue {}
    impl OnMainQueue { fn window(&self) -> &NSWindow { self.0.get() } }
    let target = OnMainQueue(native);
    let (width, height) = (f64::from(f32::from(size.width)), f64::from(f32::from(size.height)));
    DispatchQueue::main().exec_async(move || {
        let window = target.window();
        let frame = window.frame();
        let top = frame.origin.y + frame.size.height;
        window.setFrame_display(NSRect::new(NSPoint::new(frame.origin.x, top - height), NSSize::new(width, height)), true);
    });
}

/// Move by a physical-pixel delta (y down, as on Windows), clamped to the visible frame of the
/// window's screen (below the menu bar, beside the Dock).
pub fn move_by(window: NativeWindow, dx: i32, dy: i32) -> std::io::Result<()> {
    let window = window.get();
    let scale = window.backingScaleFactor();
    let frame = window.frame();
    // AppKit's y axis points up.
    let (mut x, mut y) = (frame.origin.x + dx as f64 / scale, frame.origin.y - dy as f64 / scale);
    if let Some(screen) = window.screen() {
        let work = screen.visibleFrame();
        x = x.clamp(work.origin.x, (work.origin.x + work.size.width - frame.size.width).max(work.origin.x));
        y = y.clamp(work.origin.y, (work.origin.y + work.size.height - frame.size.height).max(work.origin.y));
    }
    window.setFrameOrigin(NSPoint::new(x, y));
    Ok(())
}

/// The arrows of the move and scroll shortcuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow { Up, Down, Left, Right }

/// Arrows currently held as part of a move or scroll shortcut (one bit per `Arrow`), recorded from
/// the shortcuts' own press and release events. Reading the live keyboard state instead would need
/// the Input Monitoring permission.
static HELD_ARROWS: AtomicU8 = AtomicU8::new(0);

pub fn set_arrow_held(arrow: Arrow, held: bool) {
    let bit = 1 << arrow as u8;
    if held { HELD_ARROWS.fetch_or(bit, Ordering::Relaxed); } else { HELD_ARROWS.fetch_and(!bit, Ordering::Relaxed); }
}

/// Arrow direction currently held with Control+Option (and Shift exactly when `with_shift`), or
/// `None` once the chord is released. Global hotkeys fire once per press, so held movement polls
/// instead of waiting for repeats: the modifiers from AppKit, the arrows from `set_arrow_held`.
pub fn held_direction(with_shift: bool) -> Option<(i32, i32)> {
    let flags = NSEvent::modifierFlags_class();
    if !flags.contains(NSEventModifierFlags::Control) || !flags.contains(NSEventModifierFlags::Option) {
        // A release that arrives after the modifiers are let go can be lost, so start clean.
        HELD_ARROWS.store(0, Ordering::Relaxed);
        return None;
    }
    if flags.contains(NSEventModifierFlags::Shift) != with_shift { return None; }
    let held = |arrow: Arrow| HELD_ARROWS.load(Ordering::Relaxed) & (1 << arrow as u8) != 0;
    let axis = |negative, positive| held(positive) as i32 - held(negative) as i32;
    let direction = (axis(Arrow::Left, Arrow::Right), axis(Arrow::Up, Arrow::Down));
    (direction != (0, 0)).then_some(direction)
}

/// Whether the left mouse button is held right now, wherever the pointer is.
pub fn left_button_down() -> bool { NSEvent::pressedMouseButtons() & 1 != 0 }

/// macOS has no window regions. Outside the drawn shapes the borderless panel is transparent, and
/// whether clicks reach the overlay is decided per cursor position by `set_mouse_passthrough`.
pub fn set_shape(_window: NativeWindow, _shapes: &[Shape]) {}

/// Centre of the window in CoreGraphics global coordinates (points, origin at the top-left of the
/// main display), which is how the screenshot code picks a monitor.
pub fn center(window: NativeWindow) -> Option<(i32, i32)> {
    let main = NSScreen::screens(MainThreadMarker::new()?).firstObject()?.frame();
    let frame = window.get().frame();
    let x = frame.origin.x + frame.size.width / 2.0;
    let y = main.size.height - (frame.origin.y + frame.size.height / 2.0);
    Some((x.round() as i32, y.round() as i32))
}

/// Nothing to prepare: any window can ignore mouse events.
pub fn enable_passthrough(_window: NativeWindow) {}

/// While on, mouse input goes to whatever is underneath the overlay.
pub fn set_mouse_passthrough(window: NativeWindow, on: bool) {
    let window = window.get();
    if window.ignoresMouseEvents() != on { window.setIgnoresMouseEvents(on); }
}

/// Cursor position relative to the window's top-left, in physical pixels.
pub fn cursor_in_window(window: NativeWindow) -> Option<(i32, i32)> {
    let window = window.get();
    let (scale, frame, cursor) = (window.backingScaleFactor(), window.frame(), NSEvent::mouseLocation());
    let x = (cursor.x - frame.origin.x) * scale;
    let y = (frame.origin.y + frame.size.height - cursor.y) * scale;
    Some((x.round() as i32, y.round() as i32))
}

/// Give the overlay the keyboard, returning the app that had it. Allowed because it follows a hotkey
/// press or a click.
pub fn take_focus(window: NativeWindow) -> Option<PreviousFocus> {
    let own = std::process::id() as i32;
    let previous = NSWorkspace::sharedWorkspace().frontmostApplication().map(|app| app.processIdentifier()).filter(|pid| *pid != own);
    // Keyboard input goes to the active app, so activate CluelyRS (as SetForegroundWindow does on
    // Windows) before making the panel key. Activating first also keeps GPUI from seeing a key
    // window in an inactive app, which it handles by resigning key status under a lock (a deadlock).
    if let Some(mtm) = MainThreadMarker::new() {
        // `activate()` exists only from macOS 14.
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
    window.get().makeKeyAndOrderFront(None);
    window.get().makeFirstResponder(Some(window.view()));
    previous.map(|pid| PreviousFocus { pid, overlay: window })
}

/// Hand the keyboard back to the app `take_focus` returned. That app usually stayed frontmost, so
/// the overlay also gives up key status: ordering it out and straight back in leaves it visible but
/// no longer key.
pub fn return_focus(previous: PreviousFocus) {
    let overlay = previous.overlay.get();
    if overlay.isKeyWindow() {
        overlay.orderOut(None);
        overlay.orderFrontRegardless();
    }
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(previous.pid) {
        app.activateWithOptions(NSApplicationActivationOptions::empty());
    }
}

/// Whether the overlay was last shown rather than hidden. A hide keeps the window on screen until its
/// fade ends, so the window's own visibility would lag a quick second toggle.
static SHOWN: AtomicBool = AtomicBool::new(true);
/// Bumped by every show and hide, so a hide whose fade ends after the overlay was shown again leaves it.
static VISIBILITY_GENERATION: AtomicU64 = AtomicU64::new(0);
const FADE_SECONDS: f64 = super::FADE.as_secs_f64();
use super::POP_DISTANCE;

pub fn is_visible(window: NativeWindow) -> bool { window.get().isVisible() && SHOWN.load(Ordering::Relaxed) }

/// Show without activating CluelyRS, so the app the user is working in keeps focus. Showing pops in
/// (a fade while settling into place); hiding fades out.
pub fn set_visible(window: NativeWindow, visible: bool) {
    let generation = VISIBILITY_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    SHOWN.store(visible, Ordering::Relaxed);
    let native = window.get();
    if visible {
        let rest = native.frame();
        if !native.isVisible() {
            native.setAlphaValue(0.0);
            native.setFrame_display(NSRect::new(NSPoint::new(rest.origin.x, rest.origin.y + POP_DISTANCE), rest.size), false);
        }
        native.orderFrontRegardless();
        // Only the frame as a whole animates (setFrame), not the origin alone, so the overlay always
        // ends exactly where it was.
        let changes = RcBlock::new(move |context: NonNull<NSAnimationContext>| {
            unsafe { context.as_ref() }.setDuration(FADE_SECONDS);
            let animator = window.get().animator();
            animator.setAlphaValue(1.0);
            animator.setFrame_display(rest, true);
        });
        NSAnimationContext::runAnimationGroup(&changes);
    } else {
        let changes = RcBlock::new(move |context: NonNull<NSAnimationContext>| {
            unsafe { context.as_ref() }.setDuration(FADE_SECONDS);
            window.get().animator().setAlphaValue(0.0);
        });
        let done = RcBlock::new(move || {
            if VISIBILITY_GENERATION.load(Ordering::Relaxed) != generation { return; }
            let native = window.get();
            native.orderOut(None);
            native.setAlphaValue(1.0);
        });
        NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&done));
    }
}

/// Show or hide CluelyRS in the Dock and ⌘Tab. Hidden, it is an accessory app: no Dock icon and
/// no menu bar of its own.
pub fn set_in_dock(shown: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let policy = if shown { NSApplicationActivationPolicy::Regular } else { NSApplicationActivationPolicy::Accessory };
    NSApplication::sharedApplication(mtm).setActivationPolicy(policy);
}

/// How long to wait for macOS to make CluelyRS the active app before showing a window anyway.
const ACTIVATION_WAIT: Duration = Duration::from_millis(600);
const ACTIVATION_POLL: Duration = Duration::from_millis(20);

fn app_is_active() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive())
}

/// Bring CluelyRS forward, then call `show` once macOS has made it the active app, saying whether
/// it is. GPUI deadlocks when one of its windows becomes key while the app isn't active: its
/// key-status handler resigns key status while holding the window's lock and re-enters itself. macOS
/// may take a moment to activate an app that asks, or decline (CluelyRS's overlay never activates
/// it), so `show` must make a window key only when told the app is active, and otherwise show it
/// without focus: clicking it then activates the app first.
pub fn activate_then(cx: &mut App, show: impl FnOnce(bool, &mut App) + 'static) {
    cx.activate(true);
    if app_is_active() {
        show(true, cx);
        return;
    }
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        let active = wait_until(app_is_active, ACTIVATION_WAIT, ACTIVATION_POLL, |poll| executor.timer(poll)).await;
        let _ = cx.update(|cx| show(active, cx));
    }).detach();
}

/// Check `ready` every `poll` (waiting with `sleep`) until it holds or `limit` has passed, and
/// return whether it held.
async fn wait_until<Sleep: Future<Output = ()>>(ready: impl Fn() -> bool, limit: Duration, poll: Duration, sleep: impl Fn(Duration) -> Sleep) -> bool {
    let mut waited = Duration::ZERO;
    while !ready() {
        if waited >= limit { return false; }
        sleep(poll).await;
        waited += poll;
    }
    true
}

/// Show `window` above other windows without making it key (see [`activate_then`]).
pub fn order_front(window: NativeWindow) {
    window.get().orderFront(None);
}

/// Open a folder in Finder.
pub fn open_folder(path: &Path) -> std::io::Result<()> { Command::new("open").arg(path).spawn().map(drop) }

/// Open Finder at a file's folder with the file selected.
pub fn reveal_file(path: &Path) -> std::io::Result<()> { Command::new("open").arg("-R").arg(path).spawn().map(drop) }

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::Duration;

    use futures::executor::block_on;

    use super::wait_until;

    const LIMIT: Duration = Duration::from_millis(600);
    const POLL: Duration = Duration::from_millis(20);

    /// Waits until `ready` has been asked `ready_on` times (never when `None`); returns the result
    /// and how many sleeps it took.
    fn wait(ready_on: Option<u32>) -> (bool, u32) {
        let (asked, slept) = (Cell::new(0), Cell::new(0));
        let ready = || { asked.set(asked.get() + 1); ready_on.is_some_and(|n| asked.get() >= n) };
        let sleep = |poll: Duration| { assert_eq!(poll, POLL); slept.set(slept.get() + 1); async {} };
        (block_on(wait_until(ready, LIMIT, POLL, sleep)), slept.get())
    }

    #[test]
    fn an_active_app_is_used_at_once() {
        assert_eq!(wait(Some(1)), (true, 0));
    }

    #[test]
    fn waits_for_the_app_to_become_active() {
        assert_eq!(wait(Some(4)), (true, 3));
    }

    #[test]
    fn activation_on_the_last_check_still_counts() {
        assert_eq!(wait(Some(31)), (true, 30));
    }

    #[test]
    fn gives_up_when_the_app_never_becomes_active() {
        assert_eq!(wait(None), (false, 30));
    }
}
