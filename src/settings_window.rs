//! macOS Settings: a normal window, opened with ⌘, or the overlay's gear, with a centred bar of
//! icon tabs as Mac apps show their settings. Layout follows the Paper "Redesign v3 · Cluely 2.0
//! style" Settings artboards: a fixed-height window whose page scrolls under a thin overlay
//! scrollbar. The tabs' contents are the overlay's own settings (`settings_view::mac`) drawn here;
//! Windows keeps them in the overlay's panel.

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyBinding, MouseButton,
    MouseDownEvent, ParentElement, Render, ScrollHandle, Styled, Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, div, point, prelude::*, px, size,
};

use crate::onboarding_window::{AudioCheck, Metered, Permissions, audio_check};
use crate::overlay::{self, Overlay};
use crate::platform::{self, NativeWindow};
use crate::settings_view::{Tab, WindowAction, WindowState};
use crate::{theme, ui};

gpui::actions!(settings_window, [CloseSettings]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page { Settings(Tab), About }

/// The tab bar: page, label, icon.
const PAGES: [(Page, &str, &str); 7] = [
    (Page::Settings(Tab::Model), "Model", "icons/tab-model.svg"),
    (Page::Settings(Tab::Listening), "Listening", "icons/tab-listening.svg"),
    (Page::Settings(Tab::Modes), "Modes", "icons/tab-modes.svg"),
    (Page::Settings(Tab::Keys), "Shortcuts", "icons/tab-shortcuts.svg"),
    (Page::Settings(Tab::Window), "Window", "icons/tab-window.svg"),
    (Page::Settings(Tab::History), "Sessions", "icons/tab-sessions.svg"),
    (Page::About, "About", "icons/tab-about.svg"),
];

const TITLE: &str = "CluelyRS Settings";

/// ⌘W closes the window, as in any Mac app.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-w", CloseSettings, Some("SettingsWindow"))]);
}

/// The window's size; it never changes with the tab. A shorter screen gets a shorter window.
const WIDTH: f32 = 880.0;
const HEIGHT: f32 = 720.0;
/// Room the window always leaves on the screen.
const SCREEN_MARGIN: f32 = 80.0;
/// The page's column.
const COLUMN: f32 = 640.0;

pub struct SettingsWindow {
    overlay: Entity<Overlay>,
    page: Page,
    focus: FocusHandle,
    native: Option<NativeWindow>,
    /// The capture exclusion last applied, so it is only set when the setting changes.
    capture_hidden: Option<bool>,
    scroll: ScrollHandle,
    /// Listening's meters, and the permissions they wait for.
    permissions: Permissions,
    audio: AudioCheck,
    /// How many sessions are saved, counted as Sessions opens.
    sessions: Option<usize>,
    _overlay_changed: Subscription,
    _activation: Subscription,
}

impl Metered for SettingsWindow {
    fn meters(&mut self) -> &mut AudioCheck { &mut self.audio }
}

/// Show the Settings window on `tab`, opening it if needed, and bring CluelyRS forward so the menu
/// bar (Settings…, Quit) is its own while the window is in front.
pub fn open(tab: Tab, cx: &mut Context<Overlay>) { open_page(if tab == Tab::About { Page::About } else { Page::Settings(tab) }, cx) }

/// Show the Settings window on About (the menu's "Check for Updates…").
pub fn open_about(cx: &mut Context<Overlay>) { open_page(Page::About, cx) }

fn open_page(page: Page, cx: &mut Context<Overlay>) {
    let entity = cx.entity();
    // Deferred: the window reads the overlay while it draws, and the overlay is being updated now.
    // The window becomes key only once CluelyRS is active (`platform::activate_then`).
    cx.defer(move |cx| platform::activate_then(cx, move |active, cx| {
        if let Some(handle) = entity.read(cx).settings_window
            && handle.update(cx, |this, window, cx| {
                this.select(page, cx);
                if active { window.activate_window(); } else if let Some(native) = this.native { platform::order_front(native); }
            }).is_ok() {
            return;
        }
        let height = cx.primary_display().map_or(HEIGHT, |display| HEIGHT.min(f32::from(display.bounds().size.height) - SCREEN_MARGIN));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(WIDTH), px(height)), cx))),
            titlebar: Some(TitlebarOptions { title: Some(TITLE.into()), appears_transparent: true, traffic_light_position: Some(point(px(14.0), px(12.0))) }),
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

/// Whether Settings is in front, taking the keyboard, as when its "Show in Dock" switch is clicked.
/// Asks AppKit for the key window: while Settings handles that click, GPUI has lent its window
/// out, so reading the window through `handle` fails.
pub fn is_in_front(handle: WindowHandle<SettingsWindow>, cx: &App) -> bool {
    cx.active_window() == Some(handle.into())
}

/// CluelyRS just left the Dock while Settings was in front (`is_in_front`), which makes macOS
/// deactivate it and leaves Settings behind other apps (`platform::reactivate_after_leaving_dock`).
/// Bring CluelyRS and Settings back. Not otherwise: the overlay never activates CluelyRS itself.
pub fn keep_in_front(handle: WindowHandle<SettingsWindow>, cx: &mut App) {
    platform::reactivate_after_leaving_dock(cx, move |active, cx| {
        let _ = handle.update(cx, |this, window, _| if active { window.activate_window(); } else if let Some(native) = this.native { platform::order_front(native); });
    });
}

/// The window is about to close. Leave the Dock first (unless "Show in Dock" is on), so CluelyRS
/// is never a regular app left without a window.
fn closed(overlay: &Entity<Overlay>, cx: &mut App) {
    overlay.update(cx, |overlay, _| {
        overlay.settings_window = None;
        overlay.settings_window_tab = None;
        overlay.key_reveal.hide();
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
        // A settings change can change what the meters capture (their switches and devices).
        let _overlay_changed = cx.observe(&overlay, |this, _, cx| { this.sync_audio(cx); cx.notify(); });
        // Coming back from System Settings is when permissions change.
        let _activation = cx.observe_window_activation(window, |this, window, cx| if window.is_window_active() { this.refresh_permissions(cx) });
        let mut this = Self { overlay, page, focus, native: platform::native_window(window), capture_hidden: None, scroll: ScrollHandle::new(),
            permissions: Permissions::read(), audio: AudioCheck::default(), sessions: None, _overlay_changed, _activation };
        this.opened(cx);
        this
    }

    fn select(&mut self, page: Page, cx: &mut Context<Self>) {
        let changed = self.page != page;
        self.page = page;
        if let Page::Settings(tab) = page {
            // Refresh what the tab shows (accounts, devices, history size) from the overlay's window.
            let overlay = self.overlay.clone();
            let own = overlay.read(cx).own_window;
            let _ = own.update(cx, |_, window, cx| overlay.update(cx, |overlay, cx| overlay.prepare_tab(tab, window, cx)));
        }
        if changed { self.scroll.set_offset(point(px(0.0), px(0.0))); }
        self.opened(cx);
        cx.notify();
    }

    /// What a page needs as it opens: Listening's meters run only while it shows; Sessions counts.
    fn opened(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Settings(Tab::Listening) { self.permissions = Permissions::read(); }
        self.sync_audio(cx);
        if self.page == Page::Settings(Tab::History) { self.count_sessions(cx); }
    }

    fn refresh_permissions(&mut self, cx: &mut Context<Self>) {
        let read = Permissions::read();
        self.permissions.mic = read.mic;
        self.permissions.screen_allowed = read.screen_allowed;
        self.sync_audio(cx);
    }

    fn sync_audio(&mut self, cx: &mut Context<Self>) {
        let overlay = self.overlay.read(cx);
        let wanted = audio_check::wanted(self.page == Page::Settings(Tab::Listening), &overlay.store.value, &self.permissions, overlay.live_since.is_some());
        audio_check::sync(self, wanted, cx);
    }

    /// Count the saved sessions (their folders) off the UI thread.
    fn count_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.overlay.read(cx).archive.as_ref().map(|archive| archive.root().to_path_buf()) else { return };
        cx.spawn(async move |this, cx| {
            let count = cx.background_executor().spawn(async move {
                std::fs::read_dir(root).map_or(0, |entries| entries.flatten().filter(|entry| entry.path().is_dir()).count())
            }).await;
            let _ = this.update(cx, |this, cx| { this.sessions = Some(count); cx.notify(); });
        }).detach();
    }

    /// The title beside the traffic lights, and the tabs centred under it.
    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div().flex().items_center().gap(px(4.0));
        for (index, (page, label, icon)) in PAGES.into_iter().enumerate() {
            let selected = page == self.page;
            tabs = tabs.child(div().id(("tab", index)).w(px(80.0)).flex_none().flex().flex_col().items_center().gap(px(5.0)).pt(px(9.0)).pb(px(8.0))
                .rounded(px(crate::ui_v3::RADII.tile)).cursor_pointer()
                .when(selected, |tab| tab.bg(theme::selected_fill()))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.select(page, cx)))
                .child(ui::icon(icon, 20.0, if selected { theme::accent_soft() } else { theme::muted() }))
                .child(div().text_size(px(12.0)).line_height(px(14.0))
                    .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                    .text_color(if selected { theme::text() } else { theme::muted() })
                    .child(label)));
        }
        // The title row sits beside the traffic lights, in the transparent title bar.
        div().flex().flex_col().items_center().gap(px(10.0)).pt(px(10.0)).pb(px(12.0)).px(px(14.0)).bg(theme::title_bar())
            .border_b_1().border_color(theme::divider())
            .child(div().w_full().h(px(16.0)).flex().justify_center().text_size(px(13.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::body()).child(TITLE))
            .child(tabs)
    }

    /// About: Run setup again closes Settings as setup opens; it waits while Live is on.
    fn about(&self, cx: &mut Context<Self>) -> AnyElement {
        let live = self.overlay.read(cx).live_since.is_some();
        let overlay = self.overlay.clone();
        let run_setup = (!live).then(|| Box::new(move |_: &MouseDownEvent, window: &mut Window, cx: &mut App| {
            closed(&overlay, cx);
            window.remove_window();
            let own = overlay.read(cx).own_window;
            let _ = own.update(cx, |_, window, cx| overlay.update(cx, |overlay, cx| overlay.open_onboarding(window, cx)));
        }) as WindowAction);
        self.overlay.update(cx, |overlay, cx| overlay.in_window(cx, |overlay, cx| overlay.mac_about(run_setup, cx))).into_any_element()
    }

}

/// Settings and setup show accounts and keys, so while the overlay hides from screen capture, so do
/// they. `applied` is the exclusion last set, so it is only set again when the setting changes.
pub(crate) fn sync_capture(native: Option<NativeWindow>, applied: &mut Option<bool>, settings: &crate::settings::Settings) {
    let Some(native) = native else { return };
    if *applied == Some(settings.hide_from_capture) { return; }
    *applied = Some(settings.hide_from_capture);
    overlay::apply_capture_setting(native, settings);
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        sync_capture(self.native, &mut self.capture_hidden, &self.overlay.read(cx).store.value);
        window.set_window_title(TITLE);
        let state = WindowState { levels: [self.audio[0].shown_level(), self.audio[1].shown_level()], sessions: self.sessions };
        let body = match self.page {
            Page::Settings(tab) => self.overlay.update(cx, |overlay, cx| overlay.settings_window_body(tab, state, cx)),
            Page::About => self.about(cx),
        };
        let page = if self.page == Page::Settings(Tab::Modes) {
            // Modes fills the window with its own sidebar and editor.
            div().flex_1().min_h_0().flex().child(body)
        } else {
            div().relative().flex_1().min_h_0()
                .child(div().id("settings-page").size_full().overflow_y_scroll().track_scroll(&self.scroll)
                    .child(div().w(px(COLUMN)).mx_auto().pt(px(28.0)).pb(px(36.0)).child(body)))
                .children(crate::ui_v3::scroll_thumb(&self.scroll, 4.0))
        };
        div().size_full().flex().flex_col().bg(theme::window_bg()).font_family(theme::FONT).text_color(theme::text())
            .key_context("SettingsWindow").track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseSettings, window, cx| {
                closed(&this.overlay, cx);
                window.remove_window();
            }))
            .child(self.title_bar(cx))
            .child(page)
    }
}
