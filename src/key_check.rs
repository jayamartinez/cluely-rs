//! Checking an API key before Save puts it in the Keychain (macOS Settings and setup), so a key its
//! provider rejects is never stored. An answer provider's key is checked with the models request
//! Settings makes anyway; Deepgram's with its project list. Every check is one request, sent off the
//! UI thread, with the key only in its auth header.

use crate::providers::{self, ProviderError};
use crate::stt::deepgram;

/// Lists the projects a key belongs to: 401 for a key Deepgram doesn't know.
const DEEPGRAM_CHECK: &str = "https://api.deepgram.com/v1/projects";
const REJECTED: &str = "Key rejected. Nothing saved.";

/// What Save does with a checked key.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Save,
    /// Not saved, and why, as the key field shows it.
    Refuse(String),
}

/// Check `key` for `provider_id` (an API preset or Deepgram) with its provider. Blocks on the network.
pub fn check(provider_id: &str, key: &str, custom_base_url: &str) -> Verdict {
    if provider_id == deepgram::PROVIDER_ID {
        return deepgram_verdict(providers::get_status(DEEPGRAM_CHECK, ("authorization", &format!("Token {key}"))));
    }
    let Some(preset) = providers::preset(provider_id) else { return Verdict::Save };
    if preset.local { return Verdict::Save; }
    let base = if preset.base_url.is_empty() { custom_base_url } else { preset.base_url };
    // A missing or unusable base URL isn't the key's fault; say so before sending anything.
    if let Err(error) = providers::validate_base_url(base) { return Verdict::Refuse(format!("{} Nothing saved.", sentence(&error))); }
    verdict(providers::list_models(preset.wire, base, Some(key)).map(drop))
}

/// Refused only when the provider turned the key down (some, like Gemini, with a 400 and a message)
/// or couldn't be reached to ask. A rate limit, a missing model list or a server error doesn't show
/// the key is wrong, so it's saved and the model list reports the problem.
fn verdict(result: Result<(), ProviderError>) -> Verdict {
    match result {
        Ok(()) => Verdict::Save,
        Err(ProviderError::Unauthorized) => Verdict::Refuse(REJECTED.into()),
        Err(ProviderError::BadRequest(message)) if message.is_empty() => Verdict::Refuse(REJECTED.into()),
        Err(ProviderError::BadRequest(message)) => Verdict::Refuse(format!("Key rejected: {}. Nothing saved.", message.trim_end_matches('.'))),
        Err(ProviderError::Network(reason)) => Verdict::Refuse(format!("Couldn't check the key: {reason}. Nothing saved.")),
        Err(_) => Verdict::Save,
    }
}

/// Deepgram answers 401 for an unknown key. A 403 is a real key without access to the project list
/// (one made only for streaming), so it's saved.
fn deepgram_verdict(status: Result<u16, ProviderError>) -> Verdict {
    match status {
        Ok(401) => Verdict::Refuse(REJECTED.into()),
        Ok(_) => Verdict::Save,
        Err(error) => verdict(Err(error)),
    }
}

/// A base URL's problem as a sentence that ends with a full stop. It never reached a provider, so
/// not "The provider rejected the request: …".
fn sentence(error: &ProviderError) -> String {
    let text = match error { ProviderError::BadRequest(message) => message.clone(), other => other.to_string() };
    if text.ends_with('.') { text } else { format!("{text}.") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_turned_down_or_unchecked_key_is_refused() {
        assert_eq!(verdict(Ok(())), Verdict::Save);
        assert_eq!(verdict(Err(ProviderError::Unauthorized)), Verdict::Refuse(REJECTED.into()));
        assert_eq!(verdict(Err(ProviderError::BadRequest("API key not valid. Please pass a valid API key.".into()))),
            Verdict::Refuse("Key rejected: API key not valid. Please pass a valid API key. Nothing saved.".into()));
        assert_eq!(verdict(Err(ProviderError::Network("could not connect to the server".into()))),
            Verdict::Refuse("Couldn't check the key: could not connect to the server. Nothing saved.".into()));
        for unproven in [ProviderError::RateLimited, ProviderError::NotFound, ProviderError::Server(502), ProviderError::Empty] {
            assert_eq!(verdict(Err(unproven)), Verdict::Save, "the key isn't shown to be wrong");
        }
    }

    #[test]
    fn deepgram_refuses_only_an_unknown_key() {
        assert_eq!(deepgram_verdict(Ok(200)), Verdict::Save);
        assert_eq!(deepgram_verdict(Ok(401)), Verdict::Refuse(REJECTED.into()));
        assert_eq!(deepgram_verdict(Ok(403)), Verdict::Save, "a streaming-only key");
        assert!(matches!(deepgram_verdict(Err(ProviderError::Network("the request timed out".into()))), Verdict::Refuse(_)));
    }

    #[test]
    fn local_and_unknown_providers_and_bad_base_urls_never_send_the_key() {
        assert_eq!(check("ollama", "anything", ""), Verdict::Save, "local: nothing to check");
        assert_eq!(check("not-a-provider", "anything", ""), Verdict::Save);
        assert_eq!(check("custom", "sk-test", ""), Verdict::Refuse("Enter a base URL for this provider. Nothing saved.".into()));
        assert!(matches!(check("custom", "sk-test", "http://example.com/v1"), Verdict::Refuse(reason) if reason.ends_with("Nothing saved.")),
            "plain http only for this computer");
    }
}
