#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Executor-level lifecycle tests for `pipeline::execute` (task 08-09).
//!
//! Validates that `pipeline::execute` writes the four-state `pipeline_runs`
//! trail (INITIATED → STARTED → COMPLETED / ERRORED) through a
//! `DbPipelineWatcher` backed by the real `SeaOrmPipelineRunRepository`
//! (in-memory SQLite). Also confirms locked decision 1: empty pipelines
//! produce zero rows (early-return before INITIATED).
//!
//! See [docs/telemetry/08/09-tests.md](../../../docs/telemetry/08/09-tests.md).

#![cfg(feature = "pipeline-run-registry")]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use cognee_core::cancellation::cancellation_pair;
use cognee_core::pipeline_run_registry::DbPipelineWatcher;
use cognee_core::{
    CoreError, CpuPool, ExecutionError, NoopExecStatusManager, Pipeline, PipelineContext,
    ProgressToken, Task, TaskContext, TaskError, Value, execute,
};
use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository,
    connect, initialize,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Stub thread pool (mirrors `crates/core/tests/pipeline_chained_tasks.rs`)
// ---------------------------------------------------------------------------

struct StubPool;

impl CpuPool for StubPool {
    fn spawn_raw(
        &self,
        _task: Box<dyn FnOnce() + Send + 'static>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'static>> {
        Box::pin(async { Ok(()) })
    }
}

async fn make_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

fn make_repo(db: Arc<DatabaseConnection>) -> Arc<dyn PipelineRunRepository> {
    Arc::new(SeaOrmPipelineRunRepository::new(db))
}

async fn stub_ctx(db: Arc<DatabaseConnection>) -> Arc<TaskContext> {
    let (_handle, token) = cancellation_pair();
    Arc::new(TaskContext {
        thread_pool: Arc::new(StubPool),
        database: db,
        graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
        vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
        cancellation: token,
        progress: ProgressToken::new(),
        // Provide a `PipelineContext` so the executor surfaces
        // `user_id`/`dataset_id` on the `PipelineRunInfo` carrier.
        pipeline_ctx: Some(PipelineContext {
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "lifecycle_test".to_string(),
            user_id: Some(Uuid::new_v4()),
            tenant_id: None,
            dataset_id: None,
            current_data: None,
            run_id: None,
            user_email: None,
            provenance_visited: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }),
        exec_status: Arc::new(NoopExecStatusManager),
        pipeline_watcher: None,
    })
}

// ---------------------------------------------------------------------------
// (a) Success path: INITIATED + STARTED + COMPLETED, in `created_at` order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execute_writes_four_state_trail_on_success() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let watcher = DbPipelineWatcher::new(Arc::clone(&repo));

    // Trivial single-task pipeline.
    let pass =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x + 1)) });
    let pipeline = Pipeline::new("lifecycle_ok").with_task(pass);

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(7_i32)];
    let ctx = stub_ctx(Arc::clone(&db)).await;

    let outputs = execute(&pipeline, inputs, ctx, &watcher)
        .await
        .expect("execute succeeds");
    assert_eq!(outputs.len(), 1);

    let rows = repo.list_recent(None, 50).await.expect("list_recent");
    // Exactly three rows: INITIATED, STARTED, COMPLETED.
    assert_eq!(
        rows.len(),
        3,
        "expected INITIATED + STARTED + COMPLETED; got {} rows",
        rows.len()
    );

    // list_recent is ORDER BY created_at DESC: index 0 is the freshest.
    assert_eq!(rows[0].status, PipelineRunStatus::Completed);
    assert_eq!(rows[1].status, PipelineRunStatus::Started);
    assert_eq!(rows[2].status, PipelineRunStatus::Initiated);

    // All three rows share the same pipeline_run_id (decision 12).
    assert_eq!(rows[0].pipeline_run_id, rows[1].pipeline_run_id);
    assert_eq!(rows[1].pipeline_run_id, rows[2].pipeline_run_id);

    // INITIATED row carries `run_info = {}` (decision 5).
    let initiated_run_info = rows[2].run_info.as_ref().expect("run_info populated");
    assert_eq!(initiated_run_info.to_string(), "{}");

    // STARTED + COMPLETED rows carry `{"data": ...}`.
    let started_run_info = rows[1].run_info.as_ref().expect("run_info populated");
    assert!(
        started_run_info.get("data").is_some(),
        "STARTED must carry `data` key (decision 5)"
    );
    let completed_run_info = rows[0].run_info.as_ref().expect("run_info populated");
    assert!(
        completed_run_info.get("data").is_some(),
        "COMPLETED must carry `data` key (decision 5)"
    );
}

// ---------------------------------------------------------------------------
// (b) Failure path: INITIATED + STARTED + ERRORED with `run_info["error"]`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execute_writes_errored_trail_on_failure() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let watcher = DbPipelineWatcher::new(Arc::clone(&repo));

    // Task that always errors.
    let boom = Task::sync_typed(|_x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
        Err("boom -- test failure".into())
    });
    let pipeline = Pipeline::new("lifecycle_err").with_task(boom);

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(1_i32)];
    let ctx = stub_ctx(Arc::clone(&db)).await;

    let result = execute(&pipeline, inputs, ctx, &watcher).await;
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("execute must fail"),
    };
    match err {
        ExecutionError::TaskFailed { .. } => {}
        other => panic!("expected TaskFailed, got: {other}"),
    }

    let rows = repo.list_recent(None, 50).await.expect("list_recent");
    assert_eq!(
        rows.len(),
        3,
        "expected INITIATED + STARTED + ERRORED; got {} rows",
        rows.len()
    );

    assert_eq!(rows[0].status, PipelineRunStatus::Errored);
    assert_eq!(rows[1].status, PipelineRunStatus::Started);
    assert_eq!(rows[2].status, PipelineRunStatus::Initiated);

    // ERRORED row must carry `{"data": …, "error": "..."}` (decision 5).
    let errored = rows[0].run_info.as_ref().expect("run_info populated");
    let obj = errored.as_object().expect("object");
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    assert!(keys.contains(&"data"), "ERRORED run_info must include data");
    assert!(
        keys.contains(&"error"),
        "ERRORED run_info must include error"
    );
    // Key order: `data` precedes `error` (decision 5).
    let data_idx = keys.iter().position(|k| *k == "data").expect("data idx");
    let error_idx = keys.iter().position(|k| *k == "error").expect("error idx");
    assert!(data_idx < error_idx, "`data` must precede `error`");

    let error_message = obj
        .get("error")
        .and_then(|v| v.as_str())
        .expect("error string");
    assert!(
        error_message.contains("boom"),
        "expected error message to surface; got: {error_message}"
    );
}

// ---------------------------------------------------------------------------
// (c) NoTasks: empty pipeline writes zero rows (early-return before INITIATED)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execute_with_no_tasks_writes_no_rows() {
    let db = make_db().await;
    let repo = make_repo(Arc::clone(&db));
    let watcher = DbPipelineWatcher::new(Arc::clone(&repo));

    let pipeline = Pipeline::new("lifecycle_empty");
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
    let ctx = stub_ctx(Arc::clone(&db)).await;

    let result = execute(&pipeline, inputs, ctx, &watcher).await;
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("empty pipeline must fail"),
    };
    assert!(matches!(err, ExecutionError::NoTasks), "got: {err}");

    let rows = repo.list_recent(None, 50).await.expect("list_recent");
    assert!(
        rows.is_empty(),
        "NoTasks early-return must not write any rows; got {} rows",
        rows.len()
    );
}
