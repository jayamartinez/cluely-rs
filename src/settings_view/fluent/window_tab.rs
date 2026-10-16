//! Settings › Window (Paper "Settings · Window" and "Settings · Window · Preview"): privacy, how the
//! overlay looks — a live preview of its card at half size over a desktop, then opacity, answer text
//! size and card width — and the tray icon. The opacity slider previews while it's dragged and saves
//! on release (`appearance`).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, Bounds, Context, Div, DragMoveEvent, FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement, Pixels, Styled,
    div, linear_color_stop, linear_gradient, prelude::*, px, rgb, rgba,
};

use super::{card, card_icon, card_label, section, title, toggle};
use crate::appearance::{self, CardWidth, TextSize};
use crate::overlay::Overlay;
use crate::settings_view::listen;
use crate::theme;

/// What GPUI carries while the opacity slider's thumb is dragged.
struct OpacityDrag;

const SLIDER: f32 = 180.0;
/// The preview's mini card is the overlay's at half size (Narrow 280 / Default 320 / Wide 380).
const PREVIEW_SCALE: f32 = 0.5;
/// The preview's desktop: #13203A → #0E1A33 (45 %) → #2A2B2E at 160°.
const DESKTOP: [u32; 3] = [0x13203a, 0x0e1a33, 0x2a2b2e];

/// The mini card's text: (placeholder size, line height), (answer size, line height).
fn preview_text(size: TextSize) -> ((f32, f32), (f32, f32)) {
    match size { TextSize::Small => ((9.0, 12.0), (9.0, 13.0)), TextSize::Standard => ((9.0, 12.0), (10.0, 14.0)), TextSize::Large => ((10.0, 14.0), (12.0, 16.0)) }
}

/// "Default width · Default text · 80%".
fn preview_caption(width: CardWidth, size: TextSize, opacity: u8) -> String {
    let width = match width { CardWidth::Narrow => "Narrow", CardWidth::Standard => "Default width", CardWidth::Wide => "Wide" };
    let size = match size { TextSize::Small => "Small text", TextSize::Standard => "Default text", TextSize::Large => "Large text" };
    format!("{width} · {size} · {}%", opacity.min(100))
}

impl Overlay {
    pub(super) fn fluent_window_tab(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let privacy = vec![
            self.toggle_card("hide-capture", "icons/eye-off.svg", "Hide from screen capture", s.hide_from_capture, |s, v| s.hide_from_capture = v, cx),
            self.toggle_card("screen-on-send", "icons/setup-pictures.svg", "Attach a screenshot to every message", s.screen_on_send, |s, v| s.screen_on_send = v, cx),
            self.toggle_card("live-on-launch", "icons/setup-power.svg", "Start Live when the app opens", s.start_live_on_launch, |s, v| s.start_live_on_launch = v, cx),
        ];
        let id = |value: &dyn std::fmt::Debug| format!("{value:?}");
        let text_sizes: Vec<(String, String)> = TextSize::ALL.iter().map(|(size, label)| (id(size), label.to_string())).collect();
        let widths: Vec<(String, String)> = CardWidth::ALL.iter().map(|(width, label)| (id(width), label.to_string())).collect();
        let label_of = |all: &[(String, String)], value: String| all.iter().find(|(id, _)| *id == value).map_or(value.clone(), |(_, label)| label.clone());
        let appearance = vec![
            self.fluent_preview_card(),
            card("opacity").child(card_icon("icons/setup-contrast.svg")).child(card_label("Opacity", None)).child(self.fluent_opacity(cx)).into_any_element(),
            card("text-size").child(card_icon("icons/setup-text-size.svg")).child(card_label("Answer text size", None))
                .child(self.fluent_combo(crate::settings_view::Picker::TextSize, None, label_of(&text_sizes, id(&s.text_size)), text_sizes.clone(), &id(&s.text_size),
                    |s, v| s.text_size = TextSize::ALL.iter().find(|(size, _)| format!("{size:?}") == v).map_or(s.text_size, |(size, _)| *size), 200.0, cx))
                .into_any_element(),
            card("card-width").child(card_icon("icons/setup-width.svg")).child(card_label("Card width", None))
                .child(self.fluent_combo(crate::settings_view::Picker::CardWidth, None, label_of(&widths, id(&s.card_width)), widths.clone(), &id(&s.card_width),
                    |s, v| s.card_width = CardWidth::ALL.iter().find(|(width, _)| format!("{width:?}") == v).map_or(s.card_width, |(width, _)| *width), 200.0, cx))
                .into_any_element(),
        ];
        // Turning the icon off asks first (`Overlay::set_menu_bar_icon`).
        let tray_on = s.show_menu_bar_icon;
        let tray = card("show-tray-icon").child(card_icon("icons/setup-tray.svg")).child(card_label("Show tray icon", None))
            .child(toggle("show-tray-icon-switch", tray_on)
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| { cx.stop_propagation(); this.set_menu_bar_icon(!tray_on, window, cx) })))
            .into_any_element();
        div().flex().flex_col().gap(px(24.0))
            .child(title("Window"))
            .child(section("Privacy", privacy))
            .child(section("Appearance", appearance))
            .child(section("Taskbar and tray", vec![tray]))
    }

    /// The overlay's card at half size over a desktop, following width, text size and opacity live.
    fn fluent_preview_card(&self) -> AnyElement {
        let s = &self.store.value;
        let opacity = appearance::clamp(s.background_opacity);
        let ((hint, hint_line), (text, text_line)) = preview_text(s.text_size);
        let block = |w: f32, h: f32, color: u32, radius: f32| div().w(px(w)).h(px(h)).flex_none().rounded(px(radius)).bg(rgb(color));
        let toolbar = div().flex().items_center().gap(px(6.0)).h(px(20.0)).px(px(7.0)).border_t_1().border_color(theme::divider())
            .child(crate::ui::viewfinder(10.0))
            .child(div().w(px(34.0)).h(px(10.0)).rounded(px(3.0)).border_1().border_dashed().border_color(rgb(0x3a3d42)))
            .child(div().flex_1())
            .child(block(6.0, 6.0, 0x9c9890, 1.0)).child(block(6.0, 6.0, 0x7aaeff, 1.0)).child(block(8.0, 6.0, 0x9c9890, 1.0))
            .child(div().flex_1())
            .child(block(24.0, 6.0, 0x34373c, 2.0));
        let mini = div().w(px(s.card_width.card() * PREVIEW_SCALE)).flex().flex_col().rounded(px(6.0)).border_1().border_color(theme::keycap_border())
            .bg(appearance::fill(0x0f1012, opacity)).overflow_hidden()
            .child(div().flex().flex_col().gap(px(3.0)).px(px(9.0)).py(px(7.0))
                .text_size(px(text)).line_height(px(text_line))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("“Use a token bucket per API key.”"))
                .child(div().text_color(theme::body()).child("Keep the buckets in Redis so every node shares one count.")))
            .child(div().px(px(9.0)).py(px(6.0)).border_t_1().border_color(theme::divider()).bg(appearance::fill(0x17191c, opacity))
                .text_size(px(hint)).line_height(px(hint_line)).text_color(theme::placeholder()).child("Ask about your screen or conversation"))
            .child(toolbar);
        let behind = div().absolute().left(px(300.0)).top(px(96.0)).w(px(280.0)).h(px(140.0)).flex().flex_col().rounded(px(4.0))
            .bg(theme::fluent_expander()).border_1().border_color(theme::hairline())
            .child(div().h(px(16.0)).flex_none().border_b_1().border_color(theme::divider()))
            .child(div().flex_1().flex().gap(px(4.0)).p(px(6.0))
                .child(div().flex_1().rounded(px(3.0)).bg(theme::raised()))
                .child(div().flex_1().rounded(px(3.0)).bg(theme::raised())));
        let [top, middle, bottom] = DESKTOP;
        let desktop = div().relative().h(px(176.0)).rounded(px(4.0)).border_1().border_color(theme::divider()).overflow_hidden()
            .bg(linear_gradient(160.0, linear_color_stop(rgb(middle), 0.45), linear_color_stop(rgb(bottom), 1.0)))
            .child(div().absolute().inset_0().bg(linear_gradient(160.0, linear_color_stop(rgb(top), 0.0), linear_color_stop(rgba(middle << 8), 0.45))))
            .child(behind)
            .child(div().absolute().top(px(8.0)).left_0().right_0().flex().justify_center().child(mini));
        div().flex().flex_col().gap(px(10.0)).p(px(16.0)).rounded(px(6.0)).bg(theme::field()).border_1().border_color(theme::divider())
            .child(desktop)
            .child(div().flex().items_baseline().gap(px(8.0))
                .child(div().text_size(px(13.0)).line_height(px(16.0)).text_color(theme::text()).child("Preview"))
                .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(preview_caption(s.card_width, s.text_size, opacity))))
            .into_any_element()
    }

    /// "80%" and the slider: a 4 px track filled to the value and a ringed thumb. Pressing jumps to
    /// the pointer and dragging follows it; the overlay shows it at once and the release saves.
    fn fluent_opacity(&self, cx: &mut Context<Self>) -> Div {
        let percent = appearance::clamp(self.store.value.background_opacity);
        let fraction = appearance::slider_fraction(percent);
        let track: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
        let value_at = {
            let track = track.clone();
            move |x: Pixels| track.get().map(|bounds| appearance::opacity_at(f32::from(x - bounds.left()) / f32::from(bounds.size.width).max(1.0)))
        };
        let pressed = value_at.clone();
        let slider = div().id("fluent-opacity").relative().w(px(SLIDER)).h(px(20.0)).flex_none().cursor_pointer()
            .child(gpui::canvas(move |bounds, _, _| track.set(Some(bounds)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .child(div().absolute().left_0().top(px(8.0)).w_full().h(px(4.0)).rounded(px(2.0)).bg(theme::keycap_border()))
            .child(div().absolute().left_0().top(px(8.0)).w(px(SLIDER * fraction)).h(px(4.0)).rounded(px(2.0)).bg(theme::accent()))
            .child(div().absolute().top_0().left(px(SLIDER * fraction - 10.0)).size(px(20.0)).flex().items_center().justify_center().rounded(px(10.0))
                .bg(theme::raised()).border_1().border_color(theme::keycap_border())
                .child(div().size(px(12.0)).rounded(px(6.0)).bg(theme::accent())))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, event: &MouseDownEvent, _, cx| {
                if let Some(value) = pressed(event.position.x) { this.opacity_drag = true; this.preview_opacity(value, cx); }
            }))
            .on_drag(OpacityDrag, |_, _, _, cx| cx.new(|_| gpui::Empty))
            .on_drag_move(listen(cx, move |this, event: &DragMoveEvent<OpacityDrag>, _, cx| {
                if let Some(value) = value_at(event.event.position.x) { this.preview_opacity(value, cx); }
            }))
            .on_mouse_up(MouseButton::Left, listen(cx, |this, _: &MouseUpEvent, _, cx| this.finish_opacity_drag(cx)))
            .on_mouse_up_out(MouseButton::Left, listen(cx, |this, _: &MouseUpEvent, _, cx| this.finish_opacity_drag(cx)));
        div().flex().flex_none().items_center().gap(px(12.0))
            .child(div().w(px(36.0)).flex().justify_end().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::body()).child(format!("{percent}%")))
            .child(slider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_caption_names_each_choice() {
        assert_eq!(preview_caption(CardWidth::Standard, TextSize::Standard, 80), "Default width · Default text · 80%");
        assert_eq!(preview_caption(CardWidth::Wide, TextSize::Large, 60), "Wide · Large text · 60%");
        // Half the overlay's card: Default 300 (the mockup drew 320 for the older 640 px card).
        assert_eq!(CardWidth::Standard.card() * PREVIEW_SCALE, 300.0);
    }
}
