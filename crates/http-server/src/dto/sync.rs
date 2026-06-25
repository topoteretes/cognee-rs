//! DTOs for `/api/v1/sync` and `/api/v1/sync/status`.
//!
//! All envelope shapes match Python — including the deviation from the
//! canonical `{"detail": ...}` envelope. The 4xx/5xx body for these
//! endpoints is `{"error": "..."}`, never `{"detail": ...}`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// `POST /api/v1/sync` request body.
///
/// Pydantic: `dataset_ids: Optional[List[UUID]] = None`. Inherits `InDTO`,
/// so the wire is camelCase (`datasetIds`); snake_case is accepted as an
/// inbound alias.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncRequestDTO {
    /// `None` or `[]` means "all writable datasets for the caller".
    #[serde(default, alias = "dataset_ids")]
    pub dataset_ids: Option<Vec<Uuid>>,
}

/// 200 response body for `POST /api/v1/sync`.
///
/// `run_id` stays `String` (not `Uuid`) so the wire shape matches Python's
/// `str` annotation byte-for-byte.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncResponseDTO {
    pub run_id: String,
    pub status: String,
    pub dataset_ids: Vec<String>,
    pub dataset_names: Vec<String>,
    pub message: String,
    pub timestamp: String,
    pub user_id: String,
}

/// 409 body when another sync is already running for the user.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncConflictDTO {
    pub error: String,
    pub details: SyncConflictDetailsDTO,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncConflictDetailsDTO {
    pub run_id: String,
    pub status: String,
    pub dataset_ids: Vec<Uuid>,
    pub dataset_names: Vec<String>,
    pub message: String,
    pub timestamp: String,
    pub progress_percentage: u32,
}

/// 200 response body for `GET /api/v1/sync/status`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncStatusOverviewDTO {
    pub has_running_sync: bool,
    pub running_sync_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_running_sync: Option<LatestRunningSyncDTO>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct LatestRunningSyncDTO {
    pub run_id: String,
    pub dataset_ids: Vec<Uuid>,
    pub dataset_names: Vec<String>,
    pub progress_percentage: u32,
    pub created_at: Option<String>,
}

/// `{"error": "..."}` envelope used by the simpler error paths in this router.
///
/// Differs from the canonical `{"detail": "..."}` envelope on purpose.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncErrorDTO {
    pub error: String,
}
