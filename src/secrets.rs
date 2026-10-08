//! Provider API keys stored in the OS credential store via `keyring`: the Windows Credential
//! Manager on Windows, the login Keychain on macOS.
//!
//! Key material is never logged, formatted into errors, or returned except by [`get`].
//! Errors from the credential store are mapped to short fixed messages because some
//! `keyring` error variants carry the raw (undecodable) secret bytes.

use anyhow::{Result, bail};

/// The credential store's name in user-facing copy such as "Saved to Keychain."
pub const STORE_NAME: &str = if cfg!(windows) { "Windows Credential Manager" } else { "Keychain" };

/// Test builds use their own service so they can never read, overwrite or delete real keys.
const SERVICE: &str = if cfg!(test) { "cluely-rs-keychain-test" } else { "CluelyRS" };
const MAX_PROVIDER_ID_LEN: usize = 64;
const MAX_KEY_LEN: usize = 512;

/// Returns the stored key for `provider_id`, or `None` when absent, invalid or unreadable.
pub fn get(provider_id: &str) -> Option<String> {
    cache::get_or_load(provider_id, read_store)
}

/// Stores `key` (trimmed) for `provider_id`, replacing any previous key.
pub fn set(provider_id: &str, key: &str) -> Result<()> {
    let key = normalize_key(key)?;
    match entry(provider_id)?.set_password(&key) {
        Ok(()) => {
            cache::put(provider_id, Some(key));
            Ok(())
        }
        Err(error) => {
            cache::forget(provider_id);
            Err(store_error("save", &error))
        }
    }
}

/// Deletes the stored key; succeeds when there was nothing to delete.
pub fn remove(provider_id: &str) -> Result<()> {
    match entry(provider_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {
            cache::put(provider_id, None);
            Ok(())
        }
        Err(error) => {
            cache::forget(provider_id);
            Err(store_error("remove", &error))
        }
    }
}

/// A display-safe hint such as `••••3f9a` for a stored key, or `None` when no key is stored.
pub fn hint(provider_id: &str) -> Option<String> {
    get(provider_id).map(|key| mask_hint(&key))
}

/// Reads the key from the credential store itself, bypassing the cache.
fn read_store(provider_id: &str) -> Option<String> {
    let entry = entry(provider_id).ok()?;
    let key = entry.get_password().ok()?;
    normalize_key(&key).ok()
}

/// macOS keeps each lookup for the life of the process. Settings reads keys on every render, and
/// every Keychain lookup is a round trip to `securityd` (about 1.3 ms) that can also show an access
/// prompt when the binary was rebuilt or re-signed; caching a denied or failed lookup as `None`
/// keeps one prompt from turning into one per frame. `set` and `remove` keep it current; changes
/// made outside the app show after a restart.
#[cfg(target_os = "macos")]
mod cache {
    use std::collections::HashMap;
    use std::sync::{Mutex, PoisonError};

    static CACHE: Mutex<Option<KeyCache>> = Mutex::new(None);

    pub fn get_or_load(provider_id: &str, load: impl FnOnce(&str) -> Option<String>) -> Option<String> {
        // The lock is held while loading so concurrent readers wait for one lookup (and one prompt).
        with(|cache| cache.get_or_load(provider_id, load))
    }

    pub fn put(provider_id: &str, key: Option<String>) {
        with(|cache| cache.put(provider_id, key));
    }

    pub fn forget(provider_id: &str) {
        with(|cache| cache.forget(provider_id));
    }

    fn with<T>(f: impl FnOnce(&mut KeyCache) -> T) -> T {
        let mut guard = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        f(guard.get_or_insert_with(KeyCache::default))
    }

    /// Known lookups by provider id; `None` records that no usable key is stored.
    #[derive(Default)]
    pub(super) struct KeyCache(HashMap<String, Option<String>>);

    impl KeyCache {
        pub(super) fn get_or_load(&mut self, provider_id: &str, load: impl FnOnce(&str) -> Option<String>) -> Option<String> {
            if let Some(known) = self.0.get(provider_id) {
                return known.clone();
            }
            let key = load(provider_id);
            self.0.insert(provider_id.to_owned(), key.clone());
            key
        }

        pub(super) fn put(&mut self, provider_id: &str, key: Option<String>) {
            self.0.insert(provider_id.to_owned(), key);
        }

        pub(super) fn forget(&mut self, provider_id: &str) {
            self.0.remove(provider_id);
        }
    }
}

/// Other platforms read the credential store directly every time.
#[cfg(not(target_os = "macos"))]
mod cache {
    pub fn get_or_load(provider_id: &str, load: impl FnOnce(&str) -> Option<String>) -> Option<String> {
        load(provider_id)
    }

    pub fn put(_provider_id: &str, _key: Option<String>) {}

    pub fn forget(_provider_id: &str) {}
}

fn entry(provider_id: &str) -> Result<keyring::Entry> {
    validate_provider_id(provider_id)?;
    keyring::Entry::new(SERVICE, &account(provider_id)).map_err(|error| store_error("open", &error))
}

fn account(provider_id: &str) -> String {
    format!("api-key:{provider_id}")
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

    /// Stores, reads and deletes a dummy key in the real OS credential store, under the test
    /// service only. On macOS this can show a Keychain access prompt, so it never runs by default:
    ///   cargo test --lib secrets::tests::os_store_round_trip -- --ignored
    #[test]
    #[ignore = "writes to the OS credential store"]
    fn os_store_round_trip() {
        const PROVIDER: &str = "round-trip";
        let key = "sk-dummy-0123456789abcd";
        remove(PROVIDER).unwrap();
        assert_eq!(get(PROVIDER), None);

        set(PROVIDER, key).unwrap();
        #[cfg(target_os = "macos")]
        assert!(keychain_has_item(PROVIDER), "the key should be a persistent Keychain item");
        assert_eq!(read_store(PROVIDER).as_deref(), Some(key));
        assert_eq!(get(PROVIDER).as_deref(), Some(key));
        assert_eq!(hint(PROVIDER).as_deref(), Some("••••abcd"));
        // A Settings render reads up to two keys (`get` for the badge, `hint` for the key row).
        const READS: u32 = 50;
        let started = std::time::Instant::now();
        for _ in 0..READS {
            assert!(read_store(PROVIDER).is_some());
        }
        eprintln!("store read: {:?} per read over {READS} reads", started.elapsed() / READS);
        let started = std::time::Instant::now();
        for _ in 0..READS {
            assert!(get(PROVIDER).is_some());
        }
        eprintln!("get(): {:?} per read over {READS} reads", started.elapsed() / READS);

        remove(PROVIDER).unwrap();
        assert_eq!(get(PROVIDER), None);
        assert_eq!(read_store(PROVIDER), None);
        #[cfg(target_os = "macos")]
        assert!(!keychain_has_item(PROVIDER), "the Keychain item should be gone");
    }

    #[cfg(target_os = "macos")]
    mod cache {
        use std::cell::Cell;

        use super::super::cache::KeyCache;

        #[test]
        fn lookups_load_once_including_absent_keys() {
            let mut cache = KeyCache::default();
            let loads = Cell::new(0);
            let load = |id: &str| {
                loads.set(loads.get() + 1);
                (id == "openai").then(|| "sk-a".to_owned())
            };
            assert_eq!(cache.get_or_load("openai", load).as_deref(), Some("sk-a"));
            assert_eq!(cache.get_or_load("openai", load).as_deref(), Some("sk-a"));
            assert_eq!(cache.get_or_load("groq", load), None);
            assert_eq!(cache.get_or_load("groq", load), None);
            assert_eq!(loads.get(), 2);
        }

        #[test]
        fn put_replaces_and_forget_reloads() {
            let mut cache = KeyCache::default();
            let unreachable = |_: &str| -> Option<String> { panic!("should be served from the cache") };
            cache.put("openai", Some("sk-new".to_owned()));
            assert_eq!(cache.get_or_load("openai", unreachable).as_deref(), Some("sk-new"));
            cache.put("openai", None);
            assert_eq!(cache.get_or_load("openai", unreachable), None);
            cache.forget("openai");
            assert_eq!(cache.get_or_load("openai", |_| Some("sk-store".to_owned())).as_deref(), Some("sk-store"));
        }
    }

    /// Looks the item up from another process by its attributes only (no `-g`/`-w`), so the secret
    /// is never read and no access prompt can appear.
    #[cfg(target_os = "macos")]
    fn keychain_has_item(provider_id: &str) -> bool {
        std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", SERVICE, "-a", &account(provider_id)])
            .output()
            .is_ok_and(|output| output.status.success())
    }
}
