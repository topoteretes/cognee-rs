use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use cognee_core::cancellation::cancellation_pair;
use cognee_core::{
    CpuPool, NoopExecStatusManager, NoopWatcher, Pipeline, ProgressToken, Task, TaskContext,
    TaskError, Value, execute,
};

// ---------------------------------------------------------------------------
// Stub thread pool (no real work — just satisfies the trait bound)
// ---------------------------------------------------------------------------

struct StubPool;

impl CpuPool for StubPool {
    fn spawn_raw(
        &self,
        _task: Box<dyn FnOnce() + Send + 'static>,
    ) -> Pin<Box<dyn Future<Output = Result<(), cognee_core::CoreError>> + Send + 'static>> {
        Box::pin(async { Ok(()) })
    }
}

async fn stub_ctx() -> Arc<TaskContext> {
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
    })
}

/// Helper to extract `&i32` from a type-erased `Arc<dyn Value>`.
fn as_i32(v: &Arc<dyn Value>) -> i32 {
    *(**v).as_any().downcast_ref::<i32>().expect("expected i32")
}

// ---------------------------------------------------------------------------
// H1.1 — chained tasks produce correct output
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chained_tasks_produce_correct_output() {
    // Task 1: generate values [1, 2, 3, 4, 5] from any input.
    let generate = Task::SyncIter(Arc::new(|_input, _ctx| {
        let iter = (1..=5).map(|i| Box::new(i) as Box<dyn Value>);
        Ok(Box::new(iter) as Box<dyn Iterator<Item = Box<dyn Value>> + Send>)
    }));

    // Task 2: add 10 to each value.
    let add_ten =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x + 10)) });

    // Task 3: double each value.
    let double =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x * 2)) });

    let pipeline = Pipeline::new("chained math")
        .with_task(generate)
        .with_task(add_ten)
        .with_task(double);

    // The SyncIter task ignores its input, so pass a dummy.
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
    let ctx = stub_ctx().await;

    let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

    let values: Vec<i32> = outputs.iter().map(as_i32).collect();
    // [1,2,3,4,5] + 10 each = [11,12,13,14,15], doubled = [22,24,26,28,30]
    assert_eq!(values, vec![22, 24, 26, 28, 30]);
}

// ---------------------------------------------------------------------------
// H1.2 — pipeline handles empty input
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pipeline_handles_empty_input() {
    let passthrough =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x)) });

    let pipeline = Pipeline::new("empty input test").with_task(passthrough);

    let inputs: Vec<Arc<dyn Value>> = vec![];
    let ctx = stub_ctx().await;

    let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

    assert!(
        outputs.is_empty(),
        "empty inputs should produce empty outputs"
    );
}
