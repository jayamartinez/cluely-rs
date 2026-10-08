//! macOS overlay, laid out like the Paper "macOS · Overlay v2 · Unified · Chat above composer"
//! artboard: one card. During Live its top holds the transcript strip, the answers and the quick
//! actions; below them sit the text box and a toolbar of plain icons (the mark, the two toggles, the
//! Live waveform with its timer, Sessions and Settings). The card's top edge stays put, so the text
//! box moves down as the conversation grows. Windows keeps the pill and panel.

use gpui::{Context, Focusable, InteractiveElement, IntoElement, MouseButton, ParentElement, Styled, div, prelude::*, px, rgb, rgba};

use super::{Overlay, WIDTH};
use crate::settings_view::Tab;
use crate::toggles::{Toggle, ToggleStyle};
use crate::{theme, ui};

/// The card behind the conversation and the toolbar.
const CARD: u32 = 0x121315;
/// The text box row, lighter than the rest of the card.
const INPUT_ROW: u32 = 0x2e3034;
const EDGE: u32 = 0xffffff14;
/// The return button while there is nothing to send.
const RETURN_IDLE: u32 = 0x43464b;
/// Toolbar icons while off.
const ICON_OFF: u32 = 0x8a867e;
const HOVER: u32 = 0xffffff0f;
/// The conversation scrolls once it reaches this height.
const THREAD_MAX_HEIGHT: f32 = 344.0;

impl Overlay {
    pub(super) fn mac_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.live_since.is_some();
        let mut card = div().id("bar").relative().w(px(WIDTH - 40.0)).flex().flex_col().rounded(px(20.0))
            .bg(rgb(CARD)).border_1().border_color(rgba(EDGE));
        if live {
            let (ticker, thread, actions) = self.live_parts(cx);
            card = card.child(ticker);
            if !self.turns.is_empty() { card = card.child(thread.flex_none().max_h(px(THREAD_MAX_HEIGHT))); }
            card = card.child(actions);
        }
        card.child(self.input_row(live, cx)).child(self.toolbar(live, cx))
    }

    /// The text box and the return button, which turns blue once there is something to send.
    fn input_row(&self, live: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = !self.composer.read(cx).text().is_empty();
        let send = div().id("send").size(px(32.0)).flex_none().rounded(px(8.0)).flex().items_center().justify_center().cursor_pointer()
            .bg(if ready { theme::accent() } else { rgb(RETURN_IDLE) })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| { let input = this.composer.clone(); this.send_composer(input, window, cx); }))
            .child(ui::icon("icons/return.svg", 14.0, if ready { theme::accent_ink() } else { theme::body() }));
        // Idle, the text box is the top of the card; during Live a hairline separates it from the
        // conversation above.
        div().relative().flex().items_center().gap(px(10.0)).h(px(54.0)).pl(px(18.0)).pr(px(11.0)).bg(rgb(INPUT_ROW))
            .when(!live, |row| row.rounded_t(px(19.0)))
            .when(live, |row| row.border_t_1().border_color(rgba(EDGE)))
            .child(div().id("composer-input").flex_1().min_w_0().cursor_text()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.composer.focus_handle(cx))))
                .child(self.composer.clone()))
            .child(send)
            .child(self.hits.mark())
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
            .child(self.toggle_button(Toggle::ScreenOnSend, ToggleStyle::Plain, cx))
            .child(self.toggle_button(Toggle::HideFromCapture, ToggleStyle::Plain, cx))
            .child(div().w(px(1.0)).h(px(16.0)).bg(theme::hairline()))
            .child(wave);
        let sessions = icon_button("sessions").gap(px(8.0)).pl(px(10.0)).pr(px(4.0))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.open_sessions(cx)))
            .child(div().text_size(px(13.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::body()).child("Sessions"))
            .child(div().size(px(24.0)).flex().items_center().justify_center().rounded(px(6.0)).bg(rgb(0x26282c))
                .child(ui::icon("icons/chevron-down.svg", 10.0, theme::body())));
        let settings = icon_button("settings").w(px(32.0))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.open_settings(Tab::default(), window, cx)))
            .child(ui::icon("icons/gear.svg", 17.0, theme::muted()));
        div().flex().items_center().gap(px(6.0)).h(px(46.0)).pl(px(16.0)).pr(px(10.0))
            // Dragging the toolbar's background moves the overlay, as the pill does on Windows.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(div().flex_1().flex().items_center().child(ui::mark(22.0)))
            .child(middle)
            .child(div().flex_1().flex().items_center().justify_end().gap(px(2.0)).child(sessions).child(settings))
            .child(self.hits.mark())
    }
}
