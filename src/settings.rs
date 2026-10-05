//! Non-secret preferences, persisted atomically as JSON in the user's config folder.
//! API keys never live here; they go to the OS credential store with the provider slice.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::archive::Retention;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpeechEngine { #[default] Parakeet, Whisper }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WhisperModel { Small, #[default] Turbo, Large }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AudioSource { #[default] Both, Desktop, Microphone }

/// Spoken language. `Auto` lets the engine detect it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Language { #[default] Auto, English, Spanish, French, German, Portuguese, Italian, Japanese, Chinese, Korean, Hindi }

impl Language {
    pub const ALL: [Language; 11] = [Self::Auto, Self::English, Self::Spanish, Self::French, Self::German, Self::Portuguese,
        Self::Italian, Self::Japanese, Self::Chinese, Self::Korean, Self::Hindi];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto", Self::English => "English", Self::Spanish => "Spanish", Self::French => "French",
            Self::German => "German", Self::Portuguese => "Portuguese", Self::Italian => "Italian", Self::Japanese => "Japanese",
            Self::Chinese => "Chinese", Self::Korean => "Korean", Self::Hindi => "Hindi",
        }
    }

    /// Parakeet TDT 0.6B v3 covers 25 European languages; the rest need Whisper.
    pub fn parakeet_supported(self) -> bool {
        !matches!(self, Self::Japanese | Self::Chinese | Self::Korean | Self::Hindi)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub provider: Provider,
    pub claude_model: ClaudeModel,
    /// Codex model id from the account's model list; empty uses the Codex default.
    pub codex_model: String,
    /// Preset id from `providers::PRESETS` used with "Your API key".
    pub api_provider: String,
    /// Model chosen per API provider, so switching providers keeps each choice.
    pub api_models: BTreeMap<String, String>,
    /// Base URL for the "custom" OpenAI-compatible provider.
    pub custom_base_url: String,
    pub answer_style: AnswerStyle,
    pub speech_engine: SpeechEngine,
    pub whisper_model: WhisperModel,
    pub language: Language,
    pub audio_source: AudioSource,
    pub hide_from_capture: bool,
    pub screen_on_send: bool,
    pub start_live_on_launch: bool,
    /// Keep each Live session's transcript and answers in local history.
    pub save_sessions: bool,
    /// Also keep the screenshots attached to answers.
    pub save_screenshots: bool,
    pub keep_sessions: Retention,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::default(), claude_model: ClaudeModel::default(), codex_model: String::new(),
            api_provider: "anthropic".into(), api_models: BTreeMap::new(), custom_base_url: String::new(),
            answer_style: AnswerStyle::default(), speech_engine: SpeechEngine::default(), whisper_model: WhisperModel::default(),
            language: Language::default(), audio_source: AudioSource::default(),
            hide_from_capture: true, screen_on_send: true, start_live_on_launch: false,
            save_sessions: true, save_screenshots: true, keep_sessions: Retention::default(),
        }
    }
}

impl Settings {
    /// Model for the selected API provider; empty until the user picks one.
    pub fn api_model(&self) -> &str { self.api_models.get(&self.api_provider).map(String::as_str).unwrap_or("") }

    /// The engine that will actually transcribe: Parakeet falls back to Whisper for languages it lacks.
    pub fn effective_engine(&self) -> SpeechEngine {
        if self.speech_engine == SpeechEngine::Parakeet && !self.language.parakeet_supported() { SpeechEngine::Whisper } else { self.speech_engine }
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
    fn missing_fields_take_defaults_and_unknown_languages_fall_back_to_whisper() {
        let partial: Settings = serde_json::from_str(r#"{"provider":"claude","language":"japanese"}"#).unwrap();
        assert_eq!(partial.provider, Provider::Claude);
        assert!(partial.hide_from_capture);
        assert_eq!(partial.effective_engine(), SpeechEngine::Whisper);
        let spanish = Settings { language: Language::Spanish, ..Settings::default() };
        assert_eq!(spanish.effective_engine(), SpeechEngine::Parakeet);
    }

    #[test]
    fn invalid_values_are_rejected_instead_of_silently_coerced() {
        assert!(serde_json::from_str::<Settings>(r#"{"provider":"gemini"}"#).is_err());
    }
}
