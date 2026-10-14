//! Setup › Listening: both sources captured as Live would, each with a meter and its device (and a
//! way to Windows' microphone privacy settings when the microphone stays silent), then the speech
//! model: Parakeet on this PC (downloaded only when chosen) or Deepgram with its key.

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Div, FontWeight, IntoElement, MouseButton, ParentElement, Styled, Window, div, prelude::*, px};

use super::steps::text;
use super::{Menu, Setup, Step};
use crate::audio::capture::AudioCapture;
use crate::audio::frame::Source;
use crate::hotkeys::Action;
use crate::overlay::Overlay;
use crate::settings::{Settings, SttProvider};
use crate::settings_view::listen;
use crate::stt::{deepgram, parakeet::MODEL};
use crate::transcript_view::model_size_label;
use crate::ui_v3::{self, Kind, RADII};
use crate::{theme, ui};

/// How often the meters read the capture's levels.
const METER_EVERY: Duration = Duration::from_millis(50);
/// How long a silent microphone waits before the way to its privacy settings shows.
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

/// How long "heard" stays up after the last sound.
const HEARD_FOR: Duration = Duration::from_millis(1500);

fn slot(source: Source) -> usize { match source { Source::Me => 0, Source::Them => 1 } }

/// "102 of 176 MB" for a download `progress` of the speech model.
fn megabytes(progress: f64) -> String {
    let total = MODEL.bytes as f64 / 1e6;
    format!("{:.0} of {total:.0} MB", total * progress.clamp(0.0, 1.0))
}

impl Overlay {
    /// Listening's choice of speech model. On this PC starts the download if the model isn't here
    /// yet: it only ever starts from this click.
    fn choose_speech(&mut self, provider: SttProvider, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &mut self.setup else { return };
        if setup.speech != Some(provider) {
            setup.speech = Some(provider);
            setup.replacing_key = false;
            self.key_input.update(cx, |input, cx| input.clear(cx));
            self.key_notice = None;
        }
        self.update_settings(|s| { s.transcribe = true; s.stt_provider = provider; }, window, cx);
        if provider == SttProvider::Parakeet && !self.model_installed && self.download_progress().is_none() { self.download_model(window, cx); }
        cx.notify();
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
                        if level > HEARD { meter.last_heard = Some(now); }
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

    /// Each source with its level, then the speech model to use. Start waits until one is chosen
    /// (and, on this PC, until its download has started).
    pub(super) fn listening(&self, setup: &Setup, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let devices = self.devices.clone().unwrap_or_default();
        let options = |names: &[String], default: Option<&String>| {
            let mut options = vec![(String::new(), default.map_or("Default".to_string(), |name| format!("Default · {name}")))];
            options.extend(names.iter().map(|name| (name.clone(), name.clone())));
            options
        };
        let value = |chosen: &str, default: Option<&String>| if !chosen.is_empty() { chosen.to_string() } else {
            match default { Some(name) => name.clone(), None if self.devices_loading => "Finding devices…".into(), None => "Default".into() }
        };
        let mic_face = ui_v3::device_face(("setup-device", 0usize), value(&s.mic_device, devices.default_microphone.as_ref()));
        let mic_picker = self.setup_dropdown(setup, Menu::Mic, mic_face, 240.0, 22.0, options(&devices.microphones, devices.default_microphone.as_ref()), None,
            &s.mic_device, |this, name, window, cx| this.change_audio(|s| s.mic_device = name, window, cx), cx);
        let desktop_face = ui_v3::device_face(("setup-device", 1usize), value(&s.desktop_device, devices.default_playback.as_ref()));
        let desktop_picker = self.setup_dropdown(setup, Menu::Desktop, desktop_face, 240.0, 22.0, options(&devices.playback, devices.default_playback.as_ref()), None,
            &s.desktop_device, |this, name, window, cx| this.change_audio(|s| s.desktop_device = name, window, cx), cx);
        let mut rows = vec![self.source_row(setup, Source::Me, mic_picker.into_any_element())];
        if self.mic_silent(setup) {
            rows.push(div().flex().items_center().gap(px(10.0)).pl(px(16.0)).pr(px(12.0)).py(px(10.0)).bg(theme::field())
                .child(text(12.0, 16.0, theme::text(), "Windows may be blocking the microphone").font_weight(FontWeight::SEMIBOLD).flex_1())
                .child(div().id("setup-mic-privacy").h(px(28.0)).flex().items_center().gap(px(6.0)).px(px(10.0)).rounded(px(RADII.control)).cursor_pointer()
                    .bg(theme::raised()).border_1().border_color(theme::keycap_border())
                    .on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| cx.open_url("ms-settings:privacy-microphone")))
                    .child(text(12.0, 16.0, theme::text(), "Privacy settings"))
                    .child(ui::icon("icons/setup-external.svg", 10.0, theme::body())))
                .into_any_element());
        }
        rows.push(self.source_row(setup, Source::Them, desktop_picker.into_any_element()));
        let choice = setup.speech;
        let size = model_size_label(&MODEL);
        let on_pc: AnyElement = match self.download_progress() {
            Some(progress) => ui_v3::download_progress(progress as f32, megabytes(progress)).into_any_element(),
            None if self.model_installed => ui_v3::done_note("Downloaded").into_any_element(),
            None => ui_v3::card_button("setup-download", "icons/setup-download.svg", "Download").into_any_element(),
        };
        let deepgram_saved = s.saved_keys.get(deepgram::PROVIDER_ID).map(String::as_str);
        let deepgram: AnyElement = if choice == Some(SttProvider::Deepgram) {
            self.key_field(setup, deepgram_saved, false, 30.0, cx).into_any_element()
        } else if deepgram_saved.is_some() {
            ui_v3::done_note("Key saved").into_any_element()
        } else {
            ui_v3::card_button("setup-add-key", "icons/setup-key.svg", "Add API key").into_any_element()
        };
        let cards = div().flex().gap(px(10.0))
            .child(ui_v3::speech_card("setup-speech-pc", "icons/setup-monitor.svg", 18.0, "On this PC", format!("Parakeet · private · {size}"),
                choice == Some(SttProvider::Parakeet), on_pc)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.choose_speech(SttProvider::Parakeet, window, cx))))
            .child(ui_v3::speech_card("setup-speech-deepgram", "logos/deepgram.svg", 16.0, "Deepgram", "Cloud · your API key",
                choice == Some(SttProvider::Deepgram), deepgram)
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.choose_speech(SttProvider::Deepgram, window, cx))));
        let ready = match choice { Some(SttProvider::Parakeet) => self.model_installed || self.download_progress().is_some(), Some(SttProvider::Deepgram) => true, None => false };
        let hints = [(Action::Assist, "Assist"), (Action::Focus, "Type"), (Action::Toggle, "Show / hide")].map(|(action, what)| (self.hotkeys.label(action), what));
        let notice = match choice {
            Some(SttProvider::Parakeet) => self.model_notice.clone(),
            Some(SttProvider::Deepgram) => self.key_notice.clone(),
            None => None,
        };
        let error = setup.audio.as_ref().and_then(|check| check.error.clone());
        ui_v3::setup_page(div().gap(px(22.0)).child(ui_v3::heading("Check what it hears", "Say something, play a video, then pick a speech model."))
                .child(div().w(px(ui_v3::SETUP_COLUMN)).flex().flex_col().gap(px(12.0))
                    .child(ui_v3::rows_box(rows))
                    .child(cards)
                    .children(error.map(|error| text(12.0, 16.0, theme::danger(), error).px(px(2.0))))
                    .children(notice.map(|notice| text(12.0, 16.0, theme::accent_soft(), notice).px(px(2.0))))),
            div().child(self.next_button(Step::Transcription, "Start CluelyRS", if ready { Kind::Primary } else { Kind::Disabled }, cx))
                .child(ui_v3::shortcut_hints(&hints)),
            true)
    }

    /// Whether the microphone has been on and open for a while without a sound.
    fn mic_silent(&self, setup: &Setup) -> bool {
        let Some(check) = setup.audio.as_ref() else { return false };
        self.store.value.listen_mic && check.capture.is_some() && check.meters[slot(Source::Me)].last_heard.is_none() && check.started.elapsed() > SILENT_HINT_AFTER
    }

    /// A source's row: its level, its device, and a check while it hears something (an alert while
    /// the microphone stays silent).
    fn source_row(&self, setup: &Setup, source: Source, picker: AnyElement) -> AnyElement {
        let mic = source == Source::Me;
        let on = if mic { self.store.value.listen_mic } else { self.store.value.listen_desktop };
        let meter = setup.audio.as_ref().map(|check| check.meters[slot(source)]).unwrap_or_default();
        let hearing = on && meter.last_heard.is_some_and(|at| at.elapsed() < HEARD_FOR);
        let status = if hearing { Some(ui::icon("icons/setup-circle-check.svg", 16.0, theme::ok()).into_any_element()) }
            else if mic && self.mic_silent(setup) { Some(ui::icon("icons/setup-circle-alert.svg", 16.0, theme::text()).into_any_element()) }
            else { None };
        let (icon, who) = if mic { ("icons/setup-mic.svg", "You") } else { ("icons/setup-speaker.svg", "Them") };
        ui_v3::meter_row(icon, who, picker, if on { meter.level } else { 0.0 }, status).into_any_element()
    }
}
