//! DTOs for `POST /api/v1/memify`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Re-export shared DTO.
pub use super::pipeline_run::PipelineRunInfoDTO;

// ─── Request ──────────────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/memify`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct MemifyPayloadDTO {
    /// Dataset name. Either `dataset_id` or `dataset_name` is required.
    #[serde(default)]
    pub dataset_name: Option<String>,

    /// Dataset UUID. Empty string is treated as absent.
    /// Either `dataset_id` or `dataset_name` is required.
    #[serde(default, rename = "datasetId")]
    pub dataset_id: super::util::DatasetIdRef,

    /// When `true`, dispatch to the background and return immediately.
    #[serde(default)]
    pub run_in_background: Option<bool>,
}
