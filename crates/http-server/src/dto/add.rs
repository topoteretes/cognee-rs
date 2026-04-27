//! DTOs for `POST /api/v1/add`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Multipart form for `POST /api/v1/add`.
///
/// `axum::extract::Multipart` does not derive into a struct directly; this DTO
/// exists primarily for OpenAPI documentation. The handler reads parts
/// explicitly, populating an internal `AddRequest` that mirrors this shape.
#[derive(Debug, ToSchema)]
#[allow(dead_code)] // OpenAPI-only
pub struct AddMultipart {
    /// One or more files. Empty (zero parts) is allowed but is then a no-op.
    #[schema(format = "binary")]
    pub data: Vec<Vec<u8>>,

    /// Dataset name. Either this or `dataset_id` is required.
    #[schema(example = "research_papers", rename = "datasetName")]
    pub dataset_name: Option<String>,

    /// Dataset UUID. Either this or `dataset_name` is required. Empty string
    /// is treated as absent.
    #[schema(example = "", rename = "datasetId")]
    pub dataset_id: Option<String>,

    /// Repeated form field; each entry is one node-set tag.
    #[schema(example = json!([""]))]
    pub node_set: Option<Vec<String>>,
}

/// Internal post-parse representation; not on the wire.
pub struct AddRequest {
    pub files: Vec<UploadedPart>,
    pub dataset_name: Option<String>,
    pub dataset_id: Option<Uuid>,
    pub node_set: Option<Vec<String>>,
}

/// One uploaded file part (or URL-reference part) from the multipart body.
pub struct UploadedPart {
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    /// Spooled temp file path (valid until the `UploadGuard` is dropped).
    pub temp_path: std::path::PathBuf,
    pub byte_count: u64,
    /// Set when the part body is a URL/S3 string (< 4 KiB, valid scheme).
    /// In that case `temp_path` has been unlinked.
    pub url_payload: Option<String>,
}

/// Response shape — matches Python's `PipelineRunInfo.model_dump()`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct PipelineRunInfoDTO {
    /// "PipelineRunStarted" | "PipelineRunYield" | "PipelineRunCompleted"
    /// | "PipelineRunAlreadyCompleted" | "PipelineRunErrored".
    /// String, not enum, to keep wire compatibility with Python's str status field.
    pub status: String,
    pub pipeline_run_id: Uuid,
    pub dataset_id: Uuid,
    pub dataset_name: String,
    /// Free-form. Cognify yields a `GraphDTO` here; add yields `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Per-data-item rows from `add_pipeline`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_ingestion_info: Option<Vec<DataIngestionInfoDTO>>,
}

/// Per-data-item ingestion info row.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct DataIngestionInfoDTO {
    pub data_id: Uuid,
    pub content_hash: String,
    pub name: String,
    pub extension: String,
    pub mime_type: String,
    pub raw_data_location: String,
}

/// `add`/`update`-specific error envelope. Keep separate from `ApiError`'s
/// canonical `{detail: "..."}` shape for byte-for-byte Python parity.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ErrorResponseDTO {
    pub error: String,
    pub detail: Option<String>,
}
