//! Identity-layer helpers for `send_telemetry`.
//!
//! Three layers, each used as a key in the proxy payload:
//!
//! - [`get_anonymous_id`]: project-local uuid4, file-backed at
//!   `<project_root>/.anon_id`. Honours `TRACKING_ID` env override.
//! - [`get_persistent_id`]: machine-local uuid4, file-backed at
//!   `~/.cognee/.persistent_id`. Survives `forget(everything=True)`.
//! - [`get_api_key_tracking_id`]: deterministic PBKDF2-HMAC-SHA256
//!   hash of `LLM_API_KEY` with a configurable salt. Stable across
//!   machines for the same key.

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
    pub fn __test_only_reset_caches() {
        // lock poison is unrecoverable
        *ANON_ID.lock().unwrap() = None;
        // lock poison is unrecoverable
        *PERSISTENT_ID.lock().unwrap() = None;
    }

    /// Project-local anonymous identifier.
    pub fn get_anonymous_id() -> String {
        if let Ok(v) = std::env::var("TRACKING_ID")
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

    /// PBKDF2-HMAC-SHA256 hash of `LLM_API_KEY` with a configurable
    /// salt. Returns `"ak_<32-hex-chars>"` for non-empty keys, empty
    /// string otherwise.
    ///
    /// Read at every call (decision 11) — no caching, because tests
    /// and consumers may set `LLM_API_KEY` in-process at runtime.
    pub fn get_api_key_tracking_id() -> String {
        let key = std::env::var("LLM_API_KEY").unwrap_or_default();
        if key.is_empty() {
            return String::new();
        }
        let salt: Vec<u8> = std::env::var("TELEMETRY_API_KEY_TRACKING_SALT")
            .map(|s| s.into_bytes())
            .unwrap_or_else(|_| DEFAULT_SALT.to_vec());
        let mut out = [0u8; DKLEN];
        // PBKDF2 with dklen ≤ HMAC-SHA256 output (32) cannot fail.
        pbkdf2::<Hmac<Sha256>>(key.as_bytes(), &salt, ITERATIONS, &mut out)
            .expect("dklen 16 ≤ Sha256 output 32 — invariant holds");
        format!("ak_{}", hex::encode(out))
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
/// `<project_root>/.anon_id`). Honours `TRACKING_ID` env override.
pub use inner::get_anonymous_id;
/// Deterministic PBKDF2-HMAC-SHA256 hash of `LLM_API_KEY` with a
/// configurable salt. Stable across machines for the same key.
pub use inner::get_api_key_tracking_id;
/// Machine-local persistent identifier (uuid4, file-backed at
/// `~/.cognee/.persistent_id`).
pub use inner::get_persistent_id;

/// Test-only cache-buster re-export. Gated by `debug_assertions` so it
/// is not exposed in release builds; integration tests under
/// `crates/telemetry/tests/` need this because the `pub(crate)`
/// `reset_caches_for_test` is not reachable from a separate test
/// binary.
#[cfg(all(any(test, debug_assertions), feature = "telemetry"))]
#[doc(hidden)]
pub use inner::__test_only_reset_caches;

#[cfg(all(test, feature = "telemetry"))]
mod ids_tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    /// Save and restore HOME / TRACKING_ID / LLM_API_KEY around each
    /// test so we don't pollute the suite environment.
    ///
    /// Soundness: every env mutation is gated by `#[serial]`, so no
    /// other test in this crate is reading or writing the same vars
    /// while this guard is alive. The `Drop` impl restores the
    /// previous values inside the same serial section.
    struct EnvGuard {
        home: Option<String>,
        tracking_id: Option<String>,
        llm_api_key: Option<String>,
        salt: Option<String>,
    }
    impl EnvGuard {
        fn snapshot() -> Self {
            Self {
                home: std::env::var("HOME").ok(),
                tracking_id: std::env::var("TRACKING_ID").ok(),
                llm_api_key: std::env::var("LLM_API_KEY").ok(),
                salt: std::env::var("TELEMETRY_API_KEY_TRACKING_SALT").ok(),
            }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in [
                ("HOME", &self.home),
                ("TRACKING_ID", &self.tracking_id),
                ("LLM_API_KEY", &self.llm_api_key),
                ("TELEMETRY_API_KEY_TRACKING_SALT", &self.salt),
            ] {
                // SAFETY: the `#[serial]` attribute on every test that
                //   constructs an `EnvGuard` guarantees no concurrent
                //   reader/writer of these vars while Drop runs.
                unsafe {
                    match v {
                        Some(v) => std::env::set_var(k, v),
                        None => std::env::remove_var(k),
                    }
                }
            }
        }
    }

    #[test]
    #[serial]
    fn ak_format_invariants() {
        let _g = EnvGuard::snapshot();
        // SAFETY: `#[serial]` orders this test against all other
        //   env-mutating tests in the crate; `_g` restores prior
        //   values on drop.
        unsafe {
            std::env::set_var("LLM_API_KEY", "sk-test");
            std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
        }
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
    fn empty_llm_api_key_returns_empty() {
        let _g = EnvGuard::snapshot();
        // SAFETY: see ak_format_invariants.
        unsafe {
            std::env::remove_var("LLM_API_KEY");
        }
        assert_eq!(get_api_key_tracking_id(), "");
    }

    #[test]
    #[serial]
    fn full_key_used_not_visible_tail() {
        // Two keys sharing the last 4 chars must produce different
        // tracking ids — the whole key is hashed, not just the visible
        // tail. Mirror of Python's
        // `test_api_key_tracking_id_uses_full_key_not_visible_tail`.
        let _g = EnvGuard::snapshot();
        // SAFETY: `#[serial]` ordering, see EnvGuard.
        unsafe {
            std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
            std::env::set_var("LLM_API_KEY", "sk-aaaaaaaaaaaaaa1234");
        }
        let id_a = get_api_key_tracking_id();
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("LLM_API_KEY", "sk-bbbbbbbbbbbbbb1234");
        }
        let id_b = get_api_key_tracking_id();
        assert_ne!(id_a, id_b);
    }

    #[test]
    #[serial]
    fn deployment_salt_changes_id() {
        // Mirror of Python's
        // `test_api_key_tracking_id_supports_deployment_salt`.
        let _g = EnvGuard::snapshot();
        // SAFETY: `#[serial]` ordering.
        unsafe {
            std::env::set_var("LLM_API_KEY", "sk-test");
            std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
        }
        let default_id = get_api_key_tracking_id();
        // SAFETY: still inside the same serial section.
        unsafe {
            std::env::set_var("TELEMETRY_API_KEY_TRACKING_SALT", "private-salt-2026");
        }
        let custom_id = get_api_key_tracking_id();
        assert_ne!(default_id, custom_id);
    }

    #[test]
    #[serial]
    fn persistent_id_create_then_stable() {
        let _g = EnvGuard::snapshot();
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
    fn tracking_id_env_overrides_anon_id() {
        let _g = EnvGuard::snapshot();
        // SAFETY: `#[serial]` ordering.
        unsafe {
            std::env::set_var("TRACKING_ID", "fixed-anon-12345");
        }
        super::inner::reset_caches_for_test();
        assert_eq!(get_anonymous_id(), "fixed-anon-12345");
    }

    #[test]
    #[serial]
    fn tracking_id_empty_env_does_not_override() {
        let _g = EnvGuard::snapshot();
        let dir = TempDir::new().expect("tempdir");
        // SAFETY: `#[serial]` ordering.
        unsafe {
            std::env::set_var("HOME", dir.path());
            std::env::set_var("TRACKING_ID", "");
        }
        super::inner::reset_caches_for_test();
        let id = get_anonymous_id();
        assert_ne!(id, ""); // empty TRACKING_ID is treated as unset
    }
}
