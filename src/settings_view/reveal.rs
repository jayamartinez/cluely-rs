//! Showing a saved API key on request (the eye on the saved key in Settings › Model), as drawn in
//! the Paper "Model v3 · Reveal saved key" artboard.
//!
//! The key is read only when the eye is clicked, on a background thread, and only while Settings
//! is hidden from screen capture. It is drawn as plain text, which can't be selected, copied or read by the platform's
//! text services, and is forgotten after ten seconds, on a second click, when a tab opens, when
//! Settings closes, when the overlay hides, or when the provider or capture setting changes. It is
//! never logged or put in an error.

use std::time::{Duration, Instant};

use gpui::{AnyView, App, AppContext, Context, Div, IntoElement, MouseButton, ParentElement, Render, Styled, Task, Window, div, prelude::*, px, rgb};

use super::listen;
use crate::overlay::{Overlay, capture_excluded};
use crate::{theme, ui};

/// How long a saved key stays shown.
const SHOW_FOR: Duration = Duration::from_secs(10);
/// Why the eye is dimmed.
const NEEDS_CAPTURE_EXCLUSION: &str = "To show it, turn on Hide from screen capture (Window)";
/// Shown on the eye while reading the key may bring up a Keychain prompt.
const MAY_PROMPT: &str = "macOS may ask for your password to let CluelyRS read the key.";
/// The countdown's color.
const COUNTDOWN: u32 = 0xe6c07b;

/// The saved key being shown, if any, and the task that reads it and counts it down.
#[derive(Default)]
pub(crate) struct KeyReveal {
    shown: Option<Shown>,
    /// The key is being read (on a background thread).
    reading: bool,
    ticker: Option<Task<()>>,
}

struct Shown {
    provider: String,
    key: String,
    until: Instant,
}

impl KeyReveal {
    fn show(&mut self, provider: &str, key: String, now: Instant) {
        self.shown = Some(Shown { provider: provider.to_string(), key, until: now + SHOW_FOR });
    }

    /// Hide the key and forget it, dropping a read still in progress.
    pub(crate) fn hide(&mut self) {
        self.shown = None;
        self.reading = false;
        self.ticker = None;
    }

    /// The key, while it is shown for `provider`, its time isn't up and Settings is hidden from capture.
    fn key(&self, provider: &str, capture_excluded: bool, now: Instant) -> Option<&str> {
        self.shown.as_ref()
            .filter(|shown| capture_excluded && shown.provider == provider && now < shown.until)
            .map(|shown| shown.key.as_str())
    }

    /// Whole seconds left, rounded up.
    fn seconds_left(&self, now: Instant) -> Option<u64> {
        let left = self.shown.as_ref()?.until.checked_duration_since(now).filter(|left| !left.is_zero())?;
        Some(left.as_millis().div_ceil(1000) as u64)
    }

    /// Forget the key once its time is up. Whether it is still shown.
    fn tick(&mut self, now: Instant) -> bool {
        if self.seconds_left(now).is_none() { self.shown = None; }
        self.shown.is_some()
    }
}

impl Overlay {
    /// The eye on a saved key: show it (reading it now, off the UI thread) or hide it again.
    fn toggle_saved_key(&mut self, provider: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let allowed = capture_excluded(&self.store.value);
        if self.key_reveal.key(provider, allowed, Instant::now()).is_some() || !allowed {
            self.key_reveal.hide();
            cx.notify();
            return;
        }
        if self.key_reveal.reading { return; }
        self.key_reveal.reading = true;
        self.key_reveal.ticker = Some(cx.spawn_in(window, async move |this, cx| {
            // The credential store can block, or show a Keychain prompt: read it in the background.
            let key = cx.background_executor().spawn(async move { crate::secrets::get(provider) }).await;
            let shown = this.update(cx, |this, cx| {
                this.key_reveal.reading = false;
                let shown = match key {
                    // Capture exclusion may have been turned off while the key was read.
                    Some(key) if capture_excluded(&this.store.value) => {
                        // A key marked before it was ever read gets its hint now.
                        this.set_key_marker(provider, crate::secrets::masked(&key));
                        this.key_reveal.show(provider, key, Instant::now());
                        true
                    }
                    Some(_) => false,
                    None => { this.key_notice = Some("Couldn't read the saved key.".into()); false }
                };
                cx.notify();
                shown
            });
            if !matches!(shown, Ok(true)) { return; }
            // Redraw every second for the countdown; stop once the key is hidden.
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let shown = this.update(cx, |this, cx| {
                    let shown = this.key_reveal.tick(Instant::now());
                    cx.notify();
                    shown
                });
                if !matches!(shown, Ok(true)) { break; }
            }
        }));
        cx.notify();
    }

    /// The saved key for `provider` if it is shown right now.
    pub(crate) fn revealed_key(&self, provider: &str) -> Option<&str> {
        self.key_reveal.key(provider, capture_excluded(&self.store.value), Instant::now())
    }

    /// "Hides in 8 s" while a saved key is shown.
    pub(crate) fn reveal_countdown(&self, short: bool) -> Option<Div> {
        let left = self.key_reveal.seconds_left(Instant::now())?;
        let text = if short { format!("{left} s") } else { format!("Hides in {left} s") };
        Some(div().flex_none().text_color(rgb(COUNTDOWN)).child(text))
    }

    /// The eye beside a saved key. Dimmed, with a tooltip saying why, while Settings can be captured.
    pub(crate) fn saved_key_eye(&self, provider: &'static str, cx: &Context<Self>) -> impl IntoElement {
        let allowed = capture_excluded(&self.store.value);
        let shown = self.revealed_key(provider).is_some();
        let (icon, color) = match (shown, allowed) {
            (true, _) => ("icons/eye-off.svg", theme::body()),
            (false, true) => ("icons/eye.svg", theme::muted()),
            (false, false) => ("icons/eye.svg", rgb(0x45474c)),
        };
        let eye = div().id("saved-key-eye").flex_none().size(px(20.0)).flex().items_center().justify_center().rounded(px(5.0))
            .when(shown, |eye| eye.bg(rgb(0x25282c)))
            .child(ui::icon(icon, 14.0, color));
        if !allowed {
            return eye.tooltip(|_, cx| tooltip(NEEDS_CAPTURE_EXCLUSION, cx));
        }
        let eye = if !shown && crate::secrets::may_prompt(provider) { eye.tooltip(|_, cx| tooltip(MAY_PROMPT, cx)) } else { eye };
        eye.cursor_pointer().on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| {
            cx.stop_propagation();
            this.toggle_saved_key(provider, window, cx);
        }))
    }

    /// Why a saved key can't be shown, while Settings can be captured.
    #[cfg(target_os = "macos")]
    pub(crate) fn reveal_unavailable(&self) -> Option<&'static str> {
        (!capture_excluded(&self.store.value)).then_some(NEEDS_CAPTURE_EXCLUSION)
    }
}

/// A shown key: one line of plain monospace text that scrolls sideways when it is long.
pub(crate) fn revealed_text(key: &str) -> impl IntoElement {
    div().id("revealed-key").flex_1().min_w_0().overflow_x_scroll().whitespace_nowrap()
        .font_family(theme::MONO).text_size(px(12.0)).text_color(theme::text()).child(key.to_string())
}

struct Tooltip(&'static str);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(px(8.0)).py(px(5.0)).rounded(px(6.0)).bg(theme::raised()).border_1().border_color(theme::keycap_border())
            .text_size(px(12.0)).text_color(theme::body()).child(self.0)
    }
}

fn tooltip(text: &'static str, cx: &mut App) -> AnyView {
    cx.new(|_| Tooltip(text)).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown_at(now: Instant) -> KeyReveal {
        let mut reveal = KeyReveal::default();
        reveal.show("anthropic", "sk-ant-dummy".to_string(), now);
        reveal
    }

    #[test]
    fn a_key_shows_for_ten_seconds_with_a_countdown() {
        let start = Instant::now();
        let mut reveal = shown_at(start);
        assert_eq!(reveal.key("anthropic", true, start), Some("sk-ant-dummy"));
        assert_eq!(reveal.seconds_left(start), Some(10));
        assert_eq!(reveal.seconds_left(start + Duration::from_millis(1500)), Some(9));
        assert_eq!(reveal.seconds_left(start + Duration::from_millis(9_500)), Some(1));
        assert!(reveal.tick(start + Duration::from_millis(9_999)));
        let later = start + SHOW_FOR;
        assert_eq!(reveal.key("anthropic", true, later), None);
        assert_eq!(reveal.seconds_left(later), None);
        assert!(!reveal.tick(later));
        assert!(reveal.shown.is_none(), "the key is forgotten once its time is up");
    }

    #[test]
    fn hiding_forgets_the_key() {
        let start = Instant::now();
        let mut reveal = shown_at(start);
        reveal.hide();
        assert!(reveal.shown.is_none());
        assert_eq!(reveal.key("anthropic", true, start), None);
        assert_eq!(reveal.seconds_left(start), None);
    }

    #[test]
    fn a_key_is_never_shown_while_settings_can_be_captured() {
        let start = Instant::now();
        let reveal = shown_at(start);
        assert_eq!(reveal.key("anthropic", false, start), None);
        assert_eq!(reveal.key("anthropic", true, start), Some("sk-ant-dummy"));
    }

    #[test]
    fn a_key_shows_only_for_its_own_provider() {
        let start = Instant::now();
        let reveal = shown_at(start);
        assert_eq!(reveal.key("openai", true, start), None);
    }
}
