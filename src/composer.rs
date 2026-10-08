//! The bar under the Live panel's text box, laid out like the Paper "Live · Composer v2"
//! artboards: model switcher and Smart toggle on the left; the Assist and Type shortcuts and
//! the send button on the right. Screen on send and Hidden from screen sharing sit at the right of
//! the text box, as icons with state popovers.

use gpui::{Context, Div, FontWeight, IntoElement, MouseButton, ParentElement, Styled, deferred, div, prelude::*, px, rgb};

use crate::hotkeys::Action;
use crate::overlay::Overlay;
use crate::settings::{Provider, Settings};
use crate::settings_view::{Picker, Tab};
use crate::theme;
use crate::ui;

// The switcher's list, from the Paper "Live · Composer v2 · model open" artboard. It is more
// compact than the Settings dropdowns on purpose and doesn't share their sizes.
/// The list opens upward; its bottom edge sits this far above the bottom of the bar's row.
const MENU_OFFSET: f32 = 34.0;
const MENU_WIDTH: f32 = 150.0;
const MENU_PADDING: f32 = 3.0;
/// One row: 4 px padding above and below 16 px text.
const ROW: f32 = 24.0;
/// Models shown before the list scrolls.
const VISIBLE_MODELS: usize = 8;
/// Upper bound on the tooltip's height, for the same reason.
const TOOLTIP_HEIGHT: f32 = 84.0;
/// Gap between the Smart pill and the tooltip's pointer.
const TOOLTIP_GAP: f32 = 6.0;
const POPOVER_WIDTH: f32 = 290.0;
/// Upper bound on a toggle popover's height, reserved in the window region.
const POPOVER_HEIGHT: f32 = 118.0;
/// The popover's right edge sits this far right of its icon's, so it ends near the panel's edge.
const POPOVER_INSET: f32 = 10.0;

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
    pub(crate) fn composer_bar(&self, cx: &mut Context<Self>) -> Div {
        let left = div().relative().flex().items_center().gap(px(6.0))
            .child(self.model_switcher(cx))
            .child(self.smart_pill(cx));
        let assist = div().flex().items_center().gap(px(6.0))
            .child(ui::keycap(self.hotkeys.label(Action::Assist)))
            .child(hint("Assist"));
        let focus = self.hotkeys.label(Action::Focus);
        let type_key = if focus == "Ctrl Shift Space" { ui::chord_keycap(&focus).into_any_element() } else { ui::keycap(focus).into_any_element() };
        let typing = div().flex().items_center().gap(px(6.0)).child(type_key).child(hint("Type"));
        let send = div().id("send").size(px(30.0)).flex_none().rounded_full().bg(theme::accent()).flex().items_center().justify_center().cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { let input = this.composer.clone(); this.send_composer(input, window, cx); }))
            .child(ui::icon("icons/send.svg", 14.0, theme::accent_ink()));
        div().flex().items_center().justify_between()
            .child(left)
            .child(div().flex().items_center().gap(px(12.0)).child(assist).child(typing).child(send))
    }

    /// The current model for the selected provider; opens a list of that provider's models
    /// above the bar, with "Settings…" at the end.
    fn model_switcher(&self, cx: &mut Context<Self>) -> Div {
        let choice = self.model_choice();
        let open = self.open_picker == Some(Picker::Composer);
        let mut face = ui::pill("model-switcher").when(open, |pill| pill.border_color(theme::accent()))
            .hover(|pill| pill.bg(theme::raised()))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                // The Codex model list comes with the subscription status; fetch it the first time.
                if this.store.value.provider == Provider::Codex && this.codex_status.is_none() { this.refresh_subscriptions(window, cx); }
                this.toggle_picker(Picker::Composer, cx);
            }))
            .child(div().text_size(px(12.0)).text_color(theme::body()).child(choice.value.clone()))
            .child(ui::icon("icons/chevron-down.svg", 10.0, theme::muted()));
        let mut wrapper = div().relative();
        if open {
            // Record where the face is (so the list's outside-click ignores it) and reserve the
            // list's area in the window region now, so it shows whole in its first frame.
            let face_bounds = self.picker_face.clone();
            let hits = self.hits.clone();
            // Visible model rows, the separator, the Settings row, padding and border.
            let rows = choice.options.len().min(VISIBLE_MODELS) + 1;
            let height = px(rows as f32 * ROW + 1.0 + MENU_PADDING * 2.0 + 2.0);
            face = face.child(gpui::canvas(move |bounds, _, _| {
                face_bounds.set(Some(bounds));
                let bottom = bounds.bottom() - px(MENU_OFFSET);
                hits.reserve(gpui::Bounds::new(gpui::point(bounds.left(), bottom - height), gpui::size(px(MENU_WIDTH), height)));
            }, |_, _, _, _| {}).absolute().top_0().left_0().size_full());
            let face_bounds = self.picker_face.clone();
            let mut list = div().relative().w_full().flex().flex_col().p(px(MENU_PADDING)).rounded(px(9.0))
                .bg(theme::raised()).border_1().border_color(theme::keycap_border())
                .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    if face_bounds.get().is_some_and(|face| face.contains(&event.position)) { return; }
                    this.close_picker(cx);
                }));
            // The models scroll when there are many; "Settings…" stays visible below them.
            let mut models = div().id("composer-models").flex().flex_col().max_h(px(VISIBLE_MODELS as f32 * ROW)).overflow_y_scroll();
            let set = choice.set;
            for (index, (id, label)) in choice.options.into_iter().enumerate() {
                let chosen = id.clone();
                models = models.child(menu_row(("composer-item", index), label, id == choice.selected)
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_picker(cx);
                        let value = chosen.clone();
                        this.update_settings(|s| set(s, value), window, cx);
                    })));
            }
            list = list
                .child(models)
                .child(div().h(px(1.0)).flex_none().bg(theme::hairline()))
                .child(menu_row("composer-settings", "Settings…", false).text_color(theme::muted())
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_picker(cx);
                        this.open_settings(Tab::Model, window, cx);
                    })))
                .child(self.hits.mark());
            wrapper = wrapper.child(deferred(div().absolute().bottom(px(MENU_OFFSET)).left_0().w(px(MENU_WIDTH)).child(list)));
        }
        wrapper.child(face)
    }

    /// Screen on send and Hidden from screen sharing, at the right of the text box.
    pub(crate) fn composer_toggles(&self, cx: &mut Context<Self>) -> Div {
        div().flex().flex_none().items_center().gap(px(4.0))
            .child(self.toggle_button(Toggle::ScreenOnSend, cx))
            .child(self.toggle_button(Toggle::HideFromCapture, cx))
    }

    /// A setting shown as an icon: blue while on, plain while off. Clicking flips
    /// it; hovering shows its state popover above.
    fn toggle_button(&self, toggle: Toggle, cx: &mut Context<Self>) -> impl IntoElement {
        let on = toggle.is_on(&self.store.value);
        let mut button = div().id(toggle.title()).relative().flex().flex_none().items_center().justify_center().w(px(32.0)).h(px(30.0))
            .rounded(px(8.0)).border_1().cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.update_settings(|s| toggle.flip(s), window, cx);
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let next = if *hovered { Some(toggle) } else if this.toggle_hover == Some(toggle) { None } else { this.toggle_hover };
                if next != this.toggle_hover { this.toggle_hover = next; cx.notify(); }
            }))
            .child(ui::icon(toggle.icon(on), 17.0, if on { theme::accent_soft() } else { theme::body() }));
        button = if on { button.bg(theme::bubble()).border_color(theme::bubble_border()) } else { button.bg(theme::raised()).border_color(rgb(0x3a3e44)) };
        if self.toggle_hover == Some(toggle) {
            // Like the Smart tooltip: reserved in the window region, but not a hit area.
            let hits = self.hits.clone();
            button = button
                .child(gpui::canvas(move |bounds, _, _| {
                    let height = px(POPOVER_HEIGHT + TOOLTIP_GAP);
                    let left = bounds.right() + px(POPOVER_INSET) - px(POPOVER_WIDTH);
                    hits.reserve(gpui::Bounds::new(gpui::point(left, bounds.top() - height), gpui::size(px(POPOVER_WIDTH), height)));
                }, |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                .child(deferred(div().absolute().bottom(px(30.0 + TOOLTIP_GAP)).right(px(-POPOVER_INSET)).child(state_popover(toggle, on))));
        }
        button
    }

    /// Smart mode: higher reasoning for harder questions. Hovering explains it.
    fn smart_pill(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.store.value.smart_mode;
        let mut pill = ui::pill("smart").px(px(10.0))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.update_settings(|s| s.smart_mode = !on, window, cx);
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.smart_hover != *hovered { this.smart_hover = *hovered; cx.notify(); }
            }));
        pill = if on {
            pill.bg(theme::accent()).border_color(theme::accent())
                .child(ui::icon("icons/lightbulb.svg", 13.0, theme::accent_ink()))
                .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::accent_ink()).child("Smart"))
        } else {
            pill.hover(|pill| pill.bg(theme::raised()).border_color(rgb(0x4a4d52)))
                .child(ui::icon("icons/lightbulb.svg", 13.0, theme::muted()))
                .child(div().text_size(px(12.0)).text_color(theme::muted()).child("Smart"))
        };
        if self.smart_hover {
            // Shown above the pill at once, with no delay or fade. Its area is reserved so it is
            // visible in the window region, but it is not a hit area: clicks pass through it.
            let hits = self.hits.clone();
            pill = pill
                .child(gpui::canvas(move |bounds, _, _| {
                    // Down to the pill's top edge, so the pointer below the bubble isn't cut off.
                    let bottom = bounds.top();
                    let height = px(TOOLTIP_HEIGHT + TOOLTIP_GAP);
                    hits.reserve(gpui::Bounds::new(gpui::point(bounds.center().x - px(110.0), bottom - height), gpui::size(px(220.0), height)));
                }, |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                .child(deferred(div().absolute().bottom(px(28.0 + TOOLTIP_GAP)).left(px(-70.0))
                    .child(ui::tooltip("Smart mode", "Uses higher reasoning for harder questions. Answers may be slower."))));
        }
        pill
    }
}

/// A toggle's state popover, from the Paper "Toggle state popovers" artboard: title, an ON/OFF
/// badge, what the state means and how to change it. It shows state only; the icon is the control.
fn state_popover(toggle: Toggle, on: bool) -> Div {
    let badge = div().flex_none().px(px(7.0)).py(px(1.0)).rounded(px(5.0)).border_1()
        .text_size(px(10.0)).line_height(px(14.0)).font_weight(FontWeight::BOLD)
        .when(on, |badge| badge.bg(theme::bubble()).border_color(theme::bubble_border()).text_color(theme::accent_soft()).child("ON"))
        .when(!on, |badge| badge.bg(rgb(0x2c2f33)).border_color(theme::keycap_border()).text_color(theme::text()).child("OFF"));
    div().w(px(POPOVER_WIDTH)).flex().flex_col().items_end()
        .child(div().w_full().flex().flex_col().gap(px(8.0)).px(px(14.0)).py(px(12.0)).rounded(px(12.0))
            .bg(theme::raised()).border_1().border_color(theme::keycap_border())
            .child(div().flex().items_center().gap(px(10.0))
                .child(ui::icon(toggle.icon(on), 16.0, if on { theme::accent_soft() } else { theme::body() }))
                .child(div().flex_1().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(toggle.title()))
                .child(badge))
            .child(div().text_size(px(12.0)).line_height(px(17.0)).text_color(theme::body()).child(toggle.state(on)))
            .child(div().pt(px(8.0)).border_t_1().border_color(theme::hairline()).text_size(px(11.0)).line_height(px(14.0)).text_color(theme::muted())
                .child(if on { "Click the icon to turn off" } else { "Click the icon to turn on" })))
        // The pointer, in the popover's colour, centred under the icon.
        .child(div().pr(px(POPOVER_INSET + 10.0)).child(gpui::svg().path("icons/tooltip-pointer.svg").w(px(12.0)).h(px(6.0)).mt(px(-1.0)).flex_none().text_color(theme::raised())))
}

fn hint(text: &'static str) -> impl IntoElement {
    div().text_size(px(12.0)).text_color(theme::muted()).child(text)
}

/// A row in the switcher's list: 12 px text, the chosen model highlighted with a tick.
fn menu_row(id: impl Into<gpui::ElementId>, label: impl Into<gpui::SharedString>, selected: bool) -> gpui::Stateful<Div> {
    let row = div().id(id).flex().items_center().justify_between().gap(px(8.0)).px(px(8.0)).py(px(4.0)).rounded(px(6.0)).cursor_pointer()
        .text_size(px(12.0)).line_height(px(16.0)).hover(|row| row.bg(rgb(0x26292d)))
        .child(div().truncate().child(label.into()));
    if selected {
        row.bg(rgb(0x26292d)).text_color(theme::text())
            .child(div().flex_none().text_color(theme::accent()).font_weight(FontWeight::BOLD).child("✓"))
    } else {
        row.text_color(theme::body())
    }
}
