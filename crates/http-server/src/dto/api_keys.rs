//! DTOs for the api-keys router.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Request body for `POST /api/v1/auth/api-keys`.
/// Pydantic source: `ApiKeyCreationPayload(InDTO)`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ApiKeyCreationPayloadDTO {
    /// User-supplied display label; nullable.
    #[serde(default)]
    pub name: Option<String>,
}

/// One row in the response array of `GET /api/v1/auth/api-keys`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyListItemDTO {
    /// Raw 64-hex value when `HASH_API_KEY=false` (default),
    /// or the literal `"************"` (12 asterisks) when `HASH_API_KEY=true`.
    pub key: String,
    /// First 8 chars of the original raw key + `"****"`.
    pub label: String,
    /// User-supplied display label; nullable.
    pub name: Option<String>,
    pub id: Uuid,
}

/// Response body for `POST /api/v1/auth/api-keys`. Returned exactly once.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyCreatedDTO {
    /// Raw 64-hex value. NEVER returned again — clients must persist it.
    pub key: String,
    pub label: String,
    pub name: Option<String>,
    pub id: Uuid,
}

/// 400 error envelope unique to the api-keys router.
/// Wire shape: `{"error": {"message": "..."}}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyErrorEnvelopeDTO {
    pub error: ApiKeyErrorDetail,
}

/// Inner error detail for the api-keys router envelope.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyErrorDetail {
    pub message: String,
}
