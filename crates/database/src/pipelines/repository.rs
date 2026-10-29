use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};

/// Domain alias used in the trait signature.
pub type PipelineRunRow = PipelineRun;
/// Type alias for the database error used in this module.
type DbError = DatabaseError;

/// Row returned by [`PipelineRunRepository::list_recent_with_attribution`].
///
/// Joins `pipeline_runs ⨝ datasets ⨝ users` so the activity router can show
/// "who/what/which dataset" attribution in one query (no N+1).
#[derive(Debug, Clone)]
pub struct PipelineRunWithAttributionRow {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub status: PipelineRunStatus,
    pub pipeline_run_id: Uuid,
    pub pipeline_name: String,
    pub pipeline_id: Uuid,
    pub dataset_id: Option<Uuid>,
    pub dataset_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub owner_email: Option<String>,
    /// The user who triggered the run (`pipeline_runs.user_id`); `None` for
    /// rows written before the column existed and for system writes.
    pub user_id: Option<Uuid>,
}

/// One `pipeline_runs` row to write, for
/// [`PipelineRunRepository::log_pipeline_run_entry`].
#[derive(Debug, Clone)]
pub struct PipelineRunEntry<'a> {
    pub pipeline_run_id: Uuid,
    pub pipeline_id: Uuid,
    pub pipeline_name: &'a str,
    pub dataset_id: Option<Uuid>,
    pub status: PipelineRunStatus,
    pub run_info: Option<serde_json::Value>,
    /// Who triggered the run (Python writes `user.id` on every status row,
    /// `log_pipeline_run_start.py:35`, `log_pipeline_run_complete.py:37`,
    /// `log_pipeline_run_error.py:56@37f788bd2`). `None` for system writes.
    pub user_id: Option<Uuid>,
}

/// Who may see which runs, for [`PipelineRunRepository::list_runs`]: a run
/// is visible when it was triggered by one of `user_ids` **or** is on one of
/// `dataset_ids` — Python's activity-feed predicate `PipelineRun.user_id IN
/// visible_user_ids OR PipelineRun.dataset_id IN permitted`
/// (`get_activity_router.py:141-146@37f788bd2`). A row with a `NULL`
/// `user_id` (written before the column existed) matches only through its
/// dataset.
#[derive(Debug, Clone, Default)]
pub struct PipelineRunVisibility {
    /// Typically the caller plus its child agents (Python
    /// `get_visible_user_ids`).
    pub user_ids: Vec<Uuid>,
    /// Typically the datasets the caller may read.
    pub dataset_ids: Vec<Uuid>,
}

impl PipelineRunVisibility {
    /// Whether no row can be visible.
    pub fn is_empty(&self) -> bool {
        self.user_ids.is_empty() && self.dataset_ids.is_empty()
    }

    /// Whether `row` satisfies the predicate.
    pub fn admits(&self, row: &PipelineRunWithAttributionRow) -> bool {
        row.user_id.is_some_and(|u| self.user_ids.contains(&u))
            || row
                .dataset_id
                .is_some_and(|d| self.dataset_ids.contains(&d))
    }
}

/// A page of the runs listing, for [`PipelineRunRepository::list_runs`].
#[derive(Debug, Clone, Default)]
pub struct PipelineRunListQuery {
    /// Only runs on this dataset.
    pub dataset_id: Option<Uuid>,
    /// Only runs whose `pipeline_name` equals this.
    pub pipeline_name: Option<String>,
    /// `None`: every run (no visibility filter).
    pub visibility: Option<PipelineRunVisibility>,
    pub limit: u64,
    pub offset: u64,
}

/// Persistence abstraction for pipeline run status rows.
///
/// Each status transition writes a **new row** rather than updating in place,
/// giving a full audit trail and matching Python's writing pattern.
///
/// Implementations must be `Send + Sync` so they can be stored behind an
/// `Arc<dyn PipelineRunRepository>` and shared across async tasks.
#[async_trait]
pub trait PipelineRunRepository: Send + Sync {
    /// Insert one row representing a status transition. Returns the new row's
    /// primary key (`pipeline_runs.id`), which is a freshly generated UUIDv4.
    async fn log_pipeline_run(
        &self,
        pipeline_run_id: Uuid,
        pipeline_id: Uuid,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
        status: PipelineRunStatus,
        run_info: Option<serde_json::Value>,
    ) -> Result<Uuid, DbError>;

    /// Latest status per dataset for a given pipeline name.
    ///
    /// Returns a map from `dataset_id` to the most recent
    /// `PipelineRunStatus` row for that dataset and pipeline name.
    async fn latest_status(
        &self,
        dataset_ids: &[Uuid],
        pipeline_name: &str,
    ) -> Result<HashMap<Uuid, PipelineRunStatus>, DbError>;

    /// Recent runs for the activity router, with optional dataset filter.
    async fn list_recent(
        &self,
        dataset_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<PipelineRunRow>, DbError>;

    /// Recent runs *with attribution* (dataset + owner). Powers
    /// `GET /api/v1/activity/pipeline-runs`. Single SELECT joining
    /// `pipeline_runs ⨝ datasets ⨝ users` (LEFT JOIN both ways so orphaned
    /// runs whose dataset has been deleted still surface).
    ///
    /// Optional `dataset_id` narrows to a single dataset; `None` returns
    /// rows across every dataset on the server.
    ///
    /// Default impl falls back to [`Self::list_recent`] without the join — used
    /// only by mock implementations.
    async fn list_recent_with_attribution(
        &self,
        dataset_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<PipelineRunWithAttributionRow>, DbError> {
        let rows = self.list_recent(dataset_id, limit).await?;
        Ok(rows
            .into_iter()
            .map(|r| PipelineRunWithAttributionRow {
                id: r.id,
                created_at: r.created_at,
                status: r.status,
                pipeline_run_id: r.pipeline_run_id,
                pipeline_name: r.pipeline_name,
                pipeline_id: r.pipeline_id,
                dataset_id: r.dataset_id,
                dataset_name: None,
                owner_id: None,
                owner_email: None,
                user_id: r.user_id,
            })
            .collect())
    }

    /// Write one status row like [`Self::log_pipeline_run`], also recording
    /// who triggered the run (`pipeline_runs.user_id`).
    ///
    /// The default implementation forwards to [`Self::log_pipeline_run`] and
    /// drops `user_id`, so an implementation that does not persist it keeps
    /// working (its rows simply carry no triggerer).
    async fn log_pipeline_run_entry(&self, entry: PipelineRunEntry<'_>) -> Result<Uuid, DbError> {
        self.log_pipeline_run(
            entry.pipeline_run_id,
            entry.pipeline_id,
            entry.pipeline_name,
            entry.dataset_id,
            entry.status,
            entry.run_info,
        )
        .await
    }

    /// The runs listing behind `GET /api/v1/activity/pipeline-runs`: rows
    /// joined to their dataset (attribution), filtered by `query`'s dataset,
    /// pipeline name and [`PipelineRunVisibility`], newest first (`created_at
    /// DESC, id DESC`), paged by `limit` / `offset` (Python
    /// `get_activity_router.py:126-171@37f788bd2`).
    ///
    /// An empty visibility returns no rows. The SeaORM implementation answers
    /// in one statement with database-side paging; see
    /// `SeaOrmPipelineRunRepository` for the bind-limit fallback.
    ///
    /// The default implementation (mocks) filters
    /// [`Self::list_recent_with_attribution`] in memory.
    async fn list_runs(
        &self,
        query: &PipelineRunListQuery,
    ) -> Result<Vec<PipelineRunWithAttributionRow>, DbError> {
        if query
            .visibility
            .as_ref()
            .is_some_and(PipelineRunVisibility::is_empty)
        {
            return Ok(Vec::new());
        }
        let window = u32::try_from(query.offset.saturating_add(query.limit)).unwrap_or(u32::MAX);
        let offset = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
        Ok(self
            .list_recent_with_attribution(query.dataset_id, window)
            .await?
            .into_iter()
            .filter(|row| {
                query
                    .pipeline_name
                    .as_deref()
                    .is_none_or(|name| row.pipeline_name == name)
            })
            .filter(|row| query.visibility.as_ref().is_none_or(|v| v.admits(row)))
            .skip(offset)
            .take(limit)
            .collect())
    }

    /// The latest row per `pipeline_run_id` that is still `INITIATED` /
    /// `STARTED` with no more recent successor: Python's
    /// `get_unterminated_pipeline_runs`.
    ///
    /// Candidates for startup recovery, not proof of death: a live run is in
    /// this list too. Recovery proves a writer gone by claiming the run's
    /// ownership marker (`cognee_core::RunOwnership`) and closes the run
    /// through [`Self::reset_orphan_run`]. The rows carry both ids a rollback
    /// needs: `pipeline_run_id` (the ownership-ledger key, *not* `id`, which
    /// keys this one status transition) and `dataset_id`.
    ///
    /// The default implementation reports none, matching the implementations
    /// that persist nothing and therefore never strand a run.
    async fn list_unterminated_runs(&self) -> Result<Vec<PipelineRunRow>, DbError> {
        Ok(Vec::new())
    }

    /// Upsert a single payload field for a run. Concurrent calls with the
    /// same `(run_id, key)` are last-write-wins per row; calls with different
    /// keys do not contend.
    async fn set_payload_field(
        &self,
        run_id: Uuid,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), DbError>;

    /// Read all payload fields for a run as a `serde_json::Map`. Returns an
    /// empty map (not `None`) when the run has no payload events; returns
    /// `Err` only on actual DB failures.
    async fn get_payload(
        &self,
        run_id: Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DbError>;

    /// Return the latest row for `pipeline_run_id` (ordered by `created_at DESC`).
    ///
    /// Multiple rows share the same `pipeline_run_id` per locked decision 12 —
    /// Python intentionally reuses it across status transitions. This method
    /// picks the most recent.
    ///
    /// Python parity: matches
    /// [`get_pipeline_run.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_run.py).
    /// Python uses `session.scalar()` without an `ORDER BY` — the Rust port
    /// adds an explicit `ORDER BY created_at DESC` which is a *stronger*
    /// guarantee consistent with decision 12 ("latest by `created_at` defines
    /// current state"). Intentional, not drift.
    async fn get_pipeline_run(&self, pipeline_run_id: Uuid)
    -> Result<Option<PipelineRun>, DbError>;

    /// Return the latest run for `(dataset_id, pipeline_name)` by `created_at`.
    ///
    /// Python parity: matches
    /// [`get_pipeline_run_by_dataset.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_run_by_dataset.py).
    async fn get_pipeline_run_by_dataset(
        &self,
        dataset_id: Uuid,
        pipeline_name: &str,
    ) -> Result<Option<PipelineRun>, DbError>;

    /// Return one latest row per distinct `pipeline_name` that has runs for
    /// `dataset_id`. Result order is unspecified.
    ///
    /// Supersedes the temporary `list_pipeline_names_for_dataset` helper that
    /// task 08-05 introduced. Used by the delete crate's prune flow to
    /// enumerate pipelines per dataset.
    ///
    /// Python parity: matches
    /// [`get_pipeline_runs_by_dataset.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_runs_by_dataset.py).
    async fn get_pipeline_runs_by_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<PipelineRun>, DbError>;

    /// Close the abandoned `Initiated`/`Started` row whose primary key is
    /// `run_row_id` with an `Errored` successor whose `run_info` is
    /// `{"error": reason}`, as Python's recovery does through
    /// `log_pipeline_run_error`. Returns whether a successor was written.
    ///
    /// Startup recovery calls this after it has rolled the run back, and only
    /// for a run whose ownership marker it claimed (the writer is gone).
    ///
    /// Scoped to the row the caller observed: a real run can start between the
    /// recovery's read and this write, and retiring by pair alone would write
    /// an `Errored` successor over that healthy in-flight run.
    ///
    /// The scope is `pipeline_runs.id` — the row primary key, a fresh UUIDv4
    /// per transition — and **not** `pipeline_run_id`, which does not identify
    /// a run. `pipeline_run_id` is `uuid5(OID, "{pipeline_id}_{dataset_id}")`
    /// (see `cognee_core::pipeline_run_registry::ids::pipeline_run_id`), so for
    /// every dataset whose rows the HTTP server wrote, a *different, healthy*
    /// run started since the operator's report carries the identical value.
    /// Scoping by it would let exactly the newcomer this guard exists to
    /// protect through. The reuse is Python parity and deliberate — a
    /// re-cognify of a dataset is meant to return the same `pipeline_run_id` —
    /// so the guard is what has to change, not the id.
    ///
    /// Writes an `Errored` successor rather than deleting, keeping the
    /// new-row-per-transition audit trail intact.
    ///
    /// The default implementation reports nothing reset.
    async fn reset_orphan_run(
        &self,
        _dataset_id: Uuid,
        _pipeline_name: &str,
        _run_row_id: Uuid,
        _reason: &str,
    ) -> Result<bool, DbError> {
        Ok(false)
    }
}
