use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Error)]
pub enum DatabaseError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Query error: {0}")]
    QueryError(String),

    #[error("Unique violation: {0}")]
    UniqueViolation(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: Uuid,
    pub slug: Uuid,
    pub user_id: Uuid,
    pub data_id: Uuid,
    pub dataset_id: Uuid,
    /// The pipeline run that created this ownership row, or `None` for rows
    /// written before run ownership existed.
    pub pipeline_run_id: Option<Uuid>,
    pub label: Option<String>,
    pub node_type: String,
    pub indexed_fields: serde_json::Value,
    pub attributes: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: Uuid,
    pub slug: Uuid,
    pub user_id: Uuid,
    pub data_id: Uuid,
    pub dataset_id: Uuid,
    /// The pipeline run that created this ownership row, or `None` for rows
    /// written before run ownership existed.
    pub pipeline_run_id: Option<Uuid>,
    pub source_node_id: Uuid,
    pub destination_node_id: Uuid,
    pub relationship_name: String,
    pub label: Option<String>,
    pub attributes: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelineRunStatus {
    Initiated,
    Started,
    Completed,
    Errored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRun {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub status: PipelineRunStatus,
    pub pipeline_run_id: Uuid,
    pub pipeline_name: String,
    pub pipeline_id: Uuid,
    pub dataset_id: Option<Uuid>,
    pub run_info: Option<serde_json::Value>,
    /// The user who triggered the run; `None` for rows from before
    /// `pipeline_runs.user_id` existed and for system writes.
    #[serde(default)]
    pub user_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: Uuid,
    pub task_name: String,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub run_info: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphMetrics {
    pub id: Uuid,
    pub num_tokens: Option<i32>,
    pub num_nodes: Option<i32>,
    pub num_edges: Option<i32>,
    pub mean_degree: Option<f64>,
    pub edge_density: Option<f64>,
    pub num_connected_components: Option<i32>,
    pub sizes_of_connected_components: Option<serde_json::Value>,
    pub num_selfloops: Option<i32>,
    pub diameter: Option<i32>,
    pub avg_shortest_path_length: Option<f64>,
    pub avg_clustering: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SearchHistoryEntryType {
    Query,
    Result,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHistoryEntry {
    pub entry_id: Uuid,
    pub query_id: Uuid,
    pub entry_type: SearchHistoryEntryType,
    pub content: String,
    pub query_type: Option<String>,
    pub user_id: Option<Uuid>,
    /// The dataset the search was recorded under; `None` for an unscoped search.
    pub dataset_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// One `provenance_edge_evidence` row: a chunk that supports a graph edge
/// (Python `ProvenanceEdgeEvidence`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeEvidence {
    /// Deterministic (uuid5 of the row's identity) so a re-capture is a no-op.
    pub id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub user_id: Uuid,
    pub dataset_id: Uuid,
    pub data_id: Uuid,
    /// The run that captured the row. A row whose run never completed is
    /// ignored at read time; `None` is always readable.
    pub pipeline_run_id: Option<Uuid>,
    pub chunk_id: Uuid,
    pub chunk_index: Option<i32>,
    /// The graph's deterministic `edge_object_id`.
    pub edge_id: Uuid,
    pub source_node_id: Uuid,
    pub destination_node_id: Uuid,
    pub relationship_name: String,
    /// `extracted` or `structural`.
    pub evidence_kind: String,
    pub source_task: Option<String>,
    pub confidence: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// An active source chunk of a graph edge, with its document's name (Python
/// `EdgeEvidenceRecord`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeEvidenceRecord {
    pub edge_id: Uuid,
    pub data_id: Uuid,
    pub chunk_id: Uuid,
    pub chunk_index: Option<i32>,
    pub document_name: Option<String>,
}
