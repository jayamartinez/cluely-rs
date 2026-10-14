//! Settings › Listening (Paper "Settings · Listening"): transcription and its engine (an expander
//! with Parakeet on this PC or Deepgram, the download or the key), then each audio source with its
//! device, its switch and a live 14-segment meter. The meters capture on their own while the page is
//! open, as setup's audio check does, so they move without Live.

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Div, IntoElement, MouseButton, ParentElement, Styled, Window, div, prelude::*, px};

use super::{button, card, card_icon, card_label, chevron, dot_status, expander, expander_header, expander_row, section, status, switch, text_box, title};
use crate::audio::capture::AudioCapture;
use crate::audio::frame::Source;
use crate::overlay::Overlay;
use crate::settings::{Settings, SttProvider};
use crate::settings_view::{Picker, Tab, listen, saved_key_label};
use crate::stt::{deepgram, parakeet::MODEL};
use crate::theme;
use crate::transcript_view::model_size_label;

/// How often the meters read the capture's levels.
const METER_EVERY: Duration = Duration::from_millis(50);
const SEGMENTS: usize = 14;

/// The capture behind the meters, what it was started with, and the levels shown (Me, Them).
pub(crate) struct Meters {
    started: Instant,
    config: (bool, bool, String, String),
    capture: Option<AudioCapture>,
    levels: [f32; 2],
}

fn meter_config(s: &Settings) -> (bool, bool, String, String) { (s.listen_mic, s.listen_desktop, s.mic_device.clone(), s.desktop_device.clone()) }

/// 14 squares of 6 px, 3 apart: lit #7DD3A0 up to the level, the rest #2C2F33.
fn meter(level: f32) -> Div {
    let lit = (level.clamp(0.0, 1.0) * SEGMENTS as f32).round() as usize;
    div().flex().gap(px(3.0)).children((0..SEGMENTS).map(|index| div().size(px(6.0)).rounded(px(1.5)).bg(if index < lit { theme::ok() } else { theme::hairline() })))
}

/// A radio button: 20 px, filled accent with a dot when selected, an outline otherwise.
fn radio(selected: bool) -> Div {
    let ring = div().size(px(20.0)).flex_none().flex().items_center().justify_center().rounded(px(10.0));
    if selected { ring.bg(theme::accent()).child(div().size(px(8.0)).rounded(px(4.0)).bg(theme::accent_ink())) }
    else { ring.border_1().border_color(theme::muted()) }
}

impl Overlay {
    pub(super) fn fluent_listening(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let transcription = vec![
            self.toggle_card("transcribe", "icons/setup-wave.svg", "Transcribe conversations", s.transcribe, |s, v| s.transcribe = v, cx),
            self.fluent_engine_card(window, cx),
        ];
        let sources = vec![
            self.fluent_source_card(Source::Me, cx),
            self.fluent_source_card(Source::Them, cx),
        ];
        div().flex().flex_col().gap(px(24.0))
            .child(title("Listening"))
            .child(section("Transcription", transcription))
            .child(section("Audio sources", sources))
    }

    /// The engine: Parakeet (installed, downloading, or Download) or Deepgram (its key), and the
    /// choice between them in the body.
    fn fluent_engine_card(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.store.value;
        let open = self.fluent_expanded("engine");
        let deepgram_saved = s.saved_keys.get(deepgram::PROVIDER_ID).cloned();
        let (icon, label, line, trailing): (&str, &str, String, AnyElement) = match s.stt_provider {
            SttProvider::Parakeet => {
                let trailing = if let Some(progress) = self.download_progress() {
                    div().flex().items_center().gap(px(10.0))
                        .child(div().w(px(120.0)).h(px(4.0)).rounded(px(2.0)).bg(theme::keycap_border())
                            .child(div().h_full().rounded(px(2.0)).bg(theme::accent()).w(px(120.0 * progress.clamp(0.0, 1.0) as f32))))
                        .child(div().w(px(36.0)).text_size(px(12.0)).text_color(theme::muted()).child(format!("{:.0}%", progress * 100.0)))
                        .child(button("cancel-download", "Cancel", false)
                            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| { cx.stop_propagation(); this.cancel_download(); cx.notify(); })))
                        .into_any_element()
                } else if self.model_installed {
                    div().flex().items_center().gap(px(6.0)).child(div().size(px(6.0)).rounded(px(3.0)).bg(theme::ok()))
                        .child(div().text_size(px(13.0)).line_height(px(16.0)).text_color(theme::ok()).child("Installed")).into_any_element()
                } else {
                    button("download-model", format!("Download {}", model_size_label(&MODEL)), true)
                        .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| { cx.stop_propagation(); this.download_model(window, cx) }))
                        .into_any_element()
                };
                ("icons/laptop-minimal.svg", "Parakeet Realtime · English", format!("{} · on this PC", model_size_label(&MODEL)), trailing)
            }
            SttProvider::Deepgram => {
                let trailing = if deepgram_saved.is_some() { dot_status("Key saved", theme::ok()) } else { dot_status("Needs a key", theme::placeholder()) };
                ("icons/setup-cloud.svg", "Deepgram", "Cloud · sent while Live is on".to_string(), trailing.into_any_element())
            }
        };
        let notice = self.model_notice.clone().map(|notice| div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::accent_soft()).child(notice));
        let header = expander_header().id("engine-header").cursor_pointer()
            .on_mouse_down(MouseButton::Left, listen(cx, |this, _, _, cx| this.fluent_toggle("engine", cx)))
            .child(card_icon(icon))
            .child(card_label(label, Some(div().flex().flex_col().child(status(line)).children(notice).into_any_element())))
            .child(trailing)
            .child(chevron(open));
        let body = open.then(|| {
            let choice = |id: &'static str, provider: SttProvider, name: &'static str, detail: Option<&'static str>, cx: &mut Context<Self>| {
                expander_row(id).min_h(px(48.0)).cursor_pointer()
                    .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| {
                        this.key_notice = None;
                        this.update_settings(|s| s.stt_provider = provider, window, cx);
                    }))
                    .child(radio(s.stt_provider == provider))
                    .child(div().flex_1().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::text()).child(name))
                    .children(detail.map(status))
                    .into_any_element()
            };
            let mut rows = vec![
                choice("engine-local", SttProvider::Parakeet, "On this PC", None, cx),
                choice("engine-deepgram", SttProvider::Deepgram, "Deepgram", Some(if deepgram_saved.is_some() { "Cloud · key saved" } else { "Cloud · needs a key" }), cx),
            ];
            if s.stt_provider == SttProvider::Deepgram { rows.push(self.fluent_deepgram_key(deepgram_saved.as_deref(), window, cx)); }
            rows
        });
        expander("engine", header, body).into_any_element()
    }

    /// The Deepgram key: its state with Remove and Get a key, the field and Save.
    fn fluent_deepgram_key(&self, saved: Option<&str>, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let mut line = div().flex().items_center().gap(px(10.0)).text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted())
            .child(saved_key_label(saved));
        if saved.is_some() {
            line = line.child(div().id("remove-key").cursor_pointer().text_color(theme::accent_soft())
                .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.remove_key(window, cx))).child("Remove"));
        }
        line = line.child(div().id("key-page").cursor_pointer().text_color(theme::accent_soft())
            .on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| cx.open_url("https://console.deepgram.com/"))).child("Get a key ↗"));
        let notice = self.key_notice.clone().map(|notice| div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::accent_soft()).child(notice));
        expander_row("deepgram-key").py(px(10.0))
            .child(card_label("Deepgram API key", Some(div().flex().flex_col().child(line).children(notice).into_any_element())))
            .child(text_box("key-box", &self.key_input, window, cx).w(px(200.0)))
            .child(button("save-key", "Save", true).on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.save_key(window, cx))))
            .into_any_element()
    }

    /// Microphone or desktop audio: its meter under the label, the device and the switch.
    fn fluent_source_card(&self, source: Source, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.store.value;
        let desktop = source == Source::Them;
        let (id, icon, label, on, set_on, picker): (&'static str, _, _, _, fn(&mut Settings, bool), _) = if desktop {
            ("listen-desktop", "icons/setup-speaker.svg", "Desktop audio", s.listen_desktop, |s, v| s.listen_desktop = v, Picker::Desktop)
        } else {
            ("listen-mic", "icons/setup-mic.svg", "Microphone", s.listen_mic, |s, v| s.listen_mic = v, Picker::Mic)
        };
        let level = self.fluent.meters.as_ref().filter(|_| on).map_or(0.0, |meters| meters.levels[usize::from(desktop)]);
        let devices = self.devices.clone().unwrap_or_default();
        let (chosen, names, default) = if desktop { (&s.desktop_device, &devices.playback, devices.default_playback.as_ref()) }
            else { (&s.mic_device, &devices.microphones, devices.default_microphone.as_ref()) };
        let set: fn(&mut Settings, String) = if desktop { |s, v| s.desktop_device = v } else { |s, v| s.mic_device = v };
        let default_label = default.map_or_else(|| "Default".to_string(), |name| format!("Default · {name}"));
        let value = if !chosen.is_empty() { chosen.clone() } else if default.is_some() { default_label.clone() }
            else if self.devices_loading { "Loading devices…".into() } else { "Default".into() };
        let mut options = vec![(String::new(), default_label)];
        options.extend(names.iter().map(|name| (name.clone(), name.clone())));
        card(id).child(card_icon(icon))
            .child(div().flex().flex_col().gap(px(6.0)).flex_1().min_w_0()
                .child(div().text_size(px(14.0)).line_height(px(20.0)).text_color(theme::text()).child(label))
                .child(meter(level)))
            .child(self.fluent_combo(picker, None, value, options, chosen, set, 220.0, cx))
            .child(div().id((id, 1usize)).flex_none().cursor_pointer()
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.update_settings(|s| set_on(s, !on), window, cx)))
                .child(switch(on)))
            .into_any_element()
    }

    /// Start (or restart) the meters' capture of the sources that are on. It stops by itself when
    /// the page or the window closes, and restarts when a source or device changes.
    pub(crate) fn fluent_start_meters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = &self.store.value;
        let sources: Vec<Source> = [(Source::Me, s.listen_mic), (Source::Them, s.listen_desktop)].into_iter().filter(|(_, on)| *on).map(|(source, _)| source).collect();
        let started = Instant::now();
        self.fluent.meters = Some(Meters { started, config: meter_config(s), capture: None, levels: [0.0; 2] });
        let devices = s.devices();
        cx.spawn_in(window, async move |this, cx| {
            if !sources.is_empty() {
                // Opening a device can take seconds, so off the UI thread.
                let opened = cx.background_executor().spawn(async move {
                    let (sink, chunks) = std::sync::mpsc::channel();
                    // Only the levels are read: drain the audio so it doesn't pile up.
                    std::thread::spawn(move || for _ in chunks {});
                    AudioCapture::start_with(&sources, &devices, sink).map(|(capture, _)| capture).ok()
                }).await;
                let kept = this.update(cx, |this, _| {
                    let Some(meters) = this.fluent.meters.as_mut().filter(|meters| meters.started == started) else { return false };
                    meters.capture = opened;
                    true
                }).unwrap_or(false);
                if !kept { return; }
            }
            loop {
                cx.background_executor().timer(METER_EVERY).await;
                let going = this.update_in(cx, |this, window, cx| {
                    if this.fluent.meters.as_ref().is_none_or(|meters| meters.started != started) { return false; }
                    if this.fluent.tab != Some(Tab::Listening) || this.fluent.mode_page.is_some() {
                        this.fluent.meters = None;
                        return false;
                    }
                    if this.fluent.meters.as_ref().is_some_and(|meters| meters.config != meter_config(&this.store.value)) {
                        this.fluent_start_meters(window, cx);
                        return false;
                    }
                    let Some(meters) = this.fluent.meters.as_mut() else { return false };
                    let Some(capture) = &meters.capture else { return true };
                    for (slot, source) in [Source::Me, Source::Them].into_iter().enumerate() {
                        let level = crate::listening::level(capture.level(source));
                        // Falls a little each reading rather than flickering to the next chunk.
                        meters.levels[slot] = level.max(meters.levels[slot] * 0.8);
                    }
                    cx.notify();
                    true
                }).unwrap_or(false);
                if !going { break; }
            }
        }).detach();
    }
}
