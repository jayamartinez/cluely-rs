//! Windows: Modes in a window of its own ("Modes v3 · Windows · Modes window"), opened from
//! Settings › Modes and the mode switcher's "Edit modes…". Like Sessions it is a normal,
//! resizable window with its own title bar, in the taskbar and with the keyboard; unlike Sessions
//! it shows the user's meeting context and files, so while the overlay hides from screen capture,
//! so does it. The page itself is the one macOS shows in its Settings window (`modes_view`).

use gpui::{
    App, AppContext, Bounds, Context, Entity, ParentElement, Render, Styled, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, div, prelude::*, px, rgb, size,
};

use crate::overlay::{self, Overlay};
use crate::platform::{self, NativeWindow};
use crate::theme;

pub struct ModesWindow {
    overlay: Entity<Overlay>,
    native: Option<NativeWindow>,
    /// The capture exclusion last applied, so it is only set when the setting changes.
    capture_hidden: Option<bool>,
    _overlay_changed: Subscription,
}

/// Open the window, or bring the open one forward.
pub fn open(overlay: &mut Overlay, cx: &mut Context<Overlay>) {
    let entity = cx.entity();
    let existing: Option<WindowHandle<ModesWindow>> = overlay.modes_window;
    // Deferred: the window reads the overlay while it draws, and the overlay is being updated now.
    cx.defer(move |cx| {
        if let Some(handle) = existing && handle.update(cx, |_, window, _| window.activate_window()).is_ok() { return; }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(880.0), px(640.0)), cx))),
            // The frame is drawn by the window itself, as in Sessions.
            titlebar: Some(TitlebarOptions { title: Some("Modes".into()), appears_transparent: true, traffic_light_position: None }),
            kind: WindowKind::Normal,
            focus: true,
            show: true,
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(size(px(760.0), px(560.0))),
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| cx.new(|cx| ModesWindow::new(entity.clone(), window, cx)));
        match opened {
            Ok(handle) => entity.update(cx, |overlay, _| overlay.modes_window = Some(handle)),
            Err(error) => eprintln!("modes window could not open: {error}"),
        }
    });
}

impl ModesWindow {
    fn new(overlay: Entity<Overlay>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let closing = overlay.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            if let Some(overlay) = closing.upgrade() { overlay.update(cx, |overlay, _| overlay.modes_window = None); }
            true
        });
        let _overlay_changed = cx.observe(&overlay, |_, _, cx| cx.notify());
        Self { overlay, native: platform::native_window(window), capture_hidden: None, _overlay_changed }
    }

    fn apply_capture(&mut self, cx: &App) {
        let settings = &self.overlay.read(cx).store.value;
        let Some(native) = self.native else { return };
        if self.capture_hidden == Some(settings.hide_from_capture) { return; }
        self.capture_hidden = Some(settings.hide_from_capture);
        overlay::apply_capture_setting(native, settings);
    }
}

impl Render for ModesWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_capture(cx);
        let page = self.overlay.update(cx, |overlay, cx| overlay.modes_tab(cx));
        div().size_full().flex().flex_col().bg(rgb(0x0f1012)).font_family(theme::FONT).text_color(theme::text())
            .child(crate::sessions_window::title_bar("Modes", window))
            .child(div().flex_1().min_h_0().flex().child(page))
    }
}
