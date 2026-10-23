//! `DatasetDatabaseDb` — the `dataset_database` table: where each dataset's own
//! graph and vector databases live under per-dataset storage
//! (`ENABLE_BACKEND_ACCESS_CONTROL`). See `docs/design/per-dataset-storage.md`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::DatabaseError;

/// One `dataset_database` row (Python `DatasetDatabase`).
///
/// The connection-info objects hold what the handlers need to reach the
/// databases again — host, port, schema — and never a plaintext credential:
/// the handlers fill those in from the live configuration on every bind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetDatabaseRecord {
    pub dataset_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub graph_database_name: String,
    pub vector_database_name: String,
    pub graph_database_provider: String,
    pub vector_database_provider: String,
    pub graph_dataset_database_handler: String,
    pub vector_dataset_database_handler: String,
    pub graph_database_url: Option<String>,
    pub vector_database_url: Option<String>,
    pub graph_database_key: Option<String>,
    pub vector_database_key: Option<String>,
    /// A JSON object.
    pub graph_database_connection_info: serde_json::Value,
    /// A JSON object.
    pub vector_database_connection_info: serde_json::Value,
    pub cognee_version: Option<String>,
    pub migration_revision: Option<String>,
    pub migration_last_error: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait DatasetDatabaseDb: Send + Sync + 'static {
    /// The row for `dataset_id`, by dataset alone: one row serves the owner
    /// and every grantee, so filtering by a caller would miss it (Python
    /// #4829).
    async fn get_dataset_database(
        &self,
        dataset_id: Uuid,
    ) -> Result<Option<DatasetDatabaseRecord>, DatabaseError>;

    /// Insert `record` unless the dataset already has a row. `Ok(false)` when
    /// one existed — a concurrent first use won the race — and the existing
    /// row is left untouched.
    async fn insert_dataset_database(
        &self,
        record: &DatasetDatabaseRecord,
    ) -> Result<bool, DatabaseError>;

    /// Delete the row for `dataset_id`; `Ok(false)` when there was none.
    async fn delete_dataset_database(&self, dataset_id: Uuid) -> Result<bool, DatabaseError>;

    /// Every row, for prune.
    async fn list_dataset_databases(&self) -> Result<Vec<DatasetDatabaseRecord>, DatabaseError>;
}
