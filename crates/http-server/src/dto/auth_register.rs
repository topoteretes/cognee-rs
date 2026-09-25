//! DTOs for the auth register router.

use serde::Deserialize;
use utoipa::ToSchema;

/// Request body for `POST /api/v1/auth/register`.
/// Pydantic source: `UserCreate(BaseUserCreate)` in `User.py:52-55`.
///
/// `safe=True` semantics apply: `is_active`, `is_superuser`, `is_verified` are
/// accepted but silently coerced to server-controlled values (`true`, `false`, `true`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct RegisterPayloadDTO {
    pub email: String,
    pub password: String,
    /// Accepted but silently coerced to `true` by `safe=True` logic.
    #[serde(default)]
    pub is_active: Option<bool>,
    /// Accepted but silently coerced to `false`.
    #[serde(default)]
    pub is_superuser: Option<bool>,
    /// cognee default is `true`; accepted but stripped by `safe=True`.
    #[serde(default)]
    pub is_verified: Option<bool>,
}
