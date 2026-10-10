//! Settings › Window › Appearance, laid out like the Paper artboards "Appearance · macOS · Settings ›
//! Window · Opacity" and "Appearance · Windows · Settings › Window · Opacity": how see-through the
//! overlay's card is, the size of its reading text and its width. Every change shows on the overlay at
//! once; the opacity slider writes settings.json when it is released rather than at every step.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{Bounds, Context, Div, DragMoveEvent, FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement, Pixels, Styled, div, prelude::*, px, relative};

use super::{button, choice, listen, switch_row};
use crate::appearance::{self, CardWidth, DEFAULT_OPACITY, TextSize};
use crate::overlay::Overlay;
use crate::{theme, ui};

/// What GPUI carries while the opacity slider's knob is dragged.
struct OpacityDrag;

/// Room for Reset, kept while it's hidden so the track never changes length under the pointer.
const RESET_WIDTH: f32 = 56.0;

impl Overlay {
    pub(super) fn appearance_section(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        div().flex().flex_col().gap(px(12.0)).pt(px(16.0)).border_t_1().border_color(gpui::rgb(0x23262a))
            .child(ui::section_label("APPEARANCE"))
            .child(self.opacity_row(cx))
            .child(switch_row("Answer text size", "Answers and the transcript line; the text box and toolbar keep their size",
                choice("text-size", &TextSize::ALL, s.text_size, |s, v| s.text_size = v, cx)))
            .child(switch_row("Card width", "The overlay grows and shrinks around its centre; its top edge stays put",
                choice("card-width", &CardWidth::ALL, s.card_width, |s, v| s.card_width = v, cx)))
            // macOS: room above the Dock section's divider. On Windows this section ends the tab.
            .when(cfg!(target_os = "macos"), |section| section.pb(px(16.0)))
    }

    /// Background opacity: what it does, a preview on macOS (the overlay itself may be elsewhere on the
    /// screen; on Windows it is right above this panel), then the slider, its value and Reset.
    fn opacity_row(&self, cx: &mut Context<Self>) -> Div {
        let percent = self.store.value.background_opacity.min(100);
        let reset = div().w(px(RESET_WIDTH)).flex_none().flex().justify_end()
            .when(percent != DEFAULT_OPACITY, |slot| slot.child(button("opacity-reset", "Reset", false)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.update_settings(|s| s.background_opacity = DEFAULT_OPACITY, window, cx)))));
        let range_label = |text: &'static str| div().flex_none().text_size(px(11.0)).text_color(theme::muted()).child(text);
        let slider = div().flex().items_center().gap(px(12.0))
            .child(range_label("0%"))
            .child(self.opacity_slider(percent, cx))
            .child(range_label("100%"))
            .child(div().w(px(40.0)).flex_none().flex().justify_end().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text()).child(format!("{percent}%")))
            .child(reset);
        let about = div().flex().flex_col().gap(px(2.0)).flex_1().min_w_0()
            .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Background opacity"))
            .child(div().text_size(px(12.0)).text_color(theme::muted())
                .child("How much of what's behind shows through the overlay's card. Text, buttons and its outline stay solid."));
        let header = div().flex().items_center().gap(px(16.0)).child(about);
        #[cfg(target_os = "macos")]
        let header = header.child(preview(percent));
        div().flex().flex_col().gap(px(10.0)).child(header).child(slider)
    }

    /// The track, filled up to the value, with a tick at the default. Pressing jumps to the pointer and
    /// dragging follows it; both show on the overlay at once, and the release saves.
    fn opacity_slider(&self, percent: u8, cx: &mut Context<Self>) -> impl IntoElement {
        let fraction = f32::from(percent) / 100.0;
        // Where the track was laid out this frame, so a press or a drag maps to a value.
        let track: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
        let value_at = {
            let track = track.clone();
            move |x: Pixels| track.get().map(|bounds| appearance::opacity_at(f32::from(x - bounds.left()) / f32::from(bounds.size.width).max(1.0)))
        };
        let pressed = value_at.clone();
        let bar = || div().absolute().left_0().top(px(8.0)).h(px(4.0)).rounded_full();
        div().id("opacity-slider").relative().flex_1().min_w_0().h(px(20.0)).cursor_pointer()
            .child(gpui::canvas(move |bounds, _, _| track.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .child(bar().w_full().bg(theme::hairline()))
            .child(bar().w(relative(fraction)).bg(theme::accent()))
            .child(div().absolute().top(px(3.0)).left(relative(f32::from(DEFAULT_OPACITY) / 100.0)).ml(px(-1.0)).w(px(2.0)).h(px(14.0))
                .rounded(px(1.0)).bg(gpui::rgb(0x4a4d52)))
            .child(div().absolute().top(px(1.0)).left(relative(fraction)).ml(px(-9.0)).size(px(18.0)).rounded_full()
                .bg(theme::text()).border_1().border_color(gpui::rgb(0x0f1012)))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, event: &MouseDownEvent, _, cx| {
                if let Some(value) = pressed(event.position.x) { this.opacity_drag = true; this.preview_opacity(value, cx); }
            }))
            .on_drag(OpacityDrag, |_, _, _, cx| cx.new(|_| gpui::Empty))
            .on_drag_move(listen(cx, move |this, event: &DragMoveEvent<OpacityDrag>, _, cx| {
                if let Some(value) = value_at(event.event.position.x) { this.preview_opacity(value, cx); }
            }))
            .on_mouse_up(MouseButton::Left, listen(cx, |this, _: &MouseUpEvent, _, cx| this.finish_opacity_drag(cx)))
            .on_mouse_up_out(MouseButton::Left, listen(cx, |this, _: &MouseUpEvent, _, cx| this.finish_opacity_drag(cx)))
    }

    /// Show `percent` on the overlay without writing settings.json yet (`finish_opacity_drag` does).
    fn preview_opacity(&mut self, percent: u8, cx: &mut Context<Self>) {
        if self.store.value.background_opacity == percent { return; }
        self.store.value.background_opacity = percent;
        cx.notify();
    }

    /// The slider was released (anywhere): save what it was dragged to.
    fn finish_opacity_drag(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.opacity_drag) { return; }
        self.store.save();
        cx.notify();
    }
}

/// A small stand-in for a busy screen with the card over it at `percent`, beside the macOS slider.
#[cfg(target_os = "macos")]
fn preview(percent: u8) -> impl IntoElement {
    let line = |width: f32, color: u32| div().w(px(width)).h(px(4.0)).rounded(px(2.0)).bg(gpui::rgb(color));
    let page = div().absolute().top_0().left_0().size_full().flex().flex_col().gap(px(4.0)).p(px(8.0))
        .child(line(54.0, 0x2a2b2e)).child(line(40.0, 0x6b6f76)).child(line(46.0, 0x6b6f76));
    let card = div().absolute().left(px(14.0)).top(px(30.0)).w(px(104.0)).h(px(34.0)).rounded(px(7.0)).border_1().border_color(gpui::rgba(0xffffff14))
        .bg(appearance::fill(0x121315, percent)).flex().flex_col().gap(px(4.0)).px(px(8.0)).py(px(7.0))
        .child(line(70.0, 0xf2efe8)).child(line(52.0, 0xc9c5bc)).child(line(60.0, 0xc9c5bc));
    div().relative().w(px(132.0)).h(px(72.0)).flex_none().rounded(px(9.0)).overflow_hidden().border_1().border_color(theme::hairline())
        .bg(gpui::rgb(0xe9e4d8))
        .child(div().absolute().top_0().right_0().w(px(58.0)).h_full().bg(gpui::rgb(0x3d6b8c)))
        .child(div().absolute().top(px(10.0)).right(px(10.0)).size(px(22.0)).rounded_full().bg(gpui::rgb(0xd9a877)))
        .child(page)
        .child(card)
}
