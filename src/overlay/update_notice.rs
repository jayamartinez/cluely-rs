//! The overlay's side of update checks (`crate::update`): the automatic schedule, checks started
//! from Settings › About or the menu, and the row above the text box when a newer version is out
//! (Paper "Update check · Overlay notice · row above the text box"). The row only takes clicks,
//! never the keyboard, and it's hidden during Live and while the Settings panel is open.

use std::time::{Duration, Instant};

use gpui::{Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, Window, div, px, rgba};

use super::Overlay;
use crate::update::{self, Schedule, Status, Version};
use crate::{archive, theme, ui};

/// The row's height; the idle window grows by it while the row shows.
pub(super) const NOTICE_HEIGHT: f32 = 38.0;
/// How often the schedule is looked at. A check is due at most every few hours; this only
/// decides how soon after launch (or after Live) it starts.
const TICK: Duration = Duration::from_secs(5);
/// The card's toolbar colour, which the row shares (`card`), at the card's background opacity.
const ROW: u32 = 0x121315;
const HOVER: u32 = 0xffffff0f;

impl Overlay {
    /// Run automatic checks: about 10 s after launch, then every 6 hours, while Settings › About ›
    /// "Check for updates automatically" is on and Live isn't running.
    pub(super) fn start_update_checks(&self, window: &mut Window, cx: &mut Context<Self>) {
        let launched = Instant::now();
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            let ticked = this.update_in(cx, |this, window, cx| {
                let settings = &this.store.value;
                let schedule = Schedule {
                    automatic: settings.check_for_updates,
                    live: this.live_since.is_some(),
                    checking: this.updates.status == Status::Checking,
                    checked_this_run: this.updates.checked_this_run,
                    since_launch: launched.elapsed(),
                    last_checked: settings.last_update_check,
                    now: archive::unix_now(),
                };
                if schedule.due() { this.check_for_updates(window, cx); }
            });
            if ticked.is_err() { break; }
        }).detach();
    }

    /// Ask GitHub for the latest release, off the UI thread. Does nothing while a check runs.
    pub(crate) fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.updates.begin() { return; }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { update::check(&Version::current()) }).await;
            if let Err(error) = &result { eprintln!("update check failed: {error:?}"); }
            let _ = this.update_in(cx, |this, window, cx| {
                this.updates.finish(result);
                this.store.value.last_update_check = Some(archive::unix_now());
                this.store.save();
                this.fit(window);
                cx.notify();
            });
        }).detach();
    }

    /// The macOS menu's "Check for Updates…": show Settings › About and check.
    #[cfg(target_os = "macos")]
    pub fn check_for_updates_from_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::settings_window::open_about(cx);
        self.check_for_updates(window, cx);
    }

    pub(super) fn update_notice_shown(&self) -> bool {
        self.live_since.is_none() && self.settings_tab.is_none() && self.updates.notice(&self.store.value.dismissed_update).is_some()
    }

    /// "Update available · v0.2.0", Download (the release page) and ✕, which hides the row until a
    /// newer version is out. `None` when there's nothing to show.
    pub(super) fn update_notice(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.update_notice_shown() { return None; }
        let release = self.updates.notice(&self.store.value.dismissed_update)?;
        let (page, version) = (release.page.clone(), release.version.to_string());
        let download = div().id("update-download").flex().flex_none().items_center().h(px(24.0)).px(px(10.0)).rounded(px(7.0)).cursor_pointer()
            .bg(theme::accent()).text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink())
            .on_mouse_down(MouseButton::Left, move |_, _, cx| { cx.stop_propagation(); cx.open_url(&page); })
            .child("Download ↗");
        let dismissed = version.clone();
        let close = div().id("update-dismiss").size(px(24.0)).flex().flex_none().items_center().justify_center().rounded(px(6.0)).cursor_pointer()
            .hover(|button| button.bg(rgba(HOVER)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                let version = dismissed.clone();
                this.update_settings(|s| s.dismissed_update = version, window, cx);
                this.fit(window);
            }))
            .child(ui::icon("icons/close.svg", 10.0, theme::muted()));
        Some(div().flex().flex_none().items_center().gap(px(8.0)).h(px(NOTICE_HEIGHT)).pl(px(18.0)).pr(px(9.0)).bg(crate::appearance::fill(ROW, self.store.value.background_opacity)).rounded_t(px(19.0))
            .child(div().size(px(6.0)).flex_none().rounded_full().bg(theme::accent()))
            .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Update available"))
            .child(div().flex_1().min_w_0().text_size(px(12.0)).text_color(theme::muted()).child(format!("v{version}")))
            .child(download)
            .child(close)
            .child(self.hits.mark())
            .into_any_element())
    }
}
