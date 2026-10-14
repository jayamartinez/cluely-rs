//! Non-secret preferences, persisted atomically as JSON in the user's config folder.
//! API keys never live here; they go to the OS credential store with the provider slice. Only
//! which providers have one saved, with its masked hint, is kept here (`saved_keys`).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::appearance::{self, CardWidth, TextSize};
use crate::archive::Retention;
use crate::audio::Source;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provider { #[default] Codex, Claude, ApiKey }

/// The Claude Code model, as `claude --model` takes it: an alias that follows the newest version
/// ("sonnet", "opus", "haiku", "fable") or a full model id picked under Legacy models
/// ("claude-opus-4-6"). Saved as that text; anything that isn't shaped like a model id is rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ClaudeModel(String);

impl ClaudeModel {
    /// The aliases offered before Claude Code's model list is known (and on Windows).
    pub const ALIASES: [(&str, &str); 3] = [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")];

    pub fn new(id: &str) -> Option<Self> { crate::claude_cli::plausible_model_id(id).then(|| Self(id.to_string())) }

    pub fn id(&self) -> &str { &self.0 }
}

impl Default for ClaudeModel {
    fn default() -> Self { Self("sonnet".into()) }
}

impl TryFrom<String> for ClaudeModel {
    type Error = &'static str;
    fn try_from(id: String) -> Result<Self, Self::Error> { Self::new(&id).ok_or("not a Claude model id") }
}

impl From<ClaudeModel> for String {
    fn from(model: ClaudeModel) -> Self { model.0 }
}


#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnswerStyle { #[default] Spoken, Standard }

/// The steps of the macOS first-run setup (`onboarding_window`), saved while it's open so a relaunch
/// resumes there. Every step is listed so a newer build's step still reads; `onboarding_window::STEPS`
/// says which ones are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OnboardingStep { Welcome, Model, Transcription, Permissions, Privacy, Shortcuts, Done }

/// The setup a finished `onboarding_version` refers to; a later one can be shown to users who saw an older one.
pub const ONBOARDING_VERSION: u32 = 1;

/// Which recognizer transcribes Live audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SttProvider { #[default] Parakeet, Deepgram }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub provider: Provider,
    pub claude_model: ClaudeModel,
    /// A Claude model chosen by its id instead of an alias (Fable, or a legacy version from the
    /// model picker); empty uses `claude_model`.
    pub claude_model_id: String,
    /// The model id Claude Code last reported for each alias ("opus" → "claude-opus-5-5"), so the
    /// pickers can show real version numbers before this run's first answer.
    pub claude_models: BTreeMap<String, String>,
    /// Codex model id from the account's model list; empty uses the Codex default.
    pub codex_model: String,
    /// The Codex and Claude Code executables the user located; empty finds them automatically
    /// (see `cli_path`). A location that is no longer usable is skipped.
    pub codex_path: String,
    pub claude_path: String,
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
    /// Show the menu bar icon (macOS) or tray icon (Windows) and its menu (Live, modes, Settings,
    /// Quit). On by default, so settings saved before it existed keep the icon.
    pub show_menu_bar_icon: bool,
    /// Settings › Window › Appearance (`appearance`): the card's background opacity in percent, the
    /// size of its reading text and its width.
    pub background_opacity: u8,
    pub text_size: TextSize,
    pub card_width: CardWidth,
    /// Keep each Live session's transcript and answers in local history.
    pub save_sessions: bool,
    /// Also keep the screenshots attached to answers.
    pub save_screenshots: bool,
    pub keep_sessions: Retention,
    /// Check GitHub for a newer version at launch and every 6 hours (Settings › About).
    pub check_for_updates: bool,
    /// When updates were last checked, in Unix seconds.
    pub last_update_check: Option<u64>,
    /// The version whose notice was closed in the overlay; it shows again only for a newer one.
    pub dismissed_update: String,
    /// Windows: first-run setup was finished or skipped. A fresh install starts without it, but
    /// settings saved before setup existed read as done, so people who already use CluelyRS never see it.
    #[serde(default = "setup_done_when_missing")]
    pub setup_done: bool,
    /// macOS: the first-run setup last finished (closed or skipped counts); 0 before it ever was.
    pub onboarding_version: u32,
    /// macOS: the setup step open when CluelyRS last ran; `Some` means setup is unfinished and shows again.
    pub onboarding_step: Option<OnboardingStep>,
    /// The active mode's meeting context and file text (see `modes`). Not saved here: the
    /// overlay fills it from `modes.json` when it starts and whenever the mode changes.
    #[serde(skip)]
    pub mode: Option<std::sync::Arc<crate::modes::Active>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::default(), claude_model: ClaudeModel::default(), claude_model_id: String::new(), claude_models: BTreeMap::new(), codex_model: String::new(),
            codex_path: String::new(), claude_path: String::new(),
            api_provider: "anthropic".into(), api_models: BTreeMap::new(), custom_base_url: String::new(), saved_keys: BTreeMap::new(),
            answer_style: AnswerStyle::default(), smart_mode: false, speculative_answers: false, auto_answer: false, transcribe: true, stt_provider: SttProvider::default(),
            use_gpu: true, listen_mic: true, listen_desktop: true,
            background_opacity: appearance::DEFAULT_OPACITY, text_size: TextSize::default(), card_width: CardWidth::default(),
            mic_device: String::new(), desktop_device: String::new(),
            hide_from_capture: true, screen_on_send: true, start_live_on_launch: false, show_in_dock: false, show_menu_bar_icon: true,
            save_sessions: true, save_screenshots: true, keep_sessions: Retention::default(),
            check_for_updates: true, last_update_check: None, dismissed_update: String::new(), setup_done: false,
            onboarding_version: 0, onboarding_step: None, mode: None,
        }
    }
}

fn setup_done_when_missing() -> bool { true }

impl Settings {
    /// Providers that can have a saved key but have no marker: keys saved before markers existed
    /// are looked for among these (`secrets::find_marker`).
    pub fn unmarked_key_providers(&self) -> Vec<&'static str> {
        crate::providers::PRESETS.iter()
            .filter(|preset| preset.needs_key || preset.base_url.is_empty())
            .map(|preset| preset.id)
            .chain([crate::stt::deepgram::PROVIDER_ID])
            .filter(|id| !self.saved_keys.contains_key(*id))
            .collect()
    }

    /// Mark the keys found by that search, leaving markers set meanwhile alone. Whether any was added.
    pub fn mark_found_keys(&mut self, found: Vec<(String, String)>) -> bool {
        let mut added = false;
        for (provider, hint) in found {
            if let std::collections::btree_map::Entry::Vacant(entry) = self.saved_keys.entry(provider) {
                entry.insert(hint);
                added = true;
            }
        }
        added
    }

    /// Model for the selected API provider; empty until the user picks one.
    pub fn api_model(&self) -> &str { self.api_models.get(&self.api_provider).map(String::as_str).unwrap_or("") }

    /// What `claude --model` gets: the chosen id, or else the alias.
    pub fn claude_model_arg(&self) -> &str { if self.claude_model_id.is_empty() { self.claude_model.id() } else { &self.claude_model_id } }

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
    /// Whether settings.json was there at launch, before anything saved this run (keys are marked and
    /// updates checked within seconds), so a first run is told apart from a returning user.
    pub existed: bool,
}

impl Store {
    /// `CLUELYRS_SETTINGS` names another settings file (development: a first run without touching
    /// the real settings).
    pub fn load() -> Self {
        let path = std::env::var_os("CLUELYRS_SETTINGS").map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|dir| dir.join("CluelyRS").join("settings.json")));
        Self::load_from(path)
    }

    fn load_from(path: Option<PathBuf>) -> Self {
        let existed = path.as_ref().is_some_and(|path| path.exists());
        let mut store = Self { path, value: Settings::default(), warning: None, existed };
        if let Some(path) = &store.path && existed {
            match fs::read(path).ok().filter(|bytes| bytes.len() <= 65536).and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
                Some(value) => store.value = value,
                None => store.warning = Some("Saved settings could not be read. Defaults are in use."),
            }
        }
        store.publish_cli_paths();
        store
    }

    /// Whether the Windows first-run setup should open: not finished or skipped yet, and the saved
    /// settings were readable (unreadable ones fall back to defaults, which must not restart setup).
    pub fn setup_needed(&self) -> bool { !self.value.setup_done && self.warning.is_none() }

    /// Whether the macOS first-run setup opens at launch: on a first run, or when the last run quit
    /// in the middle of it. Users from before it existed have settings and never see it unasked.
    pub fn onboarding_pending(&self) -> bool {
        cfg!(target_os = "macos") && (!self.existed || self.value.onboarding_step.is_some())
    }

    /// Hand the located CLIs to `cli_path`, which the CLIs resolve through.
    fn publish_cli_paths(&self) {
        crate::cli_path::set_chosen(crate::cli_path::Cli::Codex, &self.value.codex_path);
        crate::cli_path::set_chosen(crate::cli_path::Cli::Claude, &self.value.claude_path);
    }

    /// Write to a temporary file, then rename over the old one so a crash never leaves half a file.
    pub fn save(&mut self) {
        self.publish_cli_paths();
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
    fn appearance_defaults_to_the_overlay_as_it_was_and_saves_under_its_own_names() {
        let older: Settings = serde_json::from_str(r#"{"provider":"claude"}"#).unwrap();
        assert_eq!((older.background_opacity, older.text_size, older.card_width), (appearance::DEFAULT_OPACITY, TextSize::Standard, CardWidth::Standard));
        let chosen = Settings { background_opacity: 0, text_size: TextSize::Large, card_width: CardWidth::Narrow, ..Settings::default() };
        let json = serde_json::to_string(&chosen).unwrap();
        assert!(json.contains(r#""backgroundOpacity":0"#) && json.contains(r#""textSize":"large""#) && json.contains(r#""cardWidth":"narrow""#), "{json}");
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), chosen);
        assert!(serde_json::from_str::<Settings>(r#"{"cardWidth":"huge"}"#).is_err(), "unknown widths are rejected like other invalid values");
    }

    #[test]
    fn keys_saved_before_markers_are_looked_for_and_marked_once() {
        let mut settings = Settings { saved_keys: [("anthropic".to_string(), "••••3f9a".to_string())].into(), ..Settings::default() };
        let unmarked = settings.unmarked_key_providers();
        assert!(unmarked.contains(&"openai") && unmarked.contains(&"custom") && unmarked.contains(&"deepgram"));
        assert!(!unmarked.contains(&"anthropic"), "already marked");
        assert!(!unmarked.contains(&"ollama"), "local providers have no key");

        // A key marked meanwhile (saved while the search ran) keeps its own hint.
        settings.saved_keys.insert("openai".into(), "••••new1".into());
        let found = vec![("openai".to_string(), String::new()), ("deepgram".to_string(), String::new())];
        assert!(settings.mark_found_keys(found.clone()));
        assert_eq!(settings.saved_keys["openai"], "••••new1");
        assert_eq!(settings.saved_keys["deepgram"], "");
        assert!(!settings.mark_found_keys(found), "nothing new the second time");
        assert!(!settings.unmarked_key_providers().contains(&"deepgram"));
    }

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
        assert!(partial.show_menu_bar_icon && !partial.show_in_dock, "the menu bar icon shows, the Dock icon doesn't");
        let hidden = Settings { show_menu_bar_icon: false, ..Settings::default() };
        let json = serde_json::to_string(&hidden).unwrap();
        assert!(json.contains(r#""showMenuBarIcon":false"#), "{json}");
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), hidden);
        assert!(partial.codex_path.is_empty() && partial.claude_path.is_empty(), "the CLIs are found automatically");
        let located: Settings = serde_json::from_str(r#"{"codexPath":"/opt/tools/codex","claudePath":""}"#).unwrap();
        assert_eq!((located.codex_path.as_str(), located.claude_path.as_str()), ("/opt/tools/codex", ""));
        assert!(partial.check_for_updates && partial.last_update_check.is_none() && partial.dismissed_update.is_empty(), "update checks are on");
        assert_eq!(partial.sources(), Source::ALL);
        assert_eq!(Settings { listen_mic: false, ..Settings::default() }.sources(), [Source::Them]);
        assert_eq!(partial.devices(), crate::audio::Devices::default());
        let picked = Settings { mic_device: " USB Audio CODEC ".into(), ..Settings::default() };
        assert_eq!(picked.devices().mic.as_deref(), Some("USB Audio CODEC"));
    }

    #[test]
    fn update_check_state_is_saved_under_its_own_names() {
        let settings = Settings { check_for_updates: false, last_update_check: Some(1_791_500_000), dismissed_update: "0.2.0".into(), ..Settings::default() };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""checkForUpdates":false"#) && json.contains(r#""lastUpdateCheck":1791500000"#) && json.contains(r#""dismissedUpdate":"0.2.0""#), "{json}");
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), settings);
    }

    #[test]
    fn setup_opens_on_a_fresh_install_only() {
        let dir = std::env::temp_dir().join(format!("cluelyrs-setup-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let _ = fs::remove_file(&path);
        assert!(Store::load_from(Some(path.clone())).setup_needed(), "no settings file: a fresh install");

        fs::write(&path, r#"{"provider":"claude"}"#).unwrap();
        let existing = Store::load_from(Some(path.clone()));
        assert!(existing.value.setup_done && !existing.setup_needed(), "settings saved before setup existed");

        fs::write(&path, "{ not json").unwrap();
        let unreadable = Store::load_from(Some(path.clone()));
        assert!(unreadable.warning.is_some() && !unreadable.setup_needed(), "unreadable settings never restart setup");

        // Quitting midway leaves it unfinished; finishing or skipping saves it as done.
        let mut store = Store::load_from(Some(path.clone()));
        store.value = Settings::default();
        store.save();
        assert!(Store::load_from(Some(path.clone())).setup_needed());
        store.value.setup_done = true;
        store.save();
        assert!(!Store::load_from(Some(path.clone())).setup_needed());
        assert!(fs::read_to_string(&path).unwrap().contains(r#""setupDone": true"#));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn onboarding_state_defaults_to_never_run_and_saves_under_its_own_names() {
        let older: Settings = serde_json::from_str(r#"{"provider":"claude"}"#).unwrap();
        assert_eq!((older.onboarding_version, older.onboarding_step), (0, None));
        let midway = Settings { onboarding_step: Some(OnboardingStep::Transcription), ..Settings::default() };
        let json = serde_json::to_string(&midway).unwrap();
        assert!(json.contains(r#""onboardingVersion":0"#) && json.contains(r#""onboardingStep":"transcription""#), "{json}");
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), midway);
    }

    #[test]
    fn existed_is_captured_at_load_and_decides_onboarding() {
        let dir = std::env::temp_dir().join(format!("cluelyrs-settings-existed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        let mut first = Store::load_from(Some(path.clone()));
        assert!(!first.existed);
        first.save();
        assert!(!first.existed, "saving during the run doesn't make it a returning user");
        assert_eq!(first.onboarding_pending(), cfg!(target_os = "macos"));

        // Setup quit midway resumes; finished setup and users from before it never see it again.
        let mut returning = Store::load_from(Some(path.clone()));
        assert!(returning.existed && !returning.onboarding_pending(), "an existing user");
        returning.value.onboarding_step = Some(OnboardingStep::Model);
        returning.save();
        assert_eq!(Store::load_from(Some(path.clone())).onboarding_pending(), cfg!(target_os = "macos"));
        fs::write(&path, b"not json").unwrap();
        let unreadable = Store::load_from(Some(path));
        assert!(unreadable.existed && unreadable.warning.is_some() && !unreadable.onboarding_pending(), "unreadable settings aren't a first run");
        assert!(!Store::load_from(None).existed);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Saved aliases from before Legacy models still load; a full id saves as itself.
    #[test]
    fn claude_model_takes_aliases_and_full_ids() {
        let older: Settings = serde_json::from_str(r#"{"claudeModel":"opus"}"#).unwrap();
        assert_eq!(older.claude_model.id(), "opus");
        assert_eq!(Settings::default().claude_model.id(), "sonnet");
        let legacy = Settings { claude_model: ClaudeModel::new("claude-opus-4-6").unwrap(), ..Settings::default() };
        let json = serde_json::to_string(&legacy).unwrap();
        assert!(json.contains(r#""claudeModel":"claude-opus-4-6""#), "{json}");
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), legacy);
        assert!(ClaudeModel::new("--dangerously-skip").is_none());
    }

    #[test]
    fn invalid_values_are_rejected_instead_of_silently_coerced() {
        assert!(serde_json::from_str::<Settings>(r#"{"provider":"gemini"}"#).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"claudeModel":"--help"}"#).is_err());
    }
}
