//! Windows Settings: a window of its own in the style of Windows 11 Settings (Fluent), from the Paper
//! "Redesign v3 · Windows" Settings artboards. A fixed 880 × 680 window with a drawn title bar, a nav
//! pane of pages on the left and the page's SettingsCards on the right, scrolling inside the fixed
//! height. Like macOS's Settings window (`settings_window`), the pages are the overlay's own settings
//! drawn here: handlers run on the overlay, in its window (`listen`), and the overlay keeps what the
//! window shows (`Fluent`) so the shared key field knows its provider (`Overlay::key_target`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, Div, ElementId, Entity, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, Pixels, Rgba, ScrollHandle,
    SharedString, Stateful, Styled, Window, WindowHandle, deferred, div, linear_color_stop, linear_gradient, prelude::*, px, rgb, rgba,
};

use super::{MENU_MAX_HEIGHT, Picker, Tab, listen};
use crate::input::TextInput;
use crate::overlay::Overlay;
use crate::settings::{Provider, Settings};
use crate::{theme, ui};

mod about;
mod calendar;
mod listening;
mod model;
mod sessions;
mod shortcuts;
mod window;
mod window_tab;

pub(crate) use window::{FluentWindow, close, open};

/// The nav pane: page, label, icon.
const PAGES: [(Tab, &str, &str); 8] = [
    (Tab::Model, "Model", "icons/tab-model.svg"),
    (Tab::Listening, "Listening", "icons/tab-listening.svg"),
    (Tab::Modes, "Modes", "icons/tab-modes.svg"),
    (Tab::Keys, "Shortcuts", "icons/tab-shortcuts.svg"),
    (Tab::Window, "Window", "icons/tab-window.svg"),
    (Tab::History, "Sessions", "icons/tab-sessions.svg"),
    (Tab::Calendar, "Calendar", "icons/setup-calendar.svg"),
    (Tab::About, "About", "icons/tab-about.svg"),
];

/// The nav pane; the page fills the rest of the fixed-width window.
const NAV_WIDTH: f32 = 240.0;
/// Hover fill for nav items and flyout rows that aren't selected.
const HOVER: u32 = 0xffffff0a;
/// Errors (a CLI that can't be used, a key that couldn't be read), as elsewhere in Settings.
const DANGER: u32 = 0xffb4a8;

/// The Settings window and what it shows, kept on the overlay.
#[derive(Default)]
pub(crate) struct Fluent {
    pub(crate) window: Option<WindowHandle<FluentWindow>>,
    /// The page shown, while the window is open.
    pub(crate) tab: Option<Tab>,
    /// Modes › <mode>: the mode whose own page is open, instead of the list.
    pub(crate) mode_page: Option<String>,
    /// The expander cards toggled from how they start (`OPEN_AT_FIRST`), by id.
    toggled: Vec<&'static str>,
    scroll: ScrollHandle,
    /// Where the selected subscription's CLI is (Model › Location), once looked up.
    cli_path: Option<(Provider, SharedString)>,
    /// Listening's level meters, while that page is open.
    meters: Option<listening::Meters>,
    /// How many sessions are saved (Sessions), once counted.
    session_count: Option<usize>,
    /// The page's height and how far it scrolls, as the scrollbar last drew them.
    extent: Rc<Cell<(f32, f32)>>,
    /// Where each ComboBox's face was last laid out, by `Picker`.
    faces: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
}

/// Expanders that start open, as drawn: Listening's engine and About's update notes.
const OPEN_AT_FIRST: [&str; 2] = ["engine", "update"];

impl Fluent {
    /// Show the page from its top.
    pub(crate) fn scroll_to_top(&self) { self.scroll.set_offset(gpui::point(px(0.0), px(0.0))); }
}

/// The page title, 28/36 600.
pub(crate) fn title(text: impl Into<SharedString>) -> Div {
    div().text_size(px(28.0)).line_height(px(36.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(text.into())
}

/// A group of cards under its header: header 14/20 600, 8 px above the cards, cards 4 px apart.
pub(crate) fn section(header: impl Into<SharedString>, cards: impl IntoIterator<Item = AnyElement>) -> Div {
    div().flex().flex_col().gap(px(8.0))
        .child(div().text_size(px(14.0)).line_height(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(header.into()))
        .child(cards_column(cards))
}

/// Cards 4 px apart, without a header.
pub(crate) fn cards_column(cards: impl IntoIterator<Item = AnyElement>) -> Div {
    div().flex().flex_col().gap(px(4.0)).children(cards)
}

/// A SettingsCard: icon, label (and status), control on the right.
pub(crate) fn card(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id).flex().items_center().gap(px(16.0)).min_h(px(68.0)).px(px(16.0)).rounded(px(6.0))
        .bg(theme::field()).border_1().border_color(theme::divider())
}

/// The card shell of an expander: its header row and, while open, the rows of its body.
pub(crate) fn expander(id: impl Into<ElementId>, header: Stateful<Div>, body: Option<Vec<AnyElement>>) -> Stateful<Div> {
    div().id(id).flex().flex_col().rounded(px(6.0)).bg(theme::field()).border_1().border_color(theme::divider()).overflow_hidden()
        .child(header.min_h(px(68.0)).px(px(16.0)))
        .children(body.into_iter().flatten())
}

/// The header row of an expander (`expander` gives it the card's height and padding).
pub(crate) fn expander_header() -> Div { div().flex().items_center().gap(px(16.0)) }

/// A row in an open expander's body, indented to the header's label.
pub(crate) fn expander_row(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id).flex().items_center().gap(px(12.0)).min_h(px(52.0)).pl(px(52.0)).pr(px(16.0))
        .bg(theme::fluent_expander()).border_t_1().border_color(theme::divider())
}

/// The selected / in-use colors on a card.
pub(crate) fn in_use(card: Stateful<Div>) -> Stateful<Div> { card.bg(theme::fluent_in_use()).border_color(theme::bubble_border()) }

pub(crate) fn card_icon(path: &'static str) -> impl IntoElement { ui::icon(path, 20.0, theme::body()) }

/// The label column: 14/20 text, and a 12/16 status line when there is one.
pub(crate) fn card_label(text: impl Into<SharedString>, status: Option<AnyElement>) -> Div {
    div().flex().flex_col().flex_1().min_w_0()
        .child(div().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::text()).child(text.into()))
        .children(status)
}

pub(crate) fn status(text: impl Into<SharedString>) -> Div {
    div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(text.into())
}

/// A status line with a colored dot before it ("● Found automatically").
pub(crate) fn dot_status(text: impl Into<SharedString>, dot: Rgba) -> Div {
    div().flex().items_center().gap(px(6.0)).child(div().size(px(6.0)).flex_none().rounded(px(3.0)).bg(dot)).child(status(text))
}

/// The 40 × 20 switch: on = accent with the knob right, off = an outline with the knob left.
pub(crate) fn switch(on: bool) -> Div {
    let track = div().w(px(40.0)).h(px(20.0)).flex_none().flex().items_center().rounded(px(10.0));
    if on { track.justify_end().px(px(4.0)).bg(theme::accent()).child(div().size(px(12.0)).rounded(px(6.0)).bg(theme::accent_ink())) }
    else { track.px(px(3.0)).border_1().border_color(theme::muted()).child(div().size(px(12.0)).rounded(px(6.0)).bg(theme::body())) }
}

/// "On" / "Off" and the switch; the caller adds the click.
pub(crate) fn toggle(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div().id(id).flex().flex_none().items_center().gap(px(12.0)).cursor_pointer()
        .child(div().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::body()).child(if on { "On" } else { "Off" }))
        .child(switch(on))
}

/// A 32 px button: secondary (#1B1D20 with an outline) or primary (accent).
pub(crate) fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    let base = div().id(id).flex().flex_none().items_center().gap(px(8.0)).h(px(32.0)).px(px(12.0)).rounded(px(4.0)).cursor_pointer()
        .text_size(px(13.0)).line_height(px(16.0)).child(label.into());
    if primary { base.bg(theme::accent()).text_color(theme::accent_ink()) }
    else { base.bg(theme::raised()).border_1().border_color(theme::keycap_border()).text_color(theme::text()).hover(|button| button.bg(theme::fluent_selected())) }
}

/// The "✓ In use" / "✓ Active" pill.
pub(crate) fn pill(text: &'static str) -> Div {
    div().flex().flex_none().items_center().gap(px(5.0)).px(px(9.0)).py(px(4.0)).rounded_full().bg(theme::accent())
        .child(ui::icon("icons/check.svg", 12.0, theme::accent_ink()))
        .child(div().text_size(px(11.0)).line_height(px(14.0)).font_weight(FontWeight::BOLD).text_color(theme::accent_ink()).child(text))
}

/// An expander's chevron: down while closed, up while open.
pub(crate) fn chevron(open: bool) -> impl IntoElement {
    ui::icon(if open { "icons/chevron-up.svg" } else { "icons/chevron-down.svg" }, 12.0, theme::body())
}

/// The › at the end of a card that opens a page.
pub(crate) fn chevron_right(size: f32, color: Rgba) -> impl IntoElement { ui::icon("icons/chevron-right.svg", size, color) }

/// "↗" after a label that opens a web page.
pub(crate) fn external(size: f32, color: Rgba) -> impl IntoElement { ui::icon("icons/setup-external.svg", size, color) }

/// One key: 24 tall, #23262B with a 2 px bottom edge. Words are 12 px, single glyphs 14 px so they
/// read at keycap size.
pub(crate) fn keycap(key: &str) -> Div {
    let glyph = key.chars().count() == 1;
    div().flex().flex_none().items_center().justify_center().h(px(24.0)).min_w(px(24.0)).px(px(7.0)).rounded(px(4.0))
        .bg(theme::fluent_selected()).border_1().border_b_2().border_color(theme::keycap_border())
        .text_size(px(if glyph { 14.0 } else { 12.0 })).line_height(px(16.0)).text_color(theme::text()).child(SharedString::from(key.to_string()))
}

/// A shortcut's keys ("Ctrl Shift '") as keycaps, 4 px apart.
pub(crate) fn keycaps(label: &str) -> Div {
    div().flex().items_center().gap(px(4.0)).children(label.split(' ').filter(|key| !key.is_empty()).map(keycap))
}

/// The app mark on its dark tile (nav header 36 px radius 8, About 88 px radius 16).
pub(crate) fn app_tile(size: f32, radius: f32, mark: f32) -> Div {
    div().size(px(size)).flex_none().flex().items_center().justify_center().rounded(px(radius)).border_1().border_color(theme::keycap_border())
        .bg(linear_gradient(180.0, linear_color_stop(rgb(theme::FLUENT_MARK_TOP), 0.0), linear_color_stop(rgb(theme::FLUENT_MARK_BOTTOM), 1.0)))
        .child(ui::viewfinder(mark))
}

/// Errors under a card's label.
pub(crate) fn error_text(text: impl Into<SharedString>) -> Div {
    div().text_size(px(12.0)).line_height(px(16.0)).text_color(rgb(DANGER)).child(text.into())
}

/// A Win11 TextBox around `input`: 32 tall, a 2 px accent bottom edge while it has the keyboard.
/// Clicking gives it the keyboard in this window.
pub(crate) fn text_box(id: impl Into<ElementId>, input: &Entity<TextInput>, window: &Window, cx: &App) -> Stateful<Div> {
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    let focus = input.clone();
    div().id(id).relative().flex().items_center().h(px(32.0)).px(px(11.0)).rounded(px(4.0)).bg(theme::raised()).cursor_text()
        .border_1().border_color(theme::hairline()).overflow_hidden().text_size(px(13.0))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&focus.focus_handle(cx)))
        .when(focused, |field| field.child(div().absolute().left_0().right_0().bottom_0().h(px(2.0)).bg(theme::accent())))
        .child(div().flex_1().min_w_0().child(input.clone()))
}

impl Overlay {
    /// The window under its title bar: the nav pane, the page (scrolling) and its scrollbar.
    pub(crate) fn fluent_main(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        self.in_window(cx, |this, cx| {
            let tab = this.fluent.tab.unwrap_or_default();
            let page = match tab {
                Tab::Model => this.fluent_model(window, cx),
                Tab::Listening => this.fluent_listening(window, cx),
                Tab::Modes => this.fluent_modes(window, cx),
                Tab::Keys => this.fluent_shortcuts(),
                Tab::Window => this.fluent_window_tab(cx),
                Tab::History => this.fluent_sessions(cx),
                Tab::Calendar => this.fluent_calendar(window, cx),
                Tab::About => this.fluent_about(cx),
            };
            let content = div().id("fluent-content").relative().flex_1().min_w_0().h_full().overflow_y_scroll().track_scroll(&this.fluent.scroll)
                .child(page.w(px(window::WIDTH - 2.0 - NAV_WIDTH)).pt(px(20.0)).pr(px(32.0)).pb(px(32.0)).pl(px(12.0)))
                .child(extent_watch(this.fluent.scroll.clone(), this.fluent.extent.clone()));
            div().relative().flex_1().min_h_0().flex()
                .child(this.fluent_nav(tab, cx))
                .child(content)
                .children(scrollbar(&this.fluent.scroll))
                .children(this.fluent_mode_menu(cx))
                .into_any_element()
        })
    }

    fn fluent_nav(&self, current: Tab, cx: &mut Context<Self>) -> Div {
        let header = div().flex().items_center().gap(px(12.0)).px(px(4.0)).pt(px(8.0)).pb(px(16.0))
            .child(app_tile(36.0, 8.0, 24.0))
            .child(div().flex().flex_col()
                .child(div().text_size(px(14.0)).line_height(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("CluelyRS"))
                .child(status(format!("Version {}", env!("CARGO_PKG_VERSION")))));
        let mut nav = div().w(px(NAV_WIDTH)).flex_none().flex().flex_col().gap(px(4.0)).pt(px(8.0)).px(px(12.0)).pb(px(16.0)).child(header);
        for (index, (tab, label, icon)) in PAGES.into_iter().enumerate() {
            let selected = tab == current;
            nav = nav.child(div().id(("fluent-nav", index)).relative().flex().flex_none().items_center().gap(px(14.0)).h(px(36.0)).px(px(12.0))
                .rounded(px(4.0)).cursor_pointer()
                .when(selected, |item| item.bg(theme::fluent_selected()))
                .when(!selected, |item| item.hover(|item| item.bg(rgba(HOVER))))
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.fluent_select(tab, window, cx)))
                .child(ui::icon(icon, 20.0, if selected { theme::accent_soft() } else { theme::body() }))
                .child(div().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::text()).child(label))
                .when(selected, |item| item.child(div().absolute().left_0().top(px(10.0)).w(px(3.0)).h(px(16.0)).rounded(px(2.0)).bg(theme::accent()))));
        }
        nav
    }

    /// Show `tab` from its top, refreshing what it shows (accounts, devices, sessions' size).
    pub(crate) fn fluent_select(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.fluent.tab == Some(tab) && self.fluent.mode_page.is_none();
        self.fluent.tab = Some(tab);
        self.fluent.mode_page = None;
        self.fluent.scroll_to_top();
        if !same {
            self.prepare_tab(tab, window, cx);
            self.fluent_prepare(tab, window, cx);
        }
        cx.notify();
    }

    /// What only the Settings window shows, as `tab` opens: Listening's meters, how many sessions there are.
    pub(crate) fn fluent_prepare(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if tab == Tab::Listening && self.fluent.meters.is_none() { self.fluent_start_meters(window, cx); }
        if tab == Tab::History { self.fluent_count_sessions(window, cx); }
    }

    pub(crate) fn fluent_expanded(&self, id: &'static str) -> bool { OPEN_AT_FIRST.contains(&id) != self.fluent.toggled.contains(&id) }

    pub(crate) fn fluent_toggle(&mut self, id: &'static str, cx: &mut Context<Self>) {
        if let Some(index) = self.fluent.toggled.iter().position(|open| *open == id) { self.fluent.toggled.remove(index); }
        else { self.fluent.toggled.push(id); }
        cx.notify();
    }

    /// "On"/"Off" and a switch bound to a boolean setting.
    pub(crate) fn fluent_switch(&self, id: &'static str, on: bool, set: fn(&mut Settings, bool), cx: &mut Context<Self>) -> Stateful<Div> {
        toggle(id, on).on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.update_settings(|s| set(s, !on), window, cx) }))
    }

    /// A card with an icon, a label and a switch bound to a boolean setting.
    pub(crate) fn toggle_card(&self, id: &'static str, icon: &'static str, label: &'static str, on: bool, set: fn(&mut Settings, bool), cx: &mut Context<Self>) -> AnyElement {
        card(id).child(card_icon(icon)).child(card_label(label, None)).child(self.fluent_switch(id, on, set, cx)).into_any_element()
    }

    /// A ComboBox bound to a string-valued setting: the shared dropdown state (`Picker`,
    /// `open_picker`), drawn as a Win11 ComboBox with its list as a flyout under it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fluent_combo(&self, picker: Picker, icon: Option<fn(&str) -> &'static str>, value: impl Into<SharedString>, options: Vec<(String, String)>,
        selected: &str, set: fn(&mut Settings, String), width: f32, cx: &mut Context<Self>) -> Div {
        let open = self.open_picker == Some(picker);
        let value: SharedString = value.into();
        // Where each face is: an open one's, so a click on it closes the list rather than reopening it,
        // and every one's, so a list near the bottom of the window opens upward.
        let (face_bounds, faces) = (self.picker_face.clone(), self.fluent.faces.clone());
        let face = combo_face(("fluent-combo", picker as usize), icon.map(|icon| icon(selected)), value, width)
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, _, cx| { cx.stop_propagation(); this.toggle_picker(picker, cx) }))
            .child(gpui::canvas(move |bounds, _, _| {
                faces.borrow_mut().insert(picker as usize, bounds);
                if open { face_bounds.set(Some(bounds)); }
            }, |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        let list_height = (options.len() as f32 * 34.0 + 8.0).min(MENU_MAX_HEIGHT + 8.0);
        let upward = self.fluent.faces.borrow().get(&(picker as usize)).is_some_and(|face| f32::from(face.bottom()) + 4.0 + list_height > window::HEIGHT - 8.0);
        let mut wrapper = div().relative().flex_none().child(face);
        if open {
            let face_bounds = self.picker_face.clone();
            let mut list = flyout().id("fluent-combo-list").w_full().min_w(px(width)).max_h(px(MENU_MAX_HEIGHT)).overflow_y_scroll()
                .on_mouse_down_out(listen(cx, move |this, event: &gpui::MouseDownEvent, _, cx| {
                    if face_bounds.get().is_some_and(|face| face.contains(&event.position)) { return; }
                    this.close_picker(cx);
                }));
            for (index, (id, label)) in options.into_iter().enumerate() {
                let chosen = id.clone();
                list = list.child(flyout_item(("fluent-option", index), id == selected, true)
                    .when_some(icon, |row, icon| row.child(div().w(px(16.0)).flex_none().flex().justify_center().child(ui::icon(icon(&id), 14.0, theme::body()))))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_picker(cx);
                        let value = chosen.clone();
                        this.update_settings(|s| set(s, value), window, cx);
                    })));
            }
            let anchor = if upward { div().absolute().bottom(px(36.0)) } else { div().absolute().top(px(36.0)) };
            wrapper = wrapper.child(deferred(anchor.left_0().w_full().child(list)).with_priority(1));
        }
        wrapper
    }
}

/// A ComboBox's closed face: 32 tall, the value and a chevron.
pub(crate) fn combo_face(id: impl Into<ElementId>, icon: Option<&'static str>, value: SharedString, width: f32) -> Stateful<Div> {
    div().id(id).relative().flex().items_center().justify_between().gap(px(10.0)).h(px(32.0)).w(px(width)).px(px(11.0)).rounded(px(4.0)).cursor_pointer()
        .bg(theme::raised()).border_1().border_color(theme::hairline())
        .hover(|face| face.bg(theme::fluent_selected()))
        .child(div().flex().items_center().gap(px(8.0)).min_w_0()
            .when_some(icon, |row, icon| row.child(ui::icon(icon, 14.0, theme::text())))
            .child(div().min_w_0().truncate().text_size(px(13.0)).line_height(px(16.0)).text_color(theme::text()).child(value)))
        .child(chevron(false))
}

/// A flyout (ComboBox list, ⋯ menu): radius 8, padding 4, rows 2 px apart.
pub(crate) fn flyout() -> Div {
    div().flex().flex_col().gap(px(2.0)).p(px(4.0)).rounded(px(8.0)).bg(theme::raised()).border_1().border_color(theme::keycap_border())
        .shadow(vec![gpui::BoxShadow { color: gpui::hsla(0.0, 0.0, 0.0, 0.5), offset: gpui::point(px(0.0), px(12.0)), blur_radius: px(32.0), spread_radius: px(0.0) }])
}

/// A flyout row, 32 tall; the selected one has the accent pill at its left edge. A disabled row is
/// muted and doesn't highlight.
pub(crate) fn flyout_item(id: impl Into<ElementId>, selected: bool, enabled: bool) -> Stateful<Div> {
    div().id(id).relative().flex().flex_none().items_center().gap(px(8.0)).h(px(32.0)).px(px(10.0)).rounded(px(4.0))
        .text_size(px(13.0)).line_height(px(16.0))
        .when(enabled, |row| row.cursor_pointer().text_color(theme::text()).hover(|row| row.bg(theme::fluent_selected())))
        .when(!enabled, |row| row.text_color(theme::muted()))
        .when(selected, |row| row.bg(theme::fluent_selected())
            .child(div().absolute().left_0().top(px(8.0)).w(px(3.0)).h(px(16.0)).rounded(px(2.0)).bg(theme::accent())))
}

/// Draw again once the page is laid out at a new height (a page or an expander opened), so the
/// scrollbar, which reads the last layout, never lags a frame behind.
fn extent_watch(scroll: ScrollHandle, drawn: Rc<Cell<(f32, f32)>>) -> impl IntoElement {
    gpui::canvas(move |_, window, _| {
        let now = (f32::from(scroll.bounds().size.height), f32::from(scroll.max_offset().height));
        if drawn.replace(now) != now { window.refresh(); }
    }, |_, _, _, _| {}).absolute().top_0().left_0().size_full()
}

/// The Win11 overlay scrollbar: a 3 px thumb, 4 px from the right edge, while the page overflows.
fn scrollbar(scroll: &ScrollHandle) -> Option<Div> {
    let viewport = f32::from(scroll.bounds().size.height);
    let max = f32::from(scroll.max_offset().height);
    if max <= 0.5 || viewport <= 0.0 { return None; }
    let track = viewport - 16.0;
    let thumb = (track * viewport / (viewport + max)).max(24.0);
    let progress = (-f32::from(scroll.offset().y) / max).clamp(0.0, 1.0);
    Some(div().absolute().right(px(4.0)).top(px(8.0 + (track - thumb) * progress)).w(px(3.0)).h(px(thumb)).rounded(px(2.0)).bg(theme::fluent_scrollbar()))
}
