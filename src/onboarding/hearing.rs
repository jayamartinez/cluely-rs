//! Setup › Listening: what it hears and how it transcribes. One panel with You (the microphone) and
//! Them (desktop audio), each with its device, a live 14-segment meter and a status icon, captured as
//! Live would while the step is open; a row with the way to Windows' privacy settings while Windows
//! blocks the microphone. Under it the speech model, picked from two cards: Parakeet on this PC (its
//! download starts on the pick) or Deepgram with its key. Start waits for a pick.

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Div, Focusable, FontWeight, IntoElement, MouseButton, ParentElement, Stateful, Styled, Window, div, prelude::*, px, relative};

use super::steps::{check_badge, heading, link, panel, small_button, tile};
use super::{Menu, Setup};
use crate::audio::capture::AudioCapture;
use crate::audio::frame::Source;
use crate::overlay::Overlay;
use crate::settings::{Settings, SttProvider};
use crate::settings_view::fluent::status;
use crate::settings_view::listen;
use crate::stt::{deepgram, parakeet::MODEL};
use crate::transcript_view::model_size_label;
use crate::{theme, ui};

/// How often the meters read the capture's levels.
const METER_EVERY: Duration = Duration::from_millis(50);
/// Readings between looks at whether Windows blocks the microphone (about a second).
const BLOCK_CHECK_EVERY: u32 = 20;
/// A level above this (0..=1, `listening::level`) counts as hearing something.
const HEARD: f32 = 0.12;
const SEGMENTS: usize = 14;
/// The meters panel and the cards under it.
const COLUMN: f32 = 520.0;

/// The capture behind Listening's meters, and what they show.
pub(super) struct AudioCheck {
    started: Instant,
    capture: Option<AudioCapture>,
    /// The sources couldn't be opened.
    failed: bool,
    levels: [f32; 2],
    heard: [bool; 2],
    /// Windows' privacy settings deny desktop apps the microphone.
    blocked: bool,
}

fn slot(source: Source) -> usize { match source { Source::Me => 0, Source::Them => 1 } }

/// "102 of 176 MB" for a download `progress` of the speech model.
fn megabytes(progress: f64) -> String {
    let total = MODEL.bytes as f64 / 1e6;
    format!("{:.0} of {total:.0} MB", total * progress.clamp(0.0, 1.0))
}

/// Whether Windows denies desktop apps the microphone: the device-wide switch (HKLM), the user's
/// "Microphone access", or "Let desktop apps access your microphone" (`NonPackaged`). Capture still
/// opens then, but hears only silence.
fn microphone_blocked() -> bool {
    use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::{PCWSTR, w};
    let denied = |root: HKEY, key: PCWSTR| {
        let mut value = [0u16; 16];
        let mut bytes = std::mem::size_of_val(&value) as u32;
        // SAFETY: a read into `value`, whose size in bytes is passed with it.
        let read = unsafe { RegGetValueW(root, key, w!("Value"), RRF_RT_REG_SZ, None, Some(value.as_mut_ptr().cast()), Some(&mut bytes)) }.is_ok();
        read && String::from_utf16_lossy(&value[..(bytes as usize / 2).saturating_sub(1)]) == "Deny"
    };
    let store = w!(r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone");
    let desktop_apps = w!(r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone\NonPackaged");
    denied(HKEY_LOCAL_MACHINE, store) || denied(HKEY_CURRENT_USER, store) || denied(HKEY_CURRENT_USER, desktop_apps)
}

/// 14 squares of 6 px, 3 apart across 154 px: lit #7DD3A0 up to the level, the rest #2C2F33.
fn meter(level: f32) -> Div {
    let lit = (level.clamp(0.0, 1.0) * SEGMENTS as f32).round() as usize;
    div().w(px(154.0)).flex_none().flex().gap(px(3.0))
        .children((0..SEGMENTS).map(|index| div().flex_1().h(px(6.0)).rounded(px(2.0)).bg(if index < lit { theme::ok() } else { theme::hairline() })))
}

/// A speech model's card: unpicked #131416, picked blue with the check.
fn choice_card(id: &'static str, chosen: bool) -> Stateful<Div> {
    let card = div().id(id).relative().flex_1().min_w_0().flex().flex_col().gap(px(10.0)).p(px(12.0)).rounded(px(6.0)).border_1();
    if chosen { card.bg(theme::selected_row()).border_color(theme::accent()).child(check_badge()) }
    else { card.bg(theme::setup_panel()).border_color(theme::divider()).cursor_pointer().hover(|card| card.border_color(theme::keycap_border())) }
}

/// The card's tile, name and line.
fn choice_header(icon: &'static str, size: f32, chosen: bool, name: &'static str, detail: String) -> Div {
    div().flex().items_center().gap(px(12.0))
        .child(tile(36.0, chosen).child(ui::icon(icon, size, if chosen { theme::accent_soft() } else { theme::body() })))
        .child(div().min_w_0().flex().flex_col().gap(px(2.0))
            .child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(name))
            .child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(detail)))
}

/// The full-width 30 px button at the bottom of an unpicked card.
fn card_button(icon: &'static str, label: &'static str) -> Div {
    div().h(px(30.0)).flex().flex_none().items_center().justify_center().gap(px(6.0)).rounded(px(4.0)).bg(theme::raised()).border_1().border_color(theme::keycap_border())
        .child(ui::icon(icon, 14.0, theme::body()))
        .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(label))
}

/// A picked card's bottom line: an icon (or none) and a few words, 30 tall like the button it replaces.
fn card_line(ok: bool, text: impl Into<gpui::SharedString>) -> Div {
    div().h(px(30.0)).flex().items_center().gap(px(8.0))
        .when(ok, |line| line.child(ui::icon("icons/setup-circle-check.svg", 14.0, theme::ok())))
        .child(div().flex_1().min_w_0().truncate().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::body()).child(text.into()))
}

impl Overlay {
    /// Capture the sources that are on, from the chosen devices, and keep the meters moving while
    /// the step is open; also watch whether Windows blocks the microphone, capturing afresh once it
    /// stops. Starting again (a device changed) replaces the capture.
    pub(super) fn start_audio_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        let s = &self.store.value;
        let sources: Vec<Source> = [(Source::Me, s.listen_mic), (Source::Them, s.listen_desktop)].into_iter().filter(|(_, on)| *on).map(|(source, _)| source).collect();
        let started = Instant::now();
        setup.audio = Some(AudioCheck { started, capture: None, failed: false, levels: [0.0; 2], heard: [false; 2], blocked: microphone_blocked() });
        cx.notify();
        let devices = s.devices();
        cx.spawn_in(window, async move |this, cx| {
            if !sources.is_empty() {
                // Opening a device can take seconds, so off the UI thread.
                let opened = cx.background_executor().spawn(async move {
                    let (sink, chunks) = std::sync::mpsc::channel();
                    // Only the levels are read: drain the audio so it doesn't pile up. The thread ends
                    // with the capture, when the last sender goes.
                    std::thread::spawn(move || for _ in chunks {});
                    AudioCapture::start_with(&sources, &devices, sink).map(|(capture, _)| capture).map_err(|error| eprintln!("setup audio check: {error}")).ok()
                }).await;
                let kept = this.update(cx, |this, cx| {
                    let Some(check) = this.setup.as_mut().and_then(|setup| setup.audio.as_mut()).filter(|check| check.started == started) else { return false };
                    check.failed = opened.is_none();
                    check.capture = opened;
                    cx.notify();
                    true
                }).unwrap_or(false);
                if !kept { return; }
            }
            let mut readings = 0u32;
            loop {
                cx.background_executor().timer(METER_EVERY).await;
                readings += 1;
                let going = this.update_in(cx, |this, window, cx| {
                    let Some(check) = this.setup.as_mut().and_then(|setup| setup.audio.as_mut()).filter(|check| check.started == started) else { return false };
                    if readings.is_multiple_of(BLOCK_CHECK_EVERY) {
                        let blocked = microphone_blocked();
                        if blocked != check.blocked {
                            // A capture opened while blocked only ever hears silence.
                            if !blocked { this.start_audio_check(window, cx); return false; }
                            check.blocked = true;
                        }
                    }
                    if let Some(capture) = &check.capture {
                        for source in Source::ALL {
                            let level = crate::listening::level(capture.level(source));
                            // Falls a little each reading rather than flickering to the next chunk.
                            check.levels[slot(source)] = level.max(check.levels[slot(source)] * 0.8);
                            if level > HEARD { check.heard[slot(source)] = true; }
                        }
                    }
                    cx.notify();
                    true
                }).unwrap_or(false);
                if !going { break; }
            }
        }).detach();
    }

    fn change_audio(&mut self, change: impl FnOnce(&mut Settings), window: &mut Window, cx: &mut Context<Self>) {
        self.update_settings(change, window, cx);
        self.start_audio_check(window, cx);
    }

    /// Pick the speech model: Parakeet starts its download (unless it is already here), Deepgram
    /// opens its key field. Either way transcription is on, with that engine.
    fn choose_speech(&mut self, provider: SttProvider, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        setup.speech = Some(provider);
        setup.replacing_key = false;
        self.key_notice = None;
        self.update_settings(|s| { s.transcribe = true; s.stt_provider = provider; }, window, cx);
        if provider == SttProvider::Parakeet && !self.model_installed { self.download_model(window, cx); }
        cx.notify();
    }

    /// Start waits for a speech model: Parakeet once its download has started (or it's here), Deepgram
    /// once its key is saved.
    fn speech_ready(&self, setup: &Setup) -> bool {
        match setup.speech {
            Some(SttProvider::Parakeet) => self.model_installed || self.download_progress().is_some(),
            Some(SttProvider::Deepgram) => self.store.value.saved_keys.contains_key(deepgram::PROVIDER_ID),
            None => false,
        }
    }

    pub(super) fn listening(&self, setup: &Setup, window: &Window, cx: &mut Context<Self>) -> Div {
        let blocked = self.store.value.listen_mic && setup.audio.as_ref().is_some_and(|check| check.blocked);
        let blocked_row = blocked.then(|| div().flex().items_center().gap(px(10.0)).py(px(10.0)).pl(px(16.0)).pr(px(12.0))
            .bg(theme::field()).border_b_1().border_color(theme::divider())
            .child(div().flex_1().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Windows is blocking the microphone"))
            .child(small_button("setup-mic-privacy", "Privacy settings", false).child(ui::icon("icons/setup-external.svg", 10.0, theme::body()))
                .on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| cx.open_url("ms-settings:privacy-microphone")))));
        let meters = panel().flex().flex_col()
            .child(self.source_row(setup, Source::Me, blocked, cx).border_b_1().border_color(theme::divider()))
            .children(blocked_row)
            .child(self.source_row(setup, Source::Them, false, cx));
        let choices = div().flex().gap(px(10.0)).child(self.parakeet_card(setup, cx)).child(self.deepgram_card(setup, window, cx));
        let actions = div().flex().flex_col().items_center().gap(px(14.0))
            .child(self.primary("setup-start", "Start CluelyRS", self.speech_ready(setup), None, cx))
            .child(self.start_shortcuts());
        // The blocked row's height comes out of the space around the content, as drawn.
        let (top, bottom, gap) = if blocked { (4.0, 22.0, 16.0) } else { (12.0, 28.0, 22.0) };
        Self::step_frame(top, bottom, gap, vec![
            heading("Check what it hears", "Say something, play a video, then pick a speech model."),
            div().w(px(COLUMN)).flex().flex_col().gap(px(12.0)).child(meters).child(choices).into_any_element(),
        ], actions)
    }

    /// You or Them: the tile, the name over its device (a dropdown), the meter and a status icon
    /// (heard, or the alert while the microphone is blocked or couldn't be opened).
    fn source_row(&self, setup: &Setup, source: Source, blocked: bool, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mic = source == Source::Me;
        let on = if mic { s.listen_mic } else { s.listen_desktop };
        let check = setup.audio.as_ref();
        let level = check.filter(|_| on && !blocked).map_or(0.0, |check| check.levels[slot(source)]);
        let devices = self.devices.clone().unwrap_or_default();
        let (chosen, names, default) = if mic { (&s.mic_device, &devices.microphones, devices.default_microphone.as_ref()) }
            else { (&s.desktop_device, &devices.playback, devices.default_playback.as_ref()) };
        let device = if !on { "Off · turn it on in Settings › Listening".to_string() } else if !chosen.is_empty() { chosen.clone() }
            else { default.cloned().unwrap_or_else(|| if self.devices_loading { "Finding devices…".into() } else { "Default".into() }) };
        let mut options = vec![(String::new(), default.map_or_else(|| "Default".to_string(), |name| format!("Default · {name}")))];
        options.extend(names.iter().map(|name| (name.clone(), name.clone())));
        let menu = if mic { Menu::Mic } else { Menu::Desktop };
        // Them's list opens upward: below it there is little room left.
        let list = self.setup_menu(setup, menu, options, None, chosen, 20.0, !mic, 300.0, move |this, name, window, cx| {
            this.change_audio(|s| if mic { s.mic_device = name } else { s.desktop_device = name }, window, cx)
        }, cx);
        let picker = div().id(("setup-device", slot(source))).relative().flex().items_center().gap(px(4.0)).cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, _, cx| this.set_menu(Some(menu), cx)))
            .child(div().min_w_0().truncate().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child(device))
            .child(ui::icon("icons/chevron-down.svg", 10.0, theme::muted()))
            .children(list);
        let failed = on && check.is_some_and(|check| check.failed);
        let heard = on && check.is_some_and(|check| check.heard[slot(source)]);
        let state = if blocked { Some(("icons/setup-circle-alert.svg", theme::text())) } else if failed { Some(("icons/setup-circle-alert.svg", theme::muted())) }
            else if heard { Some(("icons/setup-circle-check.svg", theme::ok())) } else { None };
        div().flex().items_center().gap(px(14.0)).py(px(14.0)).px(px(16.0))
            .child(tile(40.0, false).child(ui::icon(if mic { "icons/setup-mic.svg" } else { "icons/setup-speaker.svg" }, 20.0, theme::body())))
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.0))
                .child(div().text_size(px(14.0)).line_height(px(20.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(if mic { "You" } else { "Them" }))
                .child(div().flex().child(picker)))
            .child(meter(level))
            .child(div().w(px(18.0)).flex_none().flex().justify_center().children(state.map(|(icon, color)| ui::icon(icon, 16.0, color))))
    }

    /// Parakeet on this PC: Download (or use the copy that's here), then its progress or Ready.
    fn parakeet_card(&self, setup: &Setup, cx: &mut Context<Self>) -> AnyElement {
        let chosen = setup.speech == Some(SttProvider::Parakeet);
        let header = choice_header("icons/laptop-minimal.svg", 18.0, chosen, "On this PC", format!("Parakeet · private · {}", model_size_label(&MODEL)));
        let progress = self.download_progress();
        let bottom = match (chosen, progress) {
            (true, Some(progress)) => div().h(px(30.0)).flex().items_center().gap(px(10.0))
                .child(div().flex_1().h(px(4.0)).rounded(px(2.0)).bg(theme::divider())
                    .child(div().h_full().rounded(px(2.0)).bg(theme::accent()).w(relative(progress.clamp(0.0, 1.0) as f32))))
                .child(div().flex_none().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::body()).child(megabytes(progress))),
            (true, None) if self.model_installed => card_line(true, "Ready · downloaded"),
            _ if self.model_installed => card_button("icons/setup-check.svg", "Use it"),
            _ => card_button("icons/setup-download.svg", "Download"),
        };
        // A download that failed or was cancelled says why, and the card offers Download again.
        let notice = self.model_notice.clone().filter(|_| chosen).map(status);
        choice_card("setup-speech-local", chosen)
            .when(!chosen || (progress.is_none() && !self.model_installed), |card| card.cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.choose_speech(SttProvider::Parakeet, window, cx))))
            .child(header).child(bottom).children(notice)
            .into_any_element()
    }

    /// Deepgram: Add API key, then its key field in the card, then the saved key.
    fn deepgram_card(&self, setup: &Setup, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let chosen = setup.speech == Some(SttProvider::Deepgram);
        let saved = self.store.value.saved_keys.get(deepgram::PROVIDER_ID).filter(|_| !setup.replacing_key);
        let header = choice_header("icons/logo-deepgram.svg", 16.0, chosen, "Deepgram", "Cloud · your API key".into());
        let focus = self.key_input.clone();
        let take_keyboard = move |_: &gpui::MouseDownEvent, window: &mut Window, cx: &mut gpui::App| window.focus(&focus.focus_handle(cx));
        let card = choice_card("setup-speech-deepgram", chosen).child(header);
        let card = match (chosen, saved) {
            (false, saved) => card.on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.choose_speech(SttProvider::Deepgram, window, cx)))
                .on_mouse_down(MouseButton::Left, take_keyboard)
                .child(card_button("icons/setup-key.svg", if saved.is_some() { "Use saved key" } else { "Add API key" })),
            (true, Some(hint)) => card.child(card_line(true, format!("{} · saved", if hint.is_empty() { "Key" } else { hint.as_str() })).id("setup-deepgram-saved").cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| if let Some(setup) = &mut this.setup { setup.replacing_key = true; cx.notify(); }))
                .on_mouse_down(MouseButton::Left, take_keyboard)),
            (true, None) => {
                let focused = self.key_input.read(cx).focus_handle(cx).is_focused(window);
                let typed = !self.key_input.read(cx).text().trim().is_empty();
                let field = div().id("setup-deepgram-key").h(px(30.0)).px(px(10.0)).flex().items_center().gap(px(8.0)).rounded(px(4.0)).cursor_text()
                    .bg(theme::field()).border_1().border_color(if focused { theme::accent() } else { theme::hairline() })
                    .on_mouse_down(MouseButton::Left, take_keyboard)
                    .child(div().flex_1().min_w_0().text_size(px(12.0)).child(self.key_input.clone()))
                    .child(if typed {
                        link("setup-save-deepgram", "Save").text_color(theme::accent_soft())
                            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| { cx.stop_propagation(); this.save_key(window, cx) })).into_any_element()
                    } else {
                        link("setup-deepgram-page", "Get a key").text_color(theme::accent_soft())
                            .on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| { cx.stop_propagation(); cx.open_url("https://console.deepgram.com/") })).into_any_element()
                    });
                card.child(field).children(self.key_notice.clone().map(status))
            }
        };
        card.into_any_element()
    }
}
