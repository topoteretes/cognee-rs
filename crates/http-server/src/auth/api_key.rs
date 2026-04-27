//! API-key generation, hashing, label computation, and lookup.
//!
//! `HASH_API_KEY` (default `false`) controls whether keys are stored in
//! plaintext or as their SHA-256 hex digest.  The Rust default matches Python.

use rand::{RngCore, thread_rng};
use sha2::{Digest, Sha256};

use super::context::AuthContext;
use cognee_database::AuthUser;

/// Encode bytes as lowercase hex string.
fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Generate a fresh raw API key: 32 random bytes → 64 lowercase hex chars.
///
/// Matches Python's `secrets.token_hex(32)`.
pub fn generate_raw_key() -> String {
    let mut bytes = [0u8; 32];
    thread_rng().fill_bytes(&mut bytes);
    bytes_to_hex(&bytes)
}

/// Compute the value to store in `user_api_key.api_key`.
///
/// When `hash_api_key == true` → SHA-256 hex of the raw key.
/// When `hash_api_key == false` → the raw key unchanged.
pub fn prepare_for_storage(raw: &str, ctx: &AuthContext) -> String {
    if ctx.hash_api_key {
        sha256_hex(raw.as_bytes())
    } else {
        raw.to_owned()
    }
}

/// Compute the display label for a raw key: first 8 chars + `"****"`.
///
/// Matches Python's `create_api_key.py:35`.
pub fn compute_label(raw: &str) -> String {
    format!("{}****", &raw[..8])
}

/// SHA-256 hex helper (public so callers can use it for `password_fgpt`).
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    bytes_to_hex(&hasher.finalize())
}

/// Look up the user who owns the given raw key header value.
///
/// Timing note: The DB query itself determines the equality outcome.  A
/// further application-level constant-time comparison would require returning
/// the stored key from `find_user_by_api_key`, which is a larger interface
/// change deferred to a follow-up.  The DB round-trip dominates timing anyway.
/// When `HASH_API_KEY=true`, the header value is SHA-256-hashed before the
/// query, so the stored plaintext never touches application memory.
pub async fn lookup_api_key(header_value: &str, ctx: &AuthContext) -> Option<AuthUser> {
    let prepared = if ctx.hash_api_key {
        sha256_hex(header_value.as_bytes())
    } else {
        header_value.to_owned()
    };

    ctx.user_repo
        .find_user_by_api_key(&prepared)
        .await
        .ok()
        .flatten()
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_key_is_64_hex_chars() {
        let key = generate_raw_key();
        assert_eq!(key.len(), 64, "raw key must be 64 chars");
        assert!(
            key.chars().all(|c| c.is_ascii_hexdigit()),
            "raw key must be hex: {key}"
        );
        // lower-case only
        assert_eq!(key, key.to_lowercase());
    }

    #[test]
    fn prepare_for_storage_passthrough_when_not_hashed() {
        use crate::auth::context::tests::{NopApiKeyRepo, NopUserRepo};
        use crate::config::{Environment, HttpServerConfig};
        use std::sync::Arc;
        let cfg = HttpServerConfig {
            env: Environment::Dev,
            ..Default::default()
        };
        let mut ctx = AuthContext::from_env(&cfg, Arc::new(NopUserRepo), Arc::new(NopApiKeyRepo))
            .expect("ctx");
        ctx.hash_api_key = false;
        let raw = "abc123";
        assert_eq!(prepare_for_storage(raw, &ctx), raw);
    }

    #[test]
    fn prepare_for_storage_sha256_when_hashed() {
        use crate::auth::context::tests::{NopApiKeyRepo, NopUserRepo};
        use crate::config::{Environment, HttpServerConfig};
        use std::sync::Arc;
        let cfg = HttpServerConfig {
            env: Environment::Dev,
            ..Default::default()
        };
        let mut ctx = AuthContext::from_env(&cfg, Arc::new(NopUserRepo), Arc::new(NopApiKeyRepo))
            .expect("ctx");
        ctx.hash_api_key = true;
        let raw = "abc123";
        let stored = prepare_for_storage(raw, &ctx);
        assert_eq!(stored.len(), 64, "SHA-256 hex must be 64 chars");
        assert_ne!(stored, raw);
    }

    #[test]
    fn compute_label_format() {
        let raw = "a1b2c3d4e5f60011223344556677889900";
        let label = compute_label(raw);
        assert_eq!(label, "a1b2c3d4****");
    }
}
