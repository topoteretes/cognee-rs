#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for the `PassthroughSentinel` / `is_passthrough` enrichment
//! mode (Gap 2 — enrichment mode).
//!
//! Tests cover:
//! 1. An enriching task that returns `PassthroughSentinel` for some inputs
//!    and real output for others — odd inputs pass through unchanged, even
//!    inputs are wrapped.
//! 2. A non-enriching task that returns `PassthroughSentinel` must fail with
//!    `ExecutionError::TaskFailed` whose message mentions `enriches=false`.
//! 3. Output count equals input count when an enrichment stage no-ops on a
//!    subset (confirms no items are silently dropped).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use cognee_core::cancellation::cancellation_pair;
use cognee_core::{
    CpuPool, ExecutionError, NoopExecStatusManager, NoopWatcher, PassthroughSentinel, Pipeline,
    ProgressToken, Task, TaskContext, TaskError, TaskInfo, Value, execute,
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
// Test 1 — pass-through forwards input
//
// Pipeline: [enrich, collect]
// `enrich` is `.with_enriches()` and returns `PassthroughSentinel` for odd
// inputs but negates even inputs (wraps with a sign flip).
// After enrich, a collect task multiplies by 10 so we can distinguish
// "original value passed through" from "transformed value".
//
// Inputs: [1, 2, 3, 4]
//   - odd (1, 3): enrich returns PassthroughSentinel → collect receives 1, 3
//     → output: 10, 30
//   - even (2, 4): enrich returns -(input) → collect receives -2, -4
//     → output: -20, -40
// Expected sorted output: [-40, -20, 10, 30]
// ---------------------------------------------------------------------------

#[tokio::test]
async fn passthrough_forwards_input() {
    // Enriching task: odd → PassthroughSentinel, even → negate.
    let enrich_raw = Task::Sync(Arc::new(|input: Arc<dyn Value>, _ctx| {
        let n = *(*input).as_any().downcast_ref::<i32>().expect("i32");
        if n % 2 != 0 {
            Ok(Arc::new(PassthroughSentinel) as Arc<dyn Value>)
        } else {
            Ok(Arc::new(-n) as Arc<dyn Value>)
        }
    }));
    let enrich = TaskInfo::new(enrich_raw).with_enriches();

    // Collect task: multiply by 10.
    let collect =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(x * 10)) });

    let pipeline = Pipeline::new("enrich passthrough")
        .with_task(enrich)
        .with_task(collect);

    let inputs: Vec<Arc<dyn Value>> = vec![
        Arc::new(1_i32),
        Arc::new(2_i32),
        Arc::new(3_i32),
        Arc::new(4_i32),
    ];
    let ctx = stub_ctx().await;
    let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline must not fail");

    let mut values: Vec<i32> = outputs.iter().map(as_i32).collect();
    values.sort_unstable();
    assert_eq!(values, vec![-40, -20, 10, 30]);
}

// ---------------------------------------------------------------------------
// Test 2 — non-enriching pass-through errors
//
// A task *without* `.with_enriches()` returns `PassthroughSentinel`.
// The run must fail with `ExecutionError::TaskFailed` whose message mentions
// `enriches=false`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_enriching_passthrough_errors() {
    let bad_task = Task::Sync(Arc::new(|_input: Arc<dyn Value>, _ctx| {
        Ok(Arc::new(PassthroughSentinel) as Arc<dyn Value>)
    }));

    let pipeline = Pipeline::new("bad passthrough").with_task(bad_task);

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(42_i32)];
    let ctx = stub_ctx().await;
    let result = execute(&pipeline, inputs, ctx, &NoopWatcher).await;

    match result {
        Err(ExecutionError::TaskFailed { source, .. }) => {
            let msg = source.to_string();
            assert!(
                msg.contains("enriches=false"),
                "error message should mention enriches=false, got: {msg}"
            );
        }
        Ok(_) => panic!("expected TaskFailed error, but pipeline succeeded"),
        Err(e) => panic!("expected TaskFailed, got a different ExecutionError: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Test 3 — enrichment + real output mix, output count equals input count
//
// An enrichment stage no-ops (PassthroughSentinel) on half the inputs and
// wraps the other half. No items should be dropped: output count must equal
// input count.
//
// Inputs: [10, 20, 30, 40, 50]
// Enrich: even → PassthroughSentinel (pass through), odd → value * 2
//   - 10 (even) → pass through 10
//   - 20 (even) → pass through 20
//   - 30 (even) → pass through 30
//   - 40 (even) → pass through 40
//   - 50 (even) → pass through 50
// Wait — all are even. Use odd/even differently: divisible by 20 → passthrough.
//
// Simpler: values [1..=5], enrich passes through odd, doubles even.
// Expected: [1, 4, 3, 8, 5] → count 5 == input count 5.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn enrichment_output_count_equals_input_count() {
    // Enriching task: odd → PassthroughSentinel, even → value * 2.
    let enrich_raw = Task::Sync(Arc::new(|input: Arc<dyn Value>, _ctx| {
        let n = *(*input).as_any().downcast_ref::<i32>().expect("i32");
        if n % 2 != 0 {
            Ok(Arc::new(PassthroughSentinel) as Arc<dyn Value>)
        } else {
            Ok(Arc::new(n * 2) as Arc<dyn Value>)
        }
    }));
    let enrich = TaskInfo::new(enrich_raw).with_enriches();

    let pipeline = Pipeline::new("enrich count").with_task(enrich);

    let inputs: Vec<Arc<dyn Value>> = (1..=5_i32).map(|i| Arc::new(i) as Arc<dyn Value>).collect();
    let input_count = inputs.len();
    let ctx = stub_ctx().await;
    let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline must not fail");

    assert_eq!(
        outputs.len(),
        input_count,
        "output count must equal input count; no items dropped by enrichment"
    );

    // Also verify values: odd inputs (1,3,5) pass through unchanged;
    // even inputs (2,4) are doubled to (4,8).
    let mut values: Vec<i32> = outputs.iter().map(as_i32).collect();
    values.sort_unstable();
    assert_eq!(values, vec![1, 3, 4, 5, 8]);
}
