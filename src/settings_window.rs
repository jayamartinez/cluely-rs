//! macOS Settings: a normal window, opened with ⌘, or the overlay's gear, with a toolbar of tabs as
//! Mac apps show their settings. Layout follows the Paper "macOS · Settings window" artboards. The
//! tabs' contents are the overlay's own settings (`settings_view`) drawn here; Windows keeps them in
//! the overlay's panel.

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyBinding, MouseButton,
    ParentElement, Render, Styled, Subscription, TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*, px,
    rgb, size,
};

use crate::overlay::{self, Overlay};
use crate::platform::{self, NativeWindow};
use crate::settings_view::Tab;
use crate::{theme, ui};

gpui::actions!(settings_window, [CloseSettings]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page { Settings(Tab), About }

/// The toolbar: page, label, icon.
const PAGES: [(Page, &str, &str); 6] = [
    (Page::Settings(Tab::Model), "Model", "icons/tab-model.svg"),
    (Page::Settings(Tab::Listening), "Listening", "icons/tab-listening.svg"),
    (Page::Settings(Tab::Keys), "Shortcuts", "icons/tab-shortcuts.svg"),
    (Page::Settings(Tab::Window), "Window", "icons/tab-window.svg"),
    (Page::Settings(Tab::History), "Sessions", "icons/tab-sessions.svg"),
    (Page::About, "About", "icons/tab-about.svg"),
];

/// ⌘W closes the window, as in any Mac app.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-w", CloseSettings, Some("SettingsWindow"))]);
}

pub struct SettingsWindow {
    overlay: Entity<Overlay>,
    page: Page,
    focus: FocusHandle,
    native: Option<NativeWindow>,
    /// The capture exclusion last applied, so it is only set when the setting changes.
    capture_hidden: Option<bool>,
    _overlay_changed: Subscription,
}

/// Show the Settings window on `tab`, opening it if needed, and bring CluelyRS forward so the menu
/// bar (Settings…, Quit) is its own while the window is in front.
pub fn open(tab: Tab, cx: &mut Context<Overlay>) {
    let page = Page::Settings(tab);
    let entity = cx.entity();
    // Deferred: the window reads the overlay while it draws, and the overlay is being updated now.
    // The window becomes key only once CluelyRS is active (`platform::activate_then`).
    cx.defer(move |cx| platform::activate_then(cx, move |active, cx| {
        if let Some(handle) = entity.read(cx).settings_window
            && handle.update(cx, |this, window, cx| {
                this.page = page;
                if active { window.activate_window(); } else if let Some(native) = this.native { platform::order_front(native); }
                cx.notify();
            }).is_ok() {
            return;
        }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(760.0), px(640.0)), cx))),
            titlebar: Some(TitlebarOptions { title: Some("Settings".into()), appears_transparent: true, traffic_light_position: Some(point(px(14.0), px(14.0))) }),
            kind: WindowKind::Normal,
            focus: active,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| cx.new(|cx| SettingsWindow::new(entity.clone(), page, window, cx)));
        match opened {
            Ok(handle) => entity.update(cx, |overlay, _| { overlay.settings_window = Some(handle); overlay.update_dock(); }),
            Err(error) => eprintln!("settings window could not open: {error}"),
        }
    }));
}

/// The window is about to close. Leave the Dock first (unless "Show in Dock" is on), so CluelyRS
/// is never a regular app left without a window.
fn closed(overlay: &Entity<Overlay>, cx: &mut App) {
    overlay.update(cx, |overlay, _| {
        overlay.settings_window = None;
        overlay.update_dock();
    });
}

impl SettingsWindow {
    fn new(overlay: Entity<Overlay>, page: Page, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let closing = overlay.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            if let Some(overlay) = closing.upgrade() { closed(&overlay, cx); }
            true
        });
        let _overlay_changed = cx.observe(&overlay, |_, _, cx| cx.notify());
        Self { overlay, page, focus, native: platform::native_window(window), capture_hidden: None, _overlay_changed }
    }

    fn select(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        if let Page::Settings(tab) = page {
            // Refresh what the tab shows (accounts, devices, history size) from the overlay's window.
            let overlay = self.overlay.clone();
            let own = overlay.read(cx).own_window;
            let _ = own.update(cx, |_, window, cx| overlay.update(cx, |overlay, cx| overlay.prepare_tab(tab, window, cx)));
        }
        cx.notify();
    }

    /// Settings show accounts and keys, so while the overlay hides from screen capture, so do they.
    fn apply_capture(&mut self, cx: &App) {
        let settings = &self.overlay.read(cx).store.value;
        let Some(native) = self.native else { return };
        if self.capture_hidden == Some(settings.hide_from_capture) { return; }
        self.capture_hidden = Some(settings.hide_from_capture);
        overlay::apply_capture_setting(native, settings);
    }

    fn toolbar(&self, title: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div().flex().justify_center().gap(px(4.0));
        for (index, (page, label, icon)) in PAGES.into_iter().enumerate() {
            let selected = page == self.page;
            tabs = tabs.child(div().id(("tab", index)).w(px(80.0)).flex().flex_col().items_center().gap(px(3.0)).pt(px(6.0)).pb(px(5.0))
                .rounded(px(8.0)).cursor_pointer()
                .when(selected, |tab| tab.bg(rgb(0x26282c)))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.select(page, cx)))
                .child(ui::icon(icon, 22.0, if selected { theme::accent_soft() } else { theme::muted() }))
                .child(div().text_size(px(11.0)).line_height(px(14.0))
                    .when(selected, |text| text.font_weight(FontWeight::SEMIBOLD).text_color(theme::text()))
                    .when(!selected, |text| text.text_color(theme::muted()))
                    .child(label)));
        }
        // The title row sits beside the traffic lights, in the transparent title bar.
        div().flex().flex_col().gap(px(6.0)).pt(px(10.0)).pb(px(8.0)).px(px(14.0)).bg(rgb(0x17181b)).border_b_1().border_color(theme::divider())
            .child(div().h(px(16.0)).flex().justify_center().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child(title))
            .child(tabs)
    }

    fn about(&self) -> AnyElement {
        div().flex().flex_col().items_center().gap(px(10.0)).pt(px(48.0))
            .child(ui::mark(56.0))
            .child(div().pt(px(6.0)).text_size(px(20.0)).font_weight(FontWeight::BOLD).text_color(theme::text()).child("CluelyRS"))
            .child(div().text_size(px(12.0)).text_color(theme::muted()).child(format!("Version {}", env!("CARGO_PKG_VERSION"))))
            .child(div().id("quit").mt(px(18.0)).px(px(14.0)).py(px(7.0)).rounded(px(9.0)).cursor_pointer()
                .border_1().border_color(theme::hairline()).text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.quit())
                .child("Quit CluelyRS"))
            .into_any_element()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_capture(cx);
        let title = PAGES.iter().find(|(page, ..)| *page == self.page).map(|(_, label, _)| *label).unwrap_or("Settings");
        window.set_window_title(title);
        let body = match self.page {
            Page::Settings(tab) => self.overlay.update(cx, |overlay, cx| overlay.settings_window_body(tab, cx)),
            Page::About => self.about(),
        };
        div().size_full().flex().flex_col().bg(rgb(0x0f1012)).font_family(theme::FONT).text_color(theme::text())
            .key_context("SettingsWindow").track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseSettings, window, cx| {
                closed(&this.overlay, cx);
                window.remove_window();
            }))
            .child(self.toolbar(title, cx))
            .child(div().id("settings-page").flex_1().min_h_0().overflow_y_scroll()
                .child(div().w(px(600.0)).mx_auto().pt(px(22.0)).pb(px(28.0)).child(body)))
    }
}
