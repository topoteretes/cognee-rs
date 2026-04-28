//! DTOs for the `/api/v1/checks` family.
//!
//! Differ from the canonical `ApiError` envelope because Cognee's
//! configuration errors include the exception class name as a top-level
//! `name` field. We expose this DTO instead of synthesizing it inside
//! `ApiError::IntoResponse` to keep the parity contract explicit.

use serde::Serialize;
use utoipa::ToSchema;

/// 400 / 503 body for `POST /api/v1/checks/connection`.
///
/// `name` is one of:
/// - `"CloudApiKeyMissingError"` (400)
/// - `"CloudConnnectionError"` (503)  ← sic, Python typo replicated for parity
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CloudConfigErrorDTO {
    pub detail: String,
    pub name: String,
}
