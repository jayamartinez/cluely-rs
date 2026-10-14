//! Settings shown as icons in the overlay's toolbar: Screen on send and Hidden from screen sharing.
//! Clicking one flips it; hovering shows a plain tooltip under the card with its state, as in the
//! Paper artboard "Overlay · Eye: hover shows the state, click flips it" (no popover). The mode
//! chip's "Switch mode" tooltip (Paper "Overlay · Idle") hangs the same way, from `tooltip_under_card`.

use gpui::{Bounds, Context, Div, InteractiveElement, IntoElement, MouseButton, ParentElement, Stateful, Styled, deferred, div, point, prelude::*, px, relative, size};

use crate::overlay::Overlay;
use crate::settings::Settings;
use crate::{theme, ui};

/// A tooltip's pointer: it starts this far under the card's bottom edge, and the box sits right
/// under it (macOS leaves a little more air, as its artboards do).
#[cfg(not(target_os = "macos"))]
const POINTER_GAP: f32 = 1.0;
#[cfg(target_os = "macos")]
const POINTER_GAP: f32 = 4.0;
const POINTER: (f32, f32) = (12.0, 6.0);
/// The area reserved in the window region for a tooltip: wide enough for the longest one, tall
/// enough for one with keycaps.
const TOOLTIP_WIDTH: f32 = 250.0;
const TOOLTIP_HEIGHT: f32 = 36.0;
/// The chip's tooltip starts a little right of the chip, its pointer on the chip's centre.
const START_INSET: f32 = 5.0;

/// The settings the bar shows as icons. Clicking one flips it; hovering shows its state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Toggle { ScreenOnSend, HideFromCapture }

/// What the tooltip under the card is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tip { Toggle(Toggle), ModeChip, Settings }

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

    fn id(self) -> &'static str { match self { Self::ScreenOnSend => "screen-on-send", Self::HideFromCapture => "hide-from-capture" } }

    /// The tooltip: the current state in a few words.
    pub(crate) fn state(self, on: bool) -> &'static str {
        match (self, on) {
            (Self::ScreenOnSend, true) => "Screenshot sent with every message",
            (Self::ScreenOnSend, false) => "No screenshot with messages",
            (Self::HideFromCapture, true) => "Hidden from screen sharing",
            (Self::HideFromCapture, false) => "Visible to screen sharing",
        }
    }
}

/// How a tooltip lines up with its control.
#[derive(Clone, Copy)]
pub(crate) enum Align {
    /// Centred under it (the toggles).
    Centre,
    /// Starting just right of its left edge, the pointer on its centre (the mode chip, near the card's left edge).
    Start,
}

impl Overlay {
    /// A setting shown as a 32 px icon button. Hidden from screen sharing is the accent eye-off on a
    /// bubble fill while on, a plain eye after a click; Screen on send is a plain image icon, crossed
    /// out while off. Clicking flips it; hovering shows its state under the card.
    pub(crate) fn toggle_button(&self, toggle: Toggle, below_card: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let on = toggle.is_on(&self.store.value);
        let hovered = self.tip == Some(Tip::Toggle(toggle));
        let accent = on && toggle == Toggle::HideFromCapture;
        let color = if accent { theme::accent_soft() } else if on || hovered { theme::body() } else { theme::placeholder() };
        let button = div().id(toggle.id()).relative().flex().flex_none().items_center().justify_center().size(px(BUTTON)).rounded(px(theme::radius::ICON_BUTTON))
            .border_1().border_color(gpui::transparent_black()).cursor_pointer()
            .when(accent, |button| button.bg(if hovered { theme::selected_row() } else { theme::bubble() }).border_color(theme::bubble_border()))
            .when(!accent && hovered, |button| button.bg(theme::pressed()))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.update_settings(|s| toggle.flip(s), window, cx);
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.hover_tip(Tip::Toggle(toggle), *hovered, cx)))
            .child(ui::icon(toggle.icon(on), 18.0, color));
        if !hovered { return button; }
        self.tooltip_under_card(button, BUTTON, below_card, Align::Centre, tooltip(toggle.state(on), None))
    }

    /// Show or clear the tooltip for `tip` as the pointer enters or leaves its control.
    pub(crate) fn hover_tip(&mut self, tip: Tip, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered { Some(tip) } else if self.tip == Some(tip) { None } else { self.tip };
        if next != self.tip { self.tip = next; cx.notify(); }
    }

    /// Hang `tip` under the card from `control` (`height` tall, its bottom `below_card` px above the
    /// card's bottom edge). It is painted deferred, over whatever is below, and its area is reserved
    /// in the window region so it shows whole in its first frame; it isn't a hit area.
    pub(crate) fn tooltip_under_card(&self, control: Stateful<Div>, height: f32, below_card: f32, align: Align, tip: Div) -> Stateful<Div> {
        let top = height + below_card + POINTER_GAP;
        let hits = self.hits.clone();
        let reserve = gpui::canvas(move |bounds: Bounds<gpui::Pixels>, _, _| {
            let left = match align { Align::Centre => bounds.center().x - px(TOOLTIP_WIDTH / 2.0), Align::Start => bounds.left() };
            hits.reserve(Bounds::new(point(left, bounds.top() + px(top)), size(px(TOOLTIP_WIDTH), px(POINTER.1 + TOOLTIP_HEIGHT))));
        }, |_, _, _, _| {}).absolute().top_0().left_0().size_full();
        let pointer = pointer();
        let hung = match align {
            Align::Centre => div().absolute().top(px(top)).left(px((height - TOOLTIP_WIDTH) / 2.0)).w(px(TOOLTIP_WIDTH)).flex().flex_col().items_center()
                .child(pointer).child(tip),
            // The box can be wider than the chip: it hangs from the chip's left edge, the pointer from its centre.
            Align::Start => div().absolute().top(px(top)).left_0().w_full().h(px(POINTER.1))
                .child(div().absolute().top_0().left(relative(0.5)).ml(px(-POINTER.0 / 2.0)).child(pointer))
                .child(div().absolute().top(px(POINTER.1)).left(px(START_INSET)).child(tip)),
        };
        control.child(reserve).child(deferred(hung))
    }
}

/// A tooltip's pointer: in the border's colour, with (on macOS) the tooltip's fill inside it, so it
/// reads as part of the box.
fn pointer() -> Div {
    let outer = gpui::svg().path("icons/tooltip-pointer-up.svg").w(px(POINTER.0)).h(px(POINTER.1)).flex_none().text_color(theme::keycap_border());
    let inner = gpui::svg().path("icons/tooltip-pointer-up.svg").w(px(POINTER.0 - 3.0)).h(px(POINTER.1 - 1.5)).text_color(theme::pressed());
    div().relative().w(px(POINTER.0)).h(px(POINTER.1)).flex_none().child(outer)
        .when(cfg!(target_os = "macos"), |pointer| pointer.child(div().absolute().left(px(1.5)).top(px(1.5)).child(inner)))
}

/// Toolbar icon buttons are this square.
pub(crate) const BUTTON: f32 = 32.0;

/// A plain tooltip: one short label, with the shortcut's keycaps when it has one.
pub(crate) fn tooltip(label: &'static str, keys: Option<String>) -> Div {
    div().flex().flex_none().items_center().gap(px(10.0)).py(px(if keys.is_some() { 5.0 } else { 6.0 })).pl(px(10.0)).pr(px(if keys.is_some() { 6.0 } else { 10.0 }))
        .rounded(px(theme::radius::TOOLTIP)).bg(theme::pressed()).border_1().border_color(theme::keycap_border())
        .child(div().flex_none().whitespace_nowrap().text_size(px(12.0)).line_height(px(16.0)).font_weight(theme::MEDIUM).text_color(theme::text()).child(label))
        .children(keys.map(|keys| ui::keycaps(&keys, 20.0, 11.0, theme::body(), theme::raised()).flex_none()))
}
