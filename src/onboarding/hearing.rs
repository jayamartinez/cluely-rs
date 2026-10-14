//! Setup › Transcription (off, Parakeet on this PC with its download, or Deepgram with its key) and
//! Setup › Audio check: both sources captured as Live would, each with a meter, its device and a
//! hint when the microphone stays silent.

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Div, IntoElement, MouseButton, ParentElement, Styled, Window, div, prelude::*, px, relative, rgb};

use super::steps::{self, CAUTION, external, heading, hint, option_card, page, small_button, trailing};
use super::{Menu, Setup};
use crate::audio::capture::AudioCapture;
use crate::audio::frame::Source;
use crate::overlay::Overlay;
use crate::settings::{Settings, SttProvider};
use crate::settings_view::listen;
use crate::stt::parakeet::MODEL;
use crate::{theme, ui};

/// How often the meters read the capture's levels.
const METER_EVERY: Duration = Duration::from_millis(50);
/// How long a silent microphone waits before the hint about microphone access shows.
const SILENT_HINT_AFTER: Duration = Duration::from_secs(10);
/// A level above this (0..=1, `listening::level`) counts as hearing something.
const HEARD: f32 = 0.12;
/// How long the peak tick holds before it falls.
const PEAK_HOLD: Duration = Duration::from_secs(1);

/// The capture behind Audio check's meters, and what they show.
pub(super) struct AudioCheck {
    started: Instant,
    capture: Option<AudioCapture>,
    error: Option<String>,
    meters: [Meter; 2],
}

#[derive(Clone, Copy, Default)]
struct Meter { level: f32, peak: f32, peak_at: Option<Instant>, last_heard: Option<Instant> }

fn slot(source: Source) -> usize { match source { Source::Me => 0, Source::Them => 1 } }

/// "102 of 176 MB" for a download `progress` of the speech model.
pub(super) fn megabytes(progress: f64) -> String {
    let total = MODEL.bytes as f64 / 1e6;
    format!("{:.0} of {total:.0} MB", total * progress.clamp(0.0, 1.0))
}

impl Overlay {
    fn choose_transcription(&mut self, on: bool, provider: SttProvider, window: &mut Window, cx: &mut Context<Self>) {
        self.key_input.update(cx, |input, cx| input.clear(cx));
        self.key_notice = None;
        if let Some(setup) = &mut self.setup { setup.replacing_key = false; }
        self.update_settings(|s| { s.transcribe = on; if on { s.stt_provider = provider; } }, window, cx);
    }

    pub(super) fn transcription(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let local = s.transcribe && s.stt_provider == SttProvider::Parakeet;
        let cloud = s.transcribe && s.stt_provider == SttProvider::Deepgram;
        let tint = |on| if on { theme::accent_soft() } else { theme::body() };
        let deepgram_saved = s.saved_keys.contains_key(crate::stt::deepgram::PROVIDER_ID);
        let recommended = div().px(px(7.0)).py(px(1.0)).rounded_full().border_1().border_color(theme::bubble_border())
            .text_size(px(11.0)).line_height(px(14.0)).text_color(theme::accent_soft()).child("Recommended").into_any_element();
        let cards = [
            (false, SttProvider::Parakeet, !s.transcribe, ui::icon("icons/setup-wave-off.svg", 20.0, tint(!s.transcribe)).into_any_element(),
                div().into_any_element(), "Off", "Typed questions only"),
            (true, SttProvider::Parakeet, local, ui::icon("icons/setup-monitor.svg", 20.0, tint(local)).into_any_element(), recommended,
                "On this PC", "Parakeet · English"),
            (true, SttProvider::Deepgram, cloud, ui::icon("icons/logo-deepgram.svg", 18.0, tint(cloud)).into_any_element(),
                if deepgram_saved { steps::status_text("Key saved", theme::ok()) } else { steps::status_text("Key needed", theme::muted()) },
                "Deepgram", "Cloud · audio sent"),
        ];
        let mut row = div().flex().gap(px(10.0));
        for (index, (on, provider, selected, icon, status, name, detail)) in cards.into_iter().enumerate() {
            row = row.child(option_card(("setup-transcribe", index), selected, icon, status, name, steps::detail_text(detail))
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.choose_transcription(on, provider, window, cx))));
        }
        let panel = if local { Some(self.speech_model_panel(cx)) } else if cloud { Some(self.deepgram_panel(setup, cx)) } else { None };
        page().child(heading("Transcribe the conversation", "Only while Live is on."))
            .child(row)
            .children(panel)
    }

    /// The Parakeet model: Download, its progress with Cancel, or ready. Continue never waits for
    /// it, and leaving the step doesn't stop it.
    fn speech_model_panel(&self, cx: &mut Context<Self>) -> Div {
        let progress = self.download_progress();
        let (icon, color) = if self.model_installed { ("icons/setup-circle-check.svg", theme::ok()) }
            else if progress.is_some() { ("icons/setup-download.svg", theme::accent_soft()) } else { ("icons/setup-download.svg", theme::muted()) };
        let action: AnyElement = if let Some(progress) = progress {
            div().flex().items_center().gap(px(12.0))
                .child(steps::status_text(megabytes(progress), theme::muted()))
                .child(trailing(56.0, steps::link("setup-cancel-download", "Cancel")
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| { this.cancel_download(); cx.notify(); }))))
                .into_any_element()
        } else if self.model_installed {
            steps::status_text("Ready", theme::ok())
        } else {
            small_button("setup-download", format!("Download {}", crate::transcript_view::model_size_label(&MODEL)), true)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.download_model(window, cx))).into_any_element()
        };
        let mut panel = div().flex().flex_col().gap(px(10.0)).px(px(14.0)).py(px(12.0)).rounded(px(12.0)).border_1().border_color(theme::hairline())
            .child(div().flex().items_center().gap(px(12.0)).min_h(px(30.0))
                .child(ui::icon(icon, 16.0, color))
                .child(div().flex_1().text_size(px(13.0)).text_color(theme::text()).child("Speech model"))
                .child(action));
        if let Some(progress) = progress {
            panel = panel.child(div().h(px(4.0)).rounded_full().bg(theme::divider())
                .child(div().h_full().rounded_full().bg(theme::accent()).w(relative(progress.clamp(0.0, 1.0) as f32))));
        }
        if let Some(notice) = self.model_notice.clone() {
            panel = panel.child(div().text_size(px(12.0)).text_color(theme::muted()).child(notice));
        }
        panel.child(div().flex().child(external("setup-license", "NVIDIA Open Model License")
            .on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| cx.open_url(MODEL.license_url)))))
    }

    /// Deepgram's key: saved and masked with Replace, or a box to paste it into.
    fn deepgram_panel(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let saved = self.store.value.saved_keys.get(crate::stt::deepgram::PROVIDER_ID).map(String::as_str);
        let (field, action) = self.key_field(setup, saved, false, cx);
        let mut panel = div().flex().flex_col().gap(px(8.0)).px(px(14.0)).py(px(12.0)).rounded(px(12.0)).border_1().border_color(theme::hairline())
            .child(div().flex().items_center().gap(px(12.0))
                .child(ui::icon("icons/setup-key.svg", 16.0, theme::muted()))
                .child(div().flex_1().min_w_0().child(field))
                .child(action)
                .child(external("setup-deepgram-key", "Get a key").on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| cx.open_url("https://console.deepgram.com/")))));
        if let Some(notice) = self.key_notice.clone() {
            panel = panel.child(div().pl(px(28.0)).text_size(px(12.0)).text_color(theme::muted()).child(notice));
        }
        panel
    }

    /// Capture the sources that are on, from the chosen devices, and keep the meters moving while
    /// the step is open. Starting again (a device or switch changed) replaces the capture.
    pub(super) fn start_audio_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        let s = &self.store.value;
        let sources: Vec<Source> = [(Source::Me, s.listen_mic), (Source::Them, s.listen_desktop)].into_iter().filter(|(_, on)| *on).map(|(source, _)| source).collect();
        let started = Instant::now();
        setup.audio = Some(AudioCheck { started, capture: None, error: None, meters: Default::default() });
        cx.notify();
        if sources.is_empty() { return; }
        let devices = s.devices();
        cx.spawn_in(window, async move |this, cx| {
            // Opening a device can take seconds, so off the UI thread.
            let opened = cx.background_executor().spawn(async move {
                let (sink, chunks) = std::sync::mpsc::channel();
                // Only the levels are read: drain the audio so it doesn't pile up. The thread ends
                // with the capture, when the last sender goes.
                std::thread::spawn(move || for _ in chunks {});
                AudioCapture::start_with(&sources, &devices, sink).map(|(capture, _)| capture).map_err(|error| error.to_string())
            }).await;
            let kept = this.update(cx, |this, cx| {
                let Some(check) = this.setup.as_mut().and_then(|setup| setup.audio.as_mut()).filter(|check| check.started == started) else { return false };
                match opened {
                    Ok(capture) => check.capture = Some(capture),
                    Err(error) => check.error = Some(error),
                }
                cx.notify();
                check.capture.is_some()
            }).unwrap_or(false);
            if !kept { return; }
            loop {
                cx.background_executor().timer(METER_EVERY).await;
                let going = this.update(cx, |this, cx| {
                    let now = Instant::now();
                    let Some(setup) = &mut this.setup else { return false };
                    let Some(check) = setup.audio.as_mut().filter(|check| check.started == started) else { return false };
                    let Some(capture) = &check.capture else { return false };
                    for source in Source::ALL {
                        let meter = &mut check.meters[slot(source)];
                        let level = crate::listening::level(capture.level(source));
                        // Falls a little each reading rather than flickering to the next chunk.
                        meter.level = level.max(meter.level * 0.8);
                        if level >= meter.peak || meter.peak_at.is_none_or(|at| now - at > PEAK_HOLD) {
                            meter.peak = level.max(meter.peak - 0.02).max(0.0);
                            if level >= meter.peak { meter.peak_at = Some(now); }
                        }
                        if level > HEARD { meter.last_heard = Some(now); setup.heard[slot(source)] = true; }
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

    pub(super) fn audio_check(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let devices = self.devices.clone().unwrap_or_default();
        let options = |names: &[String], default: Option<&String>| {
            let mut options = vec![(String::new(), default.map_or("Default".to_string(), |name| format!("Default · {name}")))];
            options.extend(names.iter().map(|name| (name.clone(), name.clone())));
            options
        };
        let value = |chosen: &str, default: Option<&String>| if !chosen.is_empty() { chosen.to_string() } else {
            match default { Some(name) => format!("Default · {name}"), None if self.devices_loading => "Finding devices…".into(), None => "Default".into() }
        };
        let mic = self.source_card(setup, Source::Me, s.listen_mic, self.setup_dropdown(setup, Menu::Mic, value(&s.mic_device, devices.default_microphone.as_ref()),
            options(&devices.microphones, devices.default_microphone.as_ref()), None, &s.mic_device, 36.0,
            |this, name, window, cx| this.change_audio(|s| s.mic_device = name, window, cx), cx), cx);
        let desktop = self.source_card(setup, Source::Them, s.listen_desktop, self.setup_dropdown(setup, Menu::Desktop, value(&s.desktop_device, devices.default_playback.as_ref()),
            options(&devices.playback, devices.default_playback.as_ref()), None, &s.desktop_device, 36.0,
            |this, name, window, cx| this.change_audio(|s| s.desktop_device = name, window, cx), cx), cx);
        let mut page = page().child(heading("Check what it hears", "Say something, and play a video.")).child(mic).child(desktop);
        if let Some(error) = setup.audio.as_ref().and_then(|check| check.error.clone()) {
            page = page.child(steps::status_line("icons/setup-circle-alert.svg", error, rgb(steps::ERROR)).mt(px(0.0)));
        }
        page.child(hint("icons/setup-headphones.svg", "Headphones keep you and them apart"))
    }

    /// A source: its switch, what it hears, the meter with its peak, the device, and for a silent
    /// microphone the way to its privacy settings.
    fn source_card(&self, setup: &Setup, source: Source, on: bool, picker: Div, cx: &mut Context<Self>) -> Div {
        let mic = source == Source::Me;
        let now = Instant::now();
        let check = setup.audio.as_ref();
        let meter = check.map(|check| check.meters[slot(source)]).unwrap_or_default();
        let open = check.is_some_and(|check| check.capture.is_some());
        let hearing = meter.last_heard.is_some_and(|at| now - at < Duration::from_millis(1500));
        let silent = on && open && meter.last_heard.is_none() && check.is_some_and(|check| now - check.started > SILENT_HINT_AFTER);
        let (status, color) = if !on { ("Off", theme::muted()) } else if hearing { ("Hearing sound", theme::ok()) }
            else if silent && mic { ("No sound", rgb(CAUTION)) } else if silent { ("Play something", theme::muted()) }
            else if open { ("Listening…", theme::muted()) } else { ("Opening…", theme::muted()) };
        let warn = silent && mic;
        let toggle = div().id(("setup-source", slot(source))).flex_none().cursor_pointer().child(ui::switch(on))
            .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.change_audio(move |s| if mic { s.listen_mic = !on } else { s.listen_desktop = !on }, window, cx)));
        let shown = |value: f32| if on { value.clamp(0.0, 1.0) } else { 0.0 };
        let level_bar = div().relative().h(px(10.0)).rounded(px(3.0)).bg(theme::divider()).overflow_hidden()
            .child(div().h_full().bg(theme::accent()).w(relative(shown(meter.level))))
            .when(on && meter.peak > 0.0, |bar| bar.child(div().absolute().top_0().h_full().w(px(3.0)).bg(rgb(0x8fb4ff)).left(relative(shown(meter.peak)))));
        let mut card = div().flex().flex_col().gap(px(12.0)).px(px(14.0)).pt(px(14.0)).pb(px(16.0)).rounded(px(12.0)).border_1()
            .border_color(if warn { rgb(0x5a4a24) } else { theme::hairline() })
            .child(div().flex().items_center().gap(px(10.0))
                .child(ui::icon(if mic { "icons/setup-mic.svg" } else { "icons/setup-speaker.svg" }, 16.0, theme::body()))
                .child(div().flex_1().text_size(px(13.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::text()).child(if mic { "Microphone · you" } else { "Desktop audio · them" }))
                .child(steps::status_text(status, color))
                .child(toggle))
            .child(level_bar)
            .child(picker);
        if warn {
            card = card.child(div().flex().items_center().gap(px(12.0)).px(px(12.0)).py(px(10.0)).rounded(px(10.0)).bg(rgb(0x1c1912)).border_1().border_color(rgb(0x3d3420))
                .child(ui::icon("icons/setup-circle-alert.svg", 16.0, rgb(CAUTION)))
                .child(div().flex_1().text_size(px(13.0)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::text()).child("Can't hear your microphone"))
                .child(small_button("setup-mic-privacy", "Open microphone privacy settings", false)
                    .on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| cx.open_url("ms-settings:privacy-microphone")))));
        }
        card
    }
}
