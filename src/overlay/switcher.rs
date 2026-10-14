//! The mode chip in the card's toolbar and the switcher list it opens below the card, laid out like
//! the Paper artboards "Overlay · Mode switcher" (Redesign v3 · Windows) and "macOS · Overlay · Mode
//! switcher": numbered rows under a "SWITCH MODE" header, the rows scrolling past six with a fade and
//! a thin thumb, and "Edit modes…" at the bottom. It opens with a click on
//! the chip, Tab in the text box or the Switch mode shortcut (Ctrl Shift ' / ⌘⇧'), takes the keyboard
//! the way the text box does (`Overlay::take_keyboard`), and is driven with ↑↓, 1–9, ↵ and Esc.
//! "Edit modes…" opens Settings › Modes (its own window on both platforms).
//!
//! The same code runs on both platforms. The list is painted deferred, so its area is reserved in
//! the window region (`Hits::reserve`, which shapes the Windows overlay) and marked for mouse
//! hit-testing (`Hits::mark`), and the window grows to `window_height` while it is open.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, Context, Focusable, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels,
    ScrollHandle, Styled, Window, deferred, div, linear_color_stop, linear_gradient, point, prelude::*, px, relative, size,
};

use super::Overlay;
use super::card::{CONVERSATION_MAX_HEIGHT, INPUT_ROW_HEIGHT, MAC, TOOLBAR_HEIGHT};
use crate::modes::{self, Mode, ModeStore};
use crate::toggles::Tip;
use crate::{theme, ui};

const LIST_WIDTH: f32 = 248.0;
/// The rows scroll past this height (about six rows), so the whole list fits on a 900 px tall
/// screen under the tallest Live card.
const ROWS_MAX_HEIGHT: f32 = 226.0;
/// The chip: 16 px text, its padding (2 px, 5 px on macOS) and its border; centred in the toolbar at
/// the card's bottom.
const CHIP_PADDING: f32 = if MAC { 5.0 } else { 2.0 };
const CHIP_HEIGHT: f32 = 16.0 + 2.0 * CHIP_PADDING + 2.0;
/// The list's fill: the chrome on Windows, the raised popover on macOS.
fn list_fill() -> gpui::Rgba { if MAC { theme::raised() } else { theme::chrome() } }
const CHIP_IN_TOOLBAR: f32 = (TOOLBAR_HEIGHT - CHIP_HEIGHT) / 2.0;
/// The list hangs this far under the card, with the pointer (on the chip's centre) in between.
const LIST_GAP: f32 = 6.0;
const POINTER: (f32, f32) = (12.0, 6.0);
/// The list's top-left from the chip's: 9 px to the left, so the rows' numbers sit under the chip,
/// and `LIST_GAP` below the card (the chip's bottom is the rest of the toolbar and the card's
/// border above the card's bottom edge).
const LIST_FROM_CHIP: (f32, f32) = (-9.0, CHIP_HEIGHT + CHIP_IN_TOOLBAR + 1.0 + LIST_GAP);
/// The list at its tallest: border, padding, header, rows, separator (with its margins) and the
/// "Edit modes…" row.
const LIST_MAX_HEIGHT: f32 = 2.0 + 8.0 + 26.0 + ROWS_MAX_HEIGHT + 9.0 + 32.0;
/// The card's height (card.rs): its border, the text box row and the toolbar; while Live, also the
/// conversation at its cap.
const IDLE_CARD_HEIGHT: f32 = 1.0 + INPUT_ROW_HEIGHT + TOOLBAR_HEIGHT + 1.0;
const TALLEST_CARD_HEIGHT: f32 = IDLE_CARD_HEIGHT + CONVERSATION_MAX_HEIGHT;
/// The card's top in the window (the overlay's top padding).
const CARD_TOP: f32 = 8.0;
/// Under the list: its rounded edge, and slack for a card a little taller than these heights.
const BELOW_LIST: f32 = 24.0;
/// The scroll thumb: 3 px wide, inset from the rows' top and bottom.
const THUMB_INSET: f32 = 4.0;
/// The fade over the last visible rows while more are below.
const FADE: f32 = 36.0;

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
    Bounds::new(point(chip.left() + px(dx), chip.top() + px(dy - LIST_GAP)), size(px(LIST_WIDTH), px(LIST_GAP + LIST_MAX_HEIGHT)))
}

/// The overlay window's height while the list is open, so the list fits under the card at its
/// tallest: idle (taller by the update notice row while `notice` shows it), or Live with the
/// conversation at its cap. `Overlay::fit` takes the larger of this and the height the rest of
/// the overlay needs. The card's width doesn't change its height.
pub(super) fn window_height(live: bool, notice: bool) -> f32 {
    let card = if live { TALLEST_CARD_HEIGHT } else { IDLE_CARD_HEIGHT } + if notice { super::update_notice::NOTICE_HEIGHT } else { 0.0 };
    // The chip's top in the window: in the toolbar, which ends 1 px (the border) above the card's bottom.
    let chip_top = CARD_TOP + card - 1.0 - TOOLBAR_HEIGHT + CHIP_IN_TOOLBAR;
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
    /// The chip after the viewfinder: the active mode, dashed while closed, accent while its list is
    /// open. General shows only its name, in the placeholder colour; other modes show their icon.
    /// Hovering the closed chip shows "Switch mode" with its shortcut under the card.
    pub(super) fn mode_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.modes.active();
        let open = self.switcher.is_some();
        let general = mode.id == modes::GENERAL;
        let mut chip = div().id("mode-chip").relative().flex().flex_none().items_center().gap(px(5.0)).py(px(CHIP_PADDING)).px(px(7.0)).rounded(px(theme::radius::CHIP))
            .cursor_pointer().text_size(px(12.0)).line_height(px(16.0)).border_1()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.switcher.is_some() { this.close_switcher(window, cx) } else { this.open_switcher(window, cx) }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.hover_tip(Tip::ModeChip, *hovered, cx)));
        chip = if open {
            chip.bg(theme::bubble()).border_color(theme::accent()).text_color(theme::text())
        } else {
            chip.border_dashed().border_color(theme::chip_edge()).text_color(if general && !MAC { theme::placeholder() } else { theme::body() })
                .hover(|chip| chip.bg(theme::hover()))
        };
        // macOS names every mode with its icon, in semibold; Windows dims General and shows no icon.
        if MAC { chip = chip.font_weight(FontWeight::SEMIBOLD); }
        if !general || MAC { chip = chip.child(ui::icon(mode.icon.path(), 12.0, if open { theme::accent_soft() } else { theme::body() })); }
        chip = chip.child(div().whitespace_nowrap().child(mode.name.clone()))
            .child(ui::icon(if open { "icons/chevron-up.svg" } else { "icons/chevron-down.svg" }, 8.0, if open { theme::muted() } else { theme::placeholder() }));
        let Some(switcher) = &self.switcher else { return self.chip_tooltip(chip, CHIP_HEIGHT).into_any_element() };
        // The chip records where it is and reserves the list's area during the main layout pass,
        // so the window region already covers the list in the frame it first appears.
        let (hits, chip_bounds) = (self.hits.clone(), switcher.chip.clone());
        chip = chip.child(gpui::canvas(move |bounds, _, _| {
            chip_bounds.set(Some(bounds));
            hits.reserve(list_area(bounds));
        }, |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        let (dx, dy) = LIST_FROM_CHIP;
        let pointer = gpui::svg().path("icons/tooltip-pointer-up.svg").w(px(POINTER.0)).h(px(POINTER.1)).text_color(theme::keycap_border());
        // The pointer hangs from the chip's centre and the list from its left edge.
        let hung = div().absolute().top(px(dy - LIST_GAP)).left_0().w_full()
            .child(div().absolute().top_0().left(relative(0.5)).ml(px(-POINTER.0 / 2.0)).child(pointer))
            .child(div().absolute().top(px(LIST_GAP)).left(px(dx)).child(self.switcher_list(switcher, cx)));
        div().relative().flex_none().child(chip).child(deferred(hung)).into_any_element()
    }

    fn switcher_list(&self, switcher: &Switcher, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.modes.active().id.clone();
        let mut lines = div().id("mode-rows").flex().flex_col().pr(px(8.0)).max_h(px(ROWS_MAX_HEIGHT)).overflow_y_scroll().track_scroll(&switcher.scroll);
        let mut index = 0;
        for row in rows(&self.modes) {
            match row {
                Row::Group(group) => {
                    lines = lines.child(div().px(px(8.0)).pt(px(8.0)).pb(px(3.0)).text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::placeholder()).child(group.to_uppercase()));
                }
                Row::Mode { id, number } => {
                    let Some(mode) = self.modes.get(&id) else { continue };
                    lines = lines.child(self.switcher_row(mode, number, index, id == active, index == switcher.focused, cx));
                    index += 1;
                }
            }
        }
        // The rows scroll inside a fixed area; while more are below, the last ones fade out, and a
        // thin thumb shows where the view is.
        let (offset, max) = (-f32::from(switcher.scroll.offset().y), f32::from(switcher.scroll.max_offset().height));
        let track = ROWS_MAX_HEIGHT - 2.0 * THUMB_INSET;
        let thumb = (max > 0.5).then(|| {
            let height = (track * ROWS_MAX_HEIGHT / (ROWS_MAX_HEIGHT + max)).max(24.0);
            div().absolute().right(px(1.0)).top(px(THUMB_INSET + (track - height) * (offset / max).clamp(0.0, 1.0))).w(px(3.0)).h(px(height))
                .rounded(px(2.0)).bg(theme::keycap_border())
        });
        let fade = (max - offset > 0.5).then(|| div().absolute().left_0().right_0().bottom_0().h(px(FADE))
            .bg(linear_gradient(180.0, linear_color_stop(gpui::Rgba { a: 0.0, ..list_fill() }, 0.0), linear_color_stop(list_fill(), 1.0))));
        let list = div().relative().flex_none().max_h(px(ROWS_MAX_HEIGHT))
            .child(lines)
            .children(fade).children(thumb);
        let edit_focused = switcher.focused == index;
        // macOS says where "Edit modes…" goes; the Windows mockup shows no hint.
        let hint = cfg!(target_os = "macos").then_some("Settings › Modes");
        let edit = div().id("edit-modes").flex().items_center().gap(px(9.0)).px(px(8.0)).py(px(6.0)).rounded(px(theme::radius::SWITCHER_ROW)).border_1()
            .cursor_pointer()
            .when(edit_focused, |row| row.bg(theme::selected_row()).border_color(theme::bubble_border()))
            .when(!edit_focused, |row| row.border_color(gpui::transparent_black()).hover(|row| row.bg(theme::row_hover())))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { cx.stop_propagation(); this.edit_modes(window, cx); }))
            .child(div().w(px(12.0)).flex_none())
            .child(div().mx(px(4.0)).flex_none().child(ui::icon("icons/setup-sliders.svg", 16.0, if edit_focused { theme::accent_soft() } else { theme::muted() })))
            .child(div().flex_1().text_size(px(13.0)).line_height(px(18.0)).text_color(if edit_focused { theme::text() } else { theme::body() }).child("Edit modes…"))
            .children(hint.map(|hint| div().text_size(px(11.0)).line_height(px(14.0)).text_color(theme::placeholder()).child(hint)))
            .when(edit_focused, |row| row.child(enter_mark()));
        let chip = switcher.chip.clone();
        let keys = self.hotkeys.label(crate::hotkeys::Action::SwitchMode);
        div().id("mode-switcher").relative().w(px(LIST_WIDTH)).flex().flex_col().p(px(4.0)).rounded(px(theme::radius::POPOVER)).bg(list_fill())
            .border_1().border_color(theme::keycap_border())
            .when(MAC, |list| list.shadow(vec![gpui::BoxShadow { color: gpui::hsla(0.0, 0.0, 0.0, 0.5), offset: point(px(0.0), px(16.0)),
                blur_radius: px(40.0), spread_radius: px(0.0) }]))
            .track_focus(&switcher.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.switcher_key(event, window, cx)))
            .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                if chip.get().is_some_and(|chip| chip.contains(&event.position)) { return; }
                this.close_switcher(window, cx);
            }))
            .child(div().flex().flex_none().items_center().justify_between().h(px(26.0)).px(px(8.0))
                .child(div().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::placeholder()).child("SWITCH MODE"))
                .child(ui::keycaps(&keys, 18.0, 10.0, theme::muted(), if MAC { theme::field() } else { theme::raised() })))
            .child(list)
            .child(div().h(px(1.0)).my(px(4.0)).flex_none().bg(theme::divider()))
            .child(edit)
            .child(self.hits.mark())
    }

    /// A mode: its number, icon tile and name. The active mode has a check; the keyboard-focused
    /// row is the selected-row fill with an accent tile and ↵.
    fn switcher_row(&self, mode: &Mode, number: Option<usize>, index: usize, active: bool, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let id = mode.id.clone();
        let trailing = if active {
            ui::icon("icons/check.svg", 14.0, if MAC { theme::accent() } else { theme::accent_soft() }).into_any_element()
        } else if focused {
            enter_mark().into_any_element()
        } else {
            div().into_any_element()
        };
        // Windows marks the keyboard-focused row's tile; macOS marks the active mode's (tile, name and
        // check in the accent) and keeps the focused row to its fill and ↵.
        let marked = if MAC { active } else { focused };
        let tile = div().size(px(24.0)).flex().flex_none().items_center().justify_center().rounded(px(theme::radius::SMALL_TILE)).border_1()
            .bg(if marked { theme::bubble() } else if MAC { theme::field() } else { theme::raised() }).border_color(if marked { theme::bubble_border() } else { theme::hairline() })
            .child(ui::icon(mode.icon.path(), 13.0, if marked { theme::accent_soft() } else if MAC { theme::text() } else { theme::body() }));
        div().id(("switch-mode", index)).flex().flex_none().items_center().gap(px(9.0)).px(px(if MAC { 7.0 } else { 8.0 })).py(px(if MAC { 3.0 } else { 5.0 }))
            .rounded(px(theme::radius::SWITCHER_ROW))
            .border_1().cursor_pointer()
            .when(focused, |row| row.bg(theme::selected_row()).border_color(theme::bubble_border()))
            .when(!focused, |row| row.border_color(gpui::transparent_black()).hover(|row| row.bg(theme::row_hover())))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| { cx.stop_propagation(); this.choose_mode(&id, window, cx); }))
            .child(div().w(px(12.0)).flex_none().flex().justify_center().text_size(px(11.0)).line_height(px(14.0))
                .when(focused, |number| number.font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()))
                .when(!focused, |number| number.text_color(theme::placeholder()))
                .child(number.map(|n| n.to_string()).unwrap_or_default()))
            .child(tile)
            .child(div().flex_1().min_w_0().truncate().text_size(px(13.0)).line_height(px(if MAC { 16.0 } else { 18.0 }))
                .when(MAC && active, |name| name.font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()))
                .when(MAC && !active, |name| name.font_weight(theme::MEDIUM).text_color(theme::text()))
                .when(!MAC && (active || focused), |name| name.font_weight(FontWeight::SEMIBOLD).text_color(theme::text()))
                .when(!MAC && !active && !focused, |name| name.text_color(theme::body()))
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

/// ↵ on the keyboard-focused row.
fn enter_mark() -> impl IntoElement {
    div().text_size(px(13.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_soft()).child("↵")
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

    /// The chip as the card lays it out: after the 32 px viewfinder button, centred in the toolbar
    /// at the bottom of the card.
    fn chip(card: Bounds<Pixels>) -> Bounds<Pixels> {
        let left = f32::from(card.left()) + 1.0 + 8.0 + 32.0 + 6.0;
        Bounds::new(point(px(left), card.bottom() - px(1.0 + TOOLBAR_HEIGHT - CHIP_IN_TOOLBAR)), size(px(84.0), px(CHIP_HEIGHT)))
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
            assert_eq!(list.top(), card.bottom(), "{label}: the pointer and the list hang from the card's bottom edge");
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
                assert_eq!(radius, (theme::radius::POPOVER * scale).round() as i32, "a floating list gets the popover corner");
            }
        }
    }
}
