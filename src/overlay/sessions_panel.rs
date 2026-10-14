//! The Sessions panel, from the Paper artboards "Overlay · Sessions" (Redesign v3 · Windows) and
//! "macOS · Overlay · Sessions panel": Sessions in the toolbar replaces the card with a 460 px panel
//! to search past calls without leaving it. A back button and the "Ask or search anything" field on
//! top, saved sessions grouped by day (TODAY, YESTERDAY, MON, OCT 12) with their mode's icon, title,
//! length and start time, and a footer with Open ↵ and All sessions ↗ (the Sessions window).
//!
//! The panel takes the keyboard as the text box does (`Overlay::take_keyboard`): typing filters by
//! title and transcript, ↑↓ move the selection, ↵ opens it in the Sessions window, and Esc or the
//! back button closes the panel. The sessions are read off the UI thread when it opens.

use chrono::{Local, TimeZone};
use gpui::{
    Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, ScrollHandle,
    SharedString, Styled, Transformation, Window, div, prelude::*, px, radians,
};

use super::Overlay;
use crate::archive::{Archive, Summary};
use crate::input::{InputEvent, TextInput};
use crate::{theme, ui};

/// The panel's height, the card's while it is open.
pub(super) const PANEL_HEIGHT: f32 = 460.0;
/// A session without a recorded mode (saved before modes were recorded) gets this icon.
const DEFAULT_ICON: &str = "icons/tab-sessions.svg";

pub(crate) struct SessionsPanel {
    search: Entity<TextInput>,
    /// Saved sessions, newest first, each with its lower-cased searchable text; `None` while loading.
    sessions: Option<Vec<(Summary, String)>>,
    query: String,
    /// Index of the selected session among those shown.
    selected: usize,
    scroll: ScrollHandle,
}

/// A line of the list: a day's label (the first has no rule above it) or a session.
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Day { label: String, first: bool },
    Session(usize),
}

impl SessionsPanel {
    /// The sessions the search matches, in list order.
    fn shown(&self) -> Vec<&Summary> {
        let Some(sessions) = &self.sessions else { return Vec::new() };
        sessions.iter().filter(|(_, text)| self.query.is_empty() || text.contains(&self.query)).map(|(summary, _)| summary).collect()
    }
}

/// Day labels and rows for `shown`, which is newest first.
fn lines(shown: &[&Summary], today: chrono::NaiveDate) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut day = None;
    for (index, summary) in shown.iter().enumerate() {
        let date = local(summary.started_at).map(|time| time.date_naive());
        if date != day || index == 0 {
            day = date;
            lines.push(Line::Day { label: day_label(date, today), first: index == 0 });
        }
        lines.push(Line::Session(index));
    }
    lines
}

fn local(seconds: u64) -> Option<chrono::DateTime<Local>> { Local.timestamp_opt(seconds as i64, 0).single() }

/// "Today", "Yesterday", or the date as "Mon, Oct 12" (shown in capitals).
fn day_label(date: Option<chrono::NaiveDate>, today: chrono::NaiveDate) -> String {
    let Some(date) = date else { return "Earlier".into() };
    match (today - date).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => date.format("%a, %b %-d").to_string(),
    }
}

/// A session's length for its pill: "42m", "1h 12m", or "Live" while it is still being recorded.
fn duration(started_at: u64, ended_at: Option<u64>) -> String {
    let Some(ended_at) = ended_at else { return "Live".into() };
    let minutes = (ended_at.saturating_sub(started_at) / 60).max(1);
    if minutes < 60 { format!("{minutes}m") } else { format!("{}h {}m", minutes / 60, minutes % 60) }
}

/// What a key does in the panel, with `selected` selected among `count` sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step { Select(usize), Open, Close, Pass }

fn step(key: &str, selected: usize, count: usize) -> Step {
    match key {
        "escape" => Step::Close,
        "up" if count > 0 => Step::Select(selected.checked_sub(1).unwrap_or(count - 1)),
        "down" if count > 0 => Step::Select(if selected + 1 >= count { 0 } else { selected + 1 }),
        "enter" if count > 0 => Step::Open,
        _ => Step::Pass,
    }
}

impl Overlay {
    /// Sessions in the toolbar: replace the card with the panel, give its search field the keyboard,
    /// and read the saved sessions in the background.
    pub(super) fn open_sessions_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sessions_panel.is_some() { return; }
        // The panel takes the card's place, so the mode switcher (hanging from it) closes first.
        self.dismiss_switcher(window, cx);
        self.tip = None;
        self.show();
        self.take_keyboard(cx);
        let search = cx.new(|cx| TextInput::new("Ask or search anything", cx).with_text_size(14.0, 20.0));
        cx.subscribe_in(&search, window, |this, input, event, window, cx| {
            let Some(panel) = &mut this.sessions_panel else { return };
            match event {
                InputEvent::Changed => {
                    panel.query = input.read(cx).text().trim().to_lowercase();
                    panel.selected = 0;
                    panel.scroll.scroll_to_item(0);
                    cx.notify();
                }
                InputEvent::Submit => this.open_selected_session(window, cx),
            }
        }).detach();
        window.focus(&search.focus_handle(cx));
        self.sessions_panel = Some(SessionsPanel { search, sessions: None, query: String::new(), selected: 0, scroll: ScrollHandle::new() });
        if let Some(root) = self.archive.as_ref().map(|archive| archive.root().to_path_buf()) {
            cx.spawn_in(window, async move |this, cx| {
                let sessions = cx.background_executor().spawn(async move {
                    let archive = Archive::at(root);
                    archive.list().into_iter().map(|summary| {
                        let transcript = archive.load(&summary.id).map(|session| crate::notes::transcript(&session)).unwrap_or_default();
                        let text = [summary.title.as_deref().unwrap_or_default(), summary.preview.as_str(), transcript.as_str()].join("\n").to_lowercase();
                        (summary, text)
                    }).collect::<Vec<_>>()
                }).await;
                let _ = this.update(cx, |this, cx| if let Some(panel) = &mut this.sessions_panel { panel.sessions = Some(sessions); cx.notify(); });
            }).detach();
        } else {
            if let Some(panel) = &mut self.sessions_panel { panel.sessions = Some(Vec::new()); }
        }
        self.fit(window);
        cx.notify();
    }

    /// Close the panel, the card coming back. `hand_back`: return the keyboard to the app the user
    /// was in (Esc, the back button); not when a session is opening in the Sessions window.
    pub(super) fn close_sessions_panel(&mut self, hand_back: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.sessions_panel.take().is_none() { return; }
        if hand_back { self.return_to_previous_app(); }
        self.fit(window);
        cx.notify();
    }

    pub(super) fn sessions_panel_open(&self) -> bool { self.sessions_panel.is_some() }

    /// ↵ or Open: the selected session in the Sessions window.
    fn open_selected_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.sessions_panel else { return };
        let Some(id) = panel.shown().get(panel.selected).map(|summary| summary.id.clone()) else { return };
        self.close_sessions_panel(false, window, cx);
        self.open_sessions_at(Some(id), cx);
    }

    fn sessions_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &mut self.sessions_panel else { return };
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function { return; }
        let shown = panel.shown();
        let count = shown.len();
        match step(&event.keystroke.key, panel.selected, count) {
            Step::Pass => return,
            Step::Close => self.close_sessions_panel(true, window, cx),
            // ↵ also reaches the search field, which submits and opens it; nothing to do twice.
            Step::Open => return,
            Step::Select(index) => {
                let child = lines(&shown, Local::now().date_naive()).iter().position(|line| *line == Line::Session(index));
                panel.selected = index;
                if let Some(child) = child { panel.scroll.scroll_to_item(child); }
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    /// The card while the panel is open.
    pub(super) fn sessions_card(&self, panel: &SessionsPanel, cx: &mut Context<Self>) -> impl IntoElement {
        let back = div().id("sessions-back").size(px(28.0)).flex().flex_none().items_center().justify_center().rounded(px(theme::radius::BUTTON)).cursor_pointer()
            .bg(theme::pressed()).border_1().border_color(theme::keycap_border()).hover(|button| button.border_color(theme::bubble_border()))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { cx.stop_propagation(); this.close_sessions_panel(true, window, cx); }))
            // No left arrow asset: setup-arrow turned around.
            .child(gpui::svg().path("icons/setup-arrow.svg").size(px(14.0)).flex_none().text_color(theme::body())
                .with_transformation(Transformation::rotate(radians(std::f32::consts::PI))));
        let search = div().flex().flex_none().items_center().gap(px(10.0)).py(px(10.0)).pl(px(10.0)).pr(px(14.0))
            .bg(self.card_fill(theme::CARD_FIELD)).rounded_t(px(theme::radius::CARD - 1.0)).border_b_1().border_color(theme::divider())
            .child(back)
            .child(div().id("sessions-search").flex_1().min_w_0().cursor_text()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                    this.take_keyboard(cx);
                    if let Some(panel) = &this.sessions_panel { window.focus(&panel.search.focus_handle(cx)); }
                }))
                .child(panel.search.clone()))
            .child(ui::icon("icons/search.svg", 16.0, theme::muted()));
        let shown = panel.shown();
        let mut list = div().id("sessions-list").flex_1().min_h_0().flex().flex_col().py(px(8.0)).px(px(10.0)).overflow_y_scroll().track_scroll(&panel.scroll)
            .bg(self.card_fill(theme::CARD));
        let empty = match &panel.sessions {
            None => Some("Loading sessions…"),
            Some(all) if all.is_empty() => Some("No saved sessions yet. Live sessions are saved here."),
            Some(_) if shown.is_empty() => Some("No sessions match."),
            Some(_) => None,
        };
        if let Some(empty) = empty {
            list = list.child(div().px(px(8.0)).py(px(12.0)).text_size(px(13.0)).text_color(theme::muted()).child(empty));
        }
        for line in lines(&shown, Local::now().date_naive()) {
            list = list.child(match line {
                Line::Day { label, first } => div().flex().flex_col()
                    .when(!first, |day| day.child(div().h(px(1.0)).mx(px(8.0)).mt(px(8.0)).bg(theme::divider())))
                    .child(div().px(px(8.0)).pt(px(if first { 8.0 } else { 12.0 })).pb(px(6.0)).text_size(px(11.0)).line_height(px(14.0))
                        .font_weight(FontWeight::SEMIBOLD).text_color(theme::placeholder()).child(label.to_uppercase()))
                    .into_any_element(),
                Line::Session(index) => self.session_row(shown[index], index, index == panel.selected, cx).into_any_element(),
            });
        }
        let open = div().id("sessions-open").flex().items_center().gap(px(8.0)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { cx.stop_propagation(); this.open_selected_session(window, cx); }))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Open"))
            .child(ui::keycaps("↵", 22.0, 11.0, theme::body(), theme::raised()));
        let all = div().id("sessions-all").flex().items_center().gap(px(6.0)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.close_sessions_panel(false, window, cx);
                this.open_sessions_at(None, cx);
            }))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child("All sessions"))
            .child(ui::icon("icons/setup-external.svg", 10.0, theme::muted()));
        let footer = div().flex().flex_none().items_center().gap(px(10.0)).h(px(44.0)).pl(px(8.0)).pr(px(10.0))
            .bg(self.card_fill(theme::CARD)).rounded_b(px(theme::radius::CARD - 1.0)).border_t_1().border_color(theme::divider())
            .child(div().size(px(30.0)).flex().flex_none().items_center().justify_center().rounded(px(theme::radius::TILE)).bg(theme::raised())
                .border_1().border_color(theme::hairline()).child(ui::viewfinder(16.0)))
            .child(div().flex_1())
            .child(open)
            .child(div().w(px(1.0)).h(px(16.0)).flex_none().bg(theme::hairline()))
            .child(all);
        // The whole panel takes the mouse (the list scrolls with the wheel), and keys from the search field.
        div().id("card").relative().w(px(self.store.value.card_width.card())).h(px(PANEL_HEIGHT)).flex().flex_col()
            .rounded(px(theme::radius::CARD)).border_1().border_color(theme::keycap_border())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.sessions_key(event, window, cx)))
            .child(search).child(list).child(footer)
            .child(self.hits.mark())
    }

    /// A session: its mode's icon tile, title, length pill and start time. Clicking opens it.
    fn session_row(&self, summary: &Summary, index: usize, selected: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let icon = summary.mode.as_deref().and_then(|id| self.modes.get(id)).map_or(DEFAULT_ICON, |mode| mode.icon.path());
        let title: SharedString = summary.title.clone().filter(|title| !title.trim().is_empty())
            .or_else(|| (!summary.preview.trim().is_empty()).then(|| summary.preview.clone())).unwrap_or_else(|| "Untitled session".into()).into();
        let time = local(summary.started_at).map(|time| time.format("%-I:%M %p").to_string()).unwrap_or_default();
        let id = summary.id.clone();
        div().id(("session", index)).flex().flex_none().items_center().gap(px(12.0)).py(px(7.0)).px(px(8.0)).rounded(px(theme::radius::ROW)).border_1()
            .cursor_pointer()
            .when(selected, |row| row.bg(theme::selected_row()).border_color(theme::bubble_border()))
            .when(!selected, |row| row.border_color(gpui::transparent_black()).hover(|row| row.bg(theme::row_hover())))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.close_sessions_panel(false, window, cx);
                this.open_sessions_at(Some(id.clone()), cx);
            }))
            .child(div().size(px(32.0)).flex().flex_none().items_center().justify_center().rounded(px(theme::radius::TILE)).border_1()
                .bg(if selected { theme::bubble() } else { theme::raised() }).border_color(if selected { theme::bubble_border() } else { theme::hairline() })
                .child(ui::icon(icon, 15.0, if selected { theme::accent_soft() } else { theme::body() })))
            .child(div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
            .child(div().flex_none().px(px(7.0)).py(px(2.0)).rounded_full().bg(if selected { theme::bubble() } else { theme::raised() })
                .text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(if selected { theme::body() } else { theme::muted() })
                .child(duration(summary.started_at, summary.ended_at)))
            .child(div().w(px(62.0)).flex_none().flex().justify_end().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(time))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, started_at: u64) -> Summary {
        Summary { id: id.into(), title: None, mode: None, started_at, ended_at: Some(started_at + 60), lines: 0, turns: 0, screenshots: 0, preview: String::new() }
    }

    #[test]
    fn lengths_read_as_minutes_and_hours() {
        assert_eq!(duration(0, Some(42 * 60)), "42m");
        assert_eq!(duration(0, Some(20)), "1m", "under a minute rounds up to one");
        assert_eq!(duration(0, Some(72 * 60)), "1h 12m");
        assert_eq!(duration(0, None), "Live");
    }

    #[test]
    fn sessions_are_grouped_by_day_newest_first() {
        let now = Local::now();
        let today = now.date_naive();
        let at = |days_ago: i64| (now - chrono::Duration::days(days_ago)).timestamp() as u64;
        let (a, b, c) = (summary("a", at(0)), summary("b", at(0) - 1), summary("c", at(1)));
        let shown = [&a, &b, &c];
        let lines = lines(&shown, today);
        assert_eq!(lines, [
            Line::Day { label: "Today".into(), first: true }, Line::Session(0), Line::Session(1),
            Line::Day { label: "Yesterday".into(), first: false }, Line::Session(2),
        ]);
        let older = chrono::NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        assert_eq!(day_label(Some(older), chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap()), "Mon, Oct 12");
    }

    #[test]
    fn arrows_wrap_and_esc_closes() {
        assert_eq!(step("down", 0, 3), Step::Select(1));
        assert_eq!(step("down", 2, 3), Step::Select(0));
        assert_eq!(step("up", 0, 3), Step::Select(2));
        assert_eq!(step("enter", 1, 3), Step::Open);
        assert_eq!(step("escape", 0, 0), Step::Close);
        assert_eq!(step("down", 0, 0), Step::Pass, "nothing to move through");
        assert_eq!(step("a", 0, 3), Step::Pass, "letters go to the search field");
    }
}
