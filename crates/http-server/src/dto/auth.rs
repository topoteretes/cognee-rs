//! DTOs for the auth router (login / logout / me).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request body for `POST /api/v1/auth/login`.
/// Wire format: `application/x-www-form-urlencoded` (OAuth2 password grant).
/// Pydantic source: `fastapi.security.OAuth2PasswordRequestForm`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginPayloadDTO {
    /// Email address of the user. fastapi-users keeps the OAuth2 spelling.
    pub username: String,
    pub password: String,
    /// Always ignored; accepted for OAuth2 compliance.
    #[serde(default)]
    pub grant_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
}

/// Successful login response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponseDTO {
    /// JWT (HS256, audience `fastapi-users:auth`). Same value as the cookie.
    pub access_token: String,
    /// Always the literal string `"bearer"`.
    pub token_type: &'static str,
}

/// Response body for `GET /api/v1/auth/me` — cognee's narrow shape (NOT fastapi-users `UserRead`).
/// Pydantic source: ad-hoc dict in `get_auth_router.py:52-54`.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeShortResponseDTO {
    pub email: String,
}

/// Response body for `POST /api/v1/auth/logout`. Always `{}`.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct LogoutResponseDTO {}
