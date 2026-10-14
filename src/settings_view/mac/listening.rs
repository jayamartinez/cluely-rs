//! Settings › Listening: whether conversations are transcribed and by what (Parakeet on this Mac,
//! or Deepgram with its key), then each audio source with its level, device and switch.

use gpui::{AnyElement, Context, Div, FontWeight, IntoElement, MouseButton, ParentElement, Styled, div, prelude::*, px};

use super::{action, label, link, row, section, status, switch, text_field};
use crate::overlay::Overlay;
use crate::settings::{Settings, SttProvider};
use crate::settings_view::{Picker, listen, saved_key_label};
use crate::stt::{deepgram, parakeet};
use crate::transcript_view::model_size_label;
use crate::ui_v3::{self, Kind, RADII};
use crate::{theme, ui};

/// A device picker's face, options (id, label), choice and setter.
pub(in crate::settings_view) type DeviceChoice = (String, Vec<(String, String)>, String, fn(&mut Settings, String));

impl Overlay {
    pub(super) fn mac_listening_tab(&self, levels: [f32; 2], cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let mut transcription: Vec<AnyElement> = vec![
            row("icons/setup-wave.svg", "Transcribe conversations", None, switch("transcribe", s.transcribe, |s, v| s.transcribe = v, cx)).into_any_element(),
        ];
        if s.transcribe {
            transcription.push(self.speech_panel(cx).into_any_element());
            if s.stt_provider == SttProvider::Parakeet {
                let fell_back = s.use_gpu && parakeet::gpu_unavailable();
                transcription.push(row("icons/setup-chip.svg", "Use the GPU (Metal)", fell_back.then(|| status("Couldn't use the GPU, so it runs on the CPU")),
                    switch("use-gpu", s.use_gpu, |s, v| s.use_gpu = v, cx)).into_any_element());
            }
        }
        let sources = vec![
            self.source_row(Picker::Mic, levels[0], cx).into_any_element(),
            self.source_row(Picker::Desktop, levels[1], cx).into_any_element(),
        ];
        div().flex().flex_col().gap(px(30.0))
            .child(section("Transcription", 14.0, transcription))
            .child(section("Audio sources", 14.0, sources))
    }

    /// What transcribes: the speech model (or Deepgram and its key), and a switch between the two.
    fn speech_panel(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let parakeet = s.stt_provider == SttProvider::Parakeet;
        let model = parakeet::MODEL;
        let (title, detail, trailing): (&str, String, Option<AnyElement>) = if parakeet {
            let trailing = if let Some(progress) = self.download_progress() {
                div().flex().items_center().gap(px(10.0))
                    .child(div().w(px(120.0)).flex().child(ui_v3::progress_bar(progress as f32)))
                    .child(div().w(px(34.0)).text_size(px(12.0)).text_color(theme::muted()).child(format!("{:.0}%", progress * 100.0)))
                    .child(action("cancel-download", "Cancel", Kind::Secondary, cx, |this, _, cx| { this.cancel_download(); cx.notify(); }))
                    .into_any_element()
            } else if self.model_installed {
                div().flex().flex_none().items_center().gap(px(6.0))
                    .child(div().size(px(7.0)).rounded(px(2.0)).bg(theme::ok()))
                    .child(div().text_size(px(12.0)).line_height(px(16.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::ok()).child("Installed"))
                    .into_any_element()
            } else {
                action("download-model", format!("Download {}", model_size_label(&model)), Kind::Primary, cx, |this, window, cx| this.download_model(window, cx)).into_any_element()
            };
            ("Parakeet Realtime · English", format!("{} · on this Mac", model_size_label(&model)), Some(trailing))
        } else {
            let saved = s.saved_keys.get(deepgram::PROVIDER_ID);
            ("Deepgram", if saved.is_some() { saved_key_label(saved.map(String::as_str)) } else { "Cloud · your API key".into() }, None)
        };
        let mut panel = div().flex().flex_col().gap(px(12.0)).px(px(16.0)).py(px(14.0)).rounded(px(RADII.card)).bg(theme::title_bar()).border_1().border_color(theme::hairline())
            .child(div().flex().items_center().gap(px(12.0)).child(label(title, Some(status(detail)))).children(trailing));
        if parakeet {
            if let Some(notice) = self.model_notice.clone() {
                panel = panel.child(div().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::accent_soft()).child(notice));
            }
        } else {
            panel = panel.child(self.deepgram_key(cx));
        }
        let segment = |index: usize, icon: &'static str, text: &'static str, chosen: bool, provider: SttProvider, cx: &mut Context<Self>|
            ui_v3::segment(("speech", index), text, chosen).flex_1().py(px(6.0))
                .child(ui::icon(icon, 15.0, if chosen { theme::text() } else { theme::muted() }))
                // The icon goes before the label: `segment` added the label first.
                .flex_row_reverse()
                .on_mouse_down(MouseButton::Left, listen(cx, move |this, _, window, cx| this.update_settings(|s| s.stt_provider = provider, window, cx)));
        panel.child(ui_v3::segmented().w_full().gap(px(6.0))
            .child(segment(0, "icons/laptop-minimal.svg", "On this Mac", parakeet, SttProvider::Parakeet, cx))
            .child(segment(1, "icons/setup-cloud.svg", "Deepgram", !parakeet, SttProvider::Deepgram, cx)))
    }

    /// Deepgram's key: the saved one with Remove, or a field to paste one with Save.
    fn deepgram_key(&self, cx: &mut Context<Self>) -> Div {
        let saved = self.store.value.saved_keys.contains_key(deepgram::PROVIDER_ID);
        let mut key = div().flex().flex_col().gap(px(6.0));
        if saved {
            key = key.child(div().flex().items_center().gap(px(12.0))
                .child(div().flex_1().text_size(px(12.0)).line_height(px(16.0)).text_color(theme::muted()).child("In your Keychain"))
                .child(link("remove-key", "Remove", false).text_color(theme::danger())
                    .on_mouse_down(MouseButton::Left, listen(cx, |this, _, window, cx| this.remove_key(window, cx)))));
        } else {
            key = key.child(div().flex().items_center().gap(px(8.0))
                .child(text_field(&self.key_input, "key-box"))
                .child(action("save-key", "Save", Kind::Primary, cx, |this, window, cx| this.save_key(window, cx)))
                .child(link("key-page", "Get a key ↗", true).on_mouse_down(MouseButton::Left, listen(cx, |_, _, _, cx| cx.open_url("https://console.deepgram.com/")))));
        }
        key.children(self.key_notice.clone().map(|notice| div().text_size(px(12.0)).text_color(theme::accent_soft()).child(notice)))
    }

    /// An audio source: its level under its name, its device and its switch.
    fn source_row(&self, picker: Picker, level: f32, cx: &mut Context<Self>) -> Div {
        let s = &self.store.value;
        let desktop = picker == Picker::Desktop;
        let (icon, title, on, name, set): (_, _, _, _, fn(&mut Settings, bool)) = if desktop {
            ("icons/setup-speaker.svg", "Desktop audio", s.listen_desktop, "listen-desktop", |s, v| s.listen_desktop = v)
        } else {
            ("icons/setup-mic.svg", "Microphone", s.listen_mic, "listen-mic", |s, v| s.listen_mic = v)
        };
        let (value, options, chosen, set_device) = self.device_choice(picker);
        div().flex().items_center().gap(px(14.0)).px(px(12.0)).py(px(8.0)).border_1().border_color(gpui::transparent_black())
            .child(ui_v3::tile(40.0, false).child(ui::icon(icon, 20.0, theme::body())))
            .child(div().flex().flex_col().gap(px(6.0)).flex_1().min_w_0()
                .child(div().text_size(px(13.0)).line_height(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(title))
                .child(ui_v3::meter(if on { level } else { 0.0 }, None)))
            .child(self.mac_dropdown(picker, None, value, options, &chosen, set_device, cx))
            .child(switch(name, on, set, cx))
    }

    /// A device picker's face, options (the system default first), choice and setter.
    pub(in crate::settings_view) fn device_choice(&self, picker: Picker) -> DeviceChoice {
        let s = &self.store.value;
        let devices = self.devices.clone().unwrap_or_default();
        let desktop = picker == Picker::Desktop;
        let (chosen, names, default) = if desktop { (&s.desktop_device, &devices.playback, devices.default_playback.as_ref()) }
            else { (&s.mic_device, &devices.microphones, devices.default_microphone.as_ref()) };
        let set: fn(&mut Settings, String) = if desktop { |s, v| s.desktop_device = v } else { |s, v| s.mic_device = v };
        // macOS captures all system audio rather than a device, so that option reads plainly.
        let default_label = match default {
            Some(name) if name == crate::audio::capture::SYSTEM_AUDIO => name.clone(),
            Some(name) => format!("Default · {name}"),
            None => "Default".to_string(),
        };
        let value = if !chosen.is_empty() { chosen.clone() } else if default.is_some() { default_label.clone() }
            else if self.devices_loading { "Loading devices…".into() } else { "Default".into() };
        let mut options = vec![(String::new(), default_label)];
        options.extend(names.iter().map(|name| (name.clone(), name.clone())));
        (value, options, chosen.clone(), set)
    }
}
