//! The mode chip in the card's toolbar and the switcher list it opens below the card, laid out like
//! the Paper artboards "Modes v2 · macOS · Overlay switcher open" and "Modes v2.1 · Windows ·
//! Unified card switcher open". It opens with a click on the chip, Tab in the text box or the
//! Switch mode shortcut (Ctrl Shift ' / ⌘⇧'), takes the keyboard, and is driven with ↑↓, 1–9, ↵
//! and Esc. "Edit modes…" opens the Windows Modes window, or Settings › Modes on macOS.

use gpui::{
    Context, Focusable, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, ScrollHandle,
    Styled, Window, deferred, div, prelude::*, px, rgb,
};

use super::Overlay;
use crate::modes::{self, Mode, ModeStore};
use crate::{platform, theme, ui};

const LIST_WIDTH: f32 = 248.0;
/// The rows scroll past this height, so the list stays inside the overlay window.
const ROWS_MAX_HEIGHT: f32 = 226.0;
/// From the chip's top to the list's top: the chip, the rest of the toolbar and a small gap.
const LIST_OFFSET: f32 = 41.0;
/// The list's height at most (header, rows, footer, padding), reserved in the window region.
const LIST_MAX_HEIGHT: f32 = 300.0;
/// Shown in the closed chip's dashed border.
const CHIP_EDGE: u32 = 0x3a3d42;
const FOCUSED_ROW: u32 = 0x2a2d31;

/// A line of the switcher list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    Group(String),
    /// A mode, with its number (1–9) when it has one.
    Mode { id: String, number: Option<usize> },
}

/// The list in display order: General, then each group's label and modes. The first nine modes
/// are numbered.
pub(crate) fn rows(store: &ModeStore) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut number = 0;
    for (group, members) in store.sections() {
        if let Some(group) = group { rows.push(Row::Group(group.to_string())); }
        for mode in members {
            number += 1;
            rows.push(Row::Mode { id: mode.id.clone(), number: (number <= 9).then_some(number) });
        }
    }
    rows
}

/// Where the switcher was opened from, so closing it hands the keyboard back to the right place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opened { TextBox, Elsewhere }

pub(crate) struct Switcher {
    /// The focused entry: an index among the modes, or one past the last for "Edit modes…".
    focused: usize,
    opened: Opened,
    focus: FocusHandle,
    scroll: ScrollHandle,
}

impl Overlay {
    /// The chip after the mark: the active mode, dashed while closed, solid while its list is open.
    pub(super) fn mode_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.modes.active();
        let open = self.switcher.is_some();
        let general = mode.id == modes::GENERAL;
        let mut chip = div().id("mode-chip").flex().flex_none().items_center().gap(px(5.0)).py(px(2.0)).px(px(7.0)).rounded(px(6.0))
            .cursor_pointer().text_size(px(12.0)).line_height(px(16.0))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.switcher.is_some() { this.close_switcher(window, cx) } else { this.open_switcher(Opened::Elsewhere, window, cx) }
            }));
        chip = if open {
            chip.bg(theme::bubble()).border_1().border_color(theme::accent()).text_color(theme::text())
        } else {
            chip.border_1().border_dashed().border_color(rgb(CHIP_EDGE)).text_color(if general { theme::placeholder() } else { theme::body() })
                .hover(|chip| chip.bg(gpui::rgba(0xffffff0f)))
        };
        if !general { chip = chip.child(ui::icon(mode.icon.path(), 12.0, if open { theme::accent_soft() } else { theme::body() })); }
        let mut wrapper = div().relative().flex_none()
            .child(chip.child(mode.name.clone())
                .child(ui::icon(if open { "icons/chevron-up.svg" } else { "icons/chevron-down.svg" }, 8.0, if open { theme::muted() } else { theme::placeholder() })));
        if open {
            // Reserve the list's area now so the window region shows it in its first frame.
            let hits = self.hits.clone();
            wrapper = wrapper.child(gpui::canvas(move |bounds, _, _| {
                hits.reserve(gpui::Bounds::new(gpui::point(bounds.left(), bounds.top() + px(LIST_OFFSET)), gpui::size(px(LIST_WIDTH), px(LIST_MAX_HEIGHT))));
            }, |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                .child(deferred(div().absolute().top(px(LIST_OFFSET)).left_0().child(self.switcher_list(cx))));
        }
        wrapper
    }

    fn switcher_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(switcher) = &self.switcher else { return div().into_any_element() };
        let active = self.modes.active().id.clone();
        let mut list = div().id("mode-rows").flex().flex_col().max_h(px(ROWS_MAX_HEIGHT)).overflow_y_scroll().track_scroll(&switcher.scroll);
        let mut index = 0;
        for row in rows(&self.modes) {
            match row {
                Row::Group(group) => {
                    list = list.child(div().pl(px(29.0)).pr(px(8.0)).pt(px(8.0)).pb(px(3.0)).text_size(px(11.0)).line_height(px(14.0))
                        .text_color(theme::placeholder()).child(group));
                }
                Row::Mode { id, number } => {
                    let Some(mode) = self.modes.get(&id) else { continue };
                    list = list.child(self.switcher_row(mode, number, index, id == active, index == switcher.focused, cx));
                    index += 1;
                }
            }
        }
        let edit_focused = switcher.focused == index;
        let hint = if cfg!(target_os = "macos") { "Settings › Modes" } else { "Opens the Modes window" };
        let footer = div().id("edit-modes").flex().items_center().justify_between().px(px(8.0)).pt(px(7.0)).pb(px(5.0)).rounded(px(7.0)).cursor_pointer()
            .when(edit_focused, |row| row.bg(rgb(FOCUSED_ROW)))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { cx.stop_propagation(); this.edit_modes(window, cx); }))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(if edit_focused { theme::text() } else { theme::muted() }).child("Edit modes…"))
            .child(div().flex().items_center().gap(px(6.0)).text_size(px(11.0)).line_height(px(14.0)).text_color(theme::placeholder()).child(hint)
                .when(edit_focused, |hint| hint.child(div().font_family(theme::MONO).text_color(theme::muted()).child("↵"))));
        div().id("mode-switcher").w(px(LIST_WIDTH)).flex().flex_col().p(px(4.0)).rounded(px(12.0)).bg(theme::raised())
            .border_1().border_color(theme::keycap_border())
            .track_focus(&switcher.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.switcher_key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_switcher(window, cx)))
            .child(div().flex().items_center().justify_between().px(px(8.0)).pt(px(7.0)).pb(px(5.0))
                .child(div().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child("MODE"))
                .child(div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::placeholder()).child("↑↓  ↵")))
            .child(list)
            .child(div().h(px(1.0)).flex_none().bg(theme::hairline()))
            .child(footer)
            .child(self.hits.mark())
            .into_any_element()
    }

    fn switcher_row(&self, mode: &Mode, number: Option<usize>, index: usize, active: bool, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let id = mode.id.clone();
        let trailing = if active {
            ui::icon("icons/check.svg", 14.0, theme::accent()).into_any_element()
        } else if focused {
            div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted()).child("↵").into_any_element()
        } else {
            div().into_any_element()
        };
        div().id(("switch-mode", index)).flex().items_center().gap(px(9.0)).px(px(8.0)).py(px(6.0)).rounded(px(7.0)).cursor_pointer()
            .when(focused, |row| row.bg(rgb(FOCUSED_ROW)))
            .when(!focused, |row| row.hover(|row| row.bg(gpui::rgba(0xffffff08))))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { cx.stop_propagation(); this.choose_mode(&id, window, cx); }))
            .child(div().w(px(12.0)).flex_none().font_family(theme::MONO).text_size(px(11.0)).line_height(px(14.0))
                .text_color(if focused { theme::body() } else { theme::placeholder() })
                .child(number.map(|n| n.to_string()).unwrap_or_default()))
            .child(ui::icon(mode.icon.path(), 13.0, if active { theme::accent_soft() } else if focused { theme::text() } else { theme::body() }))
            .child(div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(16.0))
                .when(active, |name| name.font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()))
                .when(!active, |name| name.text_color(theme::text()))
                .child(mode.name.clone()))
            .child(div().w(px(14.0)).flex_none().flex().justify_end().child(trailing))
    }

    /// Open the list on the active mode and take the keyboard for it.
    pub(super) fn open_switcher(&mut self, opened: Opened, window: &mut Window, cx: &mut Context<Self>) {
        self.show();
        let ids = mode_ids(&self.modes);
        let focused = ids.iter().position(|id| *id == self.modes.active().id).unwrap_or(0);
        let focus = cx.focus_handle();
        if opened == Opened::Elsewhere && let Some(native) = self.native && let Some(previous) = platform::take_focus(native) {
            self.return_focus = Some(previous);
        }
        window.focus(&focus);
        self.switcher = Some(Switcher { focused, opened, focus, scroll: ScrollHandle::new() });
        self.fit(window);
        cx.notify();
    }

    /// Close the list. The keyboard goes back to the text box when the list was opened from it,
    /// otherwise to the app the user was in.
    pub(super) fn close_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(switcher) = self.switcher.take() else { return };
        match switcher.opened {
            Opened::TextBox => window.focus(&self.composer.focus_handle(cx)),
            Opened::Elsewhere => self.return_to_previous_app(),
        }
        self.fit(window);
        cx.notify();
    }

    /// The Switch mode shortcut: open the list, or close it if it is open.
    pub(super) fn switch_mode_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.is_some() { self.close_switcher(window, cx) } else { self.open_switcher(Opened::Elsewhere, window, cx) }
    }

    fn choose_mode(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_mode(id, cx);
        self.close_switcher(window, cx);
    }

    fn edit_modes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_switcher(window, cx);
        #[cfg(target_os = "macos")]
        self.open_settings(crate::settings_view::Tab::Modes, window, cx);
        #[cfg(not(target_os = "macos"))]
        self.open_modes_window(None, cx);
    }

    fn switcher_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(switcher) = &mut self.switcher else { return };
        let ids = mode_ids(&self.modes);
        let last = ids.len(); // "Edit modes…"
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => self.close_switcher(window, cx),
            "up" => { switcher.focused = switcher.focused.checked_sub(1).unwrap_or(last); scroll_to(switcher, &self.modes); cx.notify(); }
            "down" | "tab" => { switcher.focused = if switcher.focused >= last { 0 } else { switcher.focused + 1 }; scroll_to(switcher, &self.modes); cx.notify(); }
            "enter" => match ids.get(switcher.focused).cloned() {
                Some(id) => self.choose_mode(&id, window, cx),
                None => self.edit_modes(window, cx),
            },
            digit if digit.len() == 1 && ('1'..='9').contains(&digit.chars().next().unwrap_or('0')) => {
                let index = digit.parse::<usize>().unwrap_or(1) - 1;
                if let Some(id) = ids.get(index).cloned() { self.choose_mode(&id, window, cx); }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// The modes' ids in list order.
fn mode_ids(store: &ModeStore) -> Vec<String> {
    rows(store).into_iter().filter_map(|row| match row { Row::Mode { id, .. } => Some(id), Row::Group(_) => None }).collect()
}

/// Keep the focused row in view: its child index counts the group labels before it.
fn scroll_to(switcher: &Switcher, store: &ModeStore) {
    let mut modes_seen = 0;
    for (child, row) in rows(store).iter().enumerate() {
        if matches!(row, Row::Mode { .. }) {
            if modes_seen == switcher.focused { switcher.scroll.scroll_to_item(child); return; }
            modes_seen += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_follow_the_sections_and_only_the_first_nine_modes_are_numbered() {
        let store = ModeStore::at(None);
        let rows = rows(&store);
        let mode = |id: &str, number: Option<usize>| Row::Mode { id: id.into(), number };
        assert_eq!(rows[..4], [mode("general", Some(1)), Row::Group("Looking for work".into()), mode("interview", Some(2)), mode("behavioral", Some(3))]);
        assert!(rows.contains(&mode("lecture", Some(8))) && rows.contains(&mode("team-meeting", Some(9))));
        assert!(rows.contains(&mode("sales", None)) && rows.contains(&mode("customer", None)), "past nine, no number");
        assert_eq!(rows.iter().filter(|row| matches!(row, Row::Group(_))).count(), 3);
        assert_eq!(mode_ids(&store).len(), modes::BUILTINS.len());
    }
}
