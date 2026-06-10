//! Integration tests for the `DroppedSentinel` / `is_dropped` filter
//! (Gap 1 — drop/filter sentinel).
//!
//! Tests cover the three executor paths that must honour the sentinel:
//! 1. `Resolved::Single` path in `execute_from` (Steps 2 and 3 in a chain).
//! 2. `Resolved::Iter` path through `process_iter`.
//! 3. A task that returns `DroppedSentinel` when it is the *last* task.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::stream;

use cognee_core::cancellation::cancellation_pair;
use cognee_core::{
    CpuPool, DroppedSentinel, NoopExecStatusManager, NoopWatcher, Pipeline, ProgressToken, Task,
    TaskContext, TaskError, Value, execute,
};

// ---------------------------------------------------------------------------
// Shared infrastructure
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
    let db = cognee_database::connect("sqlite::memory:")
        .await
        .expect("in-memory SQLite always connects");
    cognee_database::initialize(&db)
        .await
        .expect("in-memory schema init never fails");
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
        pipeline_watcher: None,
    })
}

/// Extract an `i32` from a type-erased `Arc<dyn Value>`.
fn as_i32(v: &Arc<dyn Value>) -> i32 {
    *(**v).as_any().downcast_ref::<i32>().expect("expected i32")
}

// ---------------------------------------------------------------------------
// Test 1 — filters every other item (SyncIter + Sync filter task)
//
// A `SyncIter` task yields 0..10, then a `Sync` task returns
// `DroppedSentinel` for odd `n` and the value unchanged for even `n`.
// The final output must be exactly [0, 2, 4, 6, 8].
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filters_every_other_item() {
    // Task 1: yield integers 0..10.
    let generate = Task::SyncIter(Arc::new(|_input, _ctx| {
        let iter = (0..10_i32).map(|i| Box::new(i) as Box<dyn Value>);
        Ok(Box::new(iter) as Box<dyn Iterator<Item = Box<dyn Value>> + Send>)
    }));

    // Task 2: drop odd numbers, pass even numbers through.
    let filter_odds = Task::Sync(Arc::new(|input: Arc<dyn Value>, _ctx| {
        let n = *(*input).as_any().downcast_ref::<i32>().expect("i32");
        if n % 2 != 0 {
            Ok(Arc::new(DroppedSentinel) as Arc<dyn Value>)
        } else {
            Ok(input)
        }
    }));

    let pipeline = Pipeline::new("filter odds")
        .with_task(generate)
        .with_task(filter_odds);

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
    let ctx = stub_ctx().await;
    let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline must not fail");

    let mut values: Vec<i32> = outputs.iter().map(as_i32).collect();
    values.sort_unstable();
    assert_eq!(values, vec![0, 2, 4, 6, 8]);
}

// ---------------------------------------------------------------------------
// Test 2 — drop in the last task
//
// Multiple inputs run through a single task that drops one specific input.
// That input must be absent from the output and no error must be raised.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn drop_in_last_task() {
    // Drop the value 42, pass everything else through.
    let drop_42 = Task::Sync(Arc::new(|input: Arc<dyn Value>, _ctx| {
        let x = *(*input).as_any().downcast_ref::<i32>().expect("i32");
        if x == 42 {
            Ok(Arc::new(DroppedSentinel) as Arc<dyn Value>)
        } else {
            Ok(input)
        }
    }));

    let pipeline = Pipeline::new("drop 42").with_task(drop_42);

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(1_i32), Arc::new(42_i32), Arc::new(99_i32)];
    let ctx = stub_ctx().await;
    let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline must not fail");

    let mut values: Vec<i32> = outputs.iter().map(as_i32).collect();
    values.sort_unstable();
    assert_eq!(values, vec![1, 99]);
}

// ---------------------------------------------------------------------------
// Test 3 — iterator yields sentinels directly
//
// A `SyncIter` task yields a mix of `i32` values and `DroppedSentinel`s.
// A downstream `Sync` task must never receive a sentinel; the final output
// must contain only the real values.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn iter_yields_sentinels_directly() {
    // Yields [DroppedSentinel, 10, DroppedSentinel, 20, DroppedSentinel].
    let mixed_iter = Task::SyncIter(Arc::new(|_input, _ctx| {
        let items: Vec<Box<dyn Value>> = vec![
            Box::new(DroppedSentinel),
            Box::new(10_i32),
            Box::new(DroppedSentinel),
            Box::new(20_i32),
            Box::new(DroppedSentinel),
        ];
        Ok(Box::new(items.into_iter()) as Box<dyn Iterator<Item = Box<dyn Value>> + Send>)
    }));

    // This task must never receive a DroppedSentinel.  If it does, the
    // downcast to `i32` will panic and the test will fail.
    let assert_no_sentinel =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x)) });

    let pipeline = Pipeline::new("iter sentinels")
        .with_task(mixed_iter)
        .with_task(assert_no_sentinel);

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
    let ctx = stub_ctx().await;
    let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline must not fail");

    let mut values: Vec<i32> = outputs.iter().map(as_i32).collect();
    values.sort_unstable();
    assert_eq!(values, vec![10, 20]);
}

// ---------------------------------------------------------------------------
// Test 4 — async stream yields sentinels directly
//
// Same as Test 3 but using an `AsyncStream` task so that `process_stream`
// is exercised.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stream_yields_sentinels_directly() {
    // Yields [DroppedSentinel, 100, DroppedSentinel, 200].
    let mixed_stream = Task::AsyncStream(Arc::new(|_input, _ctx| {
        let items: Vec<Box<dyn Value>> = vec![
            Box::new(DroppedSentinel),
            Box::new(100_i32),
            Box::new(DroppedSentinel),
            Box::new(200_i32),
        ];
        Ok(Box::pin(stream::iter(items)) as cognee_core::ValueStream)
    }));

    // Must only receive i32 values.
    let assert_no_sentinel =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x)) });

    let pipeline = Pipeline::new("stream sentinels")
        .with_task(mixed_stream)
        .with_task(assert_no_sentinel);

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
    let ctx = stub_ctx().await;
    let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline must not fail");

    let mut values: Vec<i32> = outputs.iter().map(as_i32).collect();
    values.sort_unstable();
    assert_eq!(values, vec![100, 200]);
}
