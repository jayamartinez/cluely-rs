//! Settings shown as icons in the overlay: Screen on send and Hidden from screen sharing. Clicking
//! one flips it; hovering shows a popover with its state, from the Paper "Toggle state popovers"
//! artboard. Windows shows them beside the composer's text box, macOS in its bar's toolbar.

use gpui::{Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, deferred, div, prelude::*, px, rgb, rgba};

use crate::overlay::Overlay;
use crate::settings::Settings;
use crate::{theme, ui};

/// Gap between an icon and its popover's pointer.
const POPOVER_GAP: f32 = 6.0;
const POPOVER_WIDTH: f32 = 290.0;
/// Upper bound on a toggle popover's height, reserved in the window region.
const POPOVER_HEIGHT: f32 = 118.0;
/// The popover's right edge sits this far right of its icon's, so it ends near the panel's edge.
const POPOVER_INSET: f32 = 10.0;

/// How a toggle is drawn; see `Overlay::toggle_button`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToggleStyle {
    /// Beside the composer's text box (Windows).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Boxed,
    /// In the toolbar at the bottom of the macOS card.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Plain,
}

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
    pub(crate) fn toggle_button(&self, toggle: Toggle, style: ToggleStyle, cx: &mut Context<Self>) -> impl IntoElement {
        let on = toggle.is_on(&self.store.value);
        let below = style == ToggleStyle::Plain;
        let (width, height) = if below { (36.0, 32.0) } else { (32.0, 30.0) };
        let mut button = div().id(toggle.title()).relative().flex().flex_none().items_center().justify_center().w(px(width)).h(px(height))
            .rounded(px(8.0)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.update_settings(|s| toggle.flip(s), window, cx);
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let next = if *hovered { Some(toggle) } else if this.toggle_hover == Some(toggle) { None } else { this.toggle_hover };
                if next != this.toggle_hover { this.toggle_hover = next; cx.notify(); }
            }));
        let hovered = self.toggle_hover == Some(toggle);
        button = match style {
            ToggleStyle::Boxed => {
                let button = button.border_1().child(ui::icon(toggle.icon(on), 17.0, if on { theme::accent_soft() } else { theme::body() }));
                if on { button.bg(theme::bubble()).border_color(theme::bubble_border()) } else { button.bg(theme::raised()).border_color(rgb(0x3a3e44)) }
            }
            ToggleStyle::Plain => {
                let color = match (on, hovered) { (true, false) => theme::accent_soft(), (true, true) => rgb(0xa6c8ff), (false, false) => rgb(0x8a867e), (false, true) => theme::text() };
                button.when(hovered, |button| button.bg(if on { rgba(0x4c8dff1f) } else { rgba(0xffffff0f) }))
                    .child(ui::icon(toggle.icon(on), 18.0, color))
            }
        };
        if hovered {
            // Like the Smart tooltip: reserved in the window region, but not a hit area.
            let hits = self.hits.clone();
            button = button
                .child(gpui::canvas(move |bounds, _, _| {
                    let height = px(POPOVER_HEIGHT + POPOVER_GAP);
                    let left = if below { bounds.center().x - px(POPOVER_WIDTH / 2.0) } else { bounds.right() + px(POPOVER_INSET) - px(POPOVER_WIDTH) };
                    let top = if below { bounds.bottom() } else { bounds.top() - height };
                    hits.reserve(gpui::Bounds::new(gpui::point(left, top), gpui::size(px(POPOVER_WIDTH), height)));
                }, |_, _, _, _| {}).absolute().top_0().left_0().size_full());
            let popover = div().absolute().child(state_popover(toggle, on, below));
            button = button.child(deferred(if below {
                popover.top(px(height + POPOVER_GAP)).left(px((width - POPOVER_WIDTH) / 2.0))
            } else {
                popover.bottom(px(height + POPOVER_GAP)).right(px(-POPOVER_INSET))
            }));
        }
        button
    }
}

/// A toggle's state popover, from the Paper "Toggle state popovers" artboard: title, an ON/OFF
/// badge, what the state means and how to change it. It shows state only; the icon is the control.
fn state_popover(toggle: Toggle, on: bool, below: bool) -> Div {
    let badge = div().flex_none().px(px(7.0)).py(px(1.0)).rounded(px(5.0)).border_1()
        .text_size(px(10.0)).line_height(px(14.0)).font_weight(FontWeight::BOLD)
        .when(on, |badge| badge.bg(theme::bubble()).border_color(theme::bubble_border()).text_color(theme::accent_soft()).child("ON"))
        .when(!on, |badge| badge.bg(rgb(0x2c2f33)).border_color(theme::keycap_border()).text_color(theme::text()).child("OFF"));
    let pointer = |path: &'static str, offset: f32| div().when(!below, |pointer| pointer.pr(px(POPOVER_INSET + 10.0)))
        .child(gpui::svg().path(path).w(px(12.0)).h(px(6.0)).mt(px(offset)).mb(px(-offset)).flex_none().text_color(theme::raised()));
    let card = div().w_full().flex().flex_col().gap(px(8.0)).px(px(14.0)).py(px(12.0)).rounded(px(12.0))
            .bg(theme::raised()).border_1().border_color(theme::keycap_border())
            .child(div().flex().items_center().gap(px(10.0))
                .child(ui::icon(toggle.icon(on), 16.0, if on { theme::accent_soft() } else { theme::body() }))
                .child(div().flex_1().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(toggle.title()))
                .child(badge))
            .child(div().text_size(px(12.0)).line_height(px(17.0)).text_color(theme::body()).child(toggle.state(on)))
            .child(div().pt(px(8.0)).border_t_1().border_color(theme::hairline()).text_size(px(11.0)).line_height(px(14.0)).text_color(theme::muted())
                .child(if on { "Click the icon to turn off" } else { "Click the icon to turn on" }));
    // The pointer, in the popover's colour, centred on the icon and overlapping the border by a pixel.
    let popover = div().w(px(POPOVER_WIDTH)).flex().flex_col().when(below, |popover| popover.items_center()).when(!below, |popover| popover.items_end());
    if below {
        popover.child(pointer("icons/tooltip-pointer-up.svg", 1.0)).child(card)
    } else {
        popover.child(card).child(pointer("icons/tooltip-pointer.svg", -1.0))
    }
}
