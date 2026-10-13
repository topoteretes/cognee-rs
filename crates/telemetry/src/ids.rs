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

#[cfg(feature = "telemetry")]
mod inner {
    use hmac::Hmac;
    use once_cell::sync::Lazy;
    use pbkdf2::pbkdf2;
    use sha2::Sha256;
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
        if let Some(v) = crate::current_config().tracking_id
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

    /// The registered LLM API key (see [`set_llm_api_key`]).
    static LLM_API_KEY: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

    /// Last computed `(key, salt, tracking id)`. PBKDF2 at 100k iterations
    /// costs tens of milliseconds, so it runs once per distinct key/salt
    /// pair instead of once per event.
    #[allow(clippy::type_complexity)]
    static TRACKING_CACHE: Lazy<Mutex<Option<(String, Vec<u8>, String)>>> =
        Lazy::new(|| Mutex::new(None));

    /// Register the LLM API key hashed into `api_key_tracking_id`.
    #[allow(
        clippy::unwrap_used,
        reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
    )]
    pub fn set_llm_api_key(key: Option<&str>) {
        let key = key.filter(|k| !k.is_empty()).map(str::to_string);
        // lock poison is unrecoverable
        *LLM_API_KEY.lock().unwrap() = key;
    }

    /// PBKDF2-HMAC-SHA256 hash of the registered LLM API key with a
    /// configurable salt. Returns `"ak_<32-hex-chars>"` when a non-empty key
    /// is registered, empty string otherwise.
    ///
    /// Evaluated at every event (decision 11) against the key registered
    /// *now*, so re-registering a key (a config change) takes effect on the
    /// next event; the hash itself is cached per key and salt.
    #[allow(
        clippy::expect_used,
        reason = "PBKDF2 with dklen=16 ≤ SHA256 output=32 is mathematically guaranteed to succeed"
    )]
    #[allow(
        clippy::unwrap_used,
        reason = "Mutex lock poison is unrecoverable — no meaningful recovery possible"
    )]
    pub fn get_api_key_tracking_id() -> String {
        // lock poison is unrecoverable
        let Some(key) = LLM_API_KEY.lock().unwrap().clone() else {
            return String::new();
        };
        let salt: Vec<u8> = crate::current_config()
            .api_key_tracking_salt
            .map(String::into_bytes)
            .unwrap_or_else(|| DEFAULT_SALT.to_vec());

        // lock poison is unrecoverable
        let mut cache = TRACKING_CACHE.lock().unwrap();
        if let Some((k, s, id)) = cache.as_ref()
            && *k == key
            && *s == salt
        {
            return id.clone();
        }
        let mut out = [0u8; DKLEN];
        // PBKDF2 with dklen ≤ HMAC-SHA256 output (32) cannot fail.
        pbkdf2::<Hmac<Sha256>>(key.as_bytes(), &salt, ITERATIONS, &mut out)
            .expect("dklen 16 ≤ Sha256 output 32 — invariant holds");
        let id = format!("ak_{}", hex::encode(out));
        *cache = Some((key, salt, id.clone()));
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
    /// Noop stub: nothing is ever hashed when the `telemetry` feature is
    /// disabled, so the key is not retained.
    pub fn set_llm_api_key(_key: Option<&str>) {}
}

/// Project-local anonymous identifier (uuid4, file-backed at
/// `<project_root>/.anon_id`). Honours the `tracking_id` override of the
/// process [`TelemetryConfig`](crate::TelemetryConfig) (`TRACKING_ID`).
pub use inner::get_anonymous_id;
/// Deterministic PBKDF2-HMAC-SHA256 hash of the LLM API key registered with
/// [`set_llm_api_key`], with a configurable salt. Stable across machines for
/// the same key. Empty when no key is registered.
pub use inner::get_api_key_tracking_id;
/// Machine-local persistent identifier (uuid4, file-backed at
/// `~/.cognee/.persistent_id`).
pub use inner::get_persistent_id;
/// Register the LLM API key whose hash is reported as `api_key_tracking_id`.
///
/// Process-wide: the most recent registration wins. cognee's
/// `ComponentManager` registers the resolved `llm.api_key` of its
/// configuration when it is created and whenever that configuration changes,
/// so the key set from code, a binding or the CLI config store is the one
/// hashed (never the raw `LLM_API_KEY` environment variable). An empty or
/// `None` key clears the registration. Embedders that call
/// [`send_telemetry`](crate::send_telemetry) without a `ComponentManager`
/// register the key themselves.
pub use inner::set_llm_api_key;

/// Test-only cache-buster re-export. Gated by `debug_assertions` so it
/// is not exposed in release builds; integration tests under
/// `crates/telemetry/tests/` need this because the `pub(crate)`
/// `reset_caches_for_test` is not reachable from a separate test
/// binary.
#[cfg(all(any(test, debug_assertions), feature = "telemetry"))]
#[doc(hidden)]
pub use inner::__test_only_reset_caches;

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
            configure(self.config.clone());
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
