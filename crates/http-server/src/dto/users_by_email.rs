//! DTOs for the users-by-email router.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Request body for `POST /api/v1/users/get-user-id`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct GetUserIdPayloadDTO {
    pub email: String,
}

/// Response body for `POST /api/v1/users/get-user-id` on success.
#[derive(Debug, Serialize, ToSchema)]
pub struct GetUserIdResponseDTO {
    pub user_id: Uuid,
}
