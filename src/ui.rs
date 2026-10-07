//! Small styled building blocks shared by the overlay and settings.

use gpui::{Div, ElementId, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px, rgb};

use crate::theme;

pub fn keycap(label: impl Into<SharedString>) -> impl IntoElement {
    div().font_family(theme::MONO).text_size(px(11.0)).text_color(theme::muted())
        .px(px(5.0)).py(px(1.0)).border_1().border_color(theme::keycap_border()).rounded(px(5.0))
        .child(label.into())
}

/// Rounded dark capsule used inside the pill.
pub fn chip() -> Div {
    div().flex().items_center().gap(px(8.0)).h(px(30.0)).px(px(12.0)).rounded_full().bg(theme::raised())
}

pub fn round_button(id: impl Into<ElementId>) -> gpui::Stateful<Div> {
    div().id(id).size(px(30.0)).flex_none().rounded_full().bg(theme::raised()).cursor_pointer()
        .flex().items_center().justify_center()
}

/// Viewfinder mark: four corner brackets around a focus dot.
pub fn mark(diameter: f32) -> impl IntoElement {
    let scale = diameter / 30.0;
    let arm = px(5.0 * scale);
    let stroke = px((1.8 * scale).max(1.5));
    let inset = px(8.0 * scale);
    let corner = |top: bool, left: bool| {
        let mut bracket = div().absolute().size(arm).border_color(theme::accent_ink());
        bracket = if top { bracket.top(inset).border_t(stroke) } else { bracket.bottom(inset).border_b(stroke) };
        if left { bracket.left(inset).border_l(stroke) } else { bracket.right(inset).border_r(stroke) }
    };
    div().relative().size(px(diameter)).flex_none().rounded_full().bg(theme::accent())
        .flex().items_center().justify_center()
        .child(corner(true, true)).child(corner(true, false)).child(corner(false, true)).child(corner(false, false))
        .child(div().size(px(4.0 * scale)).rounded_full().bg(theme::accent_ink()))
}

pub fn section_label(text: &'static str) -> impl IntoElement {
    div().text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::muted()).child(text)
}

/// One option inside a segmented control.
pub fn segment(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> gpui::Stateful<Div> {
    let base = div().id(id).px(px(10.0)).py(px(5.0)).rounded(px(6.0)).text_size(px(12.0)).cursor_pointer().child(label.into());
    if selected { base.bg(rgb(0x26292d)).text_color(theme::text()) } else { base.text_color(theme::muted()) }
}

pub fn segmented() -> Div {
    div().flex().flex_wrap().gap(px(2.0)).p(px(3.0)).rounded(px(8.0)).border_1().border_color(theme::hairline())
}

pub fn switch(on: bool) -> impl IntoElement {
    let track = div().w(px(34.0)).h(px(20.0)).flex_none().rounded_full().p(px(2.0)).flex()
        .bg(if on { theme::accent() } else { theme::hairline() });
    let knob = div().size(px(16.0)).rounded_full().bg(if on { theme::accent_ink() } else { theme::muted() });
    if on { track.justify_end().child(knob) } else { track.child(knob) }
}

/// A full-width selectable row: leading radio, title and description; callers append trailing content.
pub fn row(id: impl Into<ElementId>, title: &'static str, detail: impl Into<SharedString>, selected: bool) -> gpui::Stateful<Div> {
    div().id(id).child(radio(selected)).flex().items_center().gap(px(12.0)).px(px(12.0)).py(px(10.0)).rounded(px(12.0)).border_1().cursor_pointer()
        .border_color(if selected { theme::bubble_border() } else { theme::hairline() })
        .when(selected, |row| row.bg(rgb(0x0d1830)))
        .child(div().flex().flex_col().gap(px(1.0)).flex_1()
            .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
            .child(div().text_size(px(12.0)).text_color(theme::muted()).child(detail.into())))
}

pub fn radio(selected: bool) -> impl IntoElement {
    let ring = div().size(px(16.0)).flex_none().rounded_full();
    if selected { ring.border(px(5.0)).border_color(theme::accent()) } else { ring.border(px(1.5)).border_color(rgb(0x4a4d52)) }
}

pub fn icon(path: &'static str, size: f32, color: gpui::Rgba) -> impl IntoElement {
    gpui::svg().path(path).size(px(size)).flex_none().text_color(color)
}

/// Close button shown at the right of panel headers; Esc does the same.
pub fn close_button(id: &'static str) -> gpui::Stateful<Div> {
    div().id(id).flex().items_center().gap(px(8.0)).pl(px(10.0)).pr(px(6.0)).h(px(28.0)).rounded_full().cursor_pointer()
        .bg(theme::raised()).border_1().border_color(theme::hairline())
        .hover(|button| button.border_color(theme::bubble_border()))
        .child(keycap("Esc"))
        .child(icon("icons/close.svg", 14.0, theme::body()))
}

/// Header row shared by Settings and History.
pub fn panel_header() -> Div {
    div().flex().items_center().justify_between().px(px(18.0)).pt(px(14.0)).pb(px(12.0)).border_b_1().border_color(theme::divider())
}

/// A dropdown's closed face: the current value and a chevron. Pair with [`menu`] while open.
pub fn picker(id: impl Into<ElementId>, value: impl Into<SharedString>, open: bool) -> gpui::Stateful<Div> {
    div().id(id).flex().items_center().justify_between().gap(px(8.0)).h(px(36.0)).px(px(12.0)).rounded(px(9.0)).cursor_pointer()
        .bg(theme::field()).border_1().border_color(if open { theme::accent() } else { theme::hairline() })
        .child(div().min_w_0().text_size(px(13.0)).text_color(theme::text()).text_ellipsis().child(value.into()))
        .child(chevron(open))
}

fn chevron(up: bool) -> impl IntoElement {
    icon(if up { "icons/chevron-up.svg" } else { "icons/chevron-down.svg" }, 12.0, theme::muted())
}

/// The open list under a [`picker`]. Place it absolutely under the picker inside a `relative()`
/// parent and wrap it in `gpui::deferred` so it paints over what follows.
pub fn menu() -> Div {
    // No shadow: the overlay's window region hugs the list, so a soft shadow would be cut square.
    div().flex().flex_col().p(px(4.0)).rounded(px(10.0)).bg(theme::raised()).border_1().border_color(theme::keycap_border())
}

pub fn menu_item(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> gpui::Stateful<Div> {
    let row = div().id(id).flex().items_center().justify_between().gap(px(8.0)).px(px(10.0)).py(px(7.0)).rounded(px(7.0)).cursor_pointer()
        .text_size(px(13.0)).hover(|row| row.bg(rgb(0x26292d)))
        .child(div().truncate().child(label.into()));
    if selected { row.bg(rgb(0x26292d)).text_color(theme::text()).child(div().text_color(theme::accent()).font_weight(FontWeight::BOLD).child("✓")) }
    else { row.text_color(theme::body()) }
}

/// A settings row: label on the left, control on the right, hairline below.
pub fn setting_row(label: &'static str, control: impl IntoElement) -> Div {
    div().flex().items_center().justify_between().gap(px(12.0)).h(px(46.0)).border_b_1().border_color(theme::divider())
        .child(div().text_size(px(13.0)).text_color(theme::text()).child(label))
        .child(control)
}
