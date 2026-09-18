//! OS-keychain backed credential storage for LLM provider API keys.
//!
//! All access goes through the [`CredentialStore`] trait so tests can swap in
//! [`MemoryStore`] without touching the real OS keychain. Production uses
//! [`KeyringStore`] which wraps `keyring::Entry`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub mod migration;

/// Whether the operator opted out of the OS keychain in favour of the legacy
/// key file. Only `DRENGR_KEYCHAIN=disable` opts out.
pub fn keychain_disabled() -> bool {
    std::env::var("DRENGR_KEYCHAIN")
        .ok()
        .map(|v| v.eq_ignore_ascii_case("disable"))
        .unwrap_or(false)
}

/// `service` value for LLM provider keys.
pub const SERVICE_LLM: &str = "dev.drengr.cli.llm";

/// Errors returned by a [`CredentialStore`].
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The platform keychain is unreachable (no D-Bus on Linux, locked, etc.).
    /// Callers may decide to fall back to a legacy file or fail loudly.
    #[error("OS keychain unavailable: {0}")]
    Unavailable(String),

    /// Any other backend error (corrupt entry, permission denied, etc.).
    #[error("keychain error: {0}")]
    Other(String),
}

/// Report an unreachable keychain on a read path. CI runners have no keychain
/// and no one at the terminal to act on it, so there it is a debug line rather
/// than a warning on every run.
pub fn report_unavailable(what: &str, why: &str) {
    if std::env::var_os("CI").is_some() {
        tracing::debug!("OS keychain unavailable for {what} ({why}); using the file fallback");
    } else {
        tracing::warn!("OS keychain unavailable for {what} ({why}); falling back to legacy file");
    }
}

/// Pluggable credential backend. Implementations MUST be thread-safe.
pub trait CredentialStore: Send + Sync {
    /// Returns `Ok(None)` when the entry does not exist.
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, CredentialError>;
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), CredentialError>;
    /// Deleting a missing entry is NOT an error.
    fn delete(&self, service: &str, account: &str) -> Result<(), CredentialError>;
}

/// Production backend: real OS keychain via the `keyring` crate.
pub struct KeyringStore;

impl KeyringStore {
    fn entry(service: &str, account: &str) -> Result<keyring::Entry, CredentialError> {
        keyring::Entry::new(service, account).map_err(map_err)
    }
}

/// How long a keychain is given to answer before we treat it as unavailable.
///
/// A healthy keychain answers in single-digit milliseconds. Anything near this
/// is a system service that is not going to answer at all.
const KEYCHAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Run one keychain operation under a deadline.
///
/// `keyring` is synchronous and talks to a system service that can block
/// forever. On macOS the access-control list is keyed to the calling binary, so
/// a rebuilt or re-signed `drengr` raises a GUI authorization prompt — and an
/// MCP server has nobody at a dialog, so the read never returns. Measured:
/// `drengr mcp` printed its config line and then hung past 400s, before the
/// license line, answering no JSON-RPC at all.
///
/// A hung keychain must cost a bounded wait and the documented file fallback,
/// never the process. The worker thread is abandoned rather than killed: there
/// is no portable way to cancel a blocked syscall, and one parked thread is a
/// far smaller price than a server that never speaks. It ends when the OS
/// finally answers.
fn under_deadline<T, F>(op: F) -> Result<T, CredentialError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("drengr-keychain".into())
        .spawn(move || {
            // The receiver is gone on timeout; dropping the result is correct.
            let _ = tx.send(op());
        })
        .map_err(|e| {
            CredentialError::Unavailable(format!("could not start keychain thread: {e}"))
        })?;

    rx.recv_timeout(KEYCHAIN_TIMEOUT).map_err(|_| {
        CredentialError::Unavailable(format!(
            "keychain did not answer within {}s (on macOS this is usually an \
             authorization prompt no one can answer; set DRENGR_KEYCHAIN=disable \
             to skip it)",
            KEYCHAIN_TIMEOUT.as_secs()
        ))
    })
}

impl CredentialStore for KeyringStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, CredentialError> {
        let entry = Self::entry(service, account)?;
        match under_deadline(move || entry.get_password())? {
            Ok(secret) => Ok(Some(secret)),
            // NoEntry is the documented "not found" case — surface as None,
            // not Err, so callers can distinguish "missing" from "broken".
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(e)),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), CredentialError> {
        let entry = Self::entry(service, account)?;
        let secret = secret.to_string();
        under_deadline(move || entry.set_password(&secret))?.map_err(map_err)
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), CredentialError> {
        let entry = Self::entry(service, account)?;
        match under_deadline(move || entry.delete_credential())? {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }
}

fn map_err(e: keyring::Error) -> CredentialError {
    match e {
        keyring::Error::NoStorageAccess(inner) => {
            CredentialError::Unavailable(format!("no storage access: {}", inner))
        }
        keyring::Error::PlatformFailure(inner) => {
            CredentialError::Unavailable(format!("platform failure: {}", inner))
        }
        other => CredentialError::Other(other.to_string()),
    }
}

/// In-memory backend used by tests. Never instantiated in production paths.
pub struct MemoryStore {
    inner: Mutex<HashMap<(String, String), String>>,
    /// When set, all operations return this error. Lets tests simulate a
    /// keychain outage without monkey-patching the keyring crate.
    fail_with: Mutex<Option<String>>,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            fail_with: Mutex::new(None),
        }
    }
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Force every subsequent op to return [`CredentialError::Unavailable`].
    #[cfg(test)]
    pub fn set_unavailable(&self, msg: &str) {
        *self.fail_with.lock().unwrap() = Some(msg.to_string());
    }

    fn check_fail(&self) -> Result<(), CredentialError> {
        if let Some(msg) = self.fail_with.lock().unwrap().clone() {
            return Err(CredentialError::Unavailable(msg));
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }
}

impl CredentialStore for MemoryStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, CredentialError> {
        self.check_fail()?;
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(service.to_string(), account.to_string()))
            .cloned())
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), CredentialError> {
        self.check_fail()?;
        self.inner.lock().unwrap().insert(
            (service.to_string(), account.to_string()),
            secret.to_string(),
        );
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), CredentialError> {
        self.check_fail()?;
        self.inner
            .lock()
            .unwrap()
            .remove(&(service.to_string(), account.to_string()));
        Ok(())
    }
}

/// Process-wide credential store. Defaults to [`KeyringStore`] in production;
/// under `cfg(test)` defaults to a fresh [`MemoryStore`] so a test that
/// forgets to call [`install_mock`] CANNOT reach the real OS keychain
/// regardless of test ordering or parallelism.
static STORE: OnceLock<Box<dyn CredentialStore>> = OnceLock::new();

#[cfg(not(test))]
pub fn store() -> &'static dyn CredentialStore {
    STORE
        .get_or_init(|| Box::new(KeyringStore) as Box<dyn CredentialStore>)
        .as_ref()
}

#[cfg(test)]
pub fn store() -> &'static dyn CredentialStore {
    STORE
        .get_or_init(|| Box::new(MemoryStore::new()) as Box<dyn CredentialStore>)
        .as_ref()
}

/// Process-wide lock for tests that mutate the global store. Acquire via
/// [`test_lock`] before calling [`install_mock`] / [`reset_mock`].
#[cfg(test)]
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Tests MUST hold this guard for the duration of any test that touches
/// the global credential store. Cargo runs tests in parallel by default
/// and the shared OnceLock is otherwise a race waiting to happen.
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    // Recover from poisoned locks: a panicking test left the mutex tainted
    // but the in-memory state is reset by every caller via reset_mock.
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Tests must call this in setup. Safe to call multiple times across the
/// test process: only the first call wins (subsequent installs are no-ops),
/// so each test that needs a fresh store should use [`reset_mock`] instead.
#[cfg(test)]
pub fn install_mock() -> &'static MemoryStore {
    // OnceLock::get_or_init guarantees one initializer; we always wrap a
    // MemoryStore for tests. Subsequent test calls reuse the same instance —
    // tests that need isolation reset state via the returned handle.
    let s = STORE.get_or_init(|| Box::new(MemoryStore::new()) as Box<dyn CredentialStore>);
    // Safe: in cfg(test) the OnceLock is only ever populated with a MemoryStore.
    let ptr = s.as_ref() as *const dyn CredentialStore as *const MemoryStore;
    unsafe { &*ptr }
}

/// Test helper: clear all entries and reset failure injection. Call at the
/// start of every test that touches the global store.
#[cfg(test)]
pub fn reset_mock() {
    let s = install_mock();
    s.inner.lock().unwrap().clear();
    *s.fail_with.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_read_path_warns_about_an_unavailable_keychain_on_its_own() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let reporter = root.join("credentials").join("mod.rs");
        let mut offenders = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path == reporter {
                    continue;
                }
                let flat = std::fs::read_to_string(&path)
                    .unwrap()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if flat.contains("warn!(\"OS keychain unavailable")
                    || flat.contains("warn!( \"OS keychain unavailable")
                {
                    offenders.push(path.display().to_string());
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these bypass credentials::report_unavailable and will shout on CI: {offenders:?}"
        );
    }

    // NOTE: tests in this file share a process-global store. They are
    // serialized implicitly by reset_mock() + the fact that each test only
    // touches its own (service, account) pair.

    #[test]
    fn keyring_store_set_then_get_round_trip() {
        // Use MemoryStore directly (NOT the global) so this test doesn't
        // race other tests that mutate the global store.
        let store = MemoryStore::new();
        assert_eq!(store.get("svc", "acct").unwrap(), None);
        store.set("svc", "acct", "hunter2").unwrap();
        assert_eq!(
            store.get("svc", "acct").unwrap().as_deref(),
            Some("hunter2")
        );
        store.delete("svc", "acct").unwrap();
        assert_eq!(store.get("svc", "acct").unwrap(), None);
        // Deleting again is fine.
        store.delete("svc", "acct").unwrap();
    }

    #[test]
    fn memory_store_unavailable_propagates() {
        let store = MemoryStore::new();
        store.set_unavailable("simulated outage");
        let err = store.get("a", "b").unwrap_err();
        assert!(matches!(err, CredentialError::Unavailable(_)));
    }
}

/// The bounded-keychain guard.
///
/// `drengr mcp` printed its config line and then hung past 400 seconds,
/// answering no JSON-RPC at all, because `keyring`'s `get_password` is a
/// synchronous call into a system service with no deadline. On macOS the
/// access-control list is keyed to the calling binary, so a rebuilt `drengr`
/// raises an authorization prompt — and an MCP server has nobody at a dialog.
///
/// The property: every keychain call runs under a deadline, and a keychain that
/// does not answer degrades to the documented file fallback.
#[cfg(test)]
mod bounded_keychain_guard {
    use super::*;
    use crate::source_guard::{line_of, rust_files_under, src_root, without_comments};

    /// The `keyring` entry points that block. Each must be reached only from
    /// inside `under_deadline`.
    const BLOCKING_CALLS: &[&str] = &["get_password", "set_password", "delete_credential"];

    #[test]
    fn every_keychain_call_runs_under_a_deadline() {
        let files = rust_files_under(&src_root());
        let (name, src) = files
            .iter()
            .find(|(n, _)| n == "credentials/mod.rs")
            .expect("guard must still read the file it polices");
        let code = without_comments(src);

        // Self-check: if the scan stops finding the blocking calls, it is not
        // passing, it is broken.
        let seen: usize = BLOCKING_CALLS.iter().map(|c| code.matches(c).count()).sum();
        assert!(
            seen >= BLOCKING_CALLS.len(),
            "guard found only {seen} keychain calls in {name}; it has stopped reading what it polices"
        );

        for call in BLOCKING_CALLS {
            for (at, _) in code.match_indices(call) {
                // The table above names them; skip its own lines.
                let line_start = code[..at].rfind('\n').map_or(0, |p| p + 1);
                let line = &code[line_start..at];
                if line.contains("BLOCKING_CALLS") || line.trim_start().starts_with('"') {
                    continue;
                }
                let before = &code[line_start..at];
                assert!(
                    before.contains("under_deadline"),
                    "{name}:{} calls {call} outside under_deadline. A keychain with no \
                     deadline hangs the MCP server at startup with no output at all.",
                    line_of(&code, at)
                );
            }
        }
    }

    /// The defect itself: a keychain that never answers must cost the deadline,
    /// not the process.
    ///
    /// Runs the wait on its own thread with a bounded receive, because the
    /// regression under test is a hang, and a check that hangs on regression is
    /// not a check.
    #[test]
    fn a_keychain_that_never_answers_becomes_unavailable() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let outcome: Result<(), CredentialError> =
                under_deadline(|| std::thread::sleep(std::time::Duration::from_secs(120)));
            let _ = tx.send((outcome, started.elapsed()));
        });

        let (outcome, waited) = rx
            .recv_timeout(KEYCHAIN_TIMEOUT + std::time::Duration::from_secs(10))
            .expect("under_deadline never returned: the deadline is gone and a wedged keychain hangs us again");

        match outcome {
            Err(CredentialError::Unavailable(why)) => assert!(
                why.contains("did not answer"),
                "the error must say the keychain did not answer, got: {why}"
            ),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert!(
            waited >= KEYCHAIN_TIMEOUT,
            "returned in {waited:?}, before the {KEYCHAIN_TIMEOUT:?} deadline could have fired"
        );
    }

    #[test]
    fn a_keychain_that_answers_in_time_passes_its_value_through() {
        assert_eq!(under_deadline(|| 7_u8).unwrap(), 7);
    }
}
