//! Types shared by the subscription providers (Codex, Claude CLI), which take the same
//! conversation shape as the API providers.

use crate::providers::Message;

#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    /// Provider-specific model id; `None` uses the provider's default.
    pub model: Option<String>,
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
    /// User-safe explanation when something is wrong.
    pub error: Option<String>,
}
