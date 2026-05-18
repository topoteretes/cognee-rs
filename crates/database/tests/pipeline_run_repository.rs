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
// (d) reset_orphans rewrites INITIATED/STARTED to ERRORED and counts them
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reset_orphans_rewrites_initiated_and_started() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let pr1 = Uuid::new_v4();
    let pr2 = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();

    // pr1: INITIATED (orphan)
    repo.log_pipeline_run(
        pr1,
        pipeline_id,
        "orphan_p",
        Some(ds),
        PipelineRunStatus::Initiated,
        None,
    )
    .await
    .expect("log pr1");

    // pr2: STARTED (orphan)
    repo.log_pipeline_run(
        pr2,
        pipeline_id,
        "orphan_p",
        Some(ds),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("log pr2");

    let count = repo
        .reset_orphans("server_restart_orphan")
        .await
        .expect("reset_orphans");

    assert_eq!(count, 2, "both orphans should be rewritten");

    // After reset, latest status should be Errored for both.
    let statuses = repo
        .latest_status(&[ds], "orphan_p")
        .await
        .expect("latest_status after reset");

    assert_eq!(statuses.get(&ds).cloned(), Some(PipelineRunStatus::Errored));
}

// ---------------------------------------------------------------------------
// (e) reset_orphans does NOT rewrite a row with a COMPLETED successor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reset_orphans_does_not_rewrite_completed_successor() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let pr = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();

    // Write INITIATED → COMPLETED (completed pipeline run, not an orphan).
    for status in [PipelineRunStatus::Initiated, PipelineRunStatus::Completed] {
        repo.log_pipeline_run(pr, pipeline_id, "done_p", Some(ds), status, None)
            .await
            .expect("log");
    }

    let count = repo
        .reset_orphans("should_not_match")
        .await
        .expect("reset_orphans");

    // The most recent row for this pipeline_run_id is COMPLETED, not an orphan.
    assert_eq!(count, 0, "completed pipeline should not be rewritten");
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
// `crates/core/src/pipeline_run_registry/data_info.rs`. The tests below
// confirm that the persisted JSON makes a full round-trip through SeaORM
// (write → read) preserving Python-parity byte shape, and that the
// four-state lifecycle / dataset_id=None / reset-helper paths behave as
// locked decisions 1, 4, 5, 7 mandate.

// ---------------------------------------------------------------------------
// (08-09) Four-state lifecycle round-trip — Initiated → Started → Completed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn four_state_lifecycle_round_trip_completed() {
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
// (08-09) Four-state lifecycle round-trip — Initiated → Started → Errored
// ---------------------------------------------------------------------------

#[tokio::test]
async fn four_state_lifecycle_round_trip_errored() {
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
// (08-09) Exact run_info JSON shape — STARTED row stores `{"data":[<uuid>]}`
// ---------------------------------------------------------------------------
//
// Tests the persistence round-trip: write a typed Started row with a
// hand-built `{"data": [uuid, uuid]}` payload (same shape as
// `cognee_core::pipeline_run_registry::run_info_for_running(&ids)`), read
// the row back, and serialise the column to a JSON string.  The string
// must be byte-identical including key ordering (locked decision 5).
#[tokio::test]
async fn run_info_started_shape_is_byte_identical_to_python() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let uuid1 = Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid lit");
    let uuid2 = Uuid::parse_str("00000000-0000-0000-0000-000000000002").expect("uuid lit");

    // Build the payload exactly like run_info_for_running(&[uuid1, uuid2]).
    let mut m = Map::with_capacity(1);
    m.insert(
        "data".into(),
        Value::Array(vec![
            Value::String(uuid1.to_string()),
            Value::String(uuid2.to_string()),
        ]),
    );
    let payload = Value::Object(m);
    // Sanity check the input matches the byte-identical wire shape.
    assert_eq!(
        payload.to_string(),
        "{\"data\":[\"00000000-0000-0000-0000-000000000001\",\
         \"00000000-0000-0000-0000-000000000002\"]}"
    );

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    repo.log_pipeline_run(
        prid,
        pid,
        "shape_p",
        Some(ds),
        PipelineRunStatus::Started,
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
    // Byte-identical, including key order and absence of whitespace.
    assert_eq!(
        run_info.to_string(),
        "{\"data\":[\"00000000-0000-0000-0000-000000000001\",\
         \"00000000-0000-0000-0000-000000000002\"]}"
    );
}

// ---------------------------------------------------------------------------
// (08-09) Exact run_info JSON shape — ERRORED row stores `data` BEFORE `error`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_info_errored_shape_preserves_data_before_error() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let uuid1 = Uuid::parse_str("00000000-0000-0000-0000-000000000003").expect("uuid lit");

    // run_info_for_errored(&[uuid1], "boom"): keys "data" then "error".
    let mut m = Map::with_capacity(2);
    m.insert(
        "data".into(),
        Value::Array(vec![Value::String(uuid1.to_string())]),
    );
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

    // Byte-identical wire shape (data first, then error).
    assert_eq!(
        run_info.to_string(),
        "{\"data\":[\"00000000-0000-0000-0000-000000000003\"],\"error\":\"boom\"}"
    );

    let obj = run_info.as_object().expect("object");
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["data", "error"], "data must precede error");
}

// ---------------------------------------------------------------------------
// (08-09) Exact run_info JSON shape — empty data_ids emit `{"data":"None"}`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_info_running_shape_with_empty_data_writes_none_literal() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    // run_info_for_running(&[]) → {"data": "None"} (literal string).
    let mut m = Map::with_capacity(1);
    m.insert("data".into(), Value::String("None".to_string()));
    let payload = Value::Object(m);

    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    repo.log_pipeline_run(
        prid,
        pid,
        "none_p",
        Some(ds),
        PipelineRunStatus::Started,
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
    assert_eq!(run_info.to_string(), "{\"data\":\"None\"}");
}

// ---------------------------------------------------------------------------
// (08-09) Exact run_info JSON shape — INITIATED row stores `{}`
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
// Reset-helper tests live in `crates/lib/tests/pipeline_runs_reset.rs`
// because `reset_pipeline_run_status` / `reset_dataset_pipeline_run_status`
// are part of the `cognee-lib` public API and adding `cognee-lib` as a
// dev-dependency of `cognee-database` would create a build cycle.
// ---------------------------------------------------------------------------
