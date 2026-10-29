#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests for `ScopedRunWatcher` — the registry's `PipelineWatcher` proxy.
//!
//! Drives a fake pipeline through lifecycle events and asserts:
//! - Events flow through the broadcast channel in order.
//! - DB rows are written for Started/Completed/Errored.
//! - Yield events do NOT trigger DB writes.
//! - `RegistryConfig::yield_throttle` is structurally correct.

#![cfg(feature = "pipeline-run-registry")]

use std::sync::Arc;

use cognee_core::pipeline::{PipelineRunInfo, PipelineRunStatus as CoreStatus, PipelineWatcher};
use cognee_core::pipeline_run_registry::scoped_watcher::{PerRunSink, ScopedRunWatcher};
use cognee_core::pipeline_run_registry::types::{RunEvent, RunEventKind, RunPhase};
use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus as DbStatus,
    SeaOrmPipelineRunRepository, connect, initialize, ops,
};
use cognee_models::Dataset;
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

async fn make_repo() -> (Arc<dyn PipelineRunRepository>, Arc<DatabaseConnection>) {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    let db = Arc::new(db);
    let repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)));
    (repo, db)
}

/// Pre-create a dataset row so FK constraints on `pipeline_runs.dataset_id` pass.
async fn create_dataset(db: &DatabaseConnection, id: Uuid) {
    let owner = Uuid::new_v4();
    let dataset = Dataset::new("test".to_string(), owner, None, id);
    ops::datasets::create_dataset(db, dataset)
        .await
        .expect("create_dataset for FK setup");
}

fn make_watcher(
    run_id: Uuid,
    repo: Arc<dyn PipelineRunRepository>,
) -> (ScopedRunWatcher, tokio::sync::broadcast::Receiver<RunEvent>) {
    let (event_tx, rx) = broadcast::channel::<RunEvent>(64);
    let (phase_tx, _phase_rx) = watch::channel(RunPhase::Pending);

    let sink = PerRunSink::from_parts(event_tx, phase_tx);
    let watcher = ScopedRunWatcher::new(run_id, sink, repo);
    (watcher, rx)
}

fn fake_run(run_id: Uuid, status: CoreStatus) -> PipelineRunInfo {
    PipelineRunInfo {
        run_id,
        pipeline_id: Uuid::new_v4(),
        pipeline_name: "fake_pipeline".to_string(),
        user_id: None,
        tenant_id: None,
        dataset_id: None,
        status,
        started_at: chrono::Utc::now(),
        completed_at: None,
        recovery_lock: None,
    }
}

// ---------------------------------------------------------------------------
// The trail is STARTED then a terminal row, with Python's `run_info` shapes:
// STARTED `{}`, COMPLETED `{}`, ERRORED `{"error": …}`. No INITIATED row and
// no `data` key (Python `dc0b4ad6a`, `a88402500` / COG-6089).
// ---------------------------------------------------------------------------

async fn trail(
    repo: &Arc<dyn PipelineRunRepository>,
    ds: Uuid,
) -> Vec<(DbStatus, serde_json::Value)> {
    let mut rows = repo.list_recent(Some(ds), 10).await.expect("list_recent");
    // `list_recent` is by `created_at DESC` — reverse to chronological order.
    rows.reverse();
    rows.into_iter()
        .map(|r| (r.status, r.run_info.expect("run_info is always written")))
        .collect()
}

#[tokio::test]
async fn completed_trail_is_started_then_completed_with_empty_run_info() {
    let (repo, db) = make_repo().await;
    let run_id = Uuid::new_v4();
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let (watcher, _rx) = make_watcher(run_id, Arc::clone(&repo));

    let mut run = fake_run(run_id, CoreStatus::Started);
    run.dataset_id = Some(ds);
    watcher.on_pipeline_run_started(&run).await;
    run.status = CoreStatus::Completed;
    watcher.on_pipeline_run_completed(&run, 1).await;

    assert_eq!(
        trail(&repo, ds).await,
        vec![
            (DbStatus::Started, serde_json::json!({})),
            (DbStatus::Completed, serde_json::json!({})),
        ]
    );
}

/// Python's `log_pipeline_run_start(recovery_token=…)`: the STARTED row records
/// the run's ownership token, and only that row does.
#[tokio::test]
async fn started_row_carries_the_recovery_lock_when_the_run_has_one() {
    let (repo, db) = make_repo().await;
    let run_id = Uuid::new_v4();
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let (watcher, _rx) = make_watcher(run_id, Arc::clone(&repo));
    let token = Uuid::new_v4();

    let mut run = fake_run(run_id, CoreStatus::Started);
    run.dataset_id = Some(ds);
    run.recovery_lock = Some(token);
    watcher.on_pipeline_run_started(&run).await;
    run.status = CoreStatus::Completed;
    watcher.on_pipeline_run_completed(&run, 1).await;

    assert_eq!(
        trail(&repo, ds).await,
        vec![
            (
                DbStatus::Started,
                serde_json::json!({ "recovery_lock": token.to_string() })
            ),
            (DbStatus::Completed, serde_json::json!({})),
        ]
    );
}

#[tokio::test]
async fn errored_trail_carries_only_the_error() {
    let (repo, db) = make_repo().await;
    let run_id = Uuid::new_v4();
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let (watcher, _rx) = make_watcher(run_id, Arc::clone(&repo));

    let mut run = fake_run(run_id, CoreStatus::Started);
    run.dataset_id = Some(ds);
    watcher.on_pipeline_run_started(&run).await;
    run.status = CoreStatus::Errored;
    watcher.on_pipeline_run_errored(&run, "boom").await;

    assert_eq!(
        trail(&repo, ds).await,
        vec![
            (DbStatus::Started, serde_json::json!({})),
            (DbStatus::Errored, serde_json::json!({"error": "boom"})),
        ]
    );
}

// ---------------------------------------------------------------------------
// Events flow in order: Started → Completed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn events_flow_in_order_started_completed() {
    let (repo, _db) = make_repo().await;
    let run_id = Uuid::new_v4();
    let (watcher, mut rx) = make_watcher(run_id, Arc::clone(&repo));

    // dataset_id is None — DB write is skipped, but events still flow.
    let run = fake_run(run_id, CoreStatus::Started);
    watcher.on_pipeline_run_started(&run).await;

    let event = rx.try_recv().expect("should have Started event");
    assert!(
        matches!(event.kind, RunEventKind::Started),
        "got {:?}",
        event.kind
    );

    // Now complete.
    let run = fake_run(run_id, CoreStatus::Completed);
    watcher.on_pipeline_run_completed(&run, 1).await;

    let event = rx.try_recv().expect("should have Completed event");
    assert!(
        matches!(event.kind, RunEventKind::Completed),
        "got {:?}",
        event.kind
    );
}

// ---------------------------------------------------------------------------
// Started and Completed write DB rows
// ---------------------------------------------------------------------------

#[tokio::test]
async fn started_and_completed_write_db_rows() {
    let (repo, db) = make_repo().await;
    let run_id = Uuid::new_v4();
    let ds = Uuid::new_v4();
    // Pre-create the dataset row so the FK constraint passes.
    create_dataset(&db, ds).await;
    let (watcher, _rx) = make_watcher(run_id, Arc::clone(&repo));

    let mut run = fake_run(run_id, CoreStatus::Started);
    run.dataset_id = Some(ds);
    run.pipeline_name = "db_write_test".to_string();
    let triggered_by = Uuid::new_v4();
    run.user_id = Some(triggered_by);

    watcher.on_pipeline_run_started(&run).await;
    watcher.on_pipeline_run_completed(&run, 1).await;

    // Both rows record who triggered the run (`pipeline_runs.user_id`).
    let rows = repo.list_recent(Some(ds), 10).await.expect("list_recent");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|r| r.user_id == Some(triggered_by)),
        "{rows:?}"
    );

    // Verify rows were written.
    let statuses = repo
        .latest_status(&[ds], "db_write_test")
        .await
        .expect("latest_status");

    assert_eq!(
        statuses.get(&ds).cloned(),
        Some(DbStatus::Completed),
        "expected Completed DB status after watcher events"
    );
}

// ---------------------------------------------------------------------------
// Errored writes DB row and emits Errored event
// ---------------------------------------------------------------------------

#[tokio::test]
async fn errored_writes_db_row_and_emits_event() {
    let (repo, db) = make_repo().await;
    let run_id = Uuid::new_v4();
    let ds = Uuid::new_v4();
    // Pre-create the dataset row so the FK constraint passes.
    create_dataset(&db, ds).await;
    let (watcher, mut rx) = make_watcher(run_id, Arc::clone(&repo));

    let mut run = fake_run(run_id, CoreStatus::Started);
    run.dataset_id = Some(ds);
    run.pipeline_name = "errored_test".to_string();

    // Emit Started first.
    watcher.on_pipeline_run_started(&run).await;
    let _ = rx.try_recv(); // consume Started

    // Emit Errored.
    watcher
        .on_pipeline_run_errored(&run, "something went wrong")
        .await;

    let event = rx.try_recv().expect("should have Errored event");
    assert!(
        matches!(&event.kind, RunEventKind::Errored { message } if message == "something went wrong"),
        "got {:?}",
        event.kind
    );

    // Verify DB row.
    let statuses = repo
        .latest_status(&[ds], "errored_test")
        .await
        .expect("latest_status");

    assert_eq!(
        statuses.get(&ds).cloned(),
        Some(DbStatus::Errored),
        "expected Errored DB status"
    );
}

// ---------------------------------------------------------------------------
// RegistryConfig::yield_throttle is structurally correct
// ---------------------------------------------------------------------------

#[test]
fn yield_throttle_config_is_structurally_correct() {
    use cognee_core::RegistryConfig;

    let default_cfg = RegistryConfig::default();
    assert!(
        default_cfg.yield_throttle.is_none(),
        "default is no throttle"
    );

    let throttled = RegistryConfig {
        yield_throttle: Some(std::time::Duration::from_millis(50)),
        ..RegistryConfig::default()
    };
    assert_eq!(
        throttled.yield_throttle,
        Some(std::time::Duration::from_millis(50))
    );
}
