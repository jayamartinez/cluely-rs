//! The overlay card, the same on Windows and macOS, laid out like the Paper artboards "macOS · Overlay
//! v2 · Unified · Chat above composer" and "Windows · Overlay · Unified card (same as macOS)". During
//! Live its top holds the transcript strip, the answers and the quick actions; below them sit the text
//! box and a toolbar of plain icons (the mark, the two toggles, the Live waveform with its timer,
//! Sessions and Settings). The card's top edge stays put, so the text box moves down as the
//! conversation grows.
//!
//! Settings: macOS opens its own window. Where Settings is a panel under the card (Windows), the card
//! shows only its text box and toolbar while the panel is open; typing still works there, and sending
//! collapses the panel into the conversation (see `Overlay::collapse_progress`).

use gpui::{Context, Focusable, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, div, prelude::*, px, rgb, rgba};

use super::{Overlay, WIDTH};
use crate::platform;
use crate::settings_view::Tab;
use crate::toggles::Toggle;
use crate::{theme, ui};

/// The card behind the conversation and the toolbar, translucent so the desktop shows faintly through
/// it.
const CARD: u32 = 0x121315cc;
/// The text box row, lighter than the rest of the card.
const INPUT_ROW: u32 = 0x2e3034cc;
const EDGE: u32 = 0xffffff14;
/// The return button while there is nothing to send.
const RETURN_IDLE: u32 = 0x43464b;
/// Toolbar icons while off.
const ICON_OFF: u32 = 0x8a867e;
const HOVER: u32 = 0xffffff0f;
/// The conversation (transcript strip, answers, quick actions) grows to this height, then the
/// answers scroll.
const CONVERSATION_MAX_HEIGHT: f32 = 418.0;
/// The card's width; the Settings panel under it matches.
pub(crate) const CARD_WIDTH: f32 = WIDTH - 40.0;

impl Overlay {
    /// `collapse` is how far Settings has collapsed into the conversation (0 to 1), while it does.
    pub(super) fn card(&self, collapse: Option<f32>, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.live_since.is_some();
        // With Settings open the card is just the text box and toolbar; the conversation grows back
        // in as Settings collapses.
        let conversation_shown = live && (self.settings_tab.is_none() || collapse.is_some());
        // Each section paints its own translucent background, so none is stacked on another and the
        // whole card stays equally see-through.
        let mut card = div().id("card").relative().w(px(CARD_WIDTH)).flex().flex_col().rounded(px(20.0))
            .border_1().border_color(rgba(EDGE));
        if conversation_shown {
            let (ticker, thread, actions) = self.live_parts(cx);
            // The conversation hugs its content up to a cap; past it the thread (which may shrink,
            // unlike the strip and the actions) scrolls.
            let grown = collapse.unwrap_or(1.0);
            let mut conversation = div().flex().flex_col().max_h(px(CONVERSATION_MAX_HEIGHT * grown)).overflow_hidden()
                .bg(rgba(CARD)).rounded_t(px(19.0)).child(ticker.flex_none());
            if collapse.is_some() { conversation = conversation.opacity(grown); }
            if !self.turns.is_empty() { conversation = conversation.child(thread.flex_initial().min_h_0()); }
            card = card.child(conversation.child(actions.flex_none()));
        }
        card.child(self.input_row(conversation_shown, cx)).child(self.toolbar(live, cx))
    }

    /// The text box and the return button, which turns blue once there is something to send.
    fn input_row(&self, below_conversation: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = !self.composer.read(cx).text().is_empty();
        let send = div().id("send").size(px(32.0)).flex_none().rounded(px(8.0)).flex().items_center().justify_center().cursor_pointer()
            .bg(if ready { theme::accent() } else { rgb(RETURN_IDLE) })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { let input = this.composer.clone(); this.send_composer(input, window, cx); }))
            .child(ui::icon("icons/return.svg", 14.0, if ready { theme::accent_ink() } else { theme::body() }));
        // Without the conversation the text box is the top of the card; under it a hairline
        // separates the two.
        div().relative().flex().items_center().gap(px(10.0)).h(px(54.0)).pl(px(18.0)).pr(px(11.0)).bg(rgba(INPUT_ROW))
            .when(!below_conversation, |row| row.rounded_t(px(19.0)))
            .when(below_conversation, |row| row.border_t_1().border_color(rgba(EDGE)))
            .child(div().id("composer-input").flex_1().min_w_0().cursor_text()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus_text_box(window, cx)))
                .child(self.composer.clone()))
            .child(send)
            .child(self.hits.mark())
    }

    /// A click in the text box gives it the keyboard. The overlay doesn't activate on a click (a
    /// non-activating panel on macOS), so take it as the Type shortcut does; Esc or sending hands it
    /// back. Settings stays open: typing there is allowed, and sending collapses it.
    fn focus_text_box(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(native) = self.native && let Some(previous) = platform::take_focus(native) { self.return_focus = Some(previous); }
        window.focus(&self.composer.focus_handle(cx));
    }

    fn toolbar(&self, live: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let icon_button = |id: &'static str| div().id(id).flex().flex_none().items_center().justify_center().h(px(32.0)).rounded(px(8.0))
            .cursor_pointer().hover(|button| button.bg(rgba(HOVER)));
        // The waveform starts and stops Live; while live it is blue with the elapsed time beside it.
        let wave = icon_button("live").gap(px(6.0)).px(px(9.0))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| this.set_live(!live, window, cx)))
            .child(ui::icon("icons/tab-listening.svg", 18.0, if live { theme::accent_soft() } else { rgb(ICON_OFF) }))
            .when(live, |wave| wave.child(div().font_family(theme::MONO).text_size(px(12.0)).text_color(theme::accent_soft()).child(self.elapsed())));
        let middle = div().flex().items_center().gap(px(6.0))
            .child(self.toggle_button(Toggle::ScreenOnSend, cx))
            .child(self.toggle_button(Toggle::HideFromCapture, cx))
            .child(div().w(px(1.0)).h(px(16.0)).bg(theme::hairline()))
            .child(wave);
        let sessions = icon_button("sessions").gap(px(8.0)).pl(px(10.0)).pr(px(4.0))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.open_sessions(cx)))
            .child(div().text_size(px(13.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::body()).child("Sessions"))
            .child(div().size(px(24.0)).flex().items_center().justify_center().rounded(px(6.0)).bg(rgb(0x26282c))
                .child(ui::icon("icons/chevron-down.svg", 10.0, theme::body())));
        // Opens Settings, or closes the panel under the card when it is open.
        let settings_open = self.settings_tab.is_some();
        let settings = icon_button("settings").w(px(32.0)).when(settings_open, |button| button.bg(rgba(HOVER)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                if settings_open { this.close_panels(window, cx) } else { this.open_settings(Tab::default(), window, cx) }
            }))
            .child(ui::icon("icons/gear.svg", 17.0, if settings_open { theme::text() } else { theme::muted() }));
        div().flex().items_center().gap(px(6.0)).h(px(46.0)).pl(px(16.0)).pr(px(10.0)).bg(rgba(CARD)).rounded_b(px(19.0))
            // Dragging the toolbar's background moves the overlay.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(div().flex_1().flex().items_center().child(ui::mark(22.0)))
            .child(middle)
            .child(div().flex_1().flex().items_center().justify_end().gap(px(2.0)).child(sessions).child(settings))
            .child(self.hits.mark())
    }
}

/// Eased progress (0 to 1) of an animation `elapsed` into one lasting `duration`: ease-out cubic, so
/// it moves most at the start and settles gently.
pub(super) fn collapse_eased(elapsed: std::time::Duration, duration: std::time::Duration) -> f32 {
    let t = (elapsed.as_secs_f32() / duration.as_secs_f32().max(f32::EPSILON)).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::collapse_eased;

    #[test]
    fn the_collapse_eases_out_and_ends_exactly_at_one() {
        let duration = Duration::from_millis(200);
        assert_eq!(collapse_eased(Duration::ZERO, duration), 0.0);
        let halfway = collapse_eased(Duration::from_millis(100), duration);
        assert!(halfway > 0.8 && halfway < 0.9, "{halfway}");
        assert_eq!(collapse_eased(duration, duration), 1.0);
        assert_eq!(collapse_eased(Duration::from_secs(5), duration), 1.0);
    }
}
