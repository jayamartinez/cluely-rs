//! The upcoming-meeting row above the text box (Paper F8b), in the update notice's pattern
//! (`update_notice`): the same slim row, one line. "Design review with Priya…  2:00 PM · in 2 min
//! [Start Live] ✕". The start time and countdown never shrink; a long title is cut with an
//! ellipsis. It only takes clicks, never the keyboard, and `calendar` only puts it up while the
//! overlay is already on screen. Start Live starts a session named after the event; the title
//! opens the meeting's link when it has one (only on that click, never by itself); ✕ dismisses
//! that reminder only.

use gpui::{Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, Window, div, px, rgba};

use super::Overlay;
use super::update_notice::NOTICE_HEIGHT;
use crate::calendar::{self, reminders::Row};
use crate::{archive, theme, ui};

const HOVER: u32 = 0xffffff0f;

impl Overlay {
    pub(super) fn upcoming_shown(&self) -> bool { self.live_since.is_none() && self.calendar.reminders.current().is_some() }

    pub(super) fn upcoming_row(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.upcoming_shown() { return None; }
        let row = self.calendar.reminders.current()?.clone();
        let start = row.event.start.at()?;
        let when = format!("{} · {}", calendar::local_time(start), calendar::countdown(start, archive::unix_now() as i64));
        let started = row.clone();
        let start_live = div().id("upcoming-start-live").flex().flex_none().items_center().h(px(24.0)).px(px(10.0)).rounded(px(7.0)).cursor_pointer()
            .bg(theme::accent()).text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink())
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { cx.stop_propagation(); this.start_live_for(&started, window, cx); }))
            .child("Start Live");
        let key = row.key.clone();
        let close = div().id("upcoming-dismiss").size(px(24.0)).flex().flex_none().items_center().justify_center().rounded(px(6.0)).cursor_pointer()
            .hover(|button| button.bg(rgba(HOVER)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.calendar.reminders.dismiss(&key);
                this.fit(window);
                cx.notify();
            }))
            .child(ui::icon("icons/close.svg", 10.0, theme::muted()));
        Some(div().flex().flex_none().items_center().gap(px(8.0)).h(px(NOTICE_HEIGHT)).pl(px(16.0)).pr(px(9.0)).bg(self.card_fill(theme::CARD)).rounded_t(px(theme::radius::CARD - 1.0))
            .child(ui::icon("icons/setup-calendar.svg", 14.0, theme::accent_soft()))
            .child(self.upcoming_title(&row, cx))
            .child(div().flex_none().pl(px(4.0)).text_size(px(12.0)).text_color(self.dim(theme::muted())).child(when))
            .child(start_live)
            .child(close)
            .child(self.hits.mark())
            .into_any_element())
    }

    /// The event's title, cut with an ellipsis; a click opens its meeting link, when there is one.
    fn upcoming_title(&self, row: &Row, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let title = div().id("upcoming-title").flex_1().min_w_0().truncate().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text())
            .child(row.event.title.clone());
        let Some(link) = row.event.meeting_link.clone() else { return title };
        title.cursor_pointer().hover(|title| title.text_color(theme::accent_soft()))
            .on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| { cx.stop_propagation(); cx.open_url(&link); }))
    }

    /// Start Live for the row's event, with the session named after it. Like the toolbar's Live
    /// button it never takes the keyboard.
    fn start_live_for(&mut self, row: &Row, window: &mut Window, cx: &mut Context<Self>) {
        if self.live_since.is_none() { self.set_live(true, window, cx); }
        if let Some(recorder) = &mut self.recorder && let Err(error) = recorder.set_title(&row.event.title) {
            eprintln!("session name could not be saved: {error}");
        }
    }
}
