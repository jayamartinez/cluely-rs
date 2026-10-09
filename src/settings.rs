//! Non-secret preferences, persisted atomically as JSON in the user's config folder.
//! API keys never live here; they go to the OS credential store with the provider slice. Only
//! which providers have one saved, with its masked hint, is kept here (`saved_keys`).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::archive::Retention;
use crate::audio::Source;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provider { #[default] Codex, Claude, ApiKey }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaudeModel { #[default] Sonnet, Opus, Haiku }

impl ClaudeModel {
    /// Alias accepted by `claude --model`.
    pub fn id(self) -> &'static str { match self { Self::Sonnet => "sonnet", Self::Opus => "opus", Self::Haiku => "haiku" } }
}


#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnswerStyle { #[default] Spoken, Standard }

/// Which recognizer transcribes Live audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SttProvider { #[default] Parakeet, Deepgram }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub provider: Provider,
    pub claude_model: ClaudeModel,
    /// The model id Claude Code last reported for each alias ("opus" → "claude-opus-5-5"), so the
    /// pickers can show real version numbers before this run's first answer.
    pub claude_models: BTreeMap<String, String>,
    /// Codex model id from the account's model list; empty uses the Codex default.
    pub codex_model: String,
    /// Preset id from `providers::PRESETS` used with "Your API key".
    pub api_provider: String,
    /// Model chosen per API provider, so switching providers keeps each choice.
    pub api_models: BTreeMap<String, String>,
    /// Base URL for the "custom" OpenAI-compatible provider.
    pub custom_base_url: String,
    /// Providers with a key in the OS credential store, each with its masked hint ("••••3f9a"),
    /// so Settings can show the saved key without reading the store. Never the key itself.
    pub saved_keys: BTreeMap<String, String>,
    pub answer_style: AnswerStyle,
    /// Slower, deeper reasoning for answers. Off answers with the least reasoning the provider
    /// offers, so the first words arrive as fast as possible.
    pub smart_mode: bool,
    /// Start answering a question from the other side as soon as it is heard, before Assist is
    /// pressed, so the answer is ready when asked for. Each one is a real request to the
    /// selected model on the user's subscription or API key (counting toward its usage and rate
    /// limits), sent with the conversation and a screenshot, including the ones nobody asks for.
    /// Off by default.
    pub speculative_answers: bool,
    /// Show those answers as they are produced, without pressing anything. Needs `speculative_answers`.
    pub auto_answer: bool,
    /// Transcribe the Live session's audio on this PC. Off keeps Live to the screen and typed questions.
    pub transcribe: bool,
    pub stt_provider: SttProvider,
    /// macOS: run Parakeet on the GPU (Metal), which takes far less CPU. Off runs it on the CPU.
    /// `PARAKEET_DEVICE` overrides it; Windows ignores it.
    pub use_gpu: bool,
    pub listen_mic: bool,
    pub listen_desktop: bool,
    /// Device names from `audio::list_devices`; empty means the system default.
    pub mic_device: String,
    pub desktop_device: String,
    pub hide_from_capture: bool,
    pub screen_on_send: bool,
    pub start_live_on_launch: bool,
    /// macOS: show CluelyRS in the Dock and ⌘Tab. Off keeps it out, as the Windows overlay stays
    /// out of the taskbar.
    pub show_in_dock: bool,
    /// Keep each Live session's transcript and answers in local history.
    pub save_sessions: bool,
    /// Also keep the screenshots attached to answers.
    pub save_screenshots: bool,
    pub keep_sessions: Retention,
    /// The active mode's meeting context and file text (see `modes`). Not saved here: the
    /// overlay fills it from `modes.json` when it starts and whenever the mode changes.
    #[serde(skip)]
    pub mode: Option<std::sync::Arc<crate::modes::Active>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::default(), claude_model: ClaudeModel::default(), claude_models: BTreeMap::new(), codex_model: String::new(),
            api_provider: "anthropic".into(), api_models: BTreeMap::new(), custom_base_url: String::new(), saved_keys: BTreeMap::new(),
            answer_style: AnswerStyle::default(), smart_mode: false, speculative_answers: false, auto_answer: false, transcribe: true, stt_provider: SttProvider::default(),
            use_gpu: true, listen_mic: true, listen_desktop: true,
            mic_device: String::new(), desktop_device: String::new(),
            hide_from_capture: true, screen_on_send: true, start_live_on_launch: false, show_in_dock: false,
            save_sessions: true, save_screenshots: true, keep_sessions: Retention::default(), mode: None,
        }
    }
}

impl Settings {
    /// Model for the selected API provider; empty until the user picks one.
    pub fn api_model(&self) -> &str { self.api_models.get(&self.api_provider).map(String::as_str).unwrap_or("") }

    /// The sources Live captures.
    pub fn sources(&self) -> Vec<Source> {
        let mut sources = Vec::new();
        if self.listen_mic { sources.push(Source::Me); }
        if self.listen_desktop { sources.push(Source::Them); }
        sources
    }

    /// Chosen capture devices; `None` is the system default.
    pub fn devices(&self) -> crate::audio::Devices {
        let pick = |name: &str| Some(name.trim().to_string()).filter(|name| !name.is_empty());
        crate::audio::Devices { mic: pick(&self.mic_device), desktop: pick(&self.desktop_device) }
    }
}

pub struct Store {
    path: Option<PathBuf>,
    pub value: Settings,
    pub warning: Option<&'static str>,
}

impl Store {
    pub fn load() -> Self {
        let path = dirs::config_dir().map(|dir| dir.join("CluelyRS").join("settings.json"));
        let mut store = Self { path, value: Settings::default(), warning: None };
        if let Some(path) = &store.path && path.exists() {
            match fs::read(path).ok().filter(|bytes| bytes.len() <= 65536).and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
                Some(value) => store.value = value,
                None => store.warning = Some("Saved settings could not be read. Defaults are in use."),
            }
        }
        store
    }

    /// Write to a temporary file, then rename over the old one so a crash never leaves half a file.
    pub fn save(&mut self) {
        let Some(path) = &self.path else { return };
        let result = (|| -> std::io::Result<()> {
            fs::create_dir_all(path.parent().expect("settings path has a parent"))?;
            let temporary = path.with_extension("json.tmp");
            let mut file = fs::File::create(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(&self.value).expect("settings serialize"))?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        self.warning = result.is_err().then_some("Settings could not be saved. Check the app data folder permissions.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_keys_are_markers_with_hints_only() {
        let settings = Settings { saved_keys: [("anthropic".to_string(), "••••3f9a".to_string())].into(), ..Settings::default() };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""savedKeys":{"anthropic":"••••3f9a"}"#));
        let older: Settings = serde_json::from_str(r#"{"provider":"apiKey"}"#).unwrap();
        assert!(older.saved_keys.is_empty(), "settings saved before markers existed have none");
    }

    #[test]
    fn missing_fields_take_defaults_and_retired_fields_are_ignored() {
        // "language" and "speechEngine" were written by earlier builds and no longer exist.
        let partial: Settings = serde_json::from_str(r#"{"provider":"claude","language":"japanese","speechEngine":"whisper"}"#).unwrap();
        assert_eq!(partial.provider, Provider::Claude);
        assert!(partial.hide_from_capture);
        assert!(!partial.smart_mode, "answers default to the fastest reasoning");
        assert!(!partial.speculative_answers && !partial.auto_answer, "speculative answers are opt-in");
        assert!(partial.transcribe);
        assert!(partial.use_gpu, "the GPU is on unless turned off");
        assert_eq!(partial.sources(), Source::ALL);
        assert_eq!(Settings { listen_mic: false, ..Settings::default() }.sources(), [Source::Them]);
        assert_eq!(partial.devices(), crate::audio::Devices::default());
        let picked = Settings { mic_device: " USB Audio CODEC ".into(), ..Settings::default() };
        assert_eq!(picked.devices().mic.as_deref(), Some("USB Audio CODEC"));
    }

    #[test]
    fn invalid_values_are_rejected_instead_of_silently_coerced() {
        assert!(serde_json::from_str::<Settings>(r#"{"provider":"gemini"}"#).is_err());
    }
}
