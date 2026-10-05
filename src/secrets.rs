//! Provider API keys stored in the Windows Credential Manager (via `keyring`).
//!
//! Key material is never logged, formatted into errors, or returned except by [`get`].
//! Errors from the credential store are mapped to short fixed messages because some
//! `keyring` error variants carry the raw (undecodable) secret bytes.

use anyhow::{Result, bail};

const SERVICE: &str = "CluelyRS";
const MAX_PROVIDER_ID_LEN: usize = 64;
const MAX_KEY_LEN: usize = 512;

/// Returns the stored key for `provider_id`, or `None` when absent, invalid or unreadable.
pub fn get(provider_id: &str) -> Option<String> {
    let entry = entry(provider_id).ok()?;
    let key = entry.get_password().ok()?;
    normalize_key(&key).ok()
}

/// Stores `key` (trimmed) for `provider_id`, replacing any previous key.
pub fn set(provider_id: &str, key: &str) -> Result<()> {
    let key = normalize_key(key)?;
    entry(provider_id)?.set_password(&key).map_err(|error| store_error("save", &error))
}

/// Deletes the stored key; succeeds when there was nothing to delete.
pub fn remove(provider_id: &str) -> Result<()> {
    match entry(provider_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(store_error("remove", &error)),
    }
}

/// A display-safe hint such as `••••3f9a` for a stored key, or `None` when no key is stored.
pub fn hint(provider_id: &str) -> Option<String> {
    get(provider_id).map(|key| mask_hint(&key))
}

fn entry(provider_id: &str) -> Result<keyring::Entry> {
    validate_provider_id(provider_id)?;
    keyring::Entry::new(SERVICE, &format!("api-key:{provider_id}"))
        .map_err(|error| store_error("open", &error))
}

fn store_error(action: &str, error: &keyring::Error) -> anyhow::Error {
    // Deliberately do not include `error` itself: BadEncoding carries secret bytes.
    let reason = match error {
        keyring::Error::NoEntry => "no key is stored",
        keyring::Error::NoStorageAccess(_) => "the credential store is unavailable",
        keyring::Error::PlatformFailure(_) => "the credential store reported an error",
        keyring::Error::TooLong(..) => "the value is too long for the credential store",
        keyring::Error::Invalid(..) => "the credential name is invalid",
        keyring::Error::BadEncoding(_) => "the stored value is not valid text",
        keyring::Error::Ambiguous(_) => "multiple matching credentials exist",
        _ => "unexpected credential store error",
    };
    anyhow::anyhow!("Could not {action} the API key: {reason}.")
}

fn validate_provider_id(provider_id: &str) -> Result<()> {
    if provider_id.is_empty() || provider_id.len() > MAX_PROVIDER_ID_LEN {
        bail!("Invalid provider id.");
    }
    if !provider_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-') {
        bail!("Invalid provider id.");
    }
    Ok(())
}

/// Trims surrounding whitespace and rejects empty, whitespace-containing, control-character or oversized keys.
fn normalize_key(key: &str) -> Result<String> {
    let key = key.trim();
    if key.is_empty() {
        bail!("The API key is empty.");
    }
    if key.chars().count() > MAX_KEY_LEN {
        bail!("The API key is longer than {MAX_KEY_LEN} characters.");
    }
    if key.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        bail!("The API key must not contain spaces or line breaks.");
    }
    Ok(key.to_owned())
}

fn mask_hint(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    // Very short keys reveal nothing; otherwise show at most the last four characters.
    let shown = if chars.len() >= 12 { 4 } else if chars.len() >= 8 { 2 } else { 0 };
    let tail: String = chars[chars.len() - shown..].iter().collect();
    format!("••••{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_are_ascii_alnum_or_dash() {
        assert!(validate_provider_id("openai").is_ok());
        assert!(validate_provider_id("lm-studio2").is_ok());
        for bad in ["", "open ai", "a/b", "a:b", "é", "a_b", &"x".repeat(65)] {
            assert!(validate_provider_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn keys_are_trimmed_and_validated() {
        assert_eq!(normalize_key("  sk-abc123 \r\n").unwrap(), "sk-abc123");
        assert!(normalize_key("").is_err());
        assert!(normalize_key("   \t").is_err());
        assert!(normalize_key("sk abc").is_err());
        assert!(normalize_key("sk\u{0}abc").is_err());
        assert!(normalize_key(&"k".repeat(512)).is_ok());
        assert!(normalize_key(&"k".repeat(513)).is_err());
    }

    #[test]
    fn validation_errors_never_echo_the_key() {
        let secret = format!("sk-{}", "s".repeat(600));
        let message = normalize_key(&secret).unwrap_err().to_string();
        assert!(!message.contains("sss"));
        let message = normalize_key("sk-secret value").unwrap_err().to_string();
        assert!(!message.contains("secret"));
    }

    #[test]
    fn hint_shows_only_the_tail() {
        assert_eq!(mask_hint("sk-proj-0123456789ab3f9a"), "••••3f9a");
        assert_eq!(mask_hint("abcdefgh"), "••••gh");
        assert_eq!(mask_hint("abc"), "••••");
    }

    #[test]
    fn store_errors_do_not_include_secret_bytes() {
        let error = store_error("save", &keyring::Error::BadEncoding(b"sk-topsecret".to_vec()));
        assert!(!error.to_string().contains("topsecret"));
    }
}
