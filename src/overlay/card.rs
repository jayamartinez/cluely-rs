//! The overlay card, the same on Windows and macOS, laid out like the Paper artboards "Overlay ·
//! Idle", "Overlay · Live" (Redesign v3 · Windows) and their macOS twins. The text box is the card's
//! top row and the toolbar its bottom row: the viewfinder (Settings) and the mode chip on the left,
//! the two toggles and the Live waveform with its timer in the middle, Sessions on the right. During
//! Live (and after it, until the next session) the conversation sits between them: what was heard,
//! the answers and the quick actions. The card's top edge stays put, so the toolbar moves down as the
//! conversation grows. Sessions replaces the whole card with its panel (`sessions_panel`). Settings
//! opens in its own window on both platforms.

use gpui::{Context, Focusable, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, div, prelude::*, px};

use super::{Overlay, WIDTH};
use crate::settings_view::Tab;
use crate::toggles::{self, Align, Tip, Toggle};
use crate::{theme, ui};

/// The text box row and the toolbar (44 px on macOS, as its artboards draw it).
pub(super) const INPUT_ROW_HEIGHT: f32 = 48.0;
pub(super) const TOOLBAR_HEIGHT: f32 = if MAC { 44.0 } else { 48.0 };
/// Where the macOS artboards (p-6-0) differ from the Windows ones in small ways.
pub(super) const MAC: bool = cfg!(target_os = "macos");
/// The toolbar's left (viewfinder, mode chip) and right (Sessions) groups, so the middle group is
/// centred on the card.
const TOOLBAR_SIDE: f32 = 190.0;
/// The conversation (what was heard, answers, quick actions) grows to this height, then the
/// answers scroll.
pub(super) const CONVERSATION_MAX_HEIGHT: f32 = 418.0;
/// The card's default width (Settings › Window › Card width); the Settings panel under it matches.
pub(crate) const CARD_WIDTH: f32 = WIDTH - 40.0;
/// How far a 32 px toolbar button's bottom is above the card's bottom edge (its border included).
const BELOW_BUTTON: f32 = (TOOLBAR_HEIGHT - toggles::BUTTON) / 2.0 + 1.0;

impl Overlay {
    pub(super) fn card(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if let Some(panel) = &self.sessions_panel { return self.sessions_card(panel, cx).into_any_element(); }
        let live = self.live_since.is_some();
        // After Live stops, the last chat stays until the next session starts (see `chat`).
        let conversation_shown = live || !self.turns.is_empty();
        // Each section paints its own translucent background, so none is stacked on another and the
        // whole card stays equally see-through. Inner corners are a pixel tighter than the border's.
        let mut card = div().id("card").relative().w(px(self.store.value.card_width.card())).flex().flex_col().rounded(px(theme::radius::CARD))
            .border_1().border_color(theme::keycap_border());
        // Files dropped anywhere on the card are attached to the question (`attach`).
        card = self.drop_target(card, cx);
        // An update notice (never during Live) is the card's top row.
        let notice = self.update_notice(cx);
        let notice_shown = notice.is_some();
        card = card.children(notice);
        // The conversation sits above the text box, so the question is typed under the answer it follows.
        if conversation_shown {
            let (heard, thread, actions) = self.live_parts(cx);
            // The conversation hugs its content up to a cap; past it the thread (which may shrink,
            // unlike the heard line and the actions) scrolls.
            let mut conversation = div().flex().flex_col().gap(px(12.0)).px(px(18.0)).py(px(14.0)).max_h(px(CONVERSATION_MAX_HEIGHT)).overflow_hidden()
                .bg(self.card_fill(theme::CARD)).child(heard.flex_none())
                .when(notice_shown, |conversation| conversation.border_t_1().border_color(theme::divider()))
                .when(!notice_shown, |conversation| conversation.rounded_t(px(theme::radius::CARD - 1.0)));
            if !self.turns.is_empty() { conversation = conversation.child(thread.flex_initial().min_h_0()); }
            card = card.child(conversation.child(actions.flex_none()));
        }
        card.child(self.input_row(notice_shown || conversation_shown, cx)).child(self.toolbar(live, cx)).into_any_element()
    }

    /// The text box, the paperclip and the return button, which turns blue once there is something
    /// to send. Attached files' chips sit above the text in the same field, and while files are
    /// dragged over the card the text row is the drop target (`attach`).
    fn input_row(&self, below: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = (!self.composer.read(cx).text().is_empty() || self.attached.has_ready()) && !self.attached.reading();
        let send = div().id("send").size(px(28.0)).flex_none().rounded(px(theme::radius::BUTTON)).flex().items_center().justify_center().cursor_pointer()
            .border_1().border_color(if ready { theme::accent() } else if MAC { theme::pressed() } else { theme::keycap_border() })
            .bg(if ready { theme::accent() } else { theme::pressed() })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { let input = this.composer.clone(); this.send_composer(input, window, cx); }))
            .child(ui::icon("icons/return.svg", 14.0, if ready { theme::accent_ink() } else { theme::body() }));
        let chips = self.chips(cx);
        let dropping = self.attached.drop_over.as_deref().map(|dragged| self.drop_zone(dragged));
        let field = div().relative().flex().flex_none().pl(px(16.0)).pr(px(10.0)).bg(self.card_fill(theme::CARD_FIELD))
            // The card's top row, unless the update notice or the conversation is above it.
            .when(!below, |field| field.rounded_t(px(theme::radius::CARD - 1.0)))
            .when(below, |field| field.border_t_1().border_color(theme::divider()));
        let field = if chips.is_none() && dropping.is_none() {
            field.items_center().h(px(INPUT_ROW_HEIGHT)).child(self.text_row(send, cx))
        } else {
            // The text box keeps its place while it is the drop target, so what was typed stays.
            let text = self.text_row(send, cx).when(dropping.is_some(), |row| row.h(px(0.0)).overflow_hidden());
            field.flex_col().gap(px(10.0)).py(px(10.0)).children(chips).children(dropping).child(text)
        };
        field.child(self.hits.mark())
    }

    /// The Type shortcut toggles: it puts the caret in the text box, or, pressed while the box has
    /// the keyboard, hands the keyboard back to the previous app as Esc does, keeping what was typed.
    pub(super) fn type_shortcut(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        match TypeShortcut::when(self.composer.read(cx).has_keyboard(window)) {
            TypeShortcut::Enter => self.focus_composer(window, cx),
            TypeShortcut::Leave => self.return_to_previous_app(),
        }
    }

    /// A click in the text box gives it the keyboard. The overlay doesn't activate on a click (a
    /// non-activating panel on macOS), so take it as the Type shortcut does; Esc, the shortcut again
    /// or sending hands it back.
    pub(super) fn focus_text_box(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.take_keyboard(cx);
        window.focus(&self.composer.focus_handle(cx));
    }

    fn toolbar(&self, live: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let icon_button = |id: &'static str| div().id(id).flex().flex_none().items_center().justify_center().h(px(toggles::BUTTON))
            .rounded(px(theme::radius::ICON_BUTTON)).cursor_pointer();
        // The viewfinder is the brand mark and opens Settings; hovering says so, since the mark alone doesn't.
        // macOS fills the idle tile a step lighter, as the idle artboard does.
        let logo = icon_button("settings").relative().w(px(toggles::BUTTON)).bg(if MAC && !live { theme::pressed() } else { theme::raised() })
            .border_1().border_color(theme::hairline())
            .hover(|button| button.bg(theme::pressed()))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { cx.stop_propagation(); this.open_settings(Tab::default(), window, cx); }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.hover_tip(Tip::Settings, *hovered, cx)))
            .child(ui::viewfinder(18.0));
        let logo = if self.tip == Some(Tip::Settings) {
            self.tooltip_under_card(logo, toggles::BUTTON, BELOW_BUTTON, Align::Start, toggles::tooltip("Open settings", None))
        } else { logo };
        // The waveform starts and stops Live; while live it moves with what is heard (`level_bars`)
        // and has the elapsed time beside it.
        let bars: gpui::AnyElement = if live && !self.levels.is_empty() { self.level_bars().into_any_element() }
            else { ui::icon("icons/tab-listening.svg", 18.0, if live { theme::accent_soft() } else { theme::body() }).into_any_element() };
        let wave = icon_button("live").gap(px(6.0)).px(px(if live { 9.0 } else { 7.0 }))
            .when(live, |wave| wave.bg(theme::selected_row()))
            .when(!live, |wave| wave.hover(|wave| wave.bg(theme::pressed())))
            // Read when clicked: the state may have changed since this frame was drawn.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.set_live(this.live_since.is_none(), window, cx)))
            .child(bars)
            .when(live, |wave| wave.child(div().font_family(theme::MONO).text_size(px(12.0)).line_height(px(16.0)).text_color(theme::accent_soft()).child(self.elapsed())));
        let middle = div().flex_1().flex().items_center().justify_center().gap(px(4.0))
            .child(self.toggle_button(Toggle::ScreenOnSend, BELOW_BUTTON, cx))
            .child(self.toggle_button(Toggle::HideFromCapture, BELOW_BUTTON, cx))
            .child(div().w(px(1.0)).h(px(18.0)).flex_none().bg(theme::hairline()))
            .child(wave);
        // Sessions opens its panel in place of the card.
        let sessions = div().id("sessions").flex().flex_none().items_center().gap(px(8.0)).h(px(toggles::BUTTON)).pl(px(6.0)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { cx.stop_propagation(); this.open_sessions_panel(window, cx); }))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::body()).child("Sessions"))
            .child(div().size(px(if MAC { 20.0 } else { 22.0 })).flex().flex_none().items_center().justify_center()
                .rounded(px(if MAC { theme::radius::CHIP } else { theme::radius::ICON_BUTTON }))
                .bg(theme::raised()).border_1().border_color(theme::hairline())
                .child(ui::icon("icons/chevron-down.svg", 12.0, theme::body())));
        // The overlay moves with its shortcuts only (Ctrl Alt arrows, ⌃⌥ arrows on macOS), not by dragging.
        div().flex().flex_none().items_center().h(px(TOOLBAR_HEIGHT)).px(px(8.0)).border_t_1().border_color(theme::divider())
            .bg(self.card_fill(theme::CARD)).rounded_b(px(theme::radius::CARD - 1.0))
            .child(div().w(px(TOOLBAR_SIDE)).flex_none().flex().items_center().gap(px(6.0)).child(logo).child(self.mode_chip(cx)))
            .child(middle)
            .child(div().w(px(TOOLBAR_SIDE)).flex_none().flex().items_center().justify_end().child(sessions))
            .child(self.hits.mark())
    }

    /// The mode chip's "Switch mode" tooltip with its shortcut, while the pointer is on the closed chip.
    pub(super) fn chip_tooltip(&self, chip: gpui::Stateful<gpui::Div>, chip_height: f32) -> gpui::Stateful<gpui::Div> {
        if self.tip != Some(Tip::ModeChip) || self.switcher.is_some() { return chip; }
        let below = (TOOLBAR_HEIGHT - chip_height) / 2.0 + 1.0;
        let keys = self.hotkeys.label(crate::hotkeys::Action::SwitchMode);
        self.tooltip_under_card(chip, chip_height, below, Align::Start, toggles::tooltip("Switch mode", Some(keys)))
    }
}

impl Overlay {
    /// One of the card's own fills at the chosen background opacity. Each section paints its own,
    /// so the whole card is equally see-through.
    pub(super) fn card_fill(&self, rgb: u32) -> gpui::Rgba { crate::appearance::fill(rgb, self.store.value.background_opacity) }

    /// A dim text colour on the card, brightened as the card gets more see-through.
    pub(crate) fn dim(&self, color: gpui::Rgba) -> gpui::Rgba { crate::appearance::legible(color, self.store.value.background_opacity) }

    /// Give the card's text boxes their placeholder colour for the current opacity.
    pub(crate) fn dim_placeholders(&mut self, cx: &mut Context<Self>) {
        let placeholder = self.dim(theme::placeholder());
        self.composer.update(cx, |input, cx| input.set_placeholder_color(placeholder, cx));
        if let Some(panel) = &self.sessions_panel { panel.search.update(cx, |input, cx| input.set_placeholder_color(placeholder, cx)); }
    }

    /// The waveform icon's five bars (same size and spacing as `tab-listening.svg`), driven by the
    /// last levels: the newest in the middle, older ones outwards, scaled by the icon's diamond so
    /// silence reads as a small, flat version of the icon.
    fn level_bars(&self) -> impl IntoElement {
        const SHAPE: [f32; 5] = [0.45, 0.75, 1.0, 0.75, 0.45];
        let newest = self.levels.len() - 1;
        let at = |age: usize| self.levels.get(newest.saturating_sub(age)).copied().unwrap_or(0.0);
        // Whole-pixel bars and gaps, so every bar renders at the same width.
        div().size(px(18.0)).flex().items_center().justify_center().gap(px(1.0))
            .children(SHAPE.iter().enumerate().map(|(index, scale)| {
                let level = at(index.abs_diff(2));
                div().w(px(2.0)).h(px((3.0 + 13.0 * scale * level).round())).rounded(px(1.0)).bg(theme::accent_soft())
            }))
    }
}

/// What a press of the Type shortcut does, from whether the text box has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypeShortcut {
    /// Take the keyboard and put the caret in the text box.
    Enter,
    /// Hand the keyboard back to the app the user was in.
    Leave,
}

impl TypeShortcut {
    fn when(text_box_has_keyboard: bool) -> Self { if text_box_has_keyboard { Self::Leave } else { Self::Enter } }
}

#[cfg(test)]
mod tests {
    use super::TypeShortcut;

    #[test]
    fn the_type_shortcut_enters_the_text_box_or_leaves_it_when_typing_there() {
        assert_eq!(TypeShortcut::when(false), TypeShortcut::Enter);
        assert_eq!(TypeShortcut::when(true), TypeShortcut::Leave);
    }
}
