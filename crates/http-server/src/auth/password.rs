//! Password hashing and verification.
//!
//! New passwords use argon2id (OWASP 2024 baseline).
//! Legacy bcrypt hashes (`$2a$`, `$2b$`, `$2y$`) still verify but trigger a
//! re-hash to argon2id on successful login.

use argon2::{
    Argon2, Params, PasswordHasher, PasswordVerifier,
    password_hash::{PasswordHash, SaltString, rand_core::OsRng},
};

// ─── Error types ─────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("hash error: {0}")]
    Hash(String),
    #[error("verify error: {0}")]
    Verify(String),
    #[error("unsupported hash algorithm")]
    UnsupportedAlgorithm,
}

/// Outcome of a successful password verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Password verified; no re-hash needed.
    Ok,
    /// Password verified; stored hash is bcrypt → client should re-hash to argon2id.
    NeedsRehash,
}

// ─── Validation ───────────────────────────────────────────────────────────────

/// Reason a password was rejected by `validate_password`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum InvalidPasswordReason {
    #[error("Password must not be empty")]
    Empty,
    #[error("Password should not contain e-mail")]
    ContainsEmail,
}

/// Mirror of fastapi-users' default `BaseUserManager.validate_password`:
/// - Must be non-empty.
/// - Must not contain the user's email as a substring (case-insensitive).
///
/// **No minimum length rule** — stock fastapi-users and cognee don't apply one.
pub fn validate_password(password: &str, email: &str) -> Result<(), InvalidPasswordReason> {
    if password.is_empty() {
        return Err(InvalidPasswordReason::Empty);
    }
    if password
        .to_ascii_lowercase()
        .contains(&email.to_ascii_lowercase())
    {
        return Err(InvalidPasswordReason::ContainsEmail);
    }
    Ok(())
}

// ─── Hash new password ────────────────────────────────────────────────────────

/// Hash a new password with argon2id using OWASP 2024 baseline params
/// (`m=19456`, `t=2`, `p=1`).
pub fn hash_new_password(plain: &str) -> Result<String, PasswordError> {
    let params = Params::new(19456, 2, 1, None)
        .map_err(|e: argon2::Error| PasswordError::Hash(e.to_string()))?;
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let salt = SaltString::generate(&mut OsRng);
    argon2
        .hash_password(plain.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e: argon2::password_hash::Error| PasswordError::Hash(e.to_string()))
}

// ─── Verify password ──────────────────────────────────────────────────────────

/// Verify a cleartext password against a stored hash (argon2id or bcrypt).
///
/// Returns `VerifyOutcome::NeedsRehash` when the stored hash is bcrypt so the
/// caller can transparently upgrade to argon2id.
pub fn verify_password(stored: &str, plain: &str) -> Result<VerifyOutcome, PasswordError> {
    if stored.starts_with("$2a$") || stored.starts_with("$2b$") || stored.starts_with("$2y$") {
        // Legacy bcrypt path
        let ok = bcrypt::verify(plain, stored).map_err(|e| PasswordError::Verify(e.to_string()))?;
        if ok {
            Ok(VerifyOutcome::NeedsRehash)
        } else {
            Err(PasswordError::Verify("password mismatch".into()))
        }
    } else if stored.starts_with("$argon2id$") {
        let parsed = PasswordHash::new(stored)
            .map_err(|e: argon2::password_hash::Error| PasswordError::Verify(e.to_string()))?;
        let params = Params::new(19456, 2, 1, None)
            .map_err(|e: argon2::Error| PasswordError::Verify(e.to_string()))?;
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        argon2
            .verify_password(plain.as_bytes(), &parsed)
            .map(|_| VerifyOutcome::Ok)
            .map_err(|_| PasswordError::Verify("password mismatch".into()))
    } else if stored.is_empty() {
        // Empty hash (default_user or pre-migration row) — always fails.
        Err(PasswordError::Verify("no password set".into()))
    } else {
        Err(PasswordError::UnsupportedAlgorithm)
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn argon2id_round_trip() {
        let hash = hash_new_password("correct horse battery staple").expect("hash");
        assert!(hash.starts_with("$argon2id$"));
        let outcome = verify_password(&hash, "correct horse battery staple").expect("verify");
        assert_eq!(outcome, VerifyOutcome::Ok);
    }

    #[test]
    fn argon2id_wrong_password_fails() {
        let hash = hash_new_password("correct horse battery staple").expect("hash");
        let result = verify_password(&hash, "wrong password");
        assert!(result.is_err());
    }

    #[test]
    fn validate_password_rejects_empty() {
        let result = validate_password("", "user@example.com");
        assert!(matches!(result, Err(InvalidPasswordReason::Empty)));
    }

    #[test]
    fn validate_password_rejects_email_substring() {
        let result = validate_password("myuser@example.com_pass", "user@example.com");
        assert!(matches!(result, Err(InvalidPasswordReason::ContainsEmail)));
    }

    #[test]
    fn validate_password_accepts_valid() {
        let result = validate_password("a_strong_password_XYZ", "user@example.com");
        assert!(result.is_ok());
    }

    #[test]
    fn verify_bcrypt_hash_fixture() {
        // This bcrypt hash is for "correct horse battery staple" (rounds=12).
        // Generated via: bcrypt::hash("correct horse battery staple", 12)
        let hash = "$2b$12$3OvDMjFb.6erL8oHgcZJ6eoLxyyOLrToIkA46US1R1Nv7mlBq8rfa";
        let outcome = verify_password(hash, "correct horse battery staple");
        match outcome {
            Ok(VerifyOutcome::NeedsRehash) => { /* expected: bcrypt → argon2id upgrade needed */ }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
}
