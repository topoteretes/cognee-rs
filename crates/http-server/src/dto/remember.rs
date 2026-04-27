//! DTOs for `POST /api/v1/remember`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Re-export shared DTO.
pub use super::pipeline_run::PipelineRunInfoDTO;

// ─── Form fields ─────────────────────────────────────────────────────────────

/// Parsed multipart form for `POST /api/v1/remember`.
///
/// Populated by the handler iterating over multipart parts; not derived via
/// serde (multipart extraction is manual).
#[derive(Debug, Default)]
pub struct RememberFormDTO {
    /// camelCase wire name: `datasetName`.
    pub dataset_name: Option<String>,
    /// camelCase wire name: `datasetId`. Empty string → `None`.
    pub dataset_id: super::util::DatasetIdRef,
    /// Repeated form field.  `[""]` is translated to `None` after extraction.
    pub node_set: Option<Vec<String>>,
    /// `"true"` / `"1"` → `true`.
    pub run_in_background: Option<bool>,
    pub custom_prompt: Option<String>,
    pub chunks_per_batch: Option<u32>,
}

// ─── Uploaded file part ───────────────────────────────────────────────────────

/// One spooled file part from the multipart body.
pub struct UploadedFilePart {
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub temp_path: std::path::PathBuf,
    pub byte_count: u64,
}

// ─── Response ─────────────────────────────────────────────────────────────────

/// Response body for `POST /api/v1/remember`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RememberResultDTO {
    pub status: String,
    pub pipeline_run_id: uuid::Uuid,
    pub dataset_id: uuid::Uuid,
    pub dataset_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
