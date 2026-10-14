//! macOS Settings, laid out like the Paper "Redesign v3 · Cluely 2.0 style" artboards: each tab is a
//! 640 px column of titled sections, and each setting a row (a 40 px icon tile, its name, its
//! control), with a second line only for a status. The settings, their handlers and what saving does
//! are the shared ones in `settings_view`; only the drawing is here.

mod listening;
mod model;
mod pages;

pub(crate) use pages::WindowAction;

use gpui::{AnyElement, Context, Div, Focusable, FontWeight, IntoElement, MouseButton, MouseDownEvent, ParentElement, SharedString, Stateful, Styled, deferred, div, prelude::*, px};

use super::{MENU_MAX_HEIGHT, Picker, Tab, listen};
use crate::overlay::Overlay;
use crate::settings::Settings;
use crate::ui_v3::{self, Kind, RADII};
use crate::{theme, ui};

/// What the Settings window measures itself and hands to its tabs.
#[derive(Clone, Copy, Default)]
pub(crate) struct WindowState {
    /// The audio check's levels (microphone, desktop audio), 0..=1, while Listening is open.
    pub levels: [f32; 2],
    /// How many sessions are saved, once counted.
    pub sessions: Option<usize>,
}

/// Width of the dropdowns at the end of rows.
const DROPDOWN_WIDTH: f32 = 210.0;

/// A section: its title, then its rows 4 px apart.
fn section(title: &'static str, gap: f32, rows: Vec<AnyElement>) -> Div {
    div().flex().flex_col().gap(px(gap))
        .child(div().text_size(px(15.0)).line_height(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
        .child(div().flex().flex_col().gap(px(4.0)).children(rows))
}

/// A row's name, and under it a status when there is one.
fn label(title: impl Into<SharedString>, status: Option<AnyElement>) -> Div {
    div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
        .child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title.into()))
        .children(status)
}

/// A muted status line under a row's name.
fn status(text: impl Into<SharedString>) -> AnyElement {
    div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(text.into()).into_any_element()
}

/// A row: its icon on a tile, its name (and status), and its control at the end.
fn row(icon: &'static str, title: impl Into<SharedString>, status: Option<AnyElement>, control: impl IntoElement) -> Div {
    div().flex().items_center().gap(px(14.0)).px(px(12.0)).py(px(8.0)).border_1().border_color(gpui::transparent_black())
        .child(ui_v3::tile(40.0, false).child(ui::icon(icon, 20.0, theme::body())))
        .child(label(title, status))
        .child(control)
}

/// A switch bound to a boolean setting.
fn switch(name: &'static str, on: bool, set: fn(&mut Settings, bool), cx: &mut Context<Overlay>) -> impl IntoElement {
    div().id(name).flex_none().cursor_pointer()
        .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.update_settings(|s| set(s, !on), window, cx) }))
        .child(ui_v3::switch(on))
}

/// A segmented control over `options`; clicking applies `set` and saves.
fn choice<T: Copy + PartialEq + 'static>(name: &'static str, options: &[(T, &'static str)], current: T, set: fn(&mut Settings, T), cx: &mut Context<Overlay>) -> Div {
    options.iter().copied().enumerate().fold(ui_v3::segmented(), |control, (index, (value, text))| control.child(
        ui_v3::segment((name, index), text, value == current)
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.update_settings(|s| set(s, value), window, cx)))))
}

/// A button that runs `f` on the overlay.
fn action(id: &'static str, text: impl Into<SharedString>, kind: Kind, cx: &mut Context<Overlay>,
    f: impl Fn(&mut Overlay, &mut gpui::Window, &mut Context<Overlay>) + 'static) -> Stateful<Div> {
    let button = ui_v3::button(id, text, kind);
    if kind == Kind::Disabled { return button; }
    button.on_mouse_down(MouseButton::Left, listen(cx, move |this, _: &MouseDownEvent, window, cx| { cx.stop_propagation(); f(this, window, cx) }))
}

/// A quiet text link ("Get a key ↗", "Sign out").
fn link(id: &'static str, text: &'static str, accent: bool) -> Stateful<Div> {
    div().id(id).flex_none().cursor_pointer().text_size(px(12.0)).line_height(px(16.0))
        .text_color(if accent { theme::accent_soft() } else { theme::muted() })
        .when(!accent, |link| link.hover(|link| link.text_color(theme::body())))
        .child(text)
}

/// A text field in the v3 style, focused when clicked.
fn text_field(input: &gpui::Entity<crate::input::TextInput>, id: &'static str) -> Stateful<Div> {
    let focus = input.clone();
    div().id(id).flex_1().min_w_0().flex().items_center().h(px(32.0)).px(px(10.0)).rounded(px(RADII.control)).cursor_text()
        .bg(theme::field()).border_1().border_color(theme::hairline()).text_size(px(12.0))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| window.focus(&focus.focus_handle(cx)))
        .child(input.clone())
}

impl Overlay {
    /// One tab of the macOS Settings window. About is drawn by the window itself.
    pub(crate) fn mac_tab(&self, tab: Tab, state: WindowState, cx: &mut Context<Self>) -> AnyElement {
        match tab {
            Tab::Model => self.mac_model_tab(cx).into_any_element(),
            Tab::Listening => self.mac_listening_tab(state.levels, cx).into_any_element(),
            Tab::Modes => self.modes_tab(cx),
            Tab::Keys => self.mac_shortcuts_tab().into_any_element(),
            Tab::Window => self.mac_window_tab(cx).into_any_element(),
            Tab::History => self.mac_sessions_tab(state.sessions, cx).into_any_element(),
            Tab::About => div().into_any_element(),
        }
    }

    /// A dropdown at the end of a row: the face, and its list floating under it while open. `logo`
    /// maps an option's id to its logo.
    #[allow(clippy::too_many_arguments)]
    fn mac_dropdown(&self, picker: Picker, logo: Option<fn(&str) -> &'static str>, value: impl Into<SharedString>, options: Vec<(String, String)>,
        selected: &str, set: fn(&mut Settings, String), cx: &mut Context<Self>) -> Div {
        let open = self.open_picker == Some(picker);
        let face = ui_v3::dropdown_face(("mac-picker", picker as usize), logo.map(|logo| logo(selected)), value, open);
        self.dropdown_with(picker, face, DROPDOWN_WIDTH, 36.0, logo, options, selected, set, cx).w(px(DROPDOWN_WIDTH)).flex_none()
    }

    /// `face` opening a list of `options` `top` px below it, at least `width` wide: the dropdowns in
    /// Settings and setup.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn dropdown_with(&self, picker: Picker, face: Stateful<Div>, width: f32, top: f32, logo: Option<fn(&str) -> &'static str>,
        options: Vec<(String, String)>, selected: &str, set: fn(&mut Settings, String), cx: &mut Context<Self>) -> Div {
        let open = self.open_picker == Some(picker);
        let face_bounds = self.picker_face.clone();
        let face = face.relative()
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, _, cx| { cx.stop_propagation(); this.toggle_picker(picker, cx) }))
            .when(open, |face| face.child(gpui::canvas(move |bounds, _, _| face_bounds.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full()));
        let mut wrapper = div().relative().child(face);
        if open {
            let face_bounds = self.picker_face.clone();
            let mut list = menu().id("mac-menu").w_full().max_h(px(MENU_MAX_HEIGHT)).overflow_y_scroll()
                .on_mouse_down_out(listen(cx, move |this, event: &MouseDownEvent, _, cx| {
                    if face_bounds.get().is_some_and(|face| face.contains(&event.position)) { return; }
                    this.close_picker(cx);
                }));
            for (index, (id, text)) in options.into_iter().enumerate() {
                let chosen = id.clone();
                list = list.child(menu_row(("mac-item", index), logo.map(|logo| logo(&id)), text, id == selected)
                    .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_picker(cx);
                        let value = chosen.clone();
                        this.update_settings(|s| set(s, value), window, cx);
                    })));
            }
            wrapper = wrapper.child(deferred(div().absolute().top(px(top)).left_0().w(px(width.max(240.0))).child(list)).with_priority(1));
        }
        wrapper
    }
}

/// A popover list: a menu or a dropdown's options.
pub(crate) fn menu() -> Div {
    div().flex().flex_col().gap(px(1.0)).p(px(5.0)).rounded(px(RADII.card)).bg(theme::raised()).border_1().border_color(theme::keycap_border())
        .shadow(ui_v3::drop_shadow(12.0, 32.0, 0.5))
}

/// A row of a popover list, its logo in a fixed lane and a check when it's the current choice.
pub(crate) fn menu_row(id: impl Into<gpui::ElementId>, logo: Option<&'static str>, text: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    div().id(id).flex().items_center().gap(px(8.0)).px(px(9.0)).py(px(6.0)).rounded(px(RADII.small)).cursor_pointer()
        .text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::MEDIUM).text_color(theme::text())
        .hover(|row| row.bg(theme::selected_fill())).when(selected, |row| row.bg(theme::selected_fill()))
        .when_some(logo, |row, logo| row.child(div().w(px(16.0)).flex_none().flex().justify_center().child(ui::icon(logo, 14.0, theme::text()))))
        .child(div().flex_1().min_w_0().truncate().child(text.into()))
        .when(selected, |row| row.child(ui::icon("icons/check.svg", 12.0, theme::accent())))
}
