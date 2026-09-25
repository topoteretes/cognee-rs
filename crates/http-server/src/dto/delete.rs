//! DTOs for `DELETE /api/v1/delete` (deprecated endpoint).

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Query string for `DELETE /api/v1/delete` (deprecated).
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "snake_case")]
pub struct DeleteQuery {
    /// UUID of the data row to delete.
    pub data_id: Uuid,

    /// UUID of the dataset that owns `data_id`.
    pub dataset_id: Uuid,

    /// `"soft"` (default) or `"hard"`. Hard mode also removes degree-one
    /// entity nodes; Python documents it as dangerous.
    #[serde(default = "default_mode")]
    pub mode: DeleteMode,

    /// If true and the dataset becomes empty after deletion, also delete
    /// the dataset row.
    #[serde(default)]
    pub delete_dataset_if_empty: bool,
}

fn default_mode() -> DeleteMode {
    DeleteMode::Soft
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum DeleteMode {
    #[default]
    Soft,
    Hard,
}

/// Response body. Python returns `{"status": "success"}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteSuccessResponseDTO {
    pub status: String,
}

impl DeleteSuccessResponseDTO {
    pub fn ok() -> Self {
        Self {
            status: "success".to_owned(),
        }
    }
}

/// `{error}` envelope for 409.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteErrorResponseDTO {
    pub error: String,
}
