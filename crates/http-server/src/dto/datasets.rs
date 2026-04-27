//! DTOs for `/api/v1/datasets/*`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// `DatasetDTO` — Python `OutDTO` (camelCase wire).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DatasetDTO {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
    pub owner_id: Uuid,
}

/// `DataDTO` — Python `OutDTO` (camelCase wire).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DataDTO {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
    pub extension: String,
    pub mime_type: String,
    pub raw_data_location: String,
    pub dataset_id: Option<Uuid>,
}

/// `GraphDTO` — Python `OutDTO` (camelCase wire).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphDTO {
    pub nodes: Vec<GraphNodeDTO>,
    pub edges: Vec<GraphEdgeDTO>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeDTO {
    pub id: Uuid,
    pub label: String,
    pub r#type: String,
    pub properties: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdgeDTO {
    pub source: Uuid,
    pub target: Uuid,
    pub label: String,
}

/// Request body for `POST /api/v1/datasets`.
/// Python `InDTO` — camelCase aliases. Single-field DTO so the wire is `{"name": "..."}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DatasetCreationPayload {
    pub name: String,
}

/// Request body for `PUT /api/v1/datasets/{id}/schema`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DatasetSchemaPayloadDTO {
    /// Free-form JSON object describing the dataset's graph schema. `null`
    /// vs absent are distinct: `null` clears the field, absent leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_prompt: Option<String>,
}

/// Response body for `GET /api/v1/datasets/{id}/schema`.
/// **snake_case** wire — Python returns a raw dict, not an `OutDTO`.
#[derive(Debug, Serialize, ToSchema)]
pub struct DatasetSchemaResponseDTO {
    pub graph_schema: Option<Value>,
    pub custom_prompt: Option<String>,
}

/// Query string for `GET /api/v1/datasets/status`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct DatasetStatusQuery {
    /// Repeated query param `?dataset=<uuid>&dataset=<uuid>`.
    /// Defaults to an empty list when the parameter is absent.
    #[serde(rename = "dataset", default)]
    pub dataset: Vec<Uuid>,
}

/// `PipelineRunStatus` enum — wire is the raw string discriminator.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub enum PipelineRunStatus {
    #[serde(rename = "DATASET_PROCESSING_INITIATED")]
    DatasetProcessingInitiated,
    #[serde(rename = "DATASET_PROCESSING_STARTED")]
    DatasetProcessingStarted,
    #[serde(rename = "DATASET_PROCESSING_COMPLETED")]
    DatasetProcessingCompleted,
    #[serde(rename = "DATASET_PROCESSING_ERRORED")]
    DatasetProcessingErrored,
}

/// Response body for `GET /api/v1/datasets/status`.
pub type DatasetStatusResponseDTO = HashMap<Uuid, PipelineRunStatus>;

/// `ErrorMessageDTO` — `{message: String}` envelope used by 2.3's 404.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorMessageDTO {
    pub message: String,
}
