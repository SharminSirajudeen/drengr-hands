//! Persistent storage for LLM provider API keys. Primary backend is the OS
//! keychain via [`crate::credentials`]; the legacy `~/.drengr/llm_keys.json`
//! is read-only after migration and only used as a last-resort fallback when
//! the keychain is unreachable AND `DRENGR_KEYCHAIN=disable` is set.

use crate::credentials::{report_unavailable, store as cred_store, CredentialError, SERVICE_LLM};
use std::collections::HashMap;
use std::path::PathBuf;

const LEGACY_FILE: &str = "llm_keys.json";

const KNOWN_PROVIDERS: &[&str] = &[
    "openai",
    "gemini",
    "anthropic",
    "groq",
    "together",
    "fireworks",
    "ollama",
];

/// Flat map of provider → API key. In-memory representation; persistence is
/// keychain-first.
#[derive(Default)]
pub struct KeyStore {
    keys: HashMap<String, String>,
    /// Set when this snapshot was loaded from the legacy file (keychain
    /// was unavailable). Used to warn on save and to mirror writes back to
    /// the file under DRENGR_KEYCHAIN=disable.
    loaded_from_legacy: bool,
}

impl KeyStore {
    /// Read every known provider out of the keychain. Falls back to the
    /// legacy file when the keychain is unreachable. Returns an empty store
    /// if nothing is configured.
    pub fn load() -> Self {
        let store = cred_store();
        let mut keys = HashMap::new();
        let mut keychain_available = true;

        for provider in KNOWN_PROVIDERS {
            match store.get(SERVICE_LLM, provider) {
                Ok(Some(secret)) => {
                    keys.insert((*provider).to_string(), secret);
                }
                Ok(None) => {}
                Err(CredentialError::Unavailable(msg)) => {
                    report_unavailable("provider keys", &msg);
                    keychain_available = false;
                    break;
                }
                Err(CredentialError::Other(msg)) => {
                    tracing::warn!("keychain read failed for {}: {}", provider, msg);
                }
            }
        }

        if !keychain_available {
            return Self::load_legacy_file();
        }
        Self {
            keys,
            loaded_from_legacy: false,
        }
    }

    /// Read the legacy `~/.drengr/llm_keys.json`. Always emits a deprecation
    /// warning when the file is found.
    fn load_legacy_file() -> Self {
        let path = legacy_path();
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => return Self::default(),
        };
        let keys: HashMap<String, String> = serde_json::from_str(&data).unwrap_or_default();
        if !keys.is_empty() {
            tracing::warn!(
                "Reading LLM keys from deprecated {}; run `drengr key set` to migrate",
                path.display()
            );
        }
        Self {
            keys,
            loaded_from_legacy: true,
        }
    }

    /// Persist every entry to the OS keychain. On `Unavailable`, writes the
    /// legacy file iff `DRENGR_KEYCHAIN=disable` is set; otherwise returns
    /// an actionable error.
    pub fn save(&self) -> std::io::Result<()> {
        let store = cred_store();
        let mut unavailable: Option<String> = None;

        for (provider, key) in &self.keys {
            match store.set(SERVICE_LLM, provider, key) {
                Ok(()) => {}
                Err(CredentialError::Unavailable(msg)) => {
                    unavailable = Some(msg);
                    break;
                }
                Err(CredentialError::Other(msg)) => {
                    return Err(std::io::Error::other(msg));
                }
            }
        }

        if let Some(msg) = unavailable {
            return self.save_legacy_fallback(&msg);
        }
        Ok(())
    }

    fn save_legacy_fallback(&self, why: &str) -> std::io::Result<()> {
        let opt_in = crate::credentials::keychain_disabled();
        if !opt_in {
            return Err(std::io::Error::other(format!(
                "OS keychain unavailable ({why}). Either:\n  \
                     (a) export the provider env var (e.g. OPENAI_API_KEY),\n  \
                     (b) set DRENGR_KEYCHAIN=disable to use the legacy file,\n  \
                     (c) start a keyring daemon (Linux: gnome-keyring / kwallet)."
            )));
        }
        tracing::warn!("DRENGR_KEYCHAIN=disable set; writing LLM keys to legacy file (insecure)");
        write_legacy_file(&self.keys)
    }

    pub fn set(&mut self, provider: &str, key: &str) {
        self.keys.insert(provider.to_lowercase(), key.to_string());
    }

    pub fn get(&self, provider: &str) -> Option<&str> {
        self.keys.get(&provider.to_lowercase()).map(|s| s.as_str())
    }

    pub fn remove(&mut self, provider: &str) -> bool {
        let removed = self.keys.remove(&provider.to_lowercase()).is_some();
        if removed {
            // Best-effort: delete from keychain immediately so removal is
            // durable even if the caller forgets to call save().
            let _ = cred_store().delete(SERVICE_LLM, &provider.to_lowercase());
        }
        removed
    }

    /// Delete every stored provider key from the OS keychain (uninstall path).
    /// Missing entries are non-errors.
    pub fn purge_keychain() {
        let store = cred_store();
        for provider in KNOWN_PROVIDERS {
            let _ = store.delete(SERVICE_LLM, provider);
        }
    }

    /// Returns sorted (provider, masked_key) pairs.
    pub fn list(&self) -> Vec<(String, String)> {
        let mut entries: Vec<_> = self
            .keys
            .iter()
            .map(|(p, k)| (p.clone(), mask_key(k)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// True when this snapshot was sourced from the deprecated file. Used
    /// by the `drengr key` CLI for accurate display.
    pub fn from_legacy_file(&self) -> bool {
        self.loaded_from_legacy
    }
}

/// Mask a key for display: first 4 + "..." + last 4.
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 12 {
        "***".to_string()
    } else {
        let prefix: String = chars[..4].iter().collect();
        let suffix: String = chars[chars.len() - 4..].iter().collect();
        format!("{}...{}", prefix, suffix)
    }
}

/// Validate provider name. Returns normalized lowercase or error message.
pub fn validate_provider(provider: &str) -> Result<String, String> {
    let normalized = provider.to_lowercase();
    if KNOWN_PROVIDERS.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(format!(
            "Unknown provider '{}'. Valid providers: {}",
            provider,
            KNOWN_PROVIDERS.join(", ")
        ))
    }
}

fn legacy_path() -> PathBuf {
    crate::paths::drengr_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(LEGACY_FILE)
}

/// Atomic write of the legacy file with 0o600. Used only under
/// `DRENGR_KEYCHAIN=disable`.
fn write_legacy_file(keys: &HashMap<String, String>) -> std::io::Result<()> {
    let dir = crate::paths::ensure_drengr_dir()?;
    let path = dir.join(LEGACY_FILE);
    let tmp = dir.join(".llm_keys.json.tmp");

    let json = serde_json::to_string_pretty(keys).map_err(std::io::Error::other)?;

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, &json)?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials;

    fn fresh_env() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: caller holds credentials::test_lock() so no other test is
        // racing on env vars or the global store at this point.
        unsafe {
            std::env::set_var("DRENGR_HOME", tmp.path());
            std::env::remove_var("DRENGR_KEYCHAIN");
        }
        credentials::install_mock();
        credentials::reset_mock();
        tmp
    }

    #[test]
    fn test_mask_key() {
        assert_eq!(mask_key("short"), "***");
        assert_eq!(mask_key("AIzaSyBcDeFgHiJkLmNoPqRsT"), "AIza...qRsT");
        assert_eq!(mask_key("sk-proj-abc123def456"), "sk-p...f456");
    }

    #[test]
    fn test_validate_provider() {
        assert!(validate_provider("gemini").is_ok());
        assert!(validate_provider("Gemini").is_ok());
        assert!(validate_provider("OPENAI").is_ok());
        assert!(validate_provider("unknown").is_err());
    }

    #[test]
    fn test_round_trip_in_memory() {
        // Pure in-memory: doesn't depend on the global store.
        let mut store = KeyStore::default();
        store.set("gemini", "AIza-test-key-12345");
        store.set("OpenAI", "sk-test-key-67890");

        assert_eq!(store.get("gemini"), Some("AIza-test-key-12345"));
        assert_eq!(store.get("openai"), Some("sk-test-key-67890"));
        assert_eq!(store.list().len(), 2);
    }

    #[test]
    fn test_save_then_load_round_trips_via_keychain() {
        let _guard = credentials::test_lock();
        let _tmp = fresh_env();
        let mut store = KeyStore::default();
        store.set("gemini", "AIza-keychain-test-1234");
        store.save().unwrap();

        let loaded = KeyStore::load();
        assert_eq!(loaded.get("gemini"), Some("AIza-keychain-test-1234"));
        assert!(!loaded.from_legacy_file());
    }

    #[test]
    fn test_legacy_file_fallback_is_read_only() {
        let _guard = credentials::test_lock();
        let tmp = fresh_env();
        // Simulate keychain outage by writing only to the legacy file.
        let dir = tmp.path();
        std::fs::create_dir_all(dir).unwrap();
        let mut keys = HashMap::new();
        keys.insert("openai".to_string(), "sk-from-legacy-file".to_string());
        std::fs::write(dir.join(LEGACY_FILE), serde_json::to_string(&keys).unwrap()).unwrap();

        // Force the credential store to look unavailable.
        let mock = credentials::install_mock();
        mock.set_unavailable("simulated outage for fallback test");

        let loaded = KeyStore::load();
        assert!(loaded.from_legacy_file());
        assert_eq!(loaded.get("openai"), Some("sk-from-legacy-file"));

        // Cleanup so the global mock failure doesn't leak into other tests.
        credentials::reset_mock();
    }
}
