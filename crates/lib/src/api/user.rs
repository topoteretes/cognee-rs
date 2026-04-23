//! Default user management.
//!
//! Mirrors Python's `get_default_user()` / `create_default_user()`.

use chrono::Utc;
use cognee_database::{DatabaseError, UserDb};
use cognee_models::User;
use uuid::Uuid;

/// Retrieve the default user, creating it if it doesn't exist.
///
/// Uses deterministic UUID5 from the email so re-runs are idempotent.
/// This mirrors Python's `get_default_user()` / `create_default_user()`.
pub async fn get_or_create_default_user(
    db: &dyn UserDb,
    email: &str,
) -> Result<User, DatabaseError> {
    if let Some(user) = db.get_user_by_email(email).await? {
        return Ok(user);
    }
    let user = User {
        id: Uuid::new_v5(&Uuid::NAMESPACE_OID, email.as_bytes()),
        email: email.to_string(),
        is_active: true,
        is_superuser: true,
        tenant_id: None,
        created_at: Utc::now(),
        updated_at: None,
    };
    db.create_user(&user).await
}
