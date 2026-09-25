//! DTOs for the verify router.

use serde::Deserialize;
use utoipa::ToSchema;

/// Request body for `POST /api/v1/auth/request-verify-token`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RequestVerifyTokenPayloadDTO {
    pub email: String,
}

/// Request body for `POST /api/v1/auth/verify`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifyPayloadDTO {
    pub token: String,
}
