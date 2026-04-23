use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A registered user. Corresponds to Python `cognee.modules.users.models.User`.
///
/// Fields intentionally omit `hashed_password` -- the Rust SDK does not
/// implement authentication (see non-goal note in the gap doc). Password
/// handling is delegated to whatever HTTP/auth layer sits on top.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub is_active: bool,
    pub is_superuser: bool,
    /// The user's currently-selected tenant (can be `None` for the
    /// single-user default tenant).
    pub tenant_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}
