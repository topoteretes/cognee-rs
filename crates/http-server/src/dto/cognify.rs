//! DTOs for `POST /api/v1/cognify` and `GET /api/v1/cognify/subscribe/{id}`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// Re-export shared DTO.
pub use super::pipeline_run::PipelineRunInfoDTO;

// ─── Request ──────────────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/cognify`.
///
/// Mirrors Python's `CognifyPayloadDTO`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CognifyPayloadDTO {
    /// Dataset names owned by the authenticated user.
    #[serde(default)]
    pub datasets: Option<Vec<String>>,

    /// Dataset UUIDs. When set, overrides `datasets` (Python parity:
    /// `dataset_ids if payload.dataset_ids else payload.datasets`).
    #[serde(default)]
    pub dataset_ids: Option<Vec<Uuid>>,

    /// When `true`, dispatch to the background and return `PipelineRunStarted`
    /// immediately. When `false` (default), await the run to completion.
    #[serde(default)]
    pub run_in_background: Option<bool>,

    /// JSON Schema describing a custom Pydantic-shaped graph model.
    #[serde(default)]
    pub graph_model: Option<serde_json::Value>,

    /// Replaces the default graph-extraction prompt for this run.
    #[serde(default)]
    pub custom_prompt: Option<String>,

    /// One or more ontology keys from `POST /api/v1/ontologies/upload`.
    #[serde(default)]
    pub ontology_key: Option<Vec<String>>,

    /// Overrides `CognifyConfig::chunks_per_batch` for this run.
    #[serde(default)]
    pub chunks_per_batch: Option<u32>,
}

// ─── Response aliases ─────────────────────────────────────────────────────────

/// Response body for `POST /api/v1/cognify` — Python returns the dict directly.
///
/// Key: stringified dataset UUID.
/// Value: terminal `PipelineRunInfoDTO` for that dataset.
pub type CognifyResponseDTO = std::collections::HashMap<String, PipelineRunInfoDTO>;

// ─── WebSocket frame ──────────────────────────────────────────────────────────

/// Single frame sent over the WebSocket at
/// `GET /api/v1/cognify/subscribe/{pipeline_run_id}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CognifyWsFrameDTO {
    pub pipeline_run_id: Uuid,
    /// One of the `PipelineRun*` live-event strings.
    pub status: String,
    /// Formatted graph snapshot (`{"nodes": [...], "edges": [...]}`),
    /// or `{}` on error, or `[]` for background dispatches.
    pub payload: serde_json::Value,
}
