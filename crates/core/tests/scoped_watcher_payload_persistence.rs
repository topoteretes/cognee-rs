//! End-to-end persistence tests for the LIB-06 payload event channel.
//!
//! Wires a `ScopedRunWatcher` against a real in-memory SQLite repo and a
//! failing repo respectively, runs a pipeline whose task emits payload via
//! `TaskContext::publish_payload_field`, and asserts:
//! - the payload is read back via `repo.get_payload(run_id)`,
//! - persistence failures are best-effort (don't abort the pipeline).

#![cfg(feature = "pipeline-run-registry")]

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_core::cancellation::cancellation_pair;
use cognee_core::error::CoreError;
use cognee_core::exec_status::NoopExecStatusManager;
use cognee_core::pipeline::{NoopWatcher, Pipeline, PipelineWatcher, execute};
use cognee_core::pipeline_run_registry::scoped_watcher::{PerRunSink, ScopedRunWatcher};
use cognee_core::pipeline_run_registry::types::RunPhase;
use cognee_core::progress::ProgressToken;
use cognee_core::task::{Task, Value};
use cognee_core::task_context::{PipelineContext, TaskContext};
use cognee_core::thread_pool::CpuPool;
use cognee_database::{
    DatabaseConnection, DatabaseError, PipelineRunRepository, PipelineRunStatus,
    SeaOrmPipelineRunRepository, connect, initialize,
};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Stub pool (no real CPU work)
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

// ---------------------------------------------------------------------------
// Failing repo: every call returns Err so we can assert pipeline still runs
// ---------------------------------------------------------------------------

struct FailingRepo;

#[async_trait]
impl PipelineRunRepository for FailingRepo {
    async fn log_pipeline_run(
        &self,
        pipeline_run_id: Uuid,
        _pipeline_id: Uuid,
        _pipeline_name: &str,
        _dataset_id: Option<Uuid>,
        _status: PipelineRunStatus,
        _run_info: Option<serde_json::Value>,
    ) -> Result<Uuid, DatabaseError> {
        // Some tests want this to succeed even if payload writes fail; here
        // we let it succeed so we can isolate the payload-failure path.
        Ok(pipeline_run_id)
    }

    async fn latest_status(
        &self,
        _dataset_ids: &[Uuid],
        _pipeline_name: &str,
    ) -> Result<HashMap<Uuid, PipelineRunStatus>, DatabaseError> {
        Ok(HashMap::new())
    }

    async fn list_recent(
        &self,
        _dataset_id: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<cognee_database::PipelineRun>, DatabaseError> {
        Ok(Vec::new())
    }

    async fn reset_orphans(&self, _reason: &str) -> Result<u64, DatabaseError> {
        Ok(0)
    }

    async fn set_payload_field(
        &self,
        _run_id: Uuid,
        _key: &str,
        _value: serde_json::Value,
    ) -> Result<(), DatabaseError> {
        Err(DatabaseError::QueryError(
            "synthetic failure for test".to_string(),
        ))
    }

    async fn get_payload(
        &self,
        _run_id: Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DatabaseError> {
        Err(DatabaseError::QueryError(
            "synthetic failure for test".to_string(),
        ))
    }

    async fn list_pipeline_names_for_dataset(
        &self,
        _dataset_id: Uuid,
    ) -> Result<Vec<(String, PipelineRunStatus)>, DatabaseError> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn make_real_repo() -> (
    Arc<dyn PipelineRunRepository>,
    Arc<DatabaseConnection>,
    Arc<SeaOrmPipelineRunRepository>,
) {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    let db = Arc::new(db);
    let repo_concrete = Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)));
    let repo: Arc<dyn PipelineRunRepository> = repo_concrete.clone();
    (repo, db, repo_concrete)
}

fn make_scoped_watcher(
    run_id: Uuid,
    repo: Arc<dyn PipelineRunRepository>,
) -> Arc<ScopedRunWatcher> {
    let (event_tx, _rx) = broadcast::channel(64);
    let (phase_tx, _phase_rx) = watch::channel(RunPhase::Pending);
    let sink = PerRunSink::from_parts(run_id, event_tx, phase_tx);
    Arc::new(ScopedRunWatcher::new(run_id, sink, repo))
}

async fn make_ctx(watcher: Arc<dyn PipelineWatcher>) -> Arc<TaskContext> {
    let db = connect("sqlite::memory:").await.expect("ctx db connect");
    initialize(&db).await.expect("ctx db init");
    let (_handle, token) = cancellation_pair();
    Arc::new(TaskContext {
        thread_pool: Arc::new(StubPool),
        database: Arc::new(db),
        graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
        vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
        cancellation: token,
        progress: ProgressToken::new(),
        pipeline_ctx: Some(PipelineContext {
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "scoped_watcher_payload_test".to_string(),
            user_id: None,
            tenant_id: None,
            dataset_id: None,
            current_data: None,
            run_id: None,
            user_email: None,
            provenance_visited: Arc::new(Mutex::new(HashSet::new())),
        }),
        exec_status: Arc::new(NoopExecStatusManager),
        pipeline_watcher: Some(watcher),
    })
}

fn payload_publishing_task() -> Task {
    Task::Async(Arc::new(move |input, ctx| {
        Box::pin(async move {
            ctx.publish_payload_field("items_processed", serde_json::json!(3))
                .await;
            ctx.publish_payload_field("note", serde_json::json!("hello"))
                .await;
            Ok(input)
        })
    }))
}

// ---------------------------------------------------------------------------
// 1. Real repo: payload events end up in the DB
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scoped_watcher_persists_payload_via_repo() {
    struct CapturingWatcher {
        inner: Arc<ScopedRunWatcher>,
        captured_run_id: Mutex<Option<Uuid>>,
    }

    #[async_trait]
    impl PipelineWatcher for CapturingWatcher {
        async fn on_pipeline(
            &self,
            pipeline_id: Uuid,
            status: cognee_core::pipeline::PipelineStatus,
        ) {
            self.inner.on_pipeline(pipeline_id, status).await;
        }

        async fn on_task(
            &self,
            pipeline_id: Uuid,
            task_index: usize,
            task_name: Option<&str>,
            total_tasks: usize,
            status: cognee_core::pipeline::TaskStatus,
        ) {
            self.inner
                .on_task(pipeline_id, task_index, task_name, total_tasks, status)
                .await;
        }

        async fn on_pipeline_run_started(&self, run: &cognee_core::pipeline::PipelineRunInfo) {
            *self.captured_run_id.lock().unwrap() = Some(run.run_id);
            self.inner.on_pipeline_run_started(run).await;
        }

        async fn on_payload_field(&self, run_id: Uuid, key: &str, value: serde_json::Value) {
            self.inner.on_payload_field(run_id, key, value).await;
        }
    }

    let (repo, _db, repo_concrete) = make_real_repo().await;
    let placeholder = Uuid::nil();
    let inner = make_scoped_watcher(placeholder, Arc::clone(&repo));
    let capturing = Arc::new(CapturingWatcher {
        inner,
        captured_run_id: Mutex::new(None),
    });
    let watcher_dyn: Arc<dyn PipelineWatcher> = capturing.clone();

    let pipeline =
        Pipeline::new("payload_persistence_pipeline").with_task(payload_publishing_task());
    let ctx = make_ctx(Arc::clone(&watcher_dyn)).await;
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];

    // Pass the capturing watcher as the executor's observer so that
    // `on_pipeline_run_started` fires (the executor only calls lifecycle
    // events on its `&dyn PipelineWatcher` parameter, not on the
    // `TaskContext::pipeline_watcher` field).
    execute(&pipeline, inputs, ctx, capturing.as_ref())
        .await
        .expect("execute should succeed");

    let run_id = capturing
        .captured_run_id
        .lock()
        .unwrap()
        .expect("on_pipeline_run_started should have captured a run_id");

    let payload = repo_concrete
        .get_payload(run_id)
        .await
        .expect("get_payload");

    assert_eq!(
        payload.get("items_processed"),
        Some(&serde_json::json!(3)),
        "items_processed should have been persisted"
    );
    assert_eq!(
        payload.get("note"),
        Some(&serde_json::json!("hello")),
        "note should have been persisted"
    );
}

// ---------------------------------------------------------------------------
// 2. Failing repo: pipeline still completes even when persistence fails
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scoped_watcher_logs_warning_on_payload_persist_failure() {
    let repo: Arc<dyn PipelineRunRepository> = Arc::new(FailingRepo);
    let watcher = make_scoped_watcher(Uuid::nil(), Arc::clone(&repo));
    let watcher_dyn: Arc<dyn PipelineWatcher> = watcher;

    let pipeline = Pipeline::new("failing_repo_pipeline").with_task(payload_publishing_task());
    let ctx = make_ctx(Arc::clone(&watcher_dyn)).await;
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
    let observer = NoopWatcher;

    let result = execute(&pipeline, inputs, ctx, &observer).await;
    assert!(
        result.is_ok(),
        "pipeline should succeed despite persistence failures, got_err={}",
        result.err().map(|e| e.to_string()).unwrap_or_default()
    );
}
