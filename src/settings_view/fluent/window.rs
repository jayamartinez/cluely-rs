//! The Settings window itself: fixed 880 × 680, not resizable, with a 40 px title bar drawn by the
//! window (viewfinder, title, ─ □ ✕). The caption buttons and the drag area report themselves to the
//! system's hit test (`window_control_area`), as in the Sessions window; □ is dimmed, since the
//! window has one size. It shows accounts and keys, so it hides from screen capture while the overlay
//! does. Esc closes it (it has the keyboard; nothing global is claimed).

use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, KeyDownEvent, ParentElement, Render, Styled, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowControlArea, WindowKind, WindowOptions, div, prelude::*, px, rgb, size,
};

use crate::overlay::{self, Overlay};
use crate::platform::{self, NativeWindow};
use crate::settings_view::Tab;
use crate::{theme, ui};

pub(super) const WIDTH: f32 = 880.0;
pub(super) const HEIGHT: f32 = 680.0;
const TITLE: &str = "CluelyRS Settings";

pub struct FluentWindow {
    overlay: Entity<Overlay>,
    native: Option<NativeWindow>,
    /// The capture exclusion last applied, so it is only set when the setting changes.
    capture_hidden: Option<bool>,
    focus: FocusHandle,
    _overlay_changed: Subscription,
}

/// Show Settings on `tab` (and, on Modes, `mode`'s own page), opening the window or bringing it
/// forward. The caller has refreshed what the tab shows (`Overlay::prepare_tab`).
pub(crate) fn open(overlay: &mut Overlay, tab: Tab, mode: Option<String>, cx: &mut Context<Overlay>) {
    if overlay.fluent.tab != Some(tab) || overlay.fluent.mode_page != mode { overlay.fluent.scroll_to_top(); }
    overlay.fluent.tab = Some(tab);
    overlay.fluent.mode_page = mode;
    cx.notify();
    let (entity, existing) = (cx.entity(), overlay.fluent.window);
    // Deferred: the window reads the overlay while it draws, and the overlay is being updated now.
    cx.defer(move |cx| {
        // Restores a minimized window too.
        if let Some(handle) = existing && handle.update(cx, |_, window, cx| {
            window.activate_window();
            cx.notify();
        }).is_ok() { return; }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx))),
            titlebar: Some(TitlebarOptions { title: Some(TITLE.into()), appears_transparent: true, traffic_light_position: None }),
            kind: WindowKind::Normal,
            focus: true,
            show: true,
            is_resizable: false,
            is_minimizable: true,
            ..Default::default()
        };
        match cx.open_window(options, |window, cx| cx.new(|cx| FluentWindow::new(entity.clone(), window, cx))) {
            Ok(handle) => entity.update(cx, |overlay, _| overlay.fluent.window = Some(handle)),
            Err(error) => {
                eprintln!("settings window could not open: {error}");
                entity.update(cx, |overlay, _| overlay.fluent.tab = None);
            }
        }
    });
}

/// Close the window if it's open (Run setup again closes it as setup opens).
pub(crate) fn close(overlay: &mut Overlay, cx: &mut Context<Overlay>) {
    let Some(handle) = overlay.fluent.window else { return };
    closed(overlay, cx);
    cx.defer(move |cx| { let _ = handle.update(cx, |_, window, _| window.remove_window()); });
}

/// The window is closing: forget what it showed, and hide anything revealed in it.
fn closed(overlay: &mut Overlay, cx: &mut Context<Overlay>) {
    overlay.fluent.window = None;
    overlay.fluent.tab = None;
    overlay.fluent.mode_page = None;
    overlay.fluent.meters = None;
    overlay.open_picker = None;
    overlay.reveal_accounts = false;
    overlay.key_reveal.hide();
    overlay.key_input.update(cx, |input, cx| input.set_revealed(false, cx));
    overlay.close_mode_menus(cx);
    cx.notify();
}

impl FluentWindow {
    fn new(overlay: Entity<Overlay>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let closing = overlay.downgrade();
        // The title bar's ✕ (and Alt+F4).
        window.on_window_should_close(cx, move |_, cx| {
            if let Some(overlay) = closing.upgrade() { overlay.update(cx, closed); }
            true
        });
        let _overlay_changed = cx.observe(&overlay, |_, _, cx| cx.notify());
        Self { overlay, native: platform::native_window(window), capture_hidden: None, focus, _overlay_changed }
    }

    fn apply_capture(&mut self, cx: &App) {
        let settings = &self.overlay.read(cx).store.value;
        let Some(native) = self.native else { return };
        if self.capture_hidden == Some(settings.hide_from_capture) { return; }
        self.capture_hidden = Some(settings.hide_from_capture);
        overlay::apply_capture_setting(native, settings);
    }

    /// Esc closes an open list or menu first, then the window.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let closed_menu = self.overlay.update(cx, |overlay, cx| {
            if overlay.open_picker.is_some() { overlay.close_picker(cx); return true; }
            overlay.close_mode_menus(cx)
        });
        if closed_menu { return; }
        self.overlay.update(cx, closed);
        window.remove_window();
    }
}

/// 16 px viewfinder and the title on the left; ─ □ ✕ on the right, 46 px each.
fn title_bar() -> impl IntoElement {
    let control = |id: &'static str, icon: &'static str| div().id(id).w(px(46.0)).h_full().flex().items_center().justify_center()
        .child(ui::icon(icon, 10.0, theme::body()));
    div().id("fluent-title-bar").flex_none().h(px(40.0)).flex().items_center()
        .child(div().id("fluent-title-drag").flex_1().h_full().flex().items_center().gap(px(12.0)).pl(px(16.0))
            .window_control_area(WindowControlArea::Drag)
            .child(crate::ui::viewfinder(16.0))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::body()).child(TITLE)))
        .child(div().flex().h_full()
            .child(control("fluent-minimize", "icons/minimize.svg").hover(|button| button.bg(theme::raised())).window_control_area(WindowControlArea::Min))
            // Fixed size: shown dimmed, and not a control.
            .child(control("fluent-maximize", "icons/maximize.svg").opacity(0.35))
            .child(control("fluent-close", "icons/window-close.svg").hover(|button| button.bg(rgb(0xc42b1c)))
                .window_control_area(WindowControlArea::Close)))
}

impl Render for FluentWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_capture(cx);
        let main = self.overlay.update(cx, |overlay, cx| overlay.fluent_main(window, cx));
        div().size_full().flex().flex_col().bg(theme::fluent_window()).border_1().border_color(theme::keycap_border())
            .font_family(theme::FONT).text_color(theme::text())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| if event.keystroke.key == "escape" { this.escape(window, cx) }))
            .child(title_bar())
            // The keyboard's home is under the title bar: an element that takes focus handles the press
            // itself, which would keep the system from seeing presses on the caption buttons and drag area.
            .child(div().flex_1().min_h_0().flex().track_focus(&self.focus).child(main))
    }
}
