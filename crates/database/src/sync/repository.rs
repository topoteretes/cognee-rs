//! `SyncOperationRepository` trait + DTO row.
//!
//! Mirrors Python's `cognee/modules/sync/methods/` module 1:1.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::types::DatabaseError;

/// Status enum used by the repository surface. String values match Python's
/// JSON column verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOperationStatus {
    Started,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

impl SyncOperationStatus {
    /// Wire/DB string for this status.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Snapshot of one `sync_operations` row (every column).
#[derive(Debug, Clone)]
pub struct SyncOperationRow {
    pub id: Uuid,
    pub run_id: String,
    pub status: String,
    pub progress_percentage: u32,
    pub dataset_ids: Vec<Uuid>,
    pub dataset_names: Vec<String>,
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub total_records_to_sync: Option<i32>,
    pub total_records_to_download: Option<i32>,
    pub total_records_to_upload: Option<i32>,
    pub records_downloaded: i32,
    pub records_uploaded: i32,
    pub bytes_downloaded: i64,
    pub bytes_uploaded: i64,
    pub dataset_sync_hashes: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub retry_count: i32,
}

/// Persistence trait for the cloud sync router.
#[async_trait]
pub trait SyncOperationRepository: Send + Sync + 'static {
    /// Insert a new row in `started` state with progress = 0%.
    async fn create_operation(
        &self,
        run_id: &str,
        dataset_ids: &[Uuid],
        dataset_names: &[String],
        user_id: Uuid,
    ) -> Result<(), DatabaseError>;

    /// Transition a row to `in_progress`, setting `started_at = NOW()`.
    async fn mark_started(&self, run_id: &str) -> Result<(), DatabaseError>;

    /// Transition a row to `completed`, set `completed_at = NOW()`,
    /// progress = 100. Optional totals get persisted alongside.
    async fn mark_completed(
        &self,
        run_id: &str,
        records_uploaded: i32,
        records_downloaded: i32,
        bytes_uploaded: i64,
        bytes_downloaded: i64,
        dataset_sync_hashes: Option<serde_json::Value>,
    ) -> Result<(), DatabaseError>;

    /// Transition a row to `failed`, set `completed_at = NOW()`, copy the
    /// error message into `error_message`.
    async fn mark_failed(&self, run_id: &str, error_message: &str) -> Result<(), DatabaseError>;

    /// Update progress only (for the background task's tick callback).
    async fn update_progress(&self, run_id: &str, percent: u32) -> Result<(), DatabaseError>;

    /// All rows for `user_id` with status in `('started', 'in_progress')`,
    /// ordered DESC by `created_at`.
    async fn running_for_user(&self, user_id: Uuid)
    -> Result<Vec<SyncOperationRow>, DatabaseError>;

    /// Look up one row by its `run_id`.
    async fn get_by_run_id(&self, run_id: &str) -> Result<Option<SyncOperationRow>, DatabaseError>;
}
