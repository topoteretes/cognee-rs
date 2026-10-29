//! Stage watermarks: when a stage last ran to completion on a dataset, so a
//! later run can report `already_completed` (`no_writes_since_last_improve`)
//! instead of redoing work nothing has invalidated — Python's
//! `cognee/modules/improve/graph_changes.py` and the stamps its stages write
//! (`stages.py`, `GlobalContextIndexStage`).
//!
//! A stage stamps its own **start** time, not its end: a write that lands
//! while the stage runs must stay visible to the next run's check. The stamp
//! is a [`CheckpointStore`] row keyed `improve_stage:{stage}:{dataset_id}`,
//! saved only when the run did not end `errored` (Python marks such a run's
//! operation row `failed`, which invalidates its stamps).
//!
//! The dataset has changed since the stamp when any `pipeline_runs` row was
//! written after it, except for reads (`search`, `recall`) and the rows a host
//! records an improve run under ([`IMPROVE_PIPELINE_NAME`]). Python excludes
//! only the stamping run's own row (and the runs it started) and noop
//! improves, counting every other improve as a change. Rust's `improve()`
//! records its own change instead: right after the first stage that may have
//! written to the graph (feedback weights, truth-subspace scores, the session
//! stages...), it logs one [`IMPROVE_WRITE_PIPELINE_NAME`] row
//! ([`record_graph_write`]). A run that only skipped, or only built the index,
//! leaves none — so a host's own run rows stay uncounted and repeated
//! index-only improves do not rebuild. That row precedes the run's own
//! `global_context_index` stamp, so it never invalidates it. A delete records
//! a `forget` row on every dataset it touched (`cognee_delete`), Python's
//! `forget` operation row.
//! Missing or unreadable state means "changed": being conservative costs extra
//! work, never lost work.

use chrono::{DateTime, Utc};
use cognee_database::{CheckpointStore, PipelineRunRepository};
use tracing::{debug, warn};
use uuid::Uuid;

use super::stages::StageKind;

/// `already_completed` reason of a stage whose dataset saw no writes since
/// the stage last ran (Python `REASON_NO_WRITES_SINCE_LAST_IMPROVE`).
pub const REASON_NO_WRITES_SINCE_LAST_IMPROVE: &str = "no_writes_since_last_improve";

/// The `pipeline_runs` name a host records an `improve()` run under.
pub const IMPROVE_PIPELINE_NAME: &str = "improve_pipeline";

/// The `pipeline_runs` name an improve run that wrote to the graph records
/// its change under (Python's `improve` operation name).
pub const IMPROVE_WRITE_PIPELINE_NAME: &str = "improve";

/// Runs that are not writes to the dataset: reads (Python
/// `READ_OPERATION_NAMES`) and the host rows of improve runs, whose writes
/// are counted through [`IMPROVE_WRITE_PIPELINE_NAME`].
pub(crate) const UNCOUNTED_PIPELINE_NAMES: [&str; 3] = ["search", "recall", IMPROVE_PIPELINE_NAME];

/// The checkpoint key of `stage`'s watermark on `dataset_id`.
pub fn stage_watermark_key(stage: StageKind, dataset_id: Uuid) -> String {
    format!("improve_stage:{}:{dataset_id}", stage.name())
}

/// A stamp a stage asks to save once the run is known not to have errored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StageStamp {
    pub key: String,
    pub started_at: DateTime<Utc>,
}

impl StageStamp {
    pub fn new(stage: StageKind, dataset_id: Uuid, started_at: DateTime<Utc>) -> Self {
        Self {
            key: stage_watermark_key(stage, dataset_id),
            started_at,
        }
    }
}

/// `false` only when `stage` has a stamp on `dataset_id` and only reads ran
/// after it. `any_dataset` counts every dataset's runs, for datasets that
/// share one graph. Fails open: no store, no stamp or any error is "changed".
pub(crate) async fn has_dataset_changed_since_last_improve(
    checkpoints: Option<&dyn CheckpointStore>,
    runs: Option<&dyn PipelineRunRepository>,
    stage: StageKind,
    dataset_id: Uuid,
    any_dataset: bool,
) -> bool {
    let (Some(checkpoints), Some(runs)) = (checkpoints, runs) else {
        return true;
    };
    let key = stage_watermark_key(stage, dataset_id);
    let stamp = match checkpoints.load(&key).await {
        Ok(Some(stamp)) => stamp,
        Ok(None) => return true,
        Err(e) => {
            debug!(
                key,
                "improve: change check could not read the stamp, running the stage: {e}"
            );
            return true;
        }
    };
    let scope = (!any_dataset).then_some(dataset_id);
    match runs
        .count_runs_created_after(stamp, scope, &UNCOUNTED_PIPELINE_NAMES)
        .await
    {
        Ok(changes) => changes > 0,
        Err(e) => {
            debug!(
                key,
                "improve: change check could not decide, running the stage: {e}"
            );
            true
        }
    }
}

/// Log the [`IMPROVE_WRITE_PIPELINE_NAME`] row of a run that wrote to the
/// graph of `dataset_id` (`None`: a dataset the run could not resolve; the
/// any-dataset check still sees it). Best effort: a missed row only leaves an
/// index stale until the next write, so a failure is logged.
pub(crate) async fn record_graph_write(runs: &dyn PipelineRunRepository, dataset_id: Option<Uuid>) {
    let pipeline = Uuid::new_v5(&Uuid::NAMESPACE_OID, IMPROVE_WRITE_PIPELINE_NAME.as_bytes());
    if let Err(e) = runs
        .log_pipeline_run(
            Uuid::new_v4(),
            pipeline,
            IMPROVE_WRITE_PIPELINE_NAME,
            dataset_id,
            cognee_database::PipelineRunStatus::Completed,
            None,
        )
        .await
    {
        warn!(
            ?dataset_id,
            "improve: could not record the run's graph write: {e}"
        );
    }
}

/// Save `stamps` unless the run errored. A failed save only costs the next
/// run a rebuild, so it is logged.
pub(crate) async fn save_stamps(
    checkpoints: Option<&dyn CheckpointStore>,
    run_errored: bool,
    stamps: &[StageStamp],
) {
    if run_errored || stamps.is_empty() {
        return;
    }
    let Some(checkpoints) = checkpoints else {
        return;
    };
    for stamp in stamps {
        if let Err(e) = checkpoints.save(&stamp.key, stamp.started_at).await {
            warn!(
                key = stamp.key,
                "improve: could not save the stage watermark: {e}"
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    //! The change check, after Python's
    //! `cognee/tests/unit/modules/improve/test_graph_changes.py`, over a real
    //! in-memory SQLite `pipeline_runs` table.

    use std::sync::Arc;

    use async_trait::async_trait;
    use chrono::Duration;
    use cognee_database::{
        DatabaseConnection, DatabaseError, PipelineRunStatus, RelationalDb, connect, initialize,
    };

    use super::*;

    const STAGE: StageKind = StageKind::GlobalContextIndex;

    struct Fixture {
        db: Arc<DatabaseConnection>,
        checkpoints: Arc<dyn CheckpointStore>,
        runs: Arc<dyn PipelineRunRepository>,
        dataset_id: Uuid,
    }

    async fn fixture() -> Fixture {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let db = Arc::new(db);
        Fixture {
            checkpoints: Arc::clone(&db).checkpoints(),
            runs: Arc::clone(&db).pipeline_runs(),
            db,
            dataset_id: Uuid::new_v4(),
        }
    }

    impl Fixture {
        async fn changed(&self, any_dataset: bool) -> bool {
            has_dataset_changed_since_last_improve(
                Some(&*self.checkpoints),
                Some(&*self.runs),
                STAGE,
                self.dataset_id,
                any_dataset,
            )
            .await
        }

        async fn stamp(&self, at: DateTime<Utc>) {
            save_stamps(
                Some(&*self.checkpoints),
                false,
                &[StageStamp::new(STAGE, self.dataset_id, at)],
            )
            .await;
        }

        async fn log(&self, name: &str, dataset_id: Uuid, status: PipelineRunStatus) {
            self.runs
                .log_pipeline_run(
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    name,
                    Some(dataset_id),
                    status,
                    None,
                )
                .await
                .unwrap();
        }
    }

    #[test]
    fn the_key_names_the_stage_and_the_dataset() {
        let id = Uuid::nil();
        assert_eq!(
            stage_watermark_key(STAGE, id),
            "improve_stage:global_context_index:00000000-0000-0000-0000-000000000000"
        );
    }

    #[tokio::test]
    async fn no_stamp_means_changed() {
        let f = fixture().await;
        assert!(f.changed(false).await);
    }

    #[tokio::test]
    async fn a_stamp_with_no_later_runs_means_unchanged() {
        let f = fixture().await;
        f.log(
            "cognify_pipeline",
            f.dataset_id,
            PipelineRunStatus::Completed,
        )
        .await;
        f.stamp(Utc::now() + Duration::milliseconds(1)).await;
        assert!(!f.changed(false).await);
    }

    // test_every_run_but_a_read_a_noop_or_the_stamped_one_changes_the_dataset
    #[tokio::test]
    async fn every_run_but_a_read_changes_the_dataset() {
        let cases = [
            ("search", PipelineRunStatus::Completed, false),
            ("recall", PipelineRunStatus::Completed, false),
            // The host's record of an improve run, this one's included.
            (IMPROVE_PIPELINE_NAME, PipelineRunStatus::Completed, false),
            ("cognify_pipeline", PipelineRunStatus::Started, true),
            ("cognify_pipeline", PipelineRunStatus::Errored, true),
            ("add_pipeline", PipelineRunStatus::Completed, true),
            ("memify_pipeline", PipelineRunStatus::Completed, true),
            // A reset by a deletion writes an `INITIATED` row.
            ("cognify_pipeline", PipelineRunStatus::Initiated, true),
        ];
        for (name, status, changed) in cases {
            let f = fixture().await;
            f.stamp(Utc::now() - Duration::seconds(1)).await;
            f.log(name, f.dataset_id, status.clone()).await;
            assert_eq!(f.changed(false).await, changed, "{name} {status:?}");
        }
    }

    // test_another_datasets_run_counts_only_when_the_graph_is_shared
    #[tokio::test]
    async fn another_datasets_run_counts_only_when_the_graph_is_shared() {
        let f = fixture().await;
        f.stamp(Utc::now() - Duration::seconds(1)).await;
        f.log(
            "cognify_pipeline",
            Uuid::new_v4(),
            PipelineRunStatus::Completed,
        )
        .await;
        assert!(!f.changed(false).await);
        assert!(f.changed(true).await);
    }

    #[tokio::test]
    async fn a_stamp_on_another_stage_or_dataset_does_not_count() {
        let f = fixture().await;
        save_stamps(
            Some(&*f.checkpoints),
            false,
            &[
                StageStamp::new(StageKind::ReviewConflicts, f.dataset_id, Utc::now()),
                StageStamp::new(STAGE, Uuid::new_v4(), Utc::now()),
            ],
        )
        .await;
        assert!(f.changed(false).await);
    }

    #[tokio::test]
    async fn an_errored_run_saves_no_stamp() {
        let f = fixture().await;
        save_stamps(
            Some(&*f.checkpoints),
            true,
            &[StageStamp::new(STAGE, f.dataset_id, Utc::now())],
        )
        .await;
        assert!(
            f.checkpoints
                .load(&stage_watermark_key(STAGE, f.dataset_id))
                .await
                .unwrap()
                .is_none()
        );
        assert!(f.changed(false).await);
    }

    #[tokio::test]
    async fn a_newer_stamp_replaces_the_older_one() {
        let f = fixture().await;
        f.stamp(Utc::now() - Duration::seconds(10)).await;
        f.log(
            "cognify_pipeline",
            f.dataset_id,
            PipelineRunStatus::Completed,
        )
        .await;
        assert!(f.changed(false).await);
        f.stamp(Utc::now() + Duration::milliseconds(1)).await;
        assert!(!f.changed(false).await);
    }

    #[tokio::test]
    async fn missing_stores_mean_changed() {
        let f = fixture().await;
        f.stamp(Utc::now()).await;
        let changed = has_dataset_changed_since_last_improve(
            None,
            Some(&*f.runs),
            STAGE,
            f.dataset_id,
            false,
        )
        .await;
        assert!(changed);
        let changed = has_dataset_changed_since_last_improve(
            Some(&*f.checkpoints),
            None,
            STAGE,
            f.dataset_id,
            false,
        )
        .await;
        assert!(changed);
        drop(f.db);
    }

    /// A repository that cannot answer the count.
    struct Unanswerable;

    #[async_trait]
    impl CheckpointStore for Unanswerable {
        async fn load(&self, _: &str) -> Result<Option<DateTime<Utc>>, DatabaseError> {
            Err(DatabaseError::ConnectionError("down".to_string()))
        }
        async fn save(&self, _: &str, _: DateTime<Utc>) -> Result<(), DatabaseError> {
            Err(DatabaseError::ConnectionError("down".to_string()))
        }
    }

    // Any error: run the stage.
    #[tokio::test]
    async fn any_error_means_changed() {
        let f = fixture().await;
        let changed = has_dataset_changed_since_last_improve(
            Some(&Unanswerable),
            Some(&*f.runs),
            STAGE,
            f.dataset_id,
            false,
        )
        .await;
        assert!(changed);

        // A repository without the count (the trait default) fails.
        f.stamp(Utc::now()).await;
        let noop = cognee_database::NoopPipelineRunRepository;
        let changed = has_dataset_changed_since_last_improve(
            Some(&*f.checkpoints),
            Some(&noop),
            STAGE,
            f.dataset_id,
            false,
        )
        .await;
        assert!(changed);

        // A failed save is logged, not raised.
        save_stamps(
            Some(&Unanswerable),
            false,
            &[StageStamp::new(STAGE, f.dataset_id, Utc::now())],
        )
        .await;
    }
}
