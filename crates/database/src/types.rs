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
    pub created_at: DateTime<Utc>,
}
