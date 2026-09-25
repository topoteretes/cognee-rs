//! Shared user DTOs (UserReadDTO, UserUpdatePayloadDTO, InvalidPasswordDetailDTO).
//! Used by auth_register, users, and users_by_email routers.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Full user record returned by most user-management endpoints.
/// Matches Python's `UserRead(BaseUser)` with cognee's `tenant_id` extension.
/// Source: `cognee/modules/users/models/User.py:46-49`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserReadDTO {
    pub id: Uuid,
    pub email: String,
    pub is_active: bool,
    pub is_superuser: bool,
    pub is_verified: bool,
    pub tenant_id: Option<Uuid>,
    pub parent_user_id: Option<Uuid>,
}

/// PATCH body for `/me` and `/{id}`. All fields optional.
/// Pydantic source: `fastapi_users.schemas.BaseUserUpdate`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UserUpdatePayloadDTO {
    /// New cleartext password. Validated before hashing.
    pub password: Option<String>,
    /// New email. Must be unique across users.
    pub email: Option<String>,
    /// `safe=True` on `/me` — silently stripped; allowed on `/{id}` for superusers.
    #[serde(default)]
    pub is_active: Option<bool>,
    /// `safe=True` on `/me` — silently stripped.
    #[serde(default)]
    pub is_superuser: Option<bool>,
    /// `safe=True` on `/me` — silently stripped.
    #[serde(default)]
    pub is_verified: Option<bool>,
}
