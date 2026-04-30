//! Tests for the pipeline payload event channel introduced by LIB-06.
//!
//! Covers:
//! - Tasks running inside `cognee_core::execute()` can publish payload via
//!   `TaskContext::publish_payload_field` and the watcher receives them.
//! - Concurrent tasks publishing distinct keys all surface to the watcher.
//! - The helper silently no-ops when no watcher is attached.
//! - The helper silently no-ops when `pipeline_ctx.run_id` is `None`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use cognee_core::cancellation::cancellation_pair;
use cognee_core::error::CoreError;
use cognee_core::exec_status::NoopExecStatusManager;
use cognee_core::pipeline::{Pipeline, PipelineStatus, PipelineWatcher, TaskStatus, execute};
use cognee_core::progress::ProgressToken;
use cognee_core::task::{Task, Value};
use cognee_core::task_context::{PipelineContext, TaskContext};
use cognee_core::thread_pool::CpuPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Fixtures
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

/// Watcher that records every payload event it receives.
#[derive(Default)]
struct RecordingWatcher {
    events: Mutex<Vec<(Uuid, String, serde_json::Value)>>,
}

impl RecordingWatcher {
    fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> Vec<(Uuid, String, serde_json::Value)> {
        self.events.lock().unwrap().clone() // lock poison is unrecoverable
    }
}

#[async_trait]
impl PipelineWatcher for RecordingWatcher {
    async fn on_pipeline(&self, _pipeline_id: Uuid, _status: PipelineStatus) {}

    async fn on_task(
        &self,
        _pipeline_id: Uuid,
        _task_index: usize,
        _task_name: Option<&str>,
        _total_tasks: usize,
        _status: TaskStatus,
    ) {
    }

    async fn on_payload_field(&self, run_id: Uuid, key: &str, value: serde_json::Value) {
        self.events
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push((run_id, key.to_string(), value));
    }
}

async fn make_ctx_with_watcher(watcher: Arc<dyn PipelineWatcher>) -> Arc<TaskContext> {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
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
            pipeline_name: "test_pipeline".to_string(),
            user_id: None,
            dataset_id: None,
            current_data: None,
            run_id: None,
        }),
        exec_status: Arc::new(NoopExecStatusManager),
        pipeline_watcher: Some(watcher),
    })
}

async fn make_ctx_without_watcher() -> Arc<TaskContext> {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
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
            pipeline_name: "no_watcher".to_string(),
            user_id: None,
            dataset_id: None,
            current_data: None,
            run_id: Some(Uuid::new_v4()),
        }),
        exec_status: Arc::new(NoopExecStatusManager),
        pipeline_watcher: None,
    })
}

async fn make_ctx_no_pipeline_or_run_id() -> Arc<TaskContext> {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
    let (_handle, token) = cancellation_pair();
    Arc::new(TaskContext {
        thread_pool: Arc::new(StubPool),
        database: Arc::new(db),
        graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
        vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
        cancellation: token,
        progress: ProgressToken::new(),
        pipeline_ctx: None,
        exec_status: Arc::new(NoopExecStatusManager),
        pipeline_watcher: Some(Arc::new(RecordingWatcher::new())),
    })
}

// ---------------------------------------------------------------------------
// 1) Tasks can publish payload via execute()
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tasks_can_publish_payload_field_during_execute() {
    let watcher = Arc::new(RecordingWatcher::new());
    let watcher_dyn: Arc<dyn PipelineWatcher> = watcher.clone();

    let task = Task::Async(Arc::new(move |input, ctx| {
        Box::pin(async move {
            ctx.publish_payload_field("the_key", serde_json::json!("the_value"))
                .await;
            Ok(input)
        })
    }));

    let pipeline = Pipeline::new("payload_publish_pipeline").with_task(task);

    let ctx = make_ctx_with_watcher(watcher_dyn).await;
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(1_i32)];

    // We need a no-op observer for the executor; pass the same watcher_dyn so
    // the events are also routed via the dyn observer call. The TaskContext
    // already has the watcher attached for `publish_payload_field` to dispatch.
    let observer = cognee_core::pipeline::NoopWatcher;
    execute(&pipeline, inputs, ctx, &observer)
        .await
        .expect("execute should succeed");

    let events = watcher.snapshot();
    assert_eq!(events.len(), 1, "expected 1 payload event, got {events:?}");
    let (run_id, key, value) = &events[0];
    assert!(!run_id.is_nil(), "run_id should be set by execute()");
    assert_eq!(key, "the_key");
    assert_eq!(value, &serde_json::json!("the_value"));
}

// ---------------------------------------------------------------------------
// 2) Multiple concurrent tasks can each publish distinct keys
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_tasks_can_publish_concurrent_payload_fields() {
    let watcher = Arc::new(RecordingWatcher::new());
    let watcher_dyn: Arc<dyn PipelineWatcher> = watcher.clone();

    // The task identifies the input value and emits a payload field whose key
    // is "key_<input>". With concurrency=4 and 4 inputs, all four tasks run
    // in parallel through `buffer_unordered`.
    let task = Task::Async(Arc::new(move |input, ctx| {
        Box::pin(async move {
            let val = (*input)
                .as_any()
                .downcast_ref::<i32>()
                .copied()
                .unwrap_or(-1);
            let key = format!("key_{val}");
            ctx.publish_payload_field(&key, serde_json::json!(val))
                .await;
            Ok(input)
        })
    }));

    let pipeline = Pipeline::new("concurrent_payload_pipeline")
        .with_task(task)
        .with_concurrency(4);

    let ctx = make_ctx_with_watcher(watcher_dyn).await;
    let inputs: Vec<Arc<dyn Value>> = vec![
        Arc::new(0_i32),
        Arc::new(1_i32),
        Arc::new(2_i32),
        Arc::new(3_i32),
    ];

    let observer = cognee_core::pipeline::NoopWatcher;
    execute(&pipeline, inputs, ctx, &observer)
        .await
        .expect("execute should succeed");

    let events = watcher.snapshot();
    let mut seen: HashMap<String, serde_json::Value> = HashMap::new();
    let mut run_ids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for (rid, k, v) in events {
        run_ids.insert(rid);
        seen.insert(k, v);
    }
    assert_eq!(seen.len(), 4, "expected 4 distinct keys, got {seen:?}");
    for i in 0..4 {
        let key = format!("key_{i}");
        assert_eq!(
            seen.get(&key),
            Some(&serde_json::json!(i)),
            "expected {key} = {i}"
        );
    }
    assert_eq!(
        run_ids.len(),
        1,
        "all events should share a single run_id, got {run_ids:?}"
    );
}

// ---------------------------------------------------------------------------
// 3) Helper silently no-ops when no watcher is attached
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_payload_field_silently_noops_when_no_watcher() {
    let ctx = make_ctx_without_watcher().await;
    // Should not panic.
    ctx.publish_payload_field("k", serde_json::json!("v")).await;
}

// ---------------------------------------------------------------------------
// 4) Helper silently no-ops when run_id is None / pipeline_ctx is None
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_payload_field_silently_noops_when_no_run_id() {
    let ctx = make_ctx_no_pipeline_or_run_id().await;
    // pipeline_ctx is None — should not panic.
    ctx.publish_payload_field("k", serde_json::json!("v")).await;
}
