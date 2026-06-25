//! Default user management.
//!
//! Mirrors Python's `get_default_user()` / `create_default_user()`.

use cognee_database::DatabaseError;
use cognee_models::User;
use uuid::Uuid;

/// Materialise the OSS default user **without** touching the database.
///
/// The OSS build does not implement authentication (the `users` table is
/// touched only by the closed cloud build via its own DB-backed
/// `get_or_create_default_user`), so this function constructs an in-memory
/// [`User`] record from the configured `default_user_email`.
///
/// The id is `Uuid::new_v5(&NAMESPACE_OID, default_user_email.as_bytes())`
/// — the same derivation used by the Python reference SDK
/// (`uuid5(NAMESPACE_OID, email)`). **This MUST NOT change** without a
/// corresponding update in the Python SDK; cross-SDK parity tests
/// (`e2e-cross-sdk` and the Neon binding's `sdk_handle.test.ts`) assert
/// this exact derivation.
///
/// Takes `&str` rather than `&Settings` so callers can read `Settings`
/// under an `RwLock` guard, snapshot the email, **drop the guard**, and
/// then `.await` — `std::sync::RwLockReadGuard` is `!Send` and would
/// otherwise poison the surrounding future's `Send` bound.
///
/// Kept `async` and fallible so call sites stay uniform with the
/// closed-build replacement (which performs real DB I/O and can fail).
pub async fn get_or_create_default_user(default_user_email: &str) -> Result<User, DatabaseError> {
    let id = Uuid::new_v5(&Uuid::NAMESPACE_OID, default_user_email.as_bytes());
    Ok(User {
        id,
        email: default_user_email.to_string(),
        is_active: true,
        is_superuser: true,
        tenant_id: None,
        created_at: chrono::Utc::now(),
        updated_at: None,
    })
}
