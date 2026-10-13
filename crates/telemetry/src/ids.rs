//! Identity-layer helpers for `send_telemetry`.
//!
//! Three layers, each used as a key in the proxy payload:
//!
//! - [`get_anonymous_id`]: project-local uuid4, file-backed at
//!   `<project_root>/.anon_id`. Honours the
//!   [`TelemetryConfig::tracking_id`](crate::TelemetryConfig::tracking_id)
//!   override (`TRACKING_ID`).
//! - [`get_persistent_id`]: machine-local uuid4, file-backed at
//!   `~/.cognee/.persistent_id`. Survives `forget(everything=True)`.
//! - [`get_api_key_tracking_id`]: deterministic PBKDF2-HMAC-SHA256
//!   hash of the LLM API key registered with [`set_llm_api_key`] (the
//!   resolved `llm.api_key` of the most recently configured handle), with
//!   a configurable salt. Stable across machines for the same key.

use std::collections::BTreeMap;
use std::sync::Mutex;

#[cfg(feature = "telemetry")]
mod inner {
    use hmac::Hmac;
    use once_cell::sync::Lazy;
    use pbkdf2::pbkdf2;
    use sha2::Sha256;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use uuid::Uuid;

    const DEFAULT_SALT: &[u8] = b"cognee.telemetry.api-key-tracking.v1";
    const ITERATIONS: u32 = 100_000;
    const DKLEN: usize = 16;

    /// Cached anonymous id. Set on first call; re-reading the file on
    /// every event would be cheap but pointless — the file content is
    /// process-stable.
    static ANON_ID: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

    /// Cached persistent id. Same caching rationale as above.
    static PERSISTENT_ID: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

    /// Cache-busting helper for tests. Not exposed outside the crate,
    /// and only intended for the `ids_tests` module — production code
    /// has no reason to drop these caches.
    #[cfg(test)]
    #[allow(
        clippy::unwrap_used,
        reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
    )]
    pub(crate) fn reset_caches_for_test() {
        // lock poison is unrecoverable
        *ANON_ID.lock().unwrap() = None;
        // lock poison is unrecoverable
        *PERSISTENT_ID.lock().unwrap() = None;
    }

    /// Wipe the cached anonymous and persistent IDs so the next call
    /// re-reads from disk. Test-only: gated by
    /// `cfg(any(test, debug_assertions))` so it stays out of release
    /// builds. Integration tests in `crates/telemetry/tests/` need this
    /// because the `pub(crate)` helper above is not visible to them.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[allow(
        clippy::unwrap_used,
        reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
    )]
    pub fn __test_only_reset_caches() {
        // lock poison is unrecoverable
        *ANON_ID.lock().unwrap() = None;
        // lock poison is unrecoverable
        *PERSISTENT_ID.lock().unwrap() = None;
    }

    /// Project-local anonymous identifier.
    #[allow(
        clippy::unwrap_used,
        reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
    )]
    pub fn get_anonymous_id() -> String {
        if let Some(v) = crate::config::with_config(|c| c.tracking_id.clone())
            && !v.is_empty()
        {
            return v;
        }
        // lock poison is unrecoverable
        let mut cached = ANON_ID.lock().unwrap();
        if let Some(v) = cached.as_ref() {
            return v.clone();
        }
        let computed = compute_anon_id();
        *cached = Some(computed.clone());
        computed
    }

    fn compute_anon_id() -> String {
        let dir = match find_project_root() {
            Some(p) => p,
            None => match std::env::current_dir() {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(
                        target: "cognee.telemetry",
                        error = %e,
                        "could not resolve project root for .anon_id"
                    );
                    return "unknown-anonymous-id".to_string();
                }
            },
        };
        let path = dir.join(".anon_id");
        match std::fs::read_to_string(&path) {
            Ok(s) => s.trim().to_string(),
            Err(_) => {
                let new_id = Uuid::new_v4().to_string();
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    tracing::debug!(
                        target: "cognee.telemetry",
                        error = %e,
                        path = %dir.display(),
                        "could not create dir for .anon_id"
                    );
                    return "unknown-anonymous-id".to_string();
                }
                if let Err(e) = std::fs::write(&path, &new_id) {
                    tracing::debug!(
                        target: "cognee.telemetry",
                        error = %e,
                        path = %path.display(),
                        "could not write .anon_id"
                    );
                    return "unknown-anonymous-id".to_string();
                }
                new_id
            }
        }
    }

    /// Walk up from `current_dir` looking for a `Cargo.toml`. Returns
    /// the first ancestor that contains one, or `None` if none is
    /// found before reaching the filesystem root.
    fn find_project_root() -> Option<PathBuf> {
        let start = std::env::current_dir().ok()?;
        let mut here: &Path = &start;
        loop {
            if here.join("Cargo.toml").is_file() {
                return Some(here.to_path_buf());
            }
            here = here.parent()?;
        }
    }

    /// Machine-local persistent identifier.
    #[allow(
        clippy::unwrap_used,
        reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
    )]
    pub fn get_persistent_id() -> String {
        // lock poison is unrecoverable
        let mut cached = PERSISTENT_ID.lock().unwrap();
        if let Some(v) = cached.as_ref() {
            return v.clone();
        }
        let computed = compute_persistent_id();
        *cached = Some(computed.clone());
        computed
    }

    fn compute_persistent_id() -> String {
        let dir = match dirs::home_dir() {
            Some(p) => p.join(".cognee"),
            None => {
                tracing::debug!(
                    target: "cognee.telemetry",
                    "no home directory; falling back to anonymous id"
                );
                return get_anonymous_id();
            }
        };
        let path = dir.join(".persistent_id");
        if let Ok(s) = std::fs::read_to_string(&path) {
            return s.trim().to_string();
        }
        // Seed from anonymous id if available.
        let mut new_id = get_anonymous_id();
        if new_id == "unknown-anonymous-id" {
            new_id = Uuid::new_v4().to_string();
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::debug!(
                target: "cognee.telemetry",
                error = %e,
                path = %dir.display(),
                "could not create ~/.cognee for persistent id"
            );
            return get_anonymous_id();
        }
        if let Err(e) = std::fs::write(&path, &new_id) {
            tracing::debug!(
                target: "cognee.telemetry",
                error = %e,
                path = %path.display(),
                "could not write persistent id"
            );
            return get_anonymous_id();
        }
        new_id
    }

    /// Computed tracking ids, keyed by `(key, salt)`. PBKDF2 at 100k
    /// iterations costs tens of milliseconds, so it runs once per distinct
    /// key/salt pair instead of once per event. Bounded: a process sees a
    /// handful of keys, and the map is simply cleared if that ever grows.
    #[allow(clippy::type_complexity)]
    static TRACKING_CACHE: Lazy<Mutex<HashMap<(String, Vec<u8>), String>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    const TRACKING_CACHE_CAP: usize = 16;

    /// PBKDF2-HMAC-SHA256 hash of the LLM API key registered for this process
    /// (see [`super::LlmKeyRegistration`]) with a configurable salt. Returns
    /// `"ak_<32-hex-chars>"` when exactly one distinct non-empty key is
    /// registered, empty string otherwise.
    ///
    /// Evaluated at every event (decision 11) against the keys registered
    /// *now*. The hash is cached per key and salt and computed **outside**
    /// the cache lock, so concurrent events never queue behind a PBKDF2 run.
    pub fn get_api_key_tracking_id() -> String {
        let salt = crate::config::with_config(|c| c.api_key_tracking_salt.clone());
        api_key_tracking_id_with_salt(salt.as_deref())
    }

    /// [`get_api_key_tracking_id`] with the salt already resolved (the
    /// dispatcher reads the configuration once per event).
    #[allow(
        clippy::expect_used,
        reason = "PBKDF2 with dklen=16 ≤ SHA256 output=32 is mathematically guaranteed to succeed"
    )]
    #[allow(
        clippy::unwrap_used,
        reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
    )]
    pub(crate) fn api_key_tracking_id_with_salt(salt: Option<&str>) -> String {
        let Some(key) = super::registered_key() else {
            return String::new();
        };
        let salt: Vec<u8> = salt.map_or_else(|| DEFAULT_SALT.to_vec(), |s| s.as_bytes().to_vec());
        let cache_key = (key, salt);
        // lock poison is unrecoverable
        if let Some(id) = TRACKING_CACHE.lock().unwrap().get(&cache_key) {
            return id.clone();
        }
        let mut out = [0u8; DKLEN];
        // PBKDF2 with dklen ≤ HMAC-SHA256 output (32) cannot fail.
        pbkdf2::<Hmac<Sha256>>(cache_key.0.as_bytes(), &cache_key.1, ITERATIONS, &mut out)
            .expect("dklen 16 ≤ Sha256 output 32 — invariant holds");
        let id = format!("ak_{}", hex::encode(out));
        // lock poison is unrecoverable
        let mut cache = TRACKING_CACHE.lock().unwrap();
        if cache.len() >= TRACKING_CACHE_CAP {
            cache.clear();
        }
        cache.insert(cache_key, id.clone());
        id
    }
}

#[cfg(not(feature = "telemetry"))]
mod inner {
    /// Noop stub returned when the `telemetry` feature is disabled.
    pub fn get_anonymous_id() -> String {
        String::new()
    }
    /// Noop stub returned when the `telemetry` feature is disabled.
    pub fn get_persistent_id() -> String {
        String::new()
    }
    /// Noop stub returned when the `telemetry` feature is disabled.
    pub fn get_api_key_tracking_id() -> String {
        String::new()
    }
}

/// Project-local anonymous identifier (uuid4, file-backed at
/// `<project_root>/.anon_id`). Honours the `tracking_id` override of the
/// process [`TelemetryConfig`](crate::TelemetryConfig) (`TRACKING_ID`).
pub use inner::get_anonymous_id;
/// Deterministic PBKDF2-HMAC-SHA256 hash of the registered LLM API key (see
/// [`LlmKeyRegistration`] / [`set_llm_api_key`]), with a configurable salt.
/// Stable across machines for the same key. Empty when no key, or more than
/// one distinct key, is registered.
pub use inner::get_api_key_tracking_id;
/// Machine-local persistent identifier (uuid4, file-backed at
/// `~/.cognee/.persistent_id`).
pub use inner::get_persistent_id;
// --- LLM key registry ---------------------------------------------------------
//
// `api_key_tracking_id` identifies *which key* a process uses. Events are
// emitted from code that has no handle on the configuration (pipeline, search,
// session), so keys are registered process-wide by whoever resolves them: each
// live `ComponentManager` holds an `LlmKeyRegistration`, and embedders without
// one call `set_llm_api_key`. A registration disappears with its owner, so a
// dropped handle's key never lingers.
//
// When every live registration agrees on one key, that key is reported. When
// live handles use *different* keys, the process-wide events cannot be
// attributed to either, so the id is reported empty rather than mislabelled.

/// Slot used by [`set_llm_api_key`].
const PROCESS_SLOT: u64 = 0;

struct KeyRegistry {
    next: u64,
    live: BTreeMap<u64, String>,
}

static KEYS: Mutex<KeyRegistry> = Mutex::new(KeyRegistry {
    next: PROCESS_SLOT + 1,
    live: BTreeMap::new(),
});

#[allow(
    clippy::unwrap_used,
    reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
)]
fn set_slot(slot: u64, key: Option<&str>) {
    // lock poison is unrecoverable
    let mut keys = KEYS.lock().unwrap();
    match key.filter(|k| !k.is_empty()) {
        Some(key) => {
            keys.live.insert(slot, key.to_string());
        }
        None => {
            keys.live.remove(&slot);
        }
    }
}

/// The key to report: the one distinct non-empty key among the live
/// registrations, or `None` when there is none or they disagree.
#[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
#[allow(
    clippy::unwrap_used,
    reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
)]
fn registered_key() -> Option<String> {
    // lock poison is unrecoverable
    let keys = KEYS.lock().unwrap();
    let mut values = keys.live.values();
    let first = values.next()?;
    values.all(|k| k == first).then(|| first.clone())
}

/// A live registration of the LLM API key one configuration uses, for
/// `api_key_tracking_id`. Dropping it unregisters the key.
///
/// cognee's `ComponentManager` holds one, updated whenever its configuration
/// changes, so the resolved `llm.api_key` (env, config file or code) is the one
/// reported — never the raw `LLM_API_KEY` environment variable. See the module
/// docs for how several live registrations combine.
#[derive(Debug)]
pub struct LlmKeyRegistration {
    slot: u64,
}

impl LlmKeyRegistration {
    /// Register `key` (`None` or empty registers nothing yet).
    #[allow(
        clippy::unwrap_used,
        reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
    )]
    #[must_use = "the key is unregistered when the registration is dropped"]
    pub fn new(key: Option<&str>) -> Self {
        let slot = {
            // lock poison is unrecoverable
            let mut keys = KEYS.lock().unwrap();
            let slot = keys.next;
            keys.next += 1;
            slot
        };
        set_slot(slot, key);
        Self { slot }
    }

    /// Replace the registered key (`None` or empty clears it).
    pub fn update(&self, key: Option<&str>) {
        set_slot(self.slot, key);
    }

    /// The key this registration currently holds.
    #[allow(
        clippy::unwrap_used,
        reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
    )]
    #[must_use]
    pub fn registered(&self) -> Option<String> {
        // lock poison is unrecoverable
        KEYS.lock().unwrap().live.get(&self.slot).cloned()
    }
}

impl Drop for LlmKeyRegistration {
    fn drop(&mut self) {
        set_slot(self.slot, None);
    }
}

/// Register the process-level LLM API key, for embedders that call
/// [`send_telemetry`](crate::send_telemetry) without a `ComponentManager`
/// (which registers its own key through [`LlmKeyRegistration`]). `None` or
/// empty clears it. Combines with live registrations as described above.
pub fn set_llm_api_key(key: Option<&str>) {
    set_slot(PROCESS_SLOT, key);
}

/// Test-only cache-buster re-export. Gated by `debug_assertions` so it
/// is not exposed in release builds; integration tests under
/// `crates/telemetry/tests/` need this because the `pub(crate)`
/// `reset_caches_for_test` is not reachable from a separate test
/// binary.
#[cfg(all(any(test, debug_assertions), feature = "telemetry"))]
#[doc(hidden)]
pub use inner::__test_only_reset_caches;

#[cfg(feature = "telemetry")]
pub(crate) use inner::api_key_tracking_id_with_salt as inner_api_key_tracking_id_with_salt;

#[cfg(all(test, feature = "telemetry"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
#[allow(
    clippy::disallowed_methods,
    reason = "tests drive configuration through the process env"
)]
mod ids_tests {
    use super::*;
    use crate::{TelemetryConfig, configure, current_config};
    use serial_test::serial;
    use tempfile::TempDir;

    /// Restores HOME, the process telemetry configuration and the registered
    /// LLM key around each test, so tests do not leak into each other.
    ///
    /// Soundness: every env mutation is gated by `#[serial]`, so no other test
    /// in this crate is reading or writing HOME while this guard is alive.
    struct Guard {
        home: Option<String>,
        config: TelemetryConfig,
    }
    impl Guard {
        fn snapshot() -> Self {
            Self {
                home: std::env::var("HOME").ok(),
                config: current_config(),
            }
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: the `#[serial]` attribute on every test that constructs a
            //   `Guard` guarantees no concurrent reader/writer of HOME.
            unsafe {
                match &self.home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
            let _ = &self.config;
            crate::reset_to_env();
            set_llm_api_key(None);
        }
    }

    fn with_salt(salt: Option<&str>) {
        configure(TelemetryConfig {
            api_key_tracking_salt: salt.map(str::to_string),
            ..current_config()
        });
    }

    #[test]
    #[serial]
    fn ak_format_invariants() {
        let _g = Guard::snapshot();
        with_salt(None);
        set_llm_api_key(Some("sk-test"));
        let id = get_api_key_tracking_id();
        assert!(id.starts_with("ak_"));
        assert_eq!(id.len(), 35);
        assert!(
            id[3..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    #[serial]
    fn no_registered_key_returns_empty() {
        let _g = Guard::snapshot();
        set_llm_api_key(None);
        assert_eq!(get_api_key_tracking_id(), "");
        set_llm_api_key(Some(""));
        assert_eq!(get_api_key_tracking_id(), "", "empty key clears");
    }

    /// The key is the registered one, never the `LLM_API_KEY` env var.
    #[test]
    #[serial]
    fn llm_api_key_env_var_is_not_read() {
        let _g = Guard::snapshot();
        set_llm_api_key(None);
        // SAFETY: `#[serial]` ordering, see Guard.
        unsafe { std::env::set_var("LLM_API_KEY", "sk-from-env") };
        let id = get_api_key_tracking_id();
        // SAFETY: still inside the same serial section.
        unsafe { std::env::remove_var("LLM_API_KEY") };
        assert_eq!(id, "");
    }

    /// A registration's key is reported while it lives, and disappears with
    /// it; disagreeing live keys are reported as unattributable (empty).
    #[test]
    #[serial]
    fn registrations_combine_and_unregister_on_drop() {
        let _g = Guard::snapshot();
        with_salt(None);
        set_llm_api_key(None);
        set_llm_api_key(Some("sk-a"));
        let id_a = get_api_key_tracking_id();
        set_llm_api_key(None);

        let a = super::LlmKeyRegistration::new(Some("sk-a"));
        assert_eq!(get_api_key_tracking_id(), id_a, "one live key is reported");
        let same = super::LlmKeyRegistration::new(Some("sk-a"));
        assert_eq!(
            get_api_key_tracking_id(),
            id_a,
            "agreeing keys are reported"
        );
        let b = super::LlmKeyRegistration::new(Some("sk-b"));
        assert_eq!(
            get_api_key_tracking_id(),
            "",
            "disagreeing keys are not attributed"
        );
        drop(a);
        drop(same);
        let id_b = get_api_key_tracking_id();
        assert!(id_b.starts_with("ak_") && id_b != id_a, "only b remains");
        b.update(None);
        assert_eq!(get_api_key_tracking_id(), "", "a cleared key is gone");
        drop(b);
        assert_eq!(get_api_key_tracking_id(), "", "nothing lingers after drop");
    }

    #[test]
    #[serial]
    fn full_key_used_not_visible_tail() {
        // Two keys sharing the last 4 chars must produce different
        // tracking ids — the whole key is hashed, not just the visible
        // tail. Mirror of Python's
        // `test_api_key_tracking_id_uses_full_key_not_visible_tail`.
        let _g = Guard::snapshot();
        with_salt(None);
        set_llm_api_key(Some("sk-aaaaaaaaaaaaaa1234"));
        let id_a = get_api_key_tracking_id();
        set_llm_api_key(Some("sk-bbbbbbbbbbbbbb1234"));
        let id_b = get_api_key_tracking_id();
        assert_ne!(id_a, id_b);
    }

    #[test]
    #[serial]
    fn deployment_salt_changes_id() {
        // Mirror of Python's
        // `test_api_key_tracking_id_supports_deployment_salt`.
        let _g = Guard::snapshot();
        set_llm_api_key(Some("sk-test"));
        with_salt(None);
        let default_id = get_api_key_tracking_id();
        with_salt(Some("private-salt-2026"));
        let custom_id = get_api_key_tracking_id();
        assert_ne!(default_id, custom_id);
        // Back to the default salt: the default id again (cache keyed by salt).
        with_salt(None);
        assert_eq!(get_api_key_tracking_id(), default_id);
    }

    #[test]
    #[serial]
    fn persistent_id_create_then_stable() {
        let _g = Guard::snapshot();
        let dir = TempDir::new().expect("tempdir");
        // SAFETY: `#[serial]` ordering.
        unsafe {
            std::env::set_var("HOME", dir.path());
        }
        // Wipe the cache so the test re-reads the (new) HOME.
        super::inner::reset_caches_for_test();

        let first = get_persistent_id();
        assert!(!first.is_empty());
        assert!(uuid::Uuid::parse_str(&first).is_ok());

        // File should now exist.
        let path = dir.path().join(".cognee").join(".persistent_id");
        assert!(path.exists());
        let on_disk = std::fs::read_to_string(&path).expect("persistent_id file readable");
        assert_eq!(on_disk.trim(), first);

        // Wipe cache; second call should read the same file.
        super::inner::reset_caches_for_test();
        let second = get_persistent_id();
        assert_eq!(first, second);
    }

    #[test]
    #[serial]
    fn tracking_id_overrides_anon_id() {
        let _g = Guard::snapshot();
        configure(TelemetryConfig {
            tracking_id: Some("fixed-anon-12345".into()),
            ..current_config()
        });
        super::inner::reset_caches_for_test();
        assert_eq!(get_anonymous_id(), "fixed-anon-12345");
    }

    #[test]
    #[serial]
    fn empty_tracking_id_does_not_override() {
        let _g = Guard::snapshot();
        let dir = TempDir::new().expect("tempdir");
        // SAFETY: `#[serial]` ordering.
        unsafe {
            std::env::set_var("HOME", dir.path());
        }
        configure(TelemetryConfig {
            tracking_id: Some(String::new()),
            ..current_config()
        });
        super::inner::reset_caches_for_test();
        let id = get_anonymous_id();
        assert_ne!(id, ""); // empty tracking id is treated as unset
    }
}
