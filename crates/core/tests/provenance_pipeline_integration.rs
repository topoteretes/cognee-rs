//! Pipeline-integration tests for provenance stamping (gap 05-10 §4.2).
//!
//! Builds a small in-memory 3-task pipeline emitting `DocumentChunk`
//! DataPoints and runs it through `cognee_core::execute`. Asserts that
//! the executor walked every emitted DataPoint and stamped the
//! pipeline + task names + user label.
//!
//! Two variants:
//!
//! 1. `pipeline_stamps_every_emitted_datapoint` — distinct UUIDs per
//!    output. The first task that touches each DP wins.
//! 2. `visited_set_keeps_first_task_attribution` — same UUID survives
//!    across tasks. Locked decision 2: visited-set short-circuits the
//!    second task; the first task's name is preserved.
//!
//! These tests do not depend on any specific `RUST_LOG` value — the
//! pipeline executor stamps independently of the tracing subscriber
//! (project rule).

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use cognee_core::cancellation::cancellation_pair;
use cognee_core::{
    CoreError, CpuPool, NoopExecStatusManager, NoopWatcher, Pipeline, PipelineContext,
    ProgressToken, Task, TaskContext, TaskInfo, execute,
};
use cognee_models::DocumentChunk;
use uuid::Uuid;

// ── Stub thread pool ────────────────────────────────────────────────────────

struct StubPool;

impl CpuPool for StubPool {
    fn spawn_raw(
        &self,
        _task: Box<dyn FnOnce() + Send + 'static>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'static>> {
        Box::pin(async { Ok(()) })
    }
}

async fn build_ctx(pipeline_ctx: PipelineContext) -> Arc<TaskContext> {
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
        pipeline_ctx: Some(pipeline_ctx),
        exec_status: Arc::new(NoopExecStatusManager),
        pipeline_watcher: None,
    })
}

fn pipeline_ctx(name: &str) -> PipelineContext {
    PipelineContext {
        pipeline_id: Uuid::new_v4(),
        pipeline_name: name.into(),
        user_id: Some(Uuid::new_v4()),
        tenant_id: None,
        dataset_id: None,
        current_data: None,
        run_id: None,
        user_email: Some("alice@example.com".into()),
        provenance_visited: Arc::new(Mutex::new(HashSet::new())),
    }
}

fn make_chunk(text: &str) -> DocumentChunk {
    DocumentChunk::new(
        Uuid::new_v4(),
        text.to_string(),
        text.split_whitespace().count(),
        0,
        "paragraph_end".into(),
        Uuid::new_v4(),
    )
}

// ── Test 1: distinct UUIDs per task — every task gets a chance to stamp ────

#[tokio::test]
async fn pipeline_stamps_every_emitted_datapoint() {
    // Task A: emit three DocumentChunks (SyncIter) — each has a fresh
    // UUID, so stamping at the iter site pins `source_task = "emit_chunks"`.
    let task_a = Task::sync_iter_typed(|_input: &i32, _ctx| {
        let chunks: Vec<Box<DocumentChunk>> = (0..3)
            .map(|i| Box::new(make_chunk(&format!("chunk-{i}"))))
            .collect();
        Ok(chunks.into_iter())
    });

    // Task B: identity over a single chunk — clones into a brand new
    // DocumentChunk (with the existing UUID preserved by `Clone`). The
    // visited-set short-circuit runs because the UUID is reused.
    let task_b = Task::sync_typed(|c: &DocumentChunk, _ctx| Ok(Box::new(c.clone())));

    // Task C: another identity, just to chain three tasks deep.
    let task_c = Task::sync_typed(|c: &DocumentChunk, _ctx| Ok(Box::new(c.clone())));

    let pipeline = Pipeline::new("test_pipeline")
        .with_name("test_pipeline")
        .with_task(TaskInfo::new(task_a).with_name("emit_chunks"))
        .with_task(TaskInfo::new(task_b).with_name("tag_chunks"))
        .with_task(TaskInfo::new(task_c).with_name("mark_chunks"));

    let pctx = pipeline_ctx("test_pipeline");
    let ctx = build_ctx(pctx).await;

    let outputs = execute(&pipeline, vec![Arc::new(0_i32)], ctx, &NoopWatcher)
        .await
        .unwrap();

    assert_eq!(outputs.len(), 3, "expected three chunks to flow through");

    for arc in outputs {
        let chunk = (*arc).as_any().downcast_ref::<DocumentChunk>().unwrap();
        assert_eq!(
            chunk.base.source_pipeline.as_deref(),
            Some("test_pipeline"),
            "every emitted DP must carry the pipeline name"
        );
        // Locked decision 2 guarantees the first task that observes the
        // UUID wins. With distinct UUIDs at emit time and `Clone` reusing
        // them across `tag_chunks` / `mark_chunks`, that first task is
        // always `emit_chunks`.
        assert_eq!(
            chunk.base.source_task.as_deref(),
            Some("emit_chunks"),
            "first stamper wins (visited-set keyed on DataPoint.id)"
        );
        assert_eq!(chunk.base.source_user.as_deref(), Some("alice@example.com"));
    }
}

// ── Test 2: same UUID across tasks — first-task attribution preserved ──────

#[tokio::test]
async fn visited_set_keeps_first_task_attribution() {
    // Task A: emit a single chunk with a known UUID.
    let fixed_id = Uuid::new_v4();
    let task_a = Task::sync_iter_typed(move |_input: &i32, _ctx| {
        let mut chunk = make_chunk("only-chunk");
        chunk.base.id = fixed_id;
        let chunks: Vec<Box<DocumentChunk>> = vec![Box::new(chunk)];
        Ok(chunks.into_iter())
    });

    // Task B: clone — preserves the UUID. Visited-set must short-circuit
    // and `source_task` must remain "emit_chunks".
    let task_b = Task::sync_typed(|c: &DocumentChunk, _ctx| Ok(Box::new(c.clone())));

    let pipeline = Pipeline::new("visited_set_pipeline")
        .with_name("visited_set_pipeline")
        .with_task(TaskInfo::new(task_a).with_name("emit_chunks"))
        .with_task(TaskInfo::new(task_b).with_name("clone_chunks"));

    let pctx = pipeline_ctx("visited_set_pipeline");
    let ctx = build_ctx(pctx).await;

    let outputs = execute(&pipeline, vec![Arc::new(0_i32)], ctx, &NoopWatcher)
        .await
        .unwrap();

    assert_eq!(outputs.len(), 1);
    let chunk = (*outputs[0])
        .as_any()
        .downcast_ref::<DocumentChunk>()
        .unwrap();
    assert_eq!(chunk.base.id, fixed_id);
    assert_eq!(
        chunk.base.source_pipeline.as_deref(),
        Some("visited_set_pipeline")
    );
    assert_eq!(
        chunk.base.source_task.as_deref(),
        Some("emit_chunks"),
        "decision 2: first task to stamp keeps the attribution; \
         clone_chunks must NOT overwrite it"
    );
}
