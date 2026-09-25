//! DTOs for the reset-password router.

use serde::Deserialize;
use utoipa::ToSchema;

/// Request body for `POST /api/v1/auth/forgot-password`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ForgotPasswordPayloadDTO {
    pub email: String,
}

/// Request body for `POST /api/v1/auth/reset-password`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetPasswordPayloadDTO {
    pub token: String,
    pub password: String,
}
