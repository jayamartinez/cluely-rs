//! Settings shown as icons in the overlay: Screen on send and Hidden from screen sharing. Clicking
//! one flips it; hovering shows a popover with its state, from the Paper "Toggle state popovers"
//! artboard. They sit in the card's toolbar; the popover hangs below the card.

use gpui::{Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, deferred, div, prelude::*, px, rgb, rgba};

use crate::overlay::Overlay;
use crate::settings::Settings;
use crate::{theme, ui};

/// Gap between an icon and its popover's pointer.
const POPOVER_GAP: f32 = 6.0;
const POPOVER_WIDTH: f32 = 290.0;
/// Upper bound on a toggle popover's height, reserved in the window region.
const POPOVER_HEIGHT: f32 = 118.0;

/// The settings the bar shows as icons. Clicking one flips it; hovering shows its state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Toggle { ScreenOnSend, HideFromCapture }

impl Toggle {
    fn is_on(self, s: &Settings) -> bool {
        match self { Self::ScreenOnSend => s.screen_on_send, Self::HideFromCapture => s.hide_from_capture }
    }

    fn flip(self, s: &mut Settings) {
        match self { Self::ScreenOnSend => s.screen_on_send = !s.screen_on_send, Self::HideFromCapture => s.hide_from_capture = !s.hide_from_capture }
    }

    fn icon(self, on: bool) -> &'static str {
        match (self, on) {
            (Self::ScreenOnSend, true) => "icons/image.svg",
            (Self::ScreenOnSend, false) => "icons/image-off.svg",
            (Self::HideFromCapture, true) => "icons/eye-off.svg",
            (Self::HideFromCapture, false) => "icons/eye.svg",
        }
    }

    fn title(self) -> &'static str {
        match self { Self::ScreenOnSend => "Screen on send", Self::HideFromCapture => "Hidden from screen sharing" }
    }

    /// What the current state means, in one line.
    fn state(self, on: bool) -> &'static str {
        match (self, on) {
            (Self::ScreenOnSend, true) => "A screenshot of your screen goes with every question.",
            (Self::ScreenOnSend, false) => "Questions go without a screenshot. Answers use the conversation and your text.",
            (Self::HideFromCapture, true) => "Only you can see CluelyRS. Screen shares, recordings and screenshots leave it out.",
            (Self::HideFromCapture, false) => "Anyone you share your screen with can see the overlay.",
        }
    }
}

impl Overlay {
    /// A setting shown as an icon, blue while on. Clicking flips it; hovering shows its state popover.
    /// `Boxed` (beside the composer on Windows): a framed button, the popover above with its right
    /// edge on the icon's. `Plain` (the macOS toolbar, at the bottom of a card at the top of the
    /// screen): a bare icon, the popover centred below it.
    pub(crate) fn toggle_button(&self, toggle: Toggle, cx: &mut Context<Self>) -> impl IntoElement {
        let on = toggle.is_on(&self.store.value);
        let (width, height) = (36.0, 32.0);
        let hovered = self.toggle_hover == Some(toggle);
        let color = match (on, hovered) { (true, false) => theme::accent_soft(), (true, true) => rgb(0xa6c8ff), (false, false) => rgb(0x8a867e), (false, true) => theme::text() };
        let mut button = div().id(toggle.title()).relative().flex().flex_none().items_center().justify_center().w(px(width)).h(px(height))
            .rounded(px(8.0)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.update_settings(|s| toggle.flip(s), window, cx);
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let next = if *hovered { Some(toggle) } else if this.toggle_hover == Some(toggle) { None } else { this.toggle_hover };
                if next != this.toggle_hover { this.toggle_hover = next; cx.notify(); }
            }))
            .when(hovered, |button| button.bg(if on { rgba(0x4c8dff1f) } else { rgba(0xffffff0f) }))
            .child(ui::icon(toggle.icon(on), 18.0, color));
        if hovered {
            // Reserved in the window region so it shows whole in its first frame, but not a hit area.
            let hits = self.hits.clone();
            button = button
                .child(gpui::canvas(move |bounds, _, _| {
                    let left = bounds.center().x - px(POPOVER_WIDTH / 2.0);
                    hits.reserve(gpui::Bounds::new(gpui::point(left, bounds.bottom()), gpui::size(px(POPOVER_WIDTH), px(POPOVER_HEIGHT + POPOVER_GAP))));
                }, |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                .child(deferred(div().absolute().child(state_popover(toggle, on)).top(px(height + POPOVER_GAP)).left(px((width - POPOVER_WIDTH) / 2.0))));
        }
        button
    }
}

/// A toggle's state popover, from the Paper "Toggle state popovers" artboard: title, an ON/OFF
/// badge, what the state means and how to change it. It shows state only; the icon is the control.
fn state_popover(toggle: Toggle, on: bool) -> Div {
    let badge = div().flex_none().px(px(7.0)).py(px(1.0)).rounded(px(5.0)).border_1()
        .text_size(px(10.0)).line_height(px(14.0)).font_weight(FontWeight::BOLD)
        .when(on, |badge| badge.bg(theme::bubble()).border_color(theme::bubble_border()).text_color(theme::accent_soft()).child("ON"))
        .when(!on, |badge| badge.bg(rgb(0x2c2f33)).border_color(theme::keycap_border()).text_color(theme::text()).child("OFF"));
    // The pointer, in the popover's colour, centred on the icon and overlapping the border by a pixel.
    let pointer = gpui::svg().path("icons/tooltip-pointer-up.svg").w(px(12.0)).h(px(6.0)).mt(px(1.0)).mb(px(-1.0)).flex_none().text_color(theme::raised());
    let card = div().w_full().flex().flex_col().gap(px(8.0)).px(px(14.0)).py(px(12.0)).rounded(px(12.0))
            .bg(theme::raised()).border_1().border_color(theme::keycap_border())
            .child(div().flex().items_center().gap(px(10.0))
                .child(ui::icon(toggle.icon(on), 16.0, if on { theme::accent_soft() } else { theme::body() }))
                .child(div().flex_1().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(toggle.title()))
                .child(badge))
            .child(div().text_size(px(12.0)).line_height(px(17.0)).text_color(theme::body()).child(toggle.state(on)))
            .child(div().pt(px(8.0)).border_t_1().border_color(theme::hairline()).text_size(px(11.0)).line_height(px(14.0)).text_color(theme::muted())
                .child(if on { "Click the icon to turn off" } else { "Click the icon to turn on" }));
    div().w(px(POPOVER_WIDTH)).flex().flex_col().items_center().child(pointer).child(card)
}
