//! DTOs for `POST /api/v1/improve`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Re-export shared DTO.
pub use super::pipeline_run::PipelineRunInfoDTO;

// ─── Request ──────────────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/improve`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ImprovePayloadDTO {
    /// Dataset name. Either `dataset_id` or `dataset_name` is required.
    #[serde(default)]
    pub dataset_name: Option<String>,

    /// Dataset UUID. Empty string is treated as absent.
    #[serde(default, rename = "datasetId")]
    pub dataset_id: super::util::DatasetIdRef,

    /// When `true`, dispatch to the background and return immediately.
    #[serde(default)]
    pub run_in_background: Option<bool>,
}
