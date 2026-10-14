//! Types shared by the subscription providers (Codex, Claude CLI), which take the same
//! conversation shape as the API providers.

use crate::providers::Message;

/// How much the model should think before answering. `Fast` is the default for live use:
/// the lowest reasoning a provider offers, so the first words arrive as soon as possible.
/// `Smart` is the user's opt-in for slower, deeper reasoning (Settings → Model → Smart mode).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Effort { #[default] Fast, Smart }

impl Effort {
    pub fn from_smart_mode(smart: bool) -> Self { if smart { Self::Smart } else { Self::Fast } }
}

#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    /// Provider-specific model id; `None` uses the provider's default.
    pub model: Option<String>,
    pub effort: Effort,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubscriptionStatus {
    /// The official CLI was found on this PC.
    pub installed: bool,
    /// The CLI in use is the one chosen in Settings, not one found automatically.
    pub custom_location: bool,
    pub signed_in: bool,
    /// Email or account label, when the CLI reports one.
    pub account: Option<String>,
    pub plan: Option<String>,
    /// (id, display name) pairs offered in Settings.
    pub models: Vec<(String, String)>,
    /// The model "Default" means, when the provider reports it.
    pub default_model: Option<String>,
    /// Every model the macOS picker offers, newest first, older ones marked `legacy`; empty when
    /// the provider reported no list (the picker then offers `models`).
    pub catalog: Vec<ModelEntry>,
    /// User-safe explanation when something is wrong.
    pub error: Option<String>,
}

/// A model in the macOS picker: the latest ones are listed, older ones wait under "Legacy models".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelEntry {
    /// What is saved and passed to the CLI (for Claude's latest models, the alias that follows them).
    pub id: String,
    pub name: String,
    /// What it's for, in a few words ("Fastest for quick answers"); may be empty.
    pub note: String,
    pub legacy: bool,
}

/// A provider's model description as a picker note: "For your toughest challenges." → "Your
/// toughest challenges", at most 60 characters.
pub(crate) fn short_note(text: &str) -> String {
    let text = text.trim().trim_end_matches('.');
    let text = text.strip_prefix("For ").unwrap_or(text);
    let mut chars = text.chars();
    let capitalized = chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default();
    capitalized.chars().take(60).collect()
}
