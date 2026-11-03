//! Stage watermarks: when a stage last ran to completion on a dataset, so a
//! later run can report `already_completed` (`no_writes_since_last_improve`)
//! instead of redoing work nothing has invalidated — Python's
//! `cognee/modules/improve/graph_changes.py` and the stamps its stages write
//! (`stages.py`, `GlobalContextIndexStage`).
//!
//! A stage stamps its own **start** time, not its end: a write that lands
//! while the stage runs must stay visible to the next run's check
//! (`graph_changes.py:3-6`). Like Python, the stamp lives in the `run_info` of
//! the improve operation's own `pipeline_runs` row, under the stage name:
//! `{"global_context_index": {"status": "completed", "started_at": "<iso>"}}`
//! (`watermark_stamp`, `graph_changes.py:46-48`; `merge_run_info`,
//! `record_operation.py:131-149`). The row is the operation record Python
//! writes (`pipeline_name` `improve`, `Completed` = `outcome == "succeeded"`,
//! the owner as `user_id`), written once the run ends and only when the run
//! did not end `errored` and a stage stamped: Python marks an errored run's
//! row `failed` and a run in which nothing ran `noop`, and its readers trust
//! only `succeeded` rows (`improve.py:238-249`), so Rust writes no row for
//! either. The newest `Completed` `improve` row carrying the stage's key is
//! the watermark ([`last_improve_watermark`], `graph_changes.py:66-97`); a
//! missing or malformed stamp means "changed".
//!
//! The dataset has changed since the stamp when any `pipeline_runs` row was
//! written after it, except for reads (`search`, `recall`), the rows a host
//! records an improve run under ([`IMPROVE_PIPELINE_NAME`]) and the stamped
//! operation's own row (`graph_changes.py:128-141`; Python also leaves out the
//! runs the stamped operation started and a `noop` improve, neither of which
//! Rust has as rows). Python counts every other improve row as a change.
//! Rust's `improve()` also records its own change instead of an operation row
//! per run: right after the first stage that may have written to the graph
//! (feedback weights, truth-subspace scores, the session stages...), it logs
//! one [`IMPROVE_WRITE_PIPELINE_NAME`] row ([`record_graph_write`]). A run that
//! only skipped, or only built the index, leaves none besides its stamp row —
//! which is the watermark itself next time — so a host's own run rows stay
//! uncounted and repeated index-only improves do not rebuild. That row
//! precedes the run's own `global_context_index` stamp, so it never
//! invalidates it. A delete records a `forget` row on every dataset it
//! touched (`cognee_delete`), Python's `forget` operation row.
//! Missing or unreadable state means "changed": being conservative costs extra
//! work, never lost work.

use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use cognee_database::{PipelineRunEntry, PipelineRunRepository, PipelineRunStatus};
use serde_json::{Map, Value, json};
use tracing::{debug, warn};
use uuid::Uuid;

use super::result::StageStatus;
use super::stages::StageKind;

/// `already_completed` reason of a stage whose dataset saw no writes since
/// the stage last ran (Python `REASON_NO_WRITES_SINCE_LAST_IMPROVE`).
pub const REASON_NO_WRITES_SINCE_LAST_IMPROVE: &str = "no_writes_since_last_improve";

/// The `pipeline_runs` name a host records an `improve()` run under.
pub const IMPROVE_PIPELINE_NAME: &str = "improve_pipeline";

/// The `pipeline_runs` name of the `improve` operation row (Python's
/// `IMPROVE_OPERATION_NAME`, `graph_changes.py:31`): the row an improve run
/// that wrote to the graph records its change under, and the row that carries
/// the run's stage stamps.
pub const IMPROVE_WRITE_PIPELINE_NAME: &str = "improve";

/// Runs that are not writes to the dataset: reads (Python
/// `READ_OPERATION_NAMES`) and the host rows of improve runs, whose writes
/// are counted through [`IMPROVE_WRITE_PIPELINE_NAME`].
pub(crate) const UNCOUNTED_PIPELINE_NAMES: [&str; 3] = ["search", "recall", IMPROVE_PIPELINE_NAME];

/// The `run_info` key of `stage`'s stamp: the stage name (Python's
/// `REVIEW_WATERMARK_KEY` / `GLOBAL_CONTEXT_WATERMARK_KEY`,
/// `graph_changes.py:33-35`).
pub fn stage_watermark_key(stage: StageKind) -> &'static str {
    stage.name()
}

/// The `pipeline_id` of `improve` operation rows.
fn improve_pipeline_id() -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, IMPROVE_WRITE_PIPELINE_NAME.as_bytes())
}

/// A stamp a stage asks to save once the run is known not to have errored
/// (Python's `StageResult.run_info_stamp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StageStamp {
    pub key: &'static str,
    pub status: StageStatus,
    pub started_at: DateTime<Utc>,
    pub dataset_id: Uuid,
}

impl StageStamp {
    pub fn new(
        stage: StageKind,
        dataset_id: Uuid,
        status: StageStatus,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            key: stage_watermark_key(stage),
            status,
            started_at,
            dataset_id,
        }
    }

    /// Python's `watermark_stamp`: `{key: {"status", "started_at"}}`
    /// (`graph_changes.py:46-48`; `started_at` is an ISO timestamp).
    fn entry(&self) -> Value {
        json!({
            "status": self.status.as_str(),
            "started_at": self.started_at.to_rfc3339_opts(SecondsFormat::Micros, false),
        })
    }
}

/// The newest succeeded improve row carrying one stage's stamp (Python's
/// `ImproveWatermark`, `graph_changes.py:38-43`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImproveWatermark {
    pub operation_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub stamp: Value,
}

/// Python's `_stamped_stage_start` (`graph_changes.py:51-63`): the stamp's
/// `started_at`, read as UTC when it carries no offset; `None` when missing
/// or not a timestamp.
fn stamped_stage_start(stamp: &Value) -> Option<DateTime<Utc>> {
    let text = stamp.get("started_at")?.as_str()?;
    if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
        return Some(parsed.with_timezone(&Utc));
    }
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
        .map(|naive| naive.and_utc())
}

/// Read the newest succeeded operation carrying `stage`'s stamp on
/// `dataset_id` (Python `last_improve_watermark`, `graph_changes.py:66-97`).
/// `None` for no row, or for a newest stamp that is malformed: being
/// conservative costs extra work, never lost work.
pub(crate) async fn last_improve_watermark(
    runs: &dyn PipelineRunRepository,
    stage: StageKind,
    dataset_id: Uuid,
) -> Result<Option<ImproveWatermark>, cognee_database::DatabaseError> {
    let key = stage_watermark_key(stage);
    let Some(row) = runs
        .latest_stamped_run(dataset_id, IMPROVE_WRITE_PIPELINE_NAME, key)
        .await?
    else {
        return Ok(None);
    };
    Ok(
        stamped_stage_start(&row.stamp).map(|started_at| ImproveWatermark {
            operation_id: row.pipeline_run_id,
            started_at,
            stamp: row.stamp,
        }),
    )
}

/// `false` only when `stage` has a stamp on `dataset_id` and only reads ran
/// after it (Python `has_dataset_changed_since_last_improve`,
/// `graph_changes.py:100-154`). `any_dataset` counts every dataset's runs, for
/// datasets that share one graph. Fails open: no repository, no stamp or any
/// error is "changed".
pub(crate) async fn has_dataset_changed_since_last_improve(
    runs: Option<&dyn PipelineRunRepository>,
    stage: StageKind,
    dataset_id: Uuid,
    any_dataset: bool,
) -> bool {
    let Some(runs) = runs else {
        return true;
    };
    let watermark = match last_improve_watermark(runs, stage, dataset_id).await {
        Ok(Some(watermark)) => watermark,
        Ok(None) => return true,
        Err(e) => {
            debug!(
                stage = stage.name(),
                "improve: change check could not read the stamp, running the stage: {e}"
            );
            return true;
        }
    };
    let scope = (!any_dataset).then_some(dataset_id);
    match runs
        .count_runs_created_after(
            watermark.started_at,
            scope,
            &UNCOUNTED_PIPELINE_NAMES,
            &[watermark.operation_id],
        )
        .await
    {
        Ok(changes) => changes > 0,
        Err(e) => {
            debug!(
                stage = stage.name(),
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
    if let Err(e) = runs
        .log_pipeline_run(
            Uuid::new_v4(),
            improve_pipeline_id(),
            IMPROVE_WRITE_PIPELINE_NAME,
            dataset_id,
            PipelineRunStatus::Completed,
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

/// Write the run's operation row carrying `stamps` in its `run_info`, unless
/// the run errored (Python's row is then `failed`, which invalidates every
/// stamp it carries: a stage after the stamping one may have half-written) or
/// nothing stamped. `owner_id` is the triggering user. A failed write only
/// costs the next run a rebuild, so it is logged.
pub(crate) async fn save_stamps(
    runs: Option<&dyn PipelineRunRepository>,
    owner_id: Uuid,
    run_errored: bool,
    stamps: &[StageStamp],
) {
    if run_errored || stamps.is_empty() {
        return;
    }
    let Some(runs) = runs else {
        return;
    };
    // One operation row per dataset; a run has one, so one row. Stages
    // namespace their entries by key, so merging never drops another's
    // (`record_operation.py:131-149`).
    let mut rows: Vec<(Uuid, Map<String, Value>)> = Vec::new();
    for stamp in stamps {
        let index = match rows.iter().position(|(id, _)| *id == stamp.dataset_id) {
            Some(index) => index,
            None => {
                rows.push((stamp.dataset_id, Map::new()));
                rows.len() - 1
            }
        };
        rows[index].1.insert(stamp.key.to_string(), stamp.entry());
    }
    for (dataset_id, run_info) in rows {
        let entry = PipelineRunEntry {
            pipeline_run_id: Uuid::new_v4(),
            pipeline_id: improve_pipeline_id(),
            pipeline_name: IMPROVE_WRITE_PIPELINE_NAME,
            dataset_id: Some(dataset_id),
            status: PipelineRunStatus::Completed,
            run_info: Some(Value::Object(run_info)),
            user_id: Some(owner_id),
        };
        if let Err(e) = runs.log_pipeline_run_entry(entry).await {
            warn!(%dataset_id, "improve: could not save the stage watermark: {e}");
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

    use chrono::Duration;
    use cognee_database::{
        DatabaseConnection, NoopPipelineRunRepository, RelationalDb, connect, initialize,
    };

    use super::*;

    #[test]
    fn an_improve_write_row_is_a_change_record() {
        let names = cognee_database::pipelines::CHANGE_RECORD_PIPELINE_NAMES;
        assert!(names.contains(&IMPROVE_WRITE_PIPELINE_NAME));
    }

    const STAGE: StageKind = StageKind::GlobalContextIndex;

    struct Fixture {
        _db: Arc<DatabaseConnection>,
        runs: Arc<dyn PipelineRunRepository>,
        dataset_id: Uuid,
    }

    async fn fixture() -> Fixture {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let db = Arc::new(db);
        Fixture {
            runs: Arc::clone(&db).pipeline_runs(),
            _db: db,
            dataset_id: Uuid::new_v4(),
        }
    }

    impl Fixture {
        async fn changed(&self, any_dataset: bool) -> bool {
            has_dataset_changed_since_last_improve(
                Some(&*self.runs),
                STAGE,
                self.dataset_id,
                any_dataset,
            )
            .await
        }

        async fn stamp(&self, at: DateTime<Utc>) {
            save_stamps(
                Some(&*self.runs),
                Uuid::new_v4(),
                false,
                &[StageStamp::new(
                    STAGE,
                    self.dataset_id,
                    StageStatus::Completed,
                    at,
                )],
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
    fn the_key_is_the_stage_name() {
        assert_eq!(stage_watermark_key(STAGE), "global_context_index");
    }

    #[tokio::test]
    async fn no_stamp_means_changed() {
        let f = fixture().await;
        assert!(f.changed(false).await);
    }

    // The stamp round-trips through the operation row's run_info, with
    // Python's keys: `{stage: {"status", "started_at"}}`
    // (`graph_changes.py:46-48`).
    #[tokio::test]
    async fn a_stamp_round_trips_through_run_info() {
        let f = fixture().await;
        let started_at = Utc::now() - Duration::seconds(5);
        f.stamp(started_at).await;

        let watermark = last_improve_watermark(&*f.runs, STAGE, f.dataset_id)
            .await
            .unwrap()
            .expect("the stamp is stored");
        assert_eq!(watermark.stamp["status"], "completed");
        // Microsecond precision, like Python's `datetime.isoformat()`.
        assert_eq!(
            watermark.started_at.timestamp_micros(),
            started_at.timestamp_micros()
        );

        // The row itself: an `improve` operation, its stamp under the stage name.
        let row = f
            .runs
            .get_pipeline_run_by_dataset(f.dataset_id, IMPROVE_WRITE_PIPELINE_NAME)
            .await
            .unwrap()
            .expect("the operation row");
        assert_eq!(row.pipeline_run_id, watermark.operation_id);
        assert_eq!(row.status, PipelineRunStatus::Completed);
        let info = row.run_info.expect("run_info");
        assert_eq!(info["global_context_index"], watermark.stamp);
        assert!(
            info["global_context_index"]["started_at"]
                .as_str()
                .unwrap()
                .ends_with("+00:00")
        );
    }

    // A stage's stamp is found by its own key only.
    #[tokio::test]
    async fn a_stamp_on_another_stage_or_dataset_does_not_count() {
        let f = fixture().await;
        save_stamps(
            Some(&*f.runs),
            Uuid::new_v4(),
            false,
            &[
                StageStamp::new(
                    StageKind::ReviewConflicts,
                    f.dataset_id,
                    StageStatus::Completed,
                    Utc::now(),
                ),
                StageStamp::new(STAGE, Uuid::new_v4(), StageStatus::Completed, Utc::now()),
            ],
        )
        .await;
        assert!(f.changed(false).await);
        let other = last_improve_watermark(&*f.runs, StageKind::ReviewConflicts, f.dataset_id)
            .await
            .unwrap();
        assert!(other.is_some());
    }

    // Stamps of one run merge into one row, none dropping another's
    // (`record_operation.py:131-149`).
    #[tokio::test]
    async fn the_stamps_of_a_run_share_one_row() {
        let f = fixture().await;
        save_stamps(
            Some(&*f.runs),
            Uuid::new_v4(),
            false,
            &[
                StageStamp::new(
                    StageKind::ReviewConflicts,
                    f.dataset_id,
                    StageStatus::Completed,
                    Utc::now(),
                ),
                StageStamp::new(
                    STAGE,
                    f.dataset_id,
                    StageStatus::AlreadyCompleted,
                    Utc::now(),
                ),
            ],
        )
        .await;
        let review = last_improve_watermark(&*f.runs, StageKind::ReviewConflicts, f.dataset_id)
            .await
            .unwrap()
            .unwrap();
        let index = last_improve_watermark(&*f.runs, STAGE, f.dataset_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(review.operation_id, index.operation_id);
        assert_eq!(index.stamp["status"], "already_completed");
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
        // The stamp's own row is created after its start and still no change.
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
            // A run of improve that wrote to the graph.
            (
                IMPROVE_WRITE_PIPELINE_NAME,
                PipelineRunStatus::Completed,
                true,
            ),
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

    // A second improve with nothing new skips: its stamp row replaces the
    // first as the watermark, and neither counts as a change (Python:
    // the stamped run's own row is excluded by id, `graph_changes.py:132`).
    #[tokio::test]
    async fn a_second_run_with_nothing_new_skips() {
        let pause = || tokio::time::sleep(std::time::Duration::from_millis(20));
        let f = fixture().await;
        f.stamp(Utc::now()).await;
        pause().await;
        assert!(!f.changed(true).await, "the first stamp is the watermark");
        // The second run started after the first one's row was written, found
        // nothing new and stamped again.
        f.stamp(Utc::now()).await;
        pause().await;
        assert!(
            !f.changed(true).await,
            "the first row is older than the second stamp"
        );
        // A write after it is seen.
        f.log(
            "cognify_pipeline",
            f.dataset_id,
            PipelineRunStatus::Completed,
        )
        .await;
        assert!(f.changed(true).await);
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

    // The row is not written for an errored run (Python's row is `failed`,
    // which invalidates its stamps) or for a run that stamped nothing.
    #[tokio::test]
    async fn an_errored_run_saves_no_stamp() {
        let f = fixture().await;
        save_stamps(
            Some(&*f.runs),
            Uuid::new_v4(),
            true,
            &[StageStamp::new(
                STAGE,
                f.dataset_id,
                StageStatus::Completed,
                Utc::now(),
            )],
        )
        .await;
        save_stamps(Some(&*f.runs), Uuid::new_v4(), false, &[]).await;
        assert!(
            f.runs
                .get_pipeline_run_by_dataset(f.dataset_id, IMPROVE_WRITE_PIPELINE_NAME)
                .await
                .unwrap()
                .is_none()
        );
        assert!(f.changed(false).await);
    }

    // `_stamped_stage_start`: missing or malformed means rerun.
    #[tokio::test]
    async fn a_malformed_stamp_means_changed() {
        for run_info in [
            json!({"global_context_index": {"status": "completed"}}),
            json!({"global_context_index": {"started_at": "yesterday"}}),
            json!({"global_context_index": "2026-01-01T00:00:00+00:00"}),
        ] {
            let f = fixture().await;
            f.runs
                .log_pipeline_run(
                    Uuid::new_v4(),
                    improve_pipeline_id(),
                    IMPROVE_WRITE_PIPELINE_NAME,
                    Some(f.dataset_id),
                    PipelineRunStatus::Completed,
                    Some(run_info.clone()),
                )
                .await
                .unwrap();
            assert!(f.changed(false).await, "{run_info}");
        }
    }

    #[test]
    fn a_stamp_without_an_offset_reads_as_utc() {
        let stamp = json!({"started_at": "2026-03-04T05:06:07.250000"});
        assert_eq!(
            stamped_stage_start(&stamp).unwrap(),
            "2026-03-04T05:06:07.250Z".parse::<DateTime<Utc>>().unwrap()
        );
        let stamp = json!({"started_at": "2026-03-04T07:06:07+02:00"});
        assert_eq!(
            stamped_stage_start(&stamp).unwrap(),
            "2026-03-04T05:06:07Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[tokio::test]
    async fn a_missing_repository_means_changed() {
        let f = fixture().await;
        f.stamp(Utc::now()).await;
        let changed =
            has_dataset_changed_since_last_improve(None, STAGE, f.dataset_id, false).await;
        assert!(changed);
    }

    // Any error: run the stage.
    #[tokio::test]
    async fn any_error_means_changed() {
        let f = fixture().await;
        // A repository without the lookup (the trait default) fails.
        let noop = NoopPipelineRunRepository;
        let changed =
            has_dataset_changed_since_last_improve(Some(&noop), STAGE, f.dataset_id, false).await;
        assert!(changed);

        // A failed save is logged, not raised.
        save_stamps(
            Some(&noop),
            Uuid::new_v4(),
            false,
            &[StageStamp::new(
                STAGE,
                f.dataset_id,
                StageStatus::Completed,
                Utc::now(),
            )],
        )
        .await;
    }
}
