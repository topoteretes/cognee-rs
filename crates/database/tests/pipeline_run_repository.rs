#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Round-trip tests for `SeaOrmPipelineRunRepository` against an in-memory
//! SQLite database.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository,
    connect, initialize, ops,
};
use cognee_models::Dataset;
use serde_json::{Map, Value};
use uuid::Uuid;

async fn make_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

fn make_repo(db: Arc<DatabaseConnection>) -> SeaOrmPipelineRunRepository {
    SeaOrmPipelineRunRepository::new(db)
}

/// Pre-create a dataset row so FK constraints on `pipeline_runs.dataset_id` pass.
async fn create_dataset(db: &DatabaseConnection, id: Uuid) {
    let owner = Uuid::new_v4();
    let dataset = Dataset::new("test".to_string(), owner, None, id);
    ops::datasets::create_dataset(db, dataset)
        .await
        .expect("create_dataset for FK setup");
}

// ---------------------------------------------------------------------------
// (a) log_pipeline_run returns a fresh Uuid per call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn log_pipeline_run_returns_fresh_uuid() {
    let db = make_db().await;
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;
    let repo = make_repo(Arc::clone(&db));

    let pipeline_run_id = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();

    let id1 = repo
        .log_pipeline_run(
            pipeline_run_id,
            pipeline_id,
            "test_pipeline",
            Some(dataset_id),
            PipelineRunStatus::Initiated,
            None,
        )
        .await
        .expect("log run 1");

    let id2 = repo
        .log_pipeline_run(
            pipeline_run_id,
            pipeline_id,
            "test_pipeline",
            Some(dataset_id),
            PipelineRunStatus::Started,
            None,
        )
        .await
        .expect("log run 2");

    assert_ne!(id1, id2, "each call must return a distinct primary key");
}

// ---------------------------------------------------------------------------
// (b) latest_status returns the most recent row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn latest_status_returns_most_recent_row() {
    let db = make_db().await;
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;
    let repo = make_repo(Arc::clone(&db));

    let pipeline_run_id = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();

    // Write Initiated, then Started, then Completed.
    for status in [
        PipelineRunStatus::Initiated,
        PipelineRunStatus::Started,
        PipelineRunStatus::Completed,
    ] {
        repo.log_pipeline_run(
            pipeline_run_id,
            pipeline_id,
            "p1",
            Some(dataset_id),
            status,
            None,
        )
        .await
        .expect("log");
    }

    let statuses = repo
        .latest_status(&[dataset_id], "p1")
        .await
        .expect("latest_status");

    assert_eq!(
        statuses.get(&dataset_id).cloned(),
        Some(PipelineRunStatus::Completed),
        "latest row should be Completed"
    );
}

// ---------------------------------------------------------------------------
// (c) latest_status batch returns latest per dataset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn latest_status_batch_returns_per_dataset() {
    let db = make_db().await;
    let ds1 = Uuid::new_v4();
    let ds2 = Uuid::new_v4();
    create_dataset(&db, ds1).await;
    create_dataset(&db, ds2).await;
    let repo = make_repo(Arc::clone(&db));

    let pipeline_id = Uuid::new_v4();

    // ds1: Initiated → Started
    for status in [PipelineRunStatus::Initiated, PipelineRunStatus::Started] {
        repo.log_pipeline_run(
            Uuid::new_v4(),
            pipeline_id,
            "batch_p",
            Some(ds1),
            status,
            None,
        )
        .await
        .expect("log ds1");
    }

    // ds2: Initiated → Completed
    for status in [PipelineRunStatus::Initiated, PipelineRunStatus::Completed] {
        repo.log_pipeline_run(
            Uuid::new_v4(),
            pipeline_id,
            "batch_p",
            Some(ds2),
            status,
            None,
        )
        .await
        .expect("log ds2");
    }

    let statuses: HashMap<Uuid, PipelineRunStatus> = repo
        .latest_status(&[ds1, ds2], "batch_p")
        .await
        .expect("latest_status batch");

    assert_eq!(
        statuses.get(&ds1).cloned(),
        Some(PipelineRunStatus::Started)
    );
    assert_eq!(
        statuses.get(&ds2).cloned(),
        Some(PipelineRunStatus::Completed)
    );
}

// ---------------------------------------------------------------------------
// list_unterminated_runs names exactly the runs still in flight
// ---------------------------------------------------------------------------

/// Startup recovery lists the runs still in flight (Python's
/// `get_unterminated_pipeline_runs`), claims each one's ownership marker, rolls
/// its artifacts back and closes it through
/// [`PipelineRunRepository::reset_orphan_run`].
///
/// The returned rows must carry `pipeline_run_id` (the ownership-ledger key)
/// and `dataset_id`, because a sweep scope is built from both. `id` — the
/// per-transition row key — is *not* what the ledger stores, and a sweep
/// scoped on it would select nothing at all and report a clean rollback.
#[tokio::test]
async fn list_unterminated_runs_names_the_runs_still_in_flight() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));
    let pipeline_id = Uuid::new_v4();

    let initiated = Uuid::new_v4();
    let started = Uuid::new_v4();
    let completed = Uuid::new_v4();
    let errored = Uuid::new_v4();

    // A row an older writer left at INITIATED is still a candidate.
    repo.log_pipeline_run(
        initiated,
        pipeline_id,
        "p",
        Some(ds),
        PipelineRunStatus::Initiated,
        None,
    )
    .await
    .expect("log initiated");
    // Its own pipeline name: `reset_orphan_run` closes the latest row of a
    // `(dataset, pipeline)` pair, and the finished runs below share "p".
    repo.log_pipeline_run(
        started,
        pipeline_id,
        "p_started",
        Some(ds),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("log started");
    // A run that finished and one that already failed: neither is in flight,
    // and sweeping either would delete a settled run's artifacts.
    for (run, last) in [
        (completed, PipelineRunStatus::Completed),
        (errored, PipelineRunStatus::Errored),
    ] {
        for status in [PipelineRunStatus::Started, last] {
            repo.log_pipeline_run(run, pipeline_id, "p", Some(ds), status, None)
                .await
                .expect("log terminal run");
        }
    }

    let mut listed: Vec<Uuid> = repo
        .list_unterminated_runs()
        .await
        .expect("list_unterminated_runs")
        .into_iter()
        .inspect(|row| {
            assert_eq!(
                row.dataset_id,
                Some(ds),
                "a sweep scope needs the dataset id"
            );
        })
        .map(|row| row.pipeline_run_id)
        .collect();
    listed.sort();

    let mut expected = vec![initiated, started];
    expected.sort();
    assert_eq!(
        listed, expected,
        "only the two runs still in flight may be listed"
    );

    // Closing a run takes it off the list, or every restart would try to
    // recover the same run again.
    let row = repo
        .get_pipeline_run(started)
        .await
        .expect("get")
        .expect("present");
    assert!(
        repo.reset_orphan_run(ds, "p_started", row.id, "closed")
            .await
            .expect("close")
    );
    let after: Vec<Uuid> = repo
        .list_unterminated_runs()
        .await
        .expect("list after close")
        .into_iter()
        .map(|row| row.pipeline_run_id)
        .collect();
    assert_eq!(after, vec![initiated]);
}

// ---------------------------------------------------------------------------
// list_recent — basic smoke test
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// get_pipeline_run — empty / single / latest of many
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_pipeline_run_returns_none_when_no_rows() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));

    let result = repo
        .get_pipeline_run(Uuid::new_v4())
        .await
        .expect("get_pipeline_run empty");
    assert!(result.is_none());
}

#[tokio::test]
async fn get_pipeline_run_returns_single_row() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    repo.log_pipeline_run(
        prid,
        pid,
        "single_p",
        Some(ds),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("log");

    let result = repo
        .get_pipeline_run(prid)
        .await
        .expect("get_pipeline_run single")
        .expect("row present");
    assert_eq!(result.pipeline_run_id, prid);
    assert_eq!(result.status, PipelineRunStatus::Started);
}

#[tokio::test]
async fn get_pipeline_run_returns_latest_of_many() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();

    // Multiple rows sharing pipeline_run_id (decision 12 reuse semantics).
    for status in [
        PipelineRunStatus::Initiated,
        PipelineRunStatus::Started,
        PipelineRunStatus::Completed,
    ] {
        repo.log_pipeline_run(prid, pid, "reuse_p", Some(ds), status, None)
            .await
            .expect("log");
        // Small sleep so created_at strictly increases — SQLite stores
        // sub-microsecond datetimes but two same-instant rows would tie
        // the ORDER BY and the test could observe either order.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let result = repo
        .get_pipeline_run(prid)
        .await
        .expect("get_pipeline_run latest")
        .expect("row present");
    assert_eq!(result.status, PipelineRunStatus::Completed);
}

// ---------------------------------------------------------------------------
// get_pipeline_run_by_dataset — empty / single / multiple
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_pipeline_run_by_dataset_returns_none_when_empty() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let result = repo
        .get_pipeline_run_by_dataset(ds, "missing_p")
        .await
        .expect("query");
    assert!(result.is_none());
}

#[tokio::test]
async fn get_pipeline_run_by_dataset_returns_single_match() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "by_ds_p",
        Some(ds),
        PipelineRunStatus::Initiated,
        None,
    )
    .await
    .expect("log");

    let result = repo
        .get_pipeline_run_by_dataset(ds, "by_ds_p")
        .await
        .expect("query")
        .expect("row present");
    assert_eq!(result.pipeline_name, "by_ds_p");
    assert_eq!(result.status, PipelineRunStatus::Initiated);
}

#[tokio::test]
async fn get_pipeline_run_by_dataset_returns_latest_match() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();

    for status in [
        PipelineRunStatus::Initiated,
        PipelineRunStatus::Started,
        PipelineRunStatus::Errored,
    ] {
        repo.log_pipeline_run(prid, pid, "latest_p", Some(ds), status, None)
            .await
            .expect("log");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let result = repo
        .get_pipeline_run_by_dataset(ds, "latest_p")
        .await
        .expect("query")
        .expect("row present");
    assert_eq!(result.status, PipelineRunStatus::Errored);
}

// ---------------------------------------------------------------------------
// get_pipeline_runs_by_dataset — empty / one name / multiple names / dedup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_pipeline_runs_by_dataset_returns_empty_when_no_rows() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let rows = repo.get_pipeline_runs_by_dataset(ds).await.expect("query");
    assert!(rows.is_empty());
}

#[tokio::test]
async fn get_pipeline_runs_by_dataset_one_pipeline_name() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "solo_p",
        Some(ds),
        PipelineRunStatus::Completed,
        None,
    )
    .await
    .expect("log");

    let rows = repo.get_pipeline_runs_by_dataset(ds).await.expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pipeline_name, "solo_p");
}

#[tokio::test]
async fn get_pipeline_runs_by_dataset_multiple_pipeline_names() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    for name in ["a_p", "b_p", "c_p"] {
        repo.log_pipeline_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            name,
            Some(ds),
            PipelineRunStatus::Initiated,
            None,
        )
        .await
        .expect("log");
    }

    let rows = repo.get_pipeline_runs_by_dataset(ds).await.expect("query");
    assert_eq!(rows.len(), 3);
    let mut names: Vec<&str> = rows.iter().map(|r| r.pipeline_name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["a_p", "b_p", "c_p"]);
}

#[tokio::test]
async fn get_pipeline_runs_by_dataset_dedupes_to_latest_per_name() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let pid_a = Uuid::new_v4();
    let prid_a = Uuid::new_v4();
    // "name_a": Initiated → Started → Completed (3 rows, latest is Completed)
    for status in [
        PipelineRunStatus::Initiated,
        PipelineRunStatus::Started,
        PipelineRunStatus::Completed,
    ] {
        repo.log_pipeline_run(prid_a, pid_a, "name_a", Some(ds), status, None)
            .await
            .expect("log a");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let pid_b = Uuid::new_v4();
    let prid_b = Uuid::new_v4();
    // "name_b": Initiated → Errored (2 rows, latest is Errored)
    for status in [PipelineRunStatus::Initiated, PipelineRunStatus::Errored] {
        repo.log_pipeline_run(prid_b, pid_b, "name_b", Some(ds), status, None)
            .await
            .expect("log b");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let rows = repo.get_pipeline_runs_by_dataset(ds).await.expect("query");

    // One row per distinct pipeline_name, each the latest by created_at.
    assert_eq!(rows.len(), 2, "expected one row per pipeline_name");
    let by_name: HashMap<String, PipelineRunStatus> = rows
        .into_iter()
        .map(|r| (r.pipeline_name, r.status))
        .collect();
    assert_eq!(
        by_name.get("name_a").cloned(),
        Some(PipelineRunStatus::Completed)
    );
    assert_eq!(
        by_name.get("name_b").cloned(),
        Some(PipelineRunStatus::Errored)
    );
}

// ---------------------------------------------------------------------------
// list_recent — basic smoke test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_recent_returns_rows_in_desc_order() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let pipeline_id = Uuid::new_v4();

    for status in [PipelineRunStatus::Initiated, PipelineRunStatus::Completed] {
        repo.log_pipeline_run(
            Uuid::new_v4(),
            pipeline_id,
            "list_p",
            Some(ds),
            status,
            None,
        )
        .await
        .expect("log");
    }

    let rows = repo.list_recent(Some(ds), 10).await.expect("list_recent");

    assert_eq!(rows.len(), 2);
    // First row is the most recent (Completed).
    assert_eq!(rows[0].status, PipelineRunStatus::Completed);
}

// ---------------------------------------------------------------------------
// Task 08-09 — gap-binding integration tests
// ---------------------------------------------------------------------------
//
// The unit-level run_info helpers are exercised inline in
// `crates/core/src/pipeline_run_registry/run_info.rs`. The tests below
// confirm that the persisted JSON makes a full round-trip through SeaORM
// (write → read) preserving Python-parity byte shape, and that the
// lifecycle / dataset_id=None / reset-helper paths behave as locked
// decisions 1, 4, 5, 7 mandate. No pipeline run writes `INITIATED` any more
// (Python `dc0b4ad6a`); the round-trips below still start from one because
// the stored-row enum keeps it for legacy rows and the explicit re-arm API.

// ---------------------------------------------------------------------------
// (08-09) Lifecycle round-trip from a legacy INITIATED row — Initiated →
// Started → Completed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_round_trip_from_legacy_initiated_completed() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();

    for status in [
        PipelineRunStatus::Initiated,
        PipelineRunStatus::Started,
        PipelineRunStatus::Completed,
    ] {
        repo.log_pipeline_run(prid, pid, "cognify_pipeline", Some(ds), status, None)
            .await
            .expect("log");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    // get_pipeline_run returns the latest row keyed on pipeline_run_id.
    let latest_by_id = repo
        .get_pipeline_run(prid)
        .await
        .expect("get_pipeline_run")
        .expect("row present");
    assert_eq!(latest_by_id.status, PipelineRunStatus::Completed);

    // get_pipeline_run_by_dataset returns the latest row keyed on
    // (dataset_id, pipeline_name).
    let latest_by_ds = repo
        .get_pipeline_run_by_dataset(ds, "cognify_pipeline")
        .await
        .expect("get_pipeline_run_by_dataset")
        .expect("row present");
    assert_eq!(latest_by_ds.status, PipelineRunStatus::Completed);

    // get_pipeline_runs_by_dataset returns one latest row per pipeline_name.
    let rows = repo
        .get_pipeline_runs_by_dataset(ds)
        .await
        .expect("get_pipeline_runs_by_dataset");
    assert_eq!(rows.len(), 1, "exactly one pipeline_name for this dataset");
    assert_eq!(rows[0].status, PipelineRunStatus::Completed);
    assert_eq!(rows[0].pipeline_name, "cognify_pipeline");
}

// ---------------------------------------------------------------------------
// (08-09) Lifecycle round-trip from a legacy INITIATED row — Initiated →
// Started → Errored
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_round_trip_from_legacy_initiated_errored() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();

    for status in [
        PipelineRunStatus::Initiated,
        PipelineRunStatus::Started,
        PipelineRunStatus::Errored,
    ] {
        repo.log_pipeline_run(prid, pid, "cognify_pipeline", Some(ds), status, None)
            .await
            .expect("log");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let latest = repo
        .get_pipeline_run_by_dataset(ds, "cognify_pipeline")
        .await
        .expect("query")
        .expect("row present");
    assert_eq!(latest.status, PipelineRunStatus::Errored);
}

// ---------------------------------------------------------------------------
// (08-09) dataset_id = None — row persists after 08-01 nullability migration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn log_pipeline_run_persists_with_none_dataset_id() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    let row_id = repo
        .log_pipeline_run(
            prid,
            pid,
            "ad_hoc_pipeline",
            None,
            PipelineRunStatus::Started,
            None,
        )
        .await
        .expect("log");

    // Row is reachable via get_pipeline_run (keyed on pipeline_run_id).
    let row = repo
        .get_pipeline_run(prid)
        .await
        .expect("get_pipeline_run")
        .expect("row present even though dataset_id is None");
    assert_eq!(row.pipeline_run_id, prid);
    assert!(
        row.dataset_id.is_none(),
        "dataset_id should round-trip as None after 08-01"
    );

    // The orphan does NOT appear in get_pipeline_run_by_dataset for any
    // arbitrary dataset — the dataset-keyed reader filters on a concrete
    // `dataset_id` value, so NULL rows are never returned.
    let some_other_ds = Uuid::new_v4();
    let nothing = repo
        .get_pipeline_run_by_dataset(some_other_ds, "ad_hoc_pipeline")
        .await
        .expect("query");
    assert!(
        nothing.is_none(),
        "dataset-keyed reader must NOT surface dataset_id=NULL rows"
    );

    // list_recent(None, _) surfaces the orphan (no filter).
    let rows = repo.list_recent(None, 10).await.expect("list_recent all");
    assert!(rows.iter().any(|r| r.id == row_id));
}

// ---------------------------------------------------------------------------
// Exact run_info JSON shapes, as Python writes them since `a88402500`
// (COG-6089): STARTED `{}`, ERRORED `{"error": …}` — no `data` key.
// ---------------------------------------------------------------------------
//
// Persistence round-trip: write a typed row with the payload the watchers
// build (`cognee_core::pipeline_run_registry::run_info_for_*`), read it back,
// and serialise the column. The string must be byte-identical.
#[tokio::test]
async fn run_info_started_shape_is_byte_identical_to_python() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    repo.log_pipeline_run(
        prid,
        pid,
        "shape_p",
        Some(ds),
        PipelineRunStatus::Started,
        Some(Value::Object(Map::new())),
    )
    .await
    .expect("log");

    let row = repo
        .get_pipeline_run(prid)
        .await
        .expect("get_pipeline_run")
        .expect("row present");
    let run_info = row.run_info.expect("run_info populated");
    assert_eq!(run_info.to_string(), "{}");
}

#[tokio::test]
async fn run_info_errored_shape_is_the_error_alone() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let mut m = Map::with_capacity(1);
    m.insert("error".into(), Value::String("boom".to_string()));
    let payload = Value::Object(m);

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    repo.log_pipeline_run(
        prid,
        pid,
        "err_p",
        Some(ds),
        PipelineRunStatus::Errored,
        Some(payload),
    )
    .await
    .expect("log");

    let row = repo
        .get_pipeline_run(prid)
        .await
        .expect("get_pipeline_run")
        .expect("row present");
    let run_info = row.run_info.expect("run_info populated");
    assert_eq!(run_info.to_string(), "{\"error\":\"boom\"}");
}

// ---------------------------------------------------------------------------
// (08-09) Exact run_info JSON shape — an INITIATED row (explicit re-arm
// API only; no pipeline run writes one) stores `{}`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_info_initiated_shape_is_empty_object() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    repo.log_pipeline_run(
        prid,
        pid,
        "init_p",
        Some(ds),
        PipelineRunStatus::Initiated,
        Some(Value::Object(Map::new())),
    )
    .await
    .expect("log");

    let row = repo
        .get_pipeline_run(prid)
        .await
        .expect("get_pipeline_run")
        .expect("row present");
    let run_info = row.run_info.expect("run_info populated");
    assert_eq!(run_info.to_string(), "{}");
}

// ---------------------------------------------------------------------------
// Scoped closing of an abandoned run (`reset_orphan_run`)
//
// Startup recovery closes only the run whose row it observed: a newer run on
// the same dataset must never be marked failed.
// ---------------------------------------------------------------------------

/// Returns the new row's primary key (`pipeline_runs.id`) — the value
/// `reset_orphan_run` is scoped by. It is *not* `run_id`: several rows share
/// one `pipeline_run_id`, by design.
async fn log_status(
    repo: &SeaOrmPipelineRunRepository,
    dataset_id: Uuid,
    pipeline: &str,
    run_id: Uuid,
    status: PipelineRunStatus,
) -> Uuid {
    repo.log_pipeline_run(
        run_id,
        Uuid::new_v4(),
        pipeline,
        Some(dataset_id),
        status,
        None,
    )
    .await
    .expect("log_pipeline_run")
}

#[tokio::test]
async fn reset_orphan_run_closes_a_started_row_with_an_errored_successor() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;

    let run_id = Uuid::new_v4();
    let row_id = log_status(
        &repo,
        dataset_id,
        "cognify_pipeline",
        run_id,
        PipelineRunStatus::Started,
    )
    .await;

    assert!(
        repo.reset_orphan_run(dataset_id, "cognify_pipeline", row_id, "operator_unblock")
            .await
            .expect("reset_orphan_run")
    );

    // The latest row must now be `Errored`. Asserting the successor's status
    // rather than an absence, because the reset writes a new row instead of
    // deleting.
    let latest = repo
        .get_pipeline_run_by_dataset(dataset_id, "cognify_pipeline")
        .await
        .expect("get latest")
        .expect("a row exists");
    assert_eq!(latest.status, PipelineRunStatus::Errored);
    assert_eq!(
        latest.pipeline_run_id, run_id,
        "the successor belongs to the run it retired, keeping the audit trail joinable"
    );
}

/// A completed run is not an orphan. Retiring it would rewrite history.
#[tokio::test]
async fn reset_orphan_run_leaves_a_completed_run_alone() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;

    let run_id = Uuid::new_v4();
    log_status(
        &repo,
        dataset_id,
        "cognify_pipeline",
        run_id,
        PipelineRunStatus::Started,
    )
    .await;
    let completed_row = log_status(
        &repo,
        dataset_id,
        "cognify_pipeline",
        run_id,
        PipelineRunStatus::Completed,
    )
    .await;

    assert!(
        !repo
            .reset_orphan_run(
                dataset_id,
                "cognify_pipeline",
                completed_row,
                "operator_unblock"
            )
            .await
            .expect("reset_orphan_run"),
        "a finished run is not an orphan"
    );

    let latest = repo
        .get_pipeline_run_by_dataset(dataset_id, "cognify_pipeline")
        .await
        .expect("get latest")
        .expect("a row exists");
    assert_eq!(latest.status, PipelineRunStatus::Completed);
}

#[tokio::test]
async fn reset_orphan_run_reports_false_when_the_pair_has_no_runs() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;

    assert!(
        !repo
            .reset_orphan_run(
                dataset_id,
                "cognify_pipeline",
                Uuid::new_v4(),
                "operator_unblock"
            )
            .await
            .expect("reset_orphan_run")
    );
}

/// Scoped by pair: closing a cognify run must not retire another pipeline's
/// run that is genuinely in flight.
#[tokio::test]
async fn reset_orphan_run_does_not_touch_another_pipeline() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;

    let row_id = log_status(
        &repo,
        dataset_id,
        "cognify_pipeline",
        Uuid::new_v4(),
        PipelineRunStatus::Started,
    )
    .await;
    log_status(
        &repo,
        dataset_id,
        "memify_pipeline",
        Uuid::new_v4(),
        PipelineRunStatus::Started,
    )
    .await;

    assert!(
        repo.reset_orphan_run(dataset_id, "cognify_pipeline", row_id, "operator_unblock")
            .await
            .expect("reset_orphan_run")
    );

    let memify = repo
        .get_pipeline_run_by_dataset(dataset_id, "memify_pipeline")
        .await
        .expect("get latest")
        .expect("a row exists");
    assert_eq!(
        memify.status,
        PipelineRunStatus::Started,
        "the live memify run must survive unblocking cognify"
    );
}

/// Scoped by dataset too.
#[tokio::test]
async fn reset_orphan_run_does_not_touch_another_dataset() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let wedged = Uuid::new_v4();
    let healthy = Uuid::new_v4();
    create_dataset(&db, wedged).await;
    create_dataset(&db, healthy).await;

    let wedged_row = log_status(
        &repo,
        wedged,
        "cognify_pipeline",
        Uuid::new_v4(),
        PipelineRunStatus::Started,
    )
    .await;
    log_status(
        &repo,
        healthy,
        "cognify_pipeline",
        Uuid::new_v4(),
        PipelineRunStatus::Started,
    )
    .await;

    assert!(
        repo.reset_orphan_run(wedged, "cognify_pipeline", wedged_row, "operator_unblock")
            .await
            .expect("reset_orphan_run")
    );

    let survivor = repo
        .get_pipeline_run_by_dataset(healthy, "cognify_pipeline")
        .await
        .expect("get latest")
        .expect("a row exists");
    assert_eq!(survivor.status, PipelineRunStatus::Started);
}

/// The TOCTOU the run-id scoping exists for.
///
/// The command reports first and clears second, so a real run can start between
/// the two invocations. Retiring by `(dataset, pipeline)` alone would write an
/// `Errored` successor over that healthy in-flight run — marking it failed and
/// re-opening the gate it had legitimately closed.
#[tokio::test]
async fn reset_orphan_run_does_not_retire_a_run_started_since_the_read() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;

    let observed = Uuid::new_v4();
    let observed_row = log_status(
        &repo,
        dataset_id,
        "cognify_pipeline",
        observed,
        PipelineRunStatus::Started,
    )
    .await;

    // The wedged run is cleaned up some other way, and a fresh one starts.
    let newcomer = Uuid::new_v4();
    log_status(
        &repo,
        dataset_id,
        "cognify_pipeline",
        newcomer,
        PipelineRunStatus::Started,
    )
    .await;

    assert!(
        !repo
            .reset_orphan_run(
                dataset_id,
                "cognify_pipeline",
                observed_row,
                "operator_unblock"
            )
            .await
            .expect("reset_orphan_run"),
        "retiring a run that is no longer the latest must do nothing"
    );

    let latest = repo
        .get_pipeline_run_by_dataset(dataset_id, "cognify_pipeline")
        .await
        .expect("get latest")
        .expect("a row exists");
    assert_eq!(
        latest.pipeline_run_id, newcomer,
        "the live run must still be the latest"
    );
    assert_eq!(
        latest.status,
        PipelineRunStatus::Started,
        "and must not have been marked failed"
    );
}

/// The same TOCTOU as above, but with the `pipeline_run_id` the **HTTP server**
/// writes — and this is the case that matters, because the guard used to be
/// written against that id.
///
/// `pipeline_run_id` is `uuid5(OID, "{pipeline_id}_{dataset_id}")`
/// (`cognee_core::pipeline_run_registry::ids::pipeline_run_id`, called from
/// `dispatch_pipeline`), so every run of one pipeline on one dataset shares it
/// — Python reuses it deliberately, so a re-cognify reports the same run id.
/// The test above only passes under a `pipeline_run_id` guard because the
/// library executor happens to mint a fresh `Uuid::new_v4()` per run; the
/// moment the rows came from the HTTP server, the observed id and the
/// newcomer's were equal and the guard waved the newcomer through to be marked
/// `Errored` while healthy and in flight.
///
/// Hence the guard is on `pipeline_runs.id`, which is a fresh UUIDv4 per row.
#[tokio::test]
async fn reset_orphan_run_does_not_retire_a_newcomer_reusing_the_same_pipeline_run_id() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;

    // Derived exactly as `dispatch_pipeline` derives it, so both runs below
    // carry the identical value — as they do in any HTTP-written database.
    let pipeline_id = Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("{}cognify_pipeline{dataset_id}", Uuid::new_v4()).as_bytes(),
    );
    let shared_run_id = Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("{pipeline_id}_{dataset_id}").as_bytes(),
    );

    let observed_row = log_status(
        &repo,
        dataset_id,
        "cognify_pipeline",
        shared_run_id,
        PipelineRunStatus::Started,
    )
    .await;

    // The wedged run is cleaned up some other way, and a fresh one starts. Same
    // pipeline, same dataset, so the HTTP server hands it the same
    // `pipeline_run_id` — only the row PK differs.
    let newcomer_row = log_status(
        &repo,
        dataset_id,
        "cognify_pipeline",
        shared_run_id,
        PipelineRunStatus::Started,
    )
    .await;
    assert_ne!(
        observed_row, newcomer_row,
        "the row PK is what distinguishes the two runs; the run id cannot"
    );

    assert!(
        !repo
            .reset_orphan_run(
                dataset_id,
                "cognify_pipeline",
                observed_row,
                "operator_unblock"
            )
            .await
            .expect("reset_orphan_run"),
        "the observed row is no longer the latest — a guard keyed on the reused \
         pipeline_run_id would not notice and would retire the live run"
    );

    let latest = repo
        .get_pipeline_run_by_dataset(dataset_id, "cognify_pipeline")
        .await
        .expect("get latest")
        .expect("a row exists");
    assert_eq!(
        latest.id, newcomer_row,
        "the live run's row must still be the latest"
    );
    assert_eq!(
        latest.status,
        PipelineRunStatus::Started,
        "and must not have been marked failed"
    );
}
