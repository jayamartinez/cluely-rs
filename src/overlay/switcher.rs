//! The mode chip in the card's toolbar and the switcher list it opens below the card, laid out like
//! the Paper artboards "Modes v2 · macOS · Overlay switcher open", "Modes v2.1 · Windows · Unified
//! card switcher open" and "Modes v3 · Windows · Open from the switcher". It opens with a click on
//! the chip, Tab in the text box or the Switch mode shortcut (Ctrl Shift ' / ⌘⇧'), takes the keyboard
//! the way the text box does (`Overlay::take_keyboard`), and is driven with ↑↓, 1–9, ↵ and Esc.
//! "Edit modes…" opens the Windows Modes window, or Settings › Modes on macOS.
//!
//! The same code runs on both platforms. The list is painted deferred, so its area is reserved in
//! the window region (`Hits::reserve`, which shapes the Windows overlay) and marked for mouse
//! hit-testing (`Hits::mark`), and the window grows to `window_height` while it is open.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, Context, Focusable, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels,
    ScrollHandle, Styled, Window, deferred, div, point, prelude::*, px, rgb, size,
};

use super::Overlay;
use crate::modes::{self, Mode, ModeStore};
use crate::{theme, ui};

const LIST_WIDTH: f32 = 248.0;
/// The rows scroll past this height. The mockups draw 290; 226 keeps the whole list on a 900 px
/// tall screen under the tallest Live card.
const ROWS_MAX_HEIGHT: f32 = 226.0;
/// The chip is 20 px tall, centred in the 46 px toolbar at the card's bottom.
const CHIP_IN_TOOLBAR: f32 = 13.0;
/// The list's top-left from the chip's: 9 px to the left, so the rows' numbers sit under the chip,
/// and 4 px below the card.
const LIST_FROM_CHIP: (f32, f32) = (-9.0, 46.0 - CHIP_IN_TOOLBAR + 1.0 + 4.0);
/// The list at its tallest: border, padding, header, rows, separator and the "Edit modes…" row.
const LIST_MAX_HEIGHT: f32 = 2.0 + 8.0 + 26.0 + ROWS_MAX_HEIGHT + 1.0 + 30.0;
/// The card's height (card.rs): its border, the text box row and the toolbar; while Live, also the
/// conversation at its cap.
const IDLE_CARD_HEIGHT: f32 = 1.0 + 54.0 + 46.0 + 1.0;
const TALLEST_CARD_HEIGHT: f32 = IDLE_CARD_HEIGHT + 418.0;
/// The card's top in the window (the overlay's top padding).
const CARD_TOP: f32 = 8.0;
/// Under the list: its rounded edge, and slack for a card a little taller than these heights.
const BELOW_LIST: f32 = 24.0;
/// The closed chip's dashed border.
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

/// The modes' ids in list order.
fn mode_ids(store: &ModeStore) -> Vec<String> {
    rows(store).into_iter().filter_map(|row| match row { Row::Mode { id, .. } => Some(id), Row::Group(_) => None }).collect()
}

/// What a key does in the open list. Entries are the modes in list order, then "Edit modes…".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Move the focus to this entry.
    Focus(usize),
    /// Make the mode at this index active.
    Choose(usize),
    EditModes,
    Close,
    /// Not the list's key: let it through.
    Pass,
}

/// The step for `key` with `focused` focused among `modes` modes. ↑↓ (and Tab, Shift Tab) move and
/// wrap through "Edit modes…", ↵ takes the focused entry, 1–9 choose that mode at once, Esc closes.
fn step(key: &str, shift: bool, focused: usize, modes: usize) -> Step {
    let edit = modes;
    match key {
        "escape" => Step::Close,
        "up" => Step::Focus(focused.checked_sub(1).unwrap_or(edit)),
        "tab" if shift => Step::Focus(focused.checked_sub(1).unwrap_or(edit)),
        "down" | "tab" => Step::Focus(if focused >= edit { 0 } else { focused + 1 }),
        "enter" if focused < modes => Step::Choose(focused),
        "enter" => Step::EditModes,
        digit => match digit.parse::<usize>() {
            Ok(number @ 1..=9) if number <= modes && !shift => Step::Choose(number - 1),
            _ => Step::Pass,
        },
    }
}

/// Where the list hangs, from the chip's bounds, at its tallest. The chip is placed by layout, so
/// the list follows the card's width (Settings › Window › Card width) and any row above the text
/// box (the update notice) without knowing about them.
fn list_area(chip: Bounds<Pixels>) -> Bounds<Pixels> {
    let (dx, dy) = LIST_FROM_CHIP;
    Bounds::new(point(chip.left() + px(dx), chip.top() + px(dy)), size(px(LIST_WIDTH), px(LIST_MAX_HEIGHT)))
}

/// The overlay window's height while the list is open, so the list fits under the card at its
/// tallest: idle (taller by the update notice row while `notice` shows it), or Live with the
/// conversation at its cap. `Overlay::fit` takes the larger of this and the height the rest of
/// the overlay needs. The card's width doesn't change its height.
pub(super) fn window_height(live: bool, notice: bool) -> f32 {
    let card = if live { TALLEST_CARD_HEIGHT } else { IDLE_CARD_HEIGHT } + if notice { super::update_notice::NOTICE_HEIGHT } else { 0.0 };
    // The chip's top in the window: in the toolbar, which ends 1 px (the border) above the card's bottom.
    let chip_top = CARD_TOP + card - 1.0 - 46.0 + CHIP_IN_TOOLBAR;
    chip_top + LIST_FROM_CHIP.1 + LIST_MAX_HEIGHT + BELOW_LIST
}

pub(crate) struct Switcher {
    /// The focused entry: an index among the modes, or one past the last for "Edit modes…".
    focused: usize,
    /// Opened while the text box had the keyboard: closing puts the caret back there instead of
    /// handing the keyboard to the app the user was in.
    from_text_box: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// Where the chip is, so a click on it is left to the chip (which closes the list) rather
    /// than taken as a click outside the list.
    chip: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Overlay {
    /// The chip after the mark: the active mode, dashed while closed, solid while its list is open.
    pub(super) fn mode_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.modes.active();
        let open = self.switcher.is_some();
        let general = mode.id == modes::GENERAL;
        let mut chip = div().id("mode-chip").relative().flex().flex_none().items_center().gap(px(5.0)).py(px(2.0)).px(px(7.0)).rounded(px(6.0))
            .cursor_pointer().text_size(px(12.0)).line_height(px(16.0))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.switcher.is_some() { this.close_switcher(window, cx) } else { this.open_switcher(window, cx) }
            }));
        chip = if open {
            chip.bg(theme::bubble()).border_1().border_color(theme::accent()).text_color(theme::text())
        } else {
            chip.border_1().border_dashed().border_color(rgb(CHIP_EDGE)).text_color(if general { theme::placeholder() } else { theme::body() })
                .hover(|chip| chip.bg(gpui::rgba(0xffffff0f)))
        };
        if !general { chip = chip.child(ui::icon(mode.icon.path(), 12.0, if open { theme::accent_soft() } else { theme::body() })); }
        chip = chip.child(mode.name.clone())
            .child(ui::icon(if open { "icons/chevron-up.svg" } else { "icons/chevron-down.svg" }, 8.0, if open { theme::muted() } else { theme::placeholder() }));
        let Some(switcher) = &self.switcher else { return chip.into_any_element() };
        // The chip records where it is and reserves the list's area during the main layout pass,
        // so the window region already covers the list in the frame it first appears.
        let (hits, chip_bounds) = (self.hits.clone(), switcher.chip.clone());
        chip = chip.child(gpui::canvas(move |bounds, _, _| {
            chip_bounds.set(Some(bounds));
            hits.reserve(list_area(bounds));
        }, |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        let (dx, dy) = LIST_FROM_CHIP;
        div().relative().flex_none().child(chip)
            .child(deferred(div().absolute().top(px(dy)).left(px(dx)).child(self.switcher_list(switcher, cx))))
            .into_any_element()
    }

    fn switcher_list(&self, switcher: &Switcher, cx: &mut Context<Self>) -> impl IntoElement {
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
        let edit = div().id("edit-modes").mt(px(2.0)).flex().items_center().justify_between().gap(px(8.0)).px(px(8.0)).py(px(6.0)).rounded(px(7.0))
            .cursor_pointer()
            .when(edit_focused, |row| row.bg(rgb(FOCUSED_ROW)))
            .when(!edit_focused, |row| row.hover(|row| row.bg(gpui::rgba(0xffffff08))))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { cx.stop_propagation(); this.edit_modes(window, cx); }))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(if edit_focused { theme::text() } else { theme::muted() }).child("Edit modes…"))
            .child(div().flex().items_center().gap(px(6.0)).text_size(px(11.0)).line_height(px(14.0))
                .text_color(if edit_focused { theme::muted() } else { theme::placeholder() }).child(hint)
                .when(edit_focused, |hint| hint.child(div().font_family(theme::MONO).child("↵"))));
        let chip = switcher.chip.clone();
        div().id("mode-switcher").relative().w(px(LIST_WIDTH)).flex().flex_col().p(px(4.0)).rounded(px(12.0)).bg(theme::raised())
            .border_1().border_color(theme::keycap_border()).shadow_lg()
            .track_focus(&switcher.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.switcher_key(event, window, cx)))
            .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                if chip.get().is_some_and(|chip| chip.contains(&event.position)) { return; }
                this.close_switcher(window, cx);
            }))
            .child(div().flex().items_center().justify_between().px(px(8.0)).pt(px(7.0)).pb(px(5.0))
                .child(div().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child("MODE"))
                .child(div().font_family(theme::MONO).text_size(px(11.0)).line_height(px(14.0)).text_color(theme::placeholder()).child("↑↓  ↵")))
            .child(list)
            .child(div().h(px(1.0)).flex_none().bg(theme::hairline()))
            .child(edit)
            .child(self.hits.mark())
    }

    fn switcher_row(&self, mode: &Mode, number: Option<usize>, index: usize, active: bool, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let id = mode.id.clone();
        let trailing = if active {
            ui::icon("icons/check.svg", 14.0, theme::accent()).into_any_element()
        } else if focused {
            div().font_family(theme::MONO).text_size(px(11.0)).line_height(px(14.0)).text_color(theme::muted()).child("↵").into_any_element()
        } else {
            div().into_any_element()
        };
        div().id(("switch-mode", index)).flex().flex_none().items_center().gap(px(9.0)).px(px(8.0)).py(px(6.0)).rounded(px(7.0)).cursor_pointer()
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

    /// Open the list on the active mode and give it the keyboard. From the text box the overlay
    /// already has the keyboard; otherwise it is taken as the text box takes it, remembering the
    /// app the user was in.
    pub(super) fn open_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show();
        let from_text_box = self.composer.read(cx).has_keyboard(window);
        if !from_text_box { self.take_keyboard(cx); }
        let focused = mode_ids(&self.modes).iter().position(|id| *id == self.modes.active().id).unwrap_or(0);
        let focus = cx.focus_handle();
        window.focus(&focus);
        self.switcher = Some(Switcher { focused, from_text_box, focus, scroll: ScrollHandle::new(), chip: Rc::default() });
        self.fit(window);
        cx.notify();
    }

    /// Close the list and hand the keyboard back: to the text box when the list was opened from
    /// it, otherwise to the app the user was in.
    pub(super) fn close_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(switcher) = self.remove_switcher(window, cx) else { return };
        if switcher.from_text_box { window.focus(&self.composer.focus_handle(cx)); } else { self.return_to_previous_app(); }
    }

    /// Close the list without handing the keyboard anywhere: the user clicked into another app,
    /// or another CluelyRS window is about to take the keyboard.
    pub(super) fn dismiss_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(switcher) = self.remove_switcher(window, cx) && switcher.from_text_box {
            window.focus(&self.composer.focus_handle(cx));
        }
    }

    fn remove_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Switcher> {
        let switcher = self.switcher.take()?;
        self.fit(window);
        cx.notify();
        Some(switcher)
    }

    pub(super) fn switcher_open(&self) -> bool { self.switcher.is_some() }

    /// The Switch mode shortcut: open the list, or close it if it is open.
    pub(super) fn switch_mode_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.is_some() { self.close_switcher(window, cx) } else { self.open_switcher(window, cx) }
    }

    /// Make a mode active (a running answer finishes; `apply_mode` cancels speculation and
    /// prewarms the new mode) and close the list.
    fn choose_mode(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_mode(id, cx);
        self.close_switcher(window, cx);
    }

    fn edit_modes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_switcher(window, cx);
        #[cfg(target_os = "macos")]
        self.open_settings(crate::settings_view::Tab::Modes, window, cx);
        #[cfg(not(target_os = "macos"))]
        self.open_modes_window(None, cx);
    }

    fn switcher_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(switcher) = &mut self.switcher else { return };
        let modifiers = event.keystroke.modifiers;
        // Leave chords (⌘Q, the global shortcuts) alone.
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function { return; }
        let ids = mode_ids(&self.modes);
        match step(&event.keystroke.key, modifiers.shift, switcher.focused, ids.len()) {
            Step::Pass => return,
            Step::Close => self.close_switcher(window, cx),
            Step::Focus(index) => {
                switcher.focused = index;
                if let Some(child) = row_child(&self.modes, index) { switcher.scroll.scroll_to_item(child); }
                cx.notify();
            }
            Step::Choose(index) => self.choose_mode(&ids[index], window, cx),
            Step::EditModes => self.edit_modes(window, cx),
        }
        cx.stop_propagation();
    }
}

/// The child index in the rows list of the mode at `index` (group labels are children too), or
/// `None` for "Edit modes…", which is outside the scrolling rows.
fn row_child(store: &ModeStore, index: usize) -> Option<usize> {
    rows(store).iter().enumerate().filter(|(_, row)| matches!(row, Row::Mode { .. })).nth(index).map(|(child, _)| child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::CardWidth;
    use crate::overlay::update_notice::NOTICE_HEIGHT;
    use crate::overlay::{IDLE_HEIGHT, region};

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
        // Scrolling to a mode skips the group labels before it.
        assert_eq!(row_child(&store, 0), Some(0));
        assert_eq!(row_child(&store, 1), Some(2), "Interview comes after the \"Looking for work\" label");
        assert_eq!(row_child(&store, modes::BUILTINS.len()), None, "Edit modes… isn't a row");
    }

    #[test]
    fn arrows_wrap_through_edit_modes_and_enter_takes_the_focused_entry() {
        let modes = 11;
        assert_eq!(step("down", false, 0, modes), Step::Focus(1));
        assert_eq!(step("down", false, 10, modes), Step::Focus(11), "the last mode, then Edit modes…");
        assert_eq!(step("down", false, 11, modes), Step::Focus(0), "past Edit modes… back to the top");
        assert_eq!(step("up", false, 0, modes), Step::Focus(11));
        assert_eq!(step("up", false, 5, modes), Step::Focus(4));
        assert_eq!(step("tab", false, 3, modes), Step::Focus(4));
        assert_eq!(step("tab", true, 3, modes), Step::Focus(2));
        assert_eq!(step("enter", false, 4, modes), Step::Choose(4));
        assert_eq!(step("enter", false, 11, modes), Step::EditModes);
        assert_eq!(step("escape", false, 4, modes), Step::Close);
    }

    #[test]
    fn digits_choose_the_numbered_modes_only() {
        assert_eq!(step("1", false, 5, 11), Step::Choose(0));
        assert_eq!(step("9", false, 0, 11), Step::Choose(8));
        assert_eq!(step("0", false, 0, 11), Step::Pass);
        assert_eq!(step("4", false, 0, 3), Step::Pass, "only three modes");
        assert_eq!(step("2", true, 0, 11), Step::Pass, "Shift 2 types a symbol");
        assert_eq!(step("a", false, 0, 11), Step::Pass);
        assert_eq!(step("space", false, 0, 11), Step::Pass);
    }

    /// The card as the overlay lays it out: centred in the window for its width, `height` tall.
    fn card(width: CardWidth, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px((width.window() - width.card()) / 2.0), px(CARD_TOP)), size(px(width.card()), px(height)))
    }

    /// The chip as the card lays it out (as measured on macOS): after the 22 px mark, centred in
    /// the toolbar at the bottom of the card.
    fn chip(card: Bounds<Pixels>) -> Bounds<Pixels> {
        let left = f32::from(card.left()) + 1.0 + 16.0 + 22.0 + 12.0;
        Bounds::new(point(px(left), card.bottom() - px(1.0 + 46.0 - CHIP_IN_TOOLBAR)), size(px(84.0), px(20.0)))
    }

    /// Every card the list can hang under: each width, idle with and without the update notice, and
    /// Live at its tallest.
    fn layouts() -> Vec<(CardWidth, bool, bool, Bounds<Pixels>)> {
        let mut layouts = Vec::new();
        for (width, _) in CardWidth::ALL {
            for (live, notice, height) in [(false, false, IDLE_CARD_HEIGHT), (false, true, IDLE_CARD_HEIGHT + NOTICE_HEIGHT), (true, false, TALLEST_CARD_HEIGHT)] {
                layouts.push((width, live, notice, card(width, height)));
            }
        }
        layouts
    }

    #[test]
    fn the_open_list_hangs_under_the_card_inside_the_grown_window() {
        for (width, live, notice, card) in layouts() {
            let label = format!("{width:?} live {live} notice {notice}");
            let list = list_area(chip(card));
            assert_eq!(list.top(), card.bottom() + px(4.0), "{label}: 4 px under the card");
            assert_eq!(list.left(), chip(card).left() - px(9.0), "{label}");
            assert!(f32::from(list.bottom()) < window_height(live, notice), "{label}: the list ends inside the window");
            assert!(f32::from(list.right()) < width.window(), "{label}: and inside its width");
        }
        assert!(window_height(false, false) > IDLE_HEIGHT, "the idle window grows for the list");
    }

    #[test]
    fn the_window_region_covers_the_whole_list_at_windows_scale_factors() {
        for (width, live, notice, card) in layouts() {
            for scale in [1.0, 1.25, 1.5, 2.0] {
                let label = format!("{width:?} live {live} notice {notice} scale {scale}");
                let list = list_area(chip(card));
                let shapes = region(&[card], &[list], scale);
                let physical = |value: Pixels| f32::from(value) * scale;
                let (left, top, right, bottom, radius) = shapes[1];
                assert!(left as f32 <= physical(list.left()) && top as f32 <= physical(list.top()), "{label}");
                assert!(right as f32 >= physical(list.right()) && bottom as f32 >= physical(list.bottom()), "{label}");
                assert!(right as f32 <= width.window() * scale, "{label}: the region stays inside the window's width");
                assert!(bottom as f32 <= window_height(live, notice) * scale, "{label}: and its height");
                assert_eq!(radius, (10.0 * scale).round() as i32, "a floating list gets the popover corner");
            }
        }
    }
}
