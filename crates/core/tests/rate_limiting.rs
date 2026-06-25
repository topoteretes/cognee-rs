#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for the `RateLimiter` / rate-limiting gap (Gap 3).
//!
//! All acquire-count tests use a `CountingLimiter` (zero-overhead, non-blocking)
//! rather than timing assertions, which avoids flakiness while still verifying
//! the key invariants: limiter is acquired once per pipeline item, once per retry
//! attempt, and not at all when overridden by a per-task limiter.

use std::future::Future;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cognee_core::cancellation::cancellation_pair;
use cognee_core::rate_limiter::RateLimiter;
use cognee_core::{
    CpuPool, NoopExecStatusManager, NoopWatcher, Pipeline, ProgressToken, RetryDelay, RetryPolicy,
    Task, TaskContext, TaskError, TaskInfo, Value, execute,
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

// ---------------------------------------------------------------------------
// CountingLimiter — test-only fake that counts acquire() calls.
// ---------------------------------------------------------------------------

struct CountingLimiter {
    count: Arc<AtomicUsize>,
}

impl CountingLimiter {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        (
            Self {
                count: count.clone(),
            },
            count,
        )
    }
}

#[async_trait]
impl RateLimiter for CountingLimiter {
    async fn acquire(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Test 1 — pipeline-level limiter is acquired once per item
//
// A pipeline with 6 inputs and a trivial pass-through task. The pipeline-level
// `CountingLimiter` must be called exactly once per item (6 times total).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pipeline_limiter_called_once_per_item() {
    let (limiter, count) = CountingLimiter::new();

    let passthrough =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x)) });

    let pipeline = Pipeline::new("count test")
        .with_task(passthrough)
        .with_rate_limiter(Arc::new(limiter));

    let inputs: Vec<Arc<dyn Value>> = (1..=6_i32).map(|i| Arc::new(i) as Arc<dyn Value>).collect();
    let ctx = stub_ctx().await;
    execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline must not fail");

    assert_eq!(
        count.load(Ordering::SeqCst),
        6,
        "pipeline limiter must be acquired once per item"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — per-task limiter overrides pipeline-level limiter
//
// Pipeline limiter A + one task with per-task limiter B. Items going through
// that task must use B only; A must not be called (it is overridden).
// A second task (no per-task limiter) uses A.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn per_task_limiter_overrides_pipeline_limiter() {
    let (pipeline_limiter, pipeline_count) = CountingLimiter::new();
    let (task_limiter, task_count) = CountingLimiter::new();

    // Task 1 has a per-task limiter — overrides the pipeline limiter.
    let task1_raw =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x * 10)) });
    let task1 = TaskInfo::new(task1_raw).with_rate_limiter(Arc::new(task_limiter));

    // Task 2 uses the pipeline limiter (no per-task override).
    let task2 =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x + 1)) });

    let pipeline = Pipeline::new("override test")
        .with_task(task1)
        .with_task(task2)
        .with_rate_limiter(Arc::new(pipeline_limiter));

    let inputs: Vec<Arc<dyn Value>> = (1..=4_i32).map(|i| Arc::new(i) as Arc<dyn Value>).collect();
    let ctx = stub_ctx().await;
    execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline must not fail");

    // task1's per-task limiter: called once per item (4 items).
    assert_eq!(
        task_count.load(Ordering::SeqCst),
        4,
        "per-task limiter must be acquired once per item through that task"
    );

    // pipeline limiter: called only for task2 (task1 overrides it). 4 items through task2.
    assert_eq!(
        pipeline_count.load(Ordering::SeqCst),
        4,
        "pipeline limiter must be called for tasks without a per-task limiter"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — acquire is called once per retry attempt
//
// A task that fails on the first two attempts and succeeds on the third.
// Under `RetryPolicy::Limited { max_attempts: 3 }`, the counting limiter
// must be called exactly 3 times (once per attempt).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn limiter_acquired_per_retry_attempt() {
    let (limiter, count) = CountingLimiter::new();

    let attempt_count = Arc::new(AtomicUsize::new(0));
    let attempt_count_clone = attempt_count.clone();

    let flaky = Task::Sync(Arc::new(move |input: Arc<dyn Value>, _ctx| {
        let attempt = attempt_count_clone.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt < 3 {
            Err(Box::new(std::io::Error::other("transient failure")) as TaskError)
        } else {
            Ok(input)
        }
    }));

    let pipeline = Pipeline::new("retry count test")
        .with_task(flaky)
        .with_retry(RetryPolicy::Limited {
            max_attempts: NonZeroU32::new(3).expect("3 is nonzero"),
            delay: RetryDelay::Constant(Duration::ZERO),
        })
        .with_rate_limiter(Arc::new(limiter));

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(42_i32)];
    let ctx = stub_ctx().await;
    execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline must succeed on 3rd attempt");

    assert_eq!(
        count.load(Ordering::SeqCst),
        3,
        "limiter must be acquired once per retry attempt (3 total: 2 failures + 1 success)"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — no limiter means no throttle (sanity)
//
// A pipeline without any rate limiter runs successfully and produces output.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_limiter_runs_without_throttle() {
    let passthrough =
        Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> { Ok(Box::new(*x)) });

    let pipeline = Pipeline::new("no limiter").with_task(passthrough);

    let inputs: Vec<Arc<dyn Value>> = (1..=5_i32).map(|i| Arc::new(i) as Arc<dyn Value>).collect();
    let ctx = stub_ctx().await;
    let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .expect("pipeline without limiter must not fail");

    assert_eq!(outputs.len(), 5);
}
