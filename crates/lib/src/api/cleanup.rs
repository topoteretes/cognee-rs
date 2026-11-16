//! `cleanup_unused_data` — delete the documents no search has touched for a
//! while (Python `cognee/tasks/cleanup/cleanup_unused_data.py`).
//!
//! It relies on access tracking: a search bumps the `last_accessed` of the
//! documents it retrieved only while `enable_last_accessed`
//! (`ENABLE_LAST_ACCESSED`) is on, so cleanup does nothing while it is off,
//! and nothing until at least one document has been tracked.
//!
//! Like Python, this is an internal maintenance task: no binding, CLI command
//! or HTTP route exposes it. It is an **operator** task: `user_id` only narrows
//! the selection to the datasets that user owns, and the stale documents are
//! deleted directly through the [`DeleteService`] (hard mode), without an ACL
//! check.
//!
//! **Without `user_id` a run deletes every owner's unused documents.** That
//! is wider than Python, which always deletes as the default user — passed
//! explicitly when `user_id` is `None`, and through `delete`'s `user=None`
//! fallback otherwise (`cleanup_unused_data.py:150`,
//! `datasets.py:513-520`) — so its deletes outside the default user's
//! datasets fail authorization (logged, not counted) while the run still
//! reports `completed`. That failure is not reproduced.
//!
//! Serialised, a [`CleanupResult`] has Python's keys and status strings;
//! `cleanup_date` is RFC 3339 with a `Z` suffix where Python's `isoformat()`
//! writes `+00:00`.

use chrono::{DateTime, Duration, Utc};
use cognee_database::{DatabaseError, DeleteDb};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use serde::Serialize;
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

/// Why a cleanup run was skipped while access tracking is off (Python's
/// reason text).
pub const REASON_TRACKING_DISABLED: &str = "ENABLE_LAST_ACCESSED not enabled";

/// Why a cleanup run was skipped while no document has been tracked yet
/// (Python's reason text).
pub const REASON_NOTHING_TRACKED: &str = "No tracked records found - tracking may be newly enabled";

/// What [`cleanup_unused_data`] removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupParams {
    /// Minutes since the last access after which a document is unused. A
    /// document never accessed is always unused. Required: Python's
    /// `minutes_threshold=None` default only raises a `TypeError`.
    pub minutes_threshold: u64,
    /// Only count what would be deleted (Python's default, `true`).
    pub dry_run: bool,
    /// Only the documents in the datasets this user owns; every owner's when
    /// `None`.
    pub user_id: Option<Uuid>,
}

impl CleanupParams {
    /// A dry run over every owner's documents, as Python's defaults.
    pub fn new(minutes_threshold: u64) -> Self {
        Self {
            minutes_threshold,
            dry_run: true,
            user_id: None,
        }
    }
}

/// How a cleanup run ended (Python's `status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    /// Nothing ran; [`CleanupResult::reason`] says why.
    Skipped,
    /// The unused documents were counted, not deleted.
    DryRun,
    /// The unused documents were deleted.
    Completed,
}

/// Documents deleted by a run (Python's `deleted_count`; both fields count
/// the same documents, as in Python).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct CleanupDeletedCount {
    pub data_items: usize,
    pub documents: usize,
}

/// What a dry run would delete (Python's `preview`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CleanupPreview {
    pub documents: usize,
}

/// The outcome of [`cleanup_unused_data`], with Python's field names.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CleanupResult {
    pub status: CleanupStatus,
    /// Why the run was skipped ([`REASON_TRACKING_DISABLED`],
    /// [`REASON_NOTHING_TRACKED`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Unused documents found.
    pub unused_count: usize,
    /// Documents deleted. A failed delete is logged and not counted. `None`
    /// on a skipped run, serialised as `{}` as in Python.
    #[serde(serialize_with = "serialize_deleted_count")]
    pub deleted_count: Option<CleanupDeletedCount>,
    /// When the run finished.
    pub cleanup_date: DateTime<Utc>,
    /// Set on a dry run only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<CleanupPreview>,
}

impl CleanupResult {
    fn skipped(reason: &str) -> Self {
        Self {
            status: CleanupStatus::Skipped,
            reason: Some(reason.to_string()),
            unused_count: 0,
            deleted_count: None,
            cleanup_date: Utc::now(),
            preview: None,
        }
    }
}

/// `deleted_count` as Python writes it: the counts, or `{}` on a skipped run.
fn serialize_deleted_count<S: serde::Serializer>(
    count: &Option<CleanupDeletedCount>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    match count {
        Some(count) => count.serialize(serializer),
        None => serializer.serialize_map(Some(0))?.end(),
    }
}

/// A cleanup run that could not look its documents up.
#[derive(Debug, Error)]
pub enum CleanupError {
    #[error("cleanup database error: {0}")]
    Database(#[from] DatabaseError),
}

/// Find the documents not accessed for `params.minutes_threshold` minutes
/// (or never) and, unless `params.dry_run`, hard-delete each one with its
/// graph and vector data. Port of Python `cleanup_unused_data`.
///
/// `enable_last_accessed` is the setting of that name
/// (`Settings::enable_last_accessed`, `ENABLE_LAST_ACCESSED`): while it is
/// off the run is skipped, as is a run before any document was tracked
/// (counted across every owner, as in Python). A document whose delete fails
/// is logged and skipped; a document with no dataset cannot be deleted by
/// dataset and is skipped with a warning.
pub async fn cleanup_unused_data(
    params: CleanupParams,
    enable_last_accessed: bool,
    db: &dyn DeleteDb,
    delete_service: &DeleteService,
) -> Result<CleanupResult, CleanupError> {
    if !enable_last_accessed {
        warn!("cleanup skipped: ENABLE_LAST_ACCESSED is not enabled");
        return Ok(CleanupResult::skipped(REASON_TRACKING_DISABLED));
    }
    if db.count_tracked_data().await? == 0 {
        warn!(
            "cleanup skipped: no records have been tracked yet; wait for searches \
             to update timestamps before running cleanup"
        );
        return Ok(CleanupResult::skipped(REASON_NOTHING_TRACKED));
    }

    info!(
        minutes_threshold = params.minutes_threshold,
        dry_run = params.dry_run,
        user_id = params.user_id.map(|id| id.to_string()),
        "starting cleanup"
    );
    let minutes = i64::try_from(params.minutes_threshold).unwrap_or(i64::MAX);
    let cutoff = Utc::now()
        .checked_sub_signed(Duration::try_minutes(minutes).unwrap_or(Duration::MAX))
        .unwrap_or(DateTime::<Utc>::MIN_UTC);
    let unused = db.list_unused_data(cutoff, params.user_id).await?;
    info!(unused = unused.len(), "found unused documents");

    if params.dry_run {
        return Ok(CleanupResult {
            status: CleanupStatus::DryRun,
            reason: None,
            unused_count: unused.len(),
            deleted_count: Some(CleanupDeletedCount::default()),
            cleanup_date: Utc::now(),
            preview: Some(CleanupPreview {
                documents: unused.len(),
            }),
        });
    }

    let mut deleted = 0usize;
    for data in &unused {
        let Some(dataset_id) = data.dataset_id else {
            warn!(data_id = %data.id, "cleanup: document has no dataset; skipped");
            continue;
        };
        let request = DeleteRequest {
            scope: DeleteScope::DataInDataset {
                dataset_id,
                data_id: data.id,
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Hard,
            memory_only: false,
        };
        match delete_service.execute(&request).await {
            Ok(_) => {
                deleted += 1;
                info!(data_id = %data.id, %dataset_id, "cleanup: deleted document");
            }
            Err(error) => {
                warn!(data_id = %data.id, %dataset_id, %error, "cleanup: failed to delete document");
            }
        }
    }
    info!(deleted, "cleanup completed");

    Ok(CleanupResult {
        status: CleanupStatus::Completed,
        reason: None,
        unused_count: unused.len(),
        deleted_count: Some(CleanupDeletedCount {
            data_items: deleted,
            documents: deleted,
        }),
        cleanup_date: Utc::now(),
        preview: None,
    })
}
