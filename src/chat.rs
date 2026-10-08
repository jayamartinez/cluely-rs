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
    pub signed_in: bool,
    /// Email or account label, when the CLI reports one.
    pub account: Option<String>,
    pub plan: Option<String>,
    /// (id, display name) pairs offered in Settings.
    pub models: Vec<(String, String)>,
    /// The model "Default" means, when the provider reports it.
    pub default_model: Option<String>,
    /// User-safe explanation when something is wrong.
    pub error: Option<String>,
}
