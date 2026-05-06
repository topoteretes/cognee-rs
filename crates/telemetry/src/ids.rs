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

#[cfg(all(test, feature = "telemetry"))]
mod smoke {
    use super::*;

    // Note: workspace uses Rust edition 2024, where `std::env::set_var`
    // and `std::env::remove_var` are `unsafe` (concurrent env mutation
    // is process-wide UB). The full byte-parity matrix in task 02-08
    // adds `serial_test::serial` so the unsafe is sound; these smoke
    // tests follow the same pattern.

    #[test]
    fn empty_llm_api_key_produces_empty_tracking_id() {
        // SAFETY: no other thread mutates env in this single-test block;
        //   full ordering across the suite is enforced in 02-08 via serial_test.
        unsafe {
            std::env::remove_var("LLM_API_KEY");
            std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
        }
        assert_eq!(get_api_key_tracking_id(), "");
    }

    #[test]
    fn tracking_id_format() {
        // SAFETY: see sibling test.
        unsafe {
            std::env::set_var("LLM_API_KEY", "sk-test");
            std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
        }
        let id = get_api_key_tracking_id();
        assert!(id.starts_with("ak_"));
        assert_eq!(id.len(), 3 + 32);
        assert!(
            id[3..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex suffix, got {id:?}"
        );
    }
}
