//! Pipeline definition and the depth-first task executor.
//!
//! [`Pipeline`] (or the type-checked [`PipelineBuilder`]) describes an ordered
//! chain of [`TaskInfo`]s; [`execute`] runs that chain over one or more input
//! values, fanning out per data item and recursing through the chain with
//! `execute_from`.
//!
//! # Batch dispatch bypasses executor services
//!
//! There are two dispatch paths through the executor, and they are **not**
//! equivalent.
//!
//! Single-value tasks go through `execute_from` → `call_with_retry` and receive
//! the executor's full set of services.
//!
//! When an upstream task yields an iterator or a stream, the executor
//! accumulates its items into a buffer (sized by [`TaskInfo::with_batch_size`])
//! and hands the buffer to `dispatch_batch`. If the consuming task is one of the
//! `*Batch` [`Task`] variants, `dispatch_batch` calls it **directly** with the
//! accumulated slice, skipping `call_with_retry` entirely. Two services live
//! in `call_with_retry` and are therefore **not** applied to such a task:
//!
//! 1. **Retries.** The configured [`RetryPolicy`] is never consulted; the batch
//!    call is made exactly once and the first error fails the item.
//! 2. **Per-task watcher events.** No `on_task` / `on_task_started` /
//!    `on_task_completed` is emitted for the batch task, so it is invisible both
//!    to watcher observers and to per-task status tracking.
//!
//! Four further services used to be missing on this path and are now provided
//! directly by `dispatch_batch` and the accumulation loops, so a batch task can
//! rely on them:
//!
//! - **Rate limiting.** The effective limiter — [`TaskInfo::with_rate_limiter`]
//!   if set, otherwise [`Pipeline::with_rate_limiter`] — is acquired once per
//!   batch call, a batch being one external request.
//! - **Progress.** The batch task is handed *its own* progress subtoken rather
//!   than the run-level one, so it can report intermediate progress; the slice
//!   is completed once its producer is exhausted. (`dispatch_batch` runs once
//!   per accumulated batch and cannot tell which call is the last, so
//!   completion happens in `process_iter` / `process_stream`.)
//! - **Cancellation.** Both accumulation loops stop pulling from the producer
//!   once the run is cancelled, and `dispatch_batch` re-checks before invoking
//!   the batch task. A terminal batch task needs its own check because
//!   `execute_from(rest=[], ..)` returns via the empty-tasks base case *before*
//!   reaching the check at the top of `execute_from`.
//! - **Provenance stamping.** `dispatch_batch` walks the batch task's own output
//!   with `stamp_tree_dyn`, so DataPoints the task newly creates are attributed
//!   to it. See the subsection below for why this one moved and how it is built.
//!
//! What a batch task *does* get, beyond those four, is the pre-accumulated
//! slice; it is expected to handle its own error semantics.
//!
//! ## Provenance stamping moved to the provided list
//!
//! Until recently this was the **third withheld service**: the executor did not
//! walk a batch task's output, so anything the task newly created stayed
//! unstamped unless it stamped itself. It is now provided, which continues the
//! direction of the change that moved rate limiting, progress and cancellation
//! across — every one of those was withheld for the same accidental reason
//! (the service happened to live inside `call_with_retry`), not by design.
//!
//! Provenance stamping was the last of the four to move because it was the only
//! one whose cost looked *recoverable*. When the withheld list was first written
//! down, [`DataPoint`](cognee_models::DataPoint) had no `topological_rank`
//! field, so skipping a batch stage cost only a missing pipeline/task label —
//! and a downstream stamp could still fill that in later. `topological_rank`
//! removes the recovery: the rank write is **single-shot** (guarded by
//! `matches!(dp.topological_rank, None | Some(0))`, mirroring Python's
//! `if current_rank is None or current_rank == 0`), so the first task to touch a
//! DataPoint fixes its rank forever. With the batch stage left out, its outputs
//! kept the *predecessor's* rank — every DataPoint born in a batch stage sat one
//! pipeline column short, with no later opportunity to correct it.
//!
//! Python has no such gap: `handle_task` assigns every task its own
//! `task_index` regardless of batching
//! (`cognee/modules/pipelines/operations/run_tasks_base.py:181-190`), so parity
//! requires stamping here too. `dispatch_batch` therefore builds a **fresh**
//! `ProvenanceInputs` for the batch task itself — its own name and
//! `first_index + 1` as the rank — and uses it for the task's `Single`, `Iter`
//! and `Stream` outputs alike. Pinned by
//! `batch_task_outputs_carry_the_batch_tasks_own_rank` and
//! `batch_task_single_output_is_stamped` in
//! `crates/core/tests/provenance_pipeline_integration.rs`.
//!
//! Items *inside* the incoming slice are unaffected: they were already stamped
//! eagerly by the upstream `process_iter` / `process_stream`, and the run's
//! shared visited-set makes re-stamping them a no-op.
//!
//! ## Remaining divergence from Python
//!
//! The two withheld services above remain a **divergence from Python cognee**,
//! where every task goes through `handle_task` and receives telemetry and spans
//! regardless of batching (there, `batch_size` only controls how many items are
//! gathered into the list handed to the task).
//!
//! Only the `*Batch` variants take this path. A **non-batch** task reached from
//! `dispatch_batch` is executed one buffered item at a time through
//! `execute_from`, so it keeps every service listed above.
//!
//! [`ProgressToken::split`]: crate::progress::ProgressToken::split
//! [`TaskContext`]: crate::task_context::TaskContext

// Mutex lock().unwrap() and invariant-guarded expect() are acceptable in this
// pipeline runtime — lock poisoning is unrecoverable and the invariants are
// upheld by construction.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "lock poisoning is unrecoverable; expect() calls guard construction-time invariants"
)]

use std::marker::PhantomData;
use std::mem;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use thiserror::Error;
use tokio::time::sleep;
use uuid::Uuid;

use crate::progress::ProgressToken;
use crate::rate_limiter::RateLimiter;
use crate::task::{
    TaggedMeta, Task, TaskCall, TaskError, TaskInfo, TypedTask, Value, ValueIter, ValueStream,
};
use crate::task_context::TaskContext;

#[derive(Debug, Clone)]
pub enum RetryPolicy {
    /// Do not retry; the first failure aborts the pipeline.
    NoRetry,
    /// Retry up to `max_attempts - 1` additional times with `delay` between
    /// each attempt.
    Limited {
        max_attempts: std::num::NonZeroU32,
        delay: RetryDelay,
    },
}

/// Delay strategy between retry attempts.
#[derive(Debug, Clone)]
pub enum RetryDelay {
    /// Same delay for every retry.
    Constant(Duration),
    /// Exponential backoff: `base * factor^retry_index` (retry_index starts at 0).
    /// Default `factor` is 2 (classic exponential backoff).
    Exponential { base: Duration, factor: u32 },
}

impl RetryDelay {
    /// Create an exponential delay with the default factor of 2.
    pub fn exponential(base: Duration) -> Self {
        RetryDelay::Exponential { base, factor: 2 }
    }
}

impl RetryPolicy {
    fn max_attempts(&self) -> u32 {
        match self {
            RetryPolicy::NoRetry => 1,
            RetryPolicy::Limited { max_attempts, .. } => max_attempts.get(),
        }
    }

    /// Compute the delay for a given retry index (0-based).
    /// Returns `None` for `NoRetry`.
    fn delay(&self, retry_index: u32) -> Option<Duration> {
        match self {
            RetryPolicy::NoRetry => None,
            RetryPolicy::Limited { delay, .. } => Some(delay.compute(retry_index)),
        }
    }
}

impl RetryDelay {
    fn compute(&self, retry_index: u32) -> Duration {
        match self {
            RetryDelay::Constant(d) => *d,
            RetryDelay::Exponential { base, factor } => {
                let multiplier = factor.checked_pow(retry_index).unwrap_or(u32::MAX);
                *base * multiplier
            }
        }
    }
}
/// Function that extracts a stable, content-addressed identifier from a
/// type-erased [`Value`].
///
/// Return `Some(id)` for values that support incremental deduplication,
/// `None` for values that should always be processed.
///
/// ```rust,ignore
/// let extract: DataIdFn = Arc::new(|v: Arc<dyn Value>| {
///     v.as_any()
///         .downcast_ref::<Document>()
///         .map(|d| d.id.to_string())
/// });
/// ```
pub type DataIdFn = Arc<dyn Fn(Arc<dyn Value>) -> Option<String> + Send + Sync>;
pub struct Pipeline {
    pub id: Uuid,
    /// Human-readable pipeline name (used as key for status tracking).
    pub name: Option<String>,
    pub description: String,
    pub tasks: Vec<TaskInfo>,
    pub retry_policy: RetryPolicy,
    /// Default maximum number of items collected from an iterator / stream
    /// before dispatching them to the next task (individually for non-batch
    /// tasks, as a slice for batch tasks).
    /// Individual tasks can override this via [`TaskInfo::batch_size`].
    pub batch_size: usize,
    /// Optional function to extract a stable data ID from input values.
    /// When set together with an [`ExecStatusManager`] on the context, the
    /// executor will skip items that are already completed.
    pub data_id_fn: Option<DataIdFn>,
    /// Maximum number of data items processed concurrently through the full
    /// task chain.  Default `1` = strictly sequential (current behaviour).
    /// Values > 1 use `buffer_unordered` for data-item-level parallelism.
    pub concurrency: usize,
    /// Optional pre-built telemetry settings snapshot (the `| config`
    /// merge from Python's pipeline lifecycle events). When `None`,
    /// `Pipeline Run *` analytics events emit with no settings merged
    /// in. Populated by `cognee` from `Config::telemetry_snapshot()`.
    ///
    /// Carried as a plain field rather than a feature-gated one so the
    /// `Pipeline` struct shape is stable across feature flips. The
    /// snapshot is only consumed when the `telemetry` feature is on.
    pub telemetry_settings: Option<serde_json::Map<String, serde_json::Value>>,
    /// Pipeline-wide proactive rate limiter applied to every task call: once per
    /// retry attempt for single-value tasks (via `call_with_retry`) and once per
    /// batch for `*Batch` tasks (via `dispatch_batch`). Individual tasks may
    /// override it via [`TaskInfo::rate_limiter`]. `None` means no throttling.
    pub rate_limiter: Option<Arc<dyn RateLimiter>>,
}

impl Pipeline {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: None,
            description: description.into(),
            tasks: Vec::new(),
            retry_policy: RetryPolicy::NoRetry,
            batch_size: 32,
            data_id_fn: None,
            concurrency: 1,
            telemetry_settings: None,
            rate_limiter: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_task(mut self, task: impl Into<TaskInfo>) -> Self {
        self.tasks.push(task.into());
        self
    }

    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Set the pipeline-wide default for how many items are accumulated from an
    /// upstream iterator / stream before they are dispatched to the consuming
    /// task. Individual tasks override it via [`TaskInfo::with_batch_size`].
    ///
    /// This sizes the accumulation buffer for **every** kind of consuming task,
    /// not only `*Batch` variants — see [`TaskInfo::with_batch_size`] for what
    /// that means for a non-batch consumer.
    ///
    /// When the consumer *is* a `*Batch` variant it skips `call_with_retry` and
    /// with it two services — retries and per-task watcher events. Rate
    /// limiting, progress, cancellation and provenance stamping are applied on
    /// both paths; see
    /// [Batch dispatch bypasses executor services](crate::pipeline#batch-dispatch-bypasses-executor-services).
    pub fn with_batch_size(mut self, size: usize) -> Self {
        assert!(size > 0, "batch_size must be > 0");
        self.batch_size = size;
        self
    }

    /// Set the function used to extract a stable data ID from input values
    /// for incremental deduplication.
    pub fn with_data_id(mut self, f: DataIdFn) -> Self {
        self.data_id_fn = Some(f);
        self
    }

    /// Set the number of data items processed concurrently.
    /// Default is `1` (sequential).  When `n > 1`, items are processed in
    /// parallel via `buffer_unordered(n)`.
    ///
    /// **Note:** output order is *not* guaranteed when `concurrency > 1`.
    pub fn with_concurrency(mut self, n: usize) -> Self {
        assert!(n > 0, "concurrency must be > 0");
        self.concurrency = n;
        self
    }

    /// Set a pipeline-wide proactive rate limiter. Individual tasks may override
    /// it via [`TaskInfo::with_rate_limiter`].
    ///
    /// The limiter is acquired inside `call_with_retry`, once per attempt, so
    /// each retry is a fresh acquisition. Use this for LLM API quota throttling
    /// or per-host crawl-rate control.
    ///
    /// Batch calls are throttled too: `dispatch_batch` acquires the effective
    /// limiter once per batch call, a batch being one external request. Note the
    /// difference in granularity — a single-value task acquires once per *retry
    /// attempt*, a batch task once per *batch*.
    ///
    /// See [`crate::rate_limiter`] for the distinction between this,
    /// [`Pipeline::with_concurrency`] (item parallelism), and [`RetryPolicy`]
    /// (reactive backoff).
    pub fn with_rate_limiter(mut self, rl: Arc<dyn RateLimiter>) -> Self {
        self.rate_limiter = Some(rl);
        self
    }

    /// Attach a pre-built telemetry settings snapshot (the `| config`
    /// merge for `Pipeline Run Started/Completed/Errored` analytics
    /// events). See [`Pipeline::telemetry_settings`] for details.
    pub fn with_telemetry_settings(
        mut self,
        settings: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        self.telemetry_settings = Some(settings);
        self
    }
}

/// A compile-time type-safe builder for [`Pipeline`].
///
/// `PipelineBuilder<I, O>` tracks the input type of the first task (`I`) and the
/// output type of the most recently added task (`O`) as type parameters.  The
/// [`add_task`](PipelineBuilder::add_task) method accepts only a
/// [`TypedTask<O, O2>`](TypedTask), ensuring that each task's input type matches the
/// previous task's output type.  When all tasks have been added, call
/// [`build`](PipelineBuilder::build) to erase the types and obtain a [`Pipeline`]
/// that the standard executor can run.
///
/// # Example
///
/// ```rust,ignore
/// let pipeline = PipelineBuilder::new_with_task(
///         "my pipeline",
///         TypedTask::sync(|s: &String, _| Ok(Box::new(s.len()))),
///     )
///     .add_task(TypedTask::sync(|n: &usize, _| Ok(Box::new(format!("len={n}")))))
///     .with_name("length-formatter")
///     .build();
/// ```
pub struct PipelineBuilder<I: Value, O: Value> {
    description: String,
    name: Option<String>,
    tasks: Vec<TaskInfo>,
    retry_policy: RetryPolicy,
    batch_size: usize,
    data_id_fn: Option<DataIdFn>,
    concurrency: usize,
    _marker: PhantomData<fn(I) -> O>,
}

impl<I: Value, O: Value> PipelineBuilder<I, O> {
    /// Create a new builder, seeding it with the first task.
    ///
    /// The type parameters `I` and `O` are inferred from `first_task`.
    pub fn new_with_task(
        description: impl Into<String>,
        first_task: TypedTask<I, O>,
    ) -> PipelineBuilder<I, O> {
        PipelineBuilder {
            description: description.into(),
            name: None,
            tasks: vec![first_task.into()],
            retry_policy: RetryPolicy::NoRetry,
            batch_size: 32,
            data_id_fn: None,
            concurrency: 1,
            _marker: PhantomData,
        }
    }

    /// Append a task whose input type must equal the current output type `O`.
    ///
    /// Returns a new builder whose output type is updated to `O2`.  The
    /// compile-time constraint `TypedTask<O, O2>` ensures type safety: passing a
    /// task with a mismatched input type is a compile error.
    pub fn add_task<O2: Value>(mut self, task: TypedTask<O, O2>) -> PipelineBuilder<I, O2> {
        self.tasks.push(task.into());
        PipelineBuilder {
            description: self.description,
            name: self.name,
            tasks: self.tasks,
            retry_policy: self.retry_policy,
            batch_size: self.batch_size,
            data_id_fn: self.data_id_fn,
            concurrency: self.concurrency,
            _marker: PhantomData,
        }
    }

    /// Append a task with an explicit human-readable name.
    ///
    /// Equivalent to [`add_task`](Self::add_task) followed by setting the
    /// resulting [`TaskInfo::name`]. The name is what the executor's
    /// `stamp_tree_dyn` writes into `DataPoint.source_task` and what the
    /// `PipelineWatcher` lifecycle hooks observe.
    pub fn add_task_named<O2: Value>(
        mut self,
        task: TypedTask<O, O2>,
        name: impl Into<String>,
    ) -> PipelineBuilder<I, O2> {
        self.tasks.push(TaskInfo::from(task).with_name(name));
        PipelineBuilder {
            description: self.description,
            name: self.name,
            tasks: self.tasks,
            retry_policy: self.retry_policy,
            batch_size: self.batch_size,
            data_id_fn: self.data_id_fn,
            concurrency: self.concurrency,
            _marker: PhantomData,
        }
    }

    /// Set the human-readable name of the **first** task already pushed by
    /// [`new_with_task`](Self::new_with_task).
    ///
    /// Used by builders that want to name the seed task without restructuring
    /// the constructor. The name is what the executor's `stamp_tree_dyn`
    /// writes into `DataPoint.source_task`.
    ///
    /// # Panics
    ///
    /// Panics if no tasks have been pushed yet (impossible via the public
    /// API, since `new_with_task` always seeds one).
    pub fn with_first_task_name(mut self, name: impl Into<String>) -> Self {
        let first = self
            .tasks
            .first_mut()
            .expect("PipelineBuilder always has at least the seed task from new_with_task");
        first.name = Some(name.into());
        self
    }

    /// Set a human-readable name (used as key for status tracking).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the retry policy applied to all tasks.
    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Set the default number of items accumulated from an upstream iterator /
    /// stream before they are dispatched to the consuming task.
    ///
    /// This sizes the accumulation buffer whatever kind the consuming task is,
    /// not only for `*Batch` variants — see [`TaskInfo::with_batch_size`].
    ///
    /// When the consumer *is* a `*Batch` variant it is dispatched off the
    /// executor's service path; see
    /// [Batch dispatch bypasses executor services](crate::pipeline#batch-dispatch-bypasses-executor-services).
    pub fn with_batch_size(mut self, size: usize) -> Self {
        assert!(size > 0, "batch_size must be > 0");
        self.batch_size = size;
        self
    }

    /// Set the number of data items processed concurrently.
    pub fn with_concurrency(mut self, n: usize) -> Self {
        assert!(n > 0, "concurrency must be > 0");
        self.concurrency = n;
        self
    }

    /// Set the function used to extract a stable data ID for incremental deduplication.
    pub fn with_data_id(mut self, f: DataIdFn) -> Self {
        self.data_id_fn = Some(f);
        self
    }

    /// Consume the builder and produce a [`Pipeline`].
    ///
    /// Type safety is fully enforced by the time `build` is called; the returned
    /// `Pipeline` uses the existing type-erased executor unchanged.
    pub fn build(self) -> Pipeline {
        Pipeline {
            id: Uuid::new_v4(),
            name: self.name,
            description: self.description,
            tasks: self.tasks,
            retry_policy: self.retry_policy,
            batch_size: self.batch_size,
            data_id_fn: self.data_id_fn,
            concurrency: self.concurrency,
            telemetry_settings: None,
            rate_limiter: None,
        }
    }
}

/// Identity and metadata of a pipeline run, passed to [`PipelineWatcher`]
/// event methods.
#[derive(Debug, Clone)]
pub struct PipelineRunInfo {
    /// Random per-invocation ID.
    pub run_id: Uuid,
    /// Deterministic ID: `uuid5(user_id + name + dataset_id)`.
    /// Falls back to `run_id` when not enough info is available.
    pub pipeline_id: Uuid,
    /// Human-readable pipeline name.
    pub pipeline_name: String,
    /// Owner / tenant.
    pub user_id: Option<Uuid>,
    /// Tenant the pipeline run belongs to. `None` for single-user
    /// deployments. Emitted as `"Single User Tenant"` on the wire
    /// when `None` (Python parity).
    pub tenant_id: Option<Uuid>,
    /// Dataset being processed.
    pub dataset_id: Option<Uuid>,
    /// `Data.id`s for the inputs of the run. Surfaced into
    /// `run_info["data"]` by the watcher. Empty when the run has no
    /// `Data` input.
    pub data_ids: Vec<Uuid>,
    /// Current run status.
    pub status: PipelineRunStatus,
    /// When the run was initiated.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When the run reached a terminal state (`Completed` or `Errored`).
    /// `None` while the run is still in flight.
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl PipelineRunInfo {
    /// Wall-clock seconds between [`Self::started_at`] and
    /// [`Self::completed_at`]. Returns `None` while the run is still in
    /// flight (i.e. `completed_at` is unset).
    pub fn elapsed_seconds(&self) -> Option<f64> {
        let end = self.completed_at?;
        let dur_ms = (end - self.started_at).num_milliseconds();
        Some(dur_ms as f64 / 1000.0)
    }
}

/// High-level status of a pipeline run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineRunStatus {
    Initiated,
    Started,
    Completed,
    Errored,
}

impl std::fmt::Display for PipelineRunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Initiated => write!(f, "INITIATED"),
            Self::Started => write!(f, "STARTED"),
            Self::Completed => write!(f, "COMPLETED"),
            Self::Errored => write!(f, "ERRORED"),
        }
    }
}

/// Build a deterministic pipeline ID from available context.
///
/// `uuid5(NAMESPACE_OID, "{user_id}{pipeline_name}{dataset_id}")`.
/// Returns `fallback` if `name` is empty / not set.
///
/// NOTE (task 04 §5a): this function is NOT replaced by
/// `ids::pipeline_id` because its `None`-arg path uses
/// `unwrap_or_default()` → `""`, while `ids::pipeline_id` uses
/// `Uuid::nil()` → `"00000000-…"`. Those produce different hashes;
/// callers pass `None` for absent user_id/dataset_id, so swapping in
/// `ids::pipeline_id` would silently change stored pipeline IDs.
fn deterministic_pipeline_id(
    name: Option<&str>,
    user_id: Option<Uuid>,
    dataset_id: Option<Uuid>,
) -> Option<Uuid> {
    let name = name.filter(|n| !n.is_empty())?;
    let key = format!(
        "{}{}{}",
        user_id.map(|u| u.to_string()).unwrap_or_default(),
        name,
        dataset_id.map(|d| d.to_string()).unwrap_or_default(),
    );
    Some(Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes()))
}
#[derive(Debug)]
pub enum TaskStatus {
    Started,
    Retrying { attempt: u32, error: String },
    Succeeded,
    Failed { attempts: u32, error: String },
}

#[derive(Debug)]
pub enum PipelineStatus {
    Started {
        task_count: usize,
    },
    Succeeded {
        output_count: usize,
    },
    Failed {
        task_index: usize,
        error: String,
    },
    Cancelled,
    /// A data item was skipped because it was already completed
    /// (reported by [`ExecStatusManager`]).
    ItemSkipped {
        data_id: String,
    },
}

/// Observer for pipeline and task lifecycle events.
///
/// The basic methods ([`on_pipeline`](PipelineWatcher::on_pipeline),
/// [`on_task`](PipelineWatcher::on_task)) use compact status enums and are
/// always called by the executor.
///
/// The richer `on_pipeline_run_*` / `on_task_*` methods receive a full
/// [`PipelineRunInfo`] and are provided with default no-op implementations
/// so existing watchers continue to work unchanged.  Override them to
/// persist run metadata (see `DbPipelineWatcher`).
#[async_trait]
pub trait PipelineWatcher: Send + Sync {
    async fn on_pipeline(&self, pipeline_id: Uuid, status: PipelineStatus);
    async fn on_task(
        &self,
        pipeline_id: Uuid,
        task_index: usize,
        task_name: Option<&str>,
        total_tasks: usize,
        status: TaskStatus,
    );

    // ── Rich lifecycle events (default no-ops) ──────────────────────────

    /// Called before any task runs. Persists the initial `INITIATED` row in
    /// the Python lifecycle. Default no-op — watchers that don't persist
    /// runs can ignore this.
    ///
    /// Does NOT broadcast a `RunEvent` — the in-memory event stream remains
    /// four-kinded (`Started`/`Yield`/`Completed`/`Errored`/`AlreadyCompleted`).
    /// Subscribers only see the run "exists" once `Started` fires
    /// (locked decision 13).
    async fn on_pipeline_run_initiated(&self, _run: &PipelineRunInfo) {}

    /// Called when the pipeline run is first created (before any tasks).
    async fn on_pipeline_run_started(&self, _run: &PipelineRunInfo) {}

    /// Called after all tasks complete successfully.
    async fn on_pipeline_run_completed(&self, _run: &PipelineRunInfo, _output_count: usize) {}

    /// Called when the pipeline run fails.
    async fn on_pipeline_run_errored(&self, _run: &PipelineRunInfo, _error: &str) {}

    /// Called when a task begins execution.
    async fn on_task_started(&self, _run: &PipelineRunInfo, _task_name: &str, _task_index: usize) {}

    /// Called when a task completes successfully.
    async fn on_task_completed(
        &self,
        _run: &PipelineRunInfo,
        _task_name: &str,
        _result_count: usize,
    ) {
    }

    /// Called when a task fails (after retries exhausted).
    async fn on_task_errored(&self, _run: &PipelineRunInfo, _task_name: &str, _error: &str) {}

    /// Tasks emit run-scoped key/value payload via this hook. Default no-op.
    ///
    /// Mirrors Python's free-form `PipelineRunInfo.payload` field but as an
    /// event stream rather than shared state on the snapshot. The
    /// registry-side `ScopedRunWatcher` overrides this to persist the field
    /// through `PipelineRunRepository::set_payload_field`. Consumers who need
    /// the accumulated payload query the registry's `get_payload(run_id)`
    /// helper.
    async fn on_payload_field(&self, _run_id: Uuid, _key: &str, _value: serde_json::Value) {}
}

pub struct NoopWatcher;

#[async_trait]
impl PipelineWatcher for NoopWatcher {
    async fn on_pipeline(&self, _: Uuid, _: PipelineStatus) {}
    async fn on_task(&self, _: Uuid, _: usize, _: Option<&str>, _: usize, _: TaskStatus) {}
}
#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("task {task_index} failed after {attempts} attempt(s): {source}")]
    TaskFailed {
        task_index: usize,
        attempts: u32,
        #[source]
        source: TaskError,
    },

    #[error("pipeline was cancelled")]
    Cancelled,

    #[error("pipeline has no tasks")]
    NoTasks,

    #[error("invalid pipeline configuration: {reason}")]
    InvalidConfig { reason: String },
}
/// Emit a `Pipeline Run *` analytics event.
///
/// Used by [`execute`] at the three terminal arms (Started, Completed,
/// Errored) to mirror Python's `cognee.modules.pipelines.operations.run_tasks_with_telemetry`
/// emissions. The payload carries `pipeline_name`, `cognee_version`,
/// and `tenant_id` (with the literal `"Single User Tenant"` fallback
/// per locked decision 1) merged on top of the caller's curated
/// settings snapshot (locked decision 5). Per locked decision 6,
/// `dataset_id` and `pipeline_run_id` are intentionally omitted from
/// the analytics payload — they remain on the OTEL span only.
///
/// No error string is included on the `Pipeline Run Errored` payload
/// (Python parity, see sub-doc §2.2).
#[cfg(feature = "telemetry")]
fn emit_pipeline_event(
    event_name: &str,
    user_id: Option<Uuid>,
    pipeline_name: &str,
    tenant_id: Option<Uuid>,
    settings: Option<&serde_json::Map<String, serde_json::Value>>,
) {
    use serde_json::{Map, Value};

    let mut props: Map<String, Value> = settings.cloned().unwrap_or_default();
    props.insert(
        "pipeline_name".into(),
        Value::String(pipeline_name.to_string()),
    );
    props.insert(
        "cognee_version".into(),
        Value::String(cognee_telemetry::cognee_version().to_string()),
    );
    props.insert(
        "tenant_id".into(),
        Value::String(cognee_telemetry::tenant_id_for_telemetry(tenant_id)),
    );

    cognee_telemetry::send_telemetry(event_name, user_id, Some(Value::Object(props)));
}

/// No-op stand-in when the `telemetry` feature is disabled. Keeps the
/// `execute()` body free of `#[cfg]` clutter.
#[cfg(not(feature = "telemetry"))]
#[inline]
fn emit_pipeline_event(
    _event_name: &str,
    _user_id: Option<Uuid>,
    _pipeline_name: &str,
    _tenant_id: Option<Uuid>,
    _settings: Option<&serde_json::Map<String, serde_json::Value>>,
) {
}

/// Emit a `${task_type} Task <stage>` analytics event via
/// `cognee_telemetry::send_telemetry`. `stage` is one of `"Started"`,
/// `"Completed"`, or `"Errored"`. The `${task_type}` portion of the
/// event name is rendered from [`Task::python_task_type`] and resolves
/// to one of `Function`, `Coroutine`, `Generator`, or `Async Generator`.
///
/// Payload keys (Python parity, see sub-doc 03/05 §1):
/// - `task_name` — falls back to `"unknown"` when the optional
///   `task_name` parameter is `None`, matching the OTEL span fallback.
/// - `cognee_version` — from `cognee_telemetry::cognee_version()`.
/// - `tenant_id` — from `cognee_telemetry::tenant_id_for_telemetry`
///   (literal `"Single User Tenant"` when `None`).
///
/// Per locked decision 7, this fires **once per task**, not per retry
/// attempt — call sites must live outside the `for attempt` loop.
/// Per Python parity (sub-doc §2.3), the `Errored` payload deliberately
/// omits the error string.
#[cfg(feature = "telemetry")]
fn emit_task_event(
    stage: &'static str,
    task: &Task,
    task_name: Option<&str>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
) {
    let event_name = format!("{} Task {}", task.python_task_type(), stage);
    let props = serde_json::json!({
        "task_name": task_name.unwrap_or("unknown"),
        "cognee_version": cognee_telemetry::cognee_version(),
        "tenant_id": cognee_telemetry::tenant_id_for_telemetry(tenant_id),
    });
    cognee_telemetry::send_telemetry(&event_name, user_id, Some(props));
}

/// No-op stand-in when the `telemetry` feature is disabled.
#[cfg(not(feature = "telemetry"))]
#[inline]
fn emit_task_event(
    _stage: &'static str,
    _task: &Task,
    _task_name: Option<&str>,
    _user_id: Option<Uuid>,
    _tenant_id: Option<Uuid>,
) {
}

/// Execute `pipeline` against a set of `inputs`.
///
/// Each input item is run through the full task chain.  When
/// `pipeline.concurrency > 1`, up to that many items are processed in
/// parallel via `buffer_unordered`.  **Output order is not guaranteed when
/// `concurrency > 1`.**
///
/// Within a single item's execution, tasks run sequentially.  When a task
/// produces an iterator or stream, items are gathered into batches of
/// `batch_size`.  If the next task is a batch variant, the slice is passed
/// directly; otherwise each item is executed individually through the
/// remaining pipeline before the next batch is pulled.
pub async fn execute(
    pipeline: &Pipeline,
    inputs: Vec<Arc<dyn Value>>,
    ctx: Arc<TaskContext>,
    watcher: &dyn PipelineWatcher,
) -> Result<Vec<Arc<dyn Value>>, ExecutionError> {
    if pipeline.tasks.is_empty() {
        return Err(ExecutionError::NoTasks);
    }
    if pipeline.batch_size == 0 {
        return Err(ExecutionError::InvalidConfig {
            reason: "batch_size must be > 0".into(),
        });
    }
    if pipeline.concurrency == 0 {
        return Err(ExecutionError::InvalidConfig {
            reason: "concurrency must be > 0".into(),
        });
    }

    let run_id = Uuid::new_v4();
    let task_count = pipeline.tasks.len();

    let user_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.user_id);
    let tenant_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.tenant_id);
    let dataset_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.dataset_id);
    let pipeline_id = deterministic_pipeline_id(pipeline.name.as_deref(), user_id, dataset_id)
        .unwrap_or(pipeline.id);

    // Collect `Data.id`s from the inputs for the watcher's `run_info["data"]`
    // payload. Uses the pipeline's `data_id_fn` extractor when present; falls
    // back to an empty vec, which the watcher maps to Python's `"None"`.
    //
    // `DataIdFn` returns `Option<String>` (the extractor stringifies whatever
    // identity its inputs carry). For `run_info["data"]` we only surface the
    // ones that parse as canonical UUIDs — anything else is silently
    // dropped, mirroring Python's `list[Data]` branch which only emits
    // `str(item.id)` for genuine `Data` instances.
    let data_ids: Vec<Uuid> = if let Some(id_fn) = pipeline.data_id_fn.as_ref() {
        inputs
            .iter()
            .filter_map(|x| id_fn(Arc::clone(x)))
            .filter_map(|s| Uuid::parse_str(&s).ok())
            .collect()
    } else {
        Vec::new()
    };

    let mut run_info = PipelineRunInfo {
        run_id,
        pipeline_id,
        pipeline_name: pipeline.name.clone().unwrap_or_default(),
        user_id,
        tenant_id,
        dataset_id,
        data_ids,
        status: PipelineRunStatus::Initiated,
        started_at: chrono::Utc::now(),
        completed_at: None,
    };

    // Propagate `run_id` into the pipeline context so tasks can attribute
    // payload events via `TaskContext::publish_payload_field`.
    let ctx = ctx.with_run_id(run_id);

    // Clear the per-run provenance visited-set so each pipeline run is
    // isolated (a recycled `TaskContext` would otherwise carry stamps
    // from the previous run). Locked decision 2: visited-set is keyed
    // on `DataPoint.id: Uuid`. See gap 05-06 §4.6.
    if let Some(pctx) = ctx.pipeline_ctx.as_ref() {
        // lock poison is unrecoverable
        pctx.provenance_visited.lock().unwrap().clear();
    }

    // ── INITIATED ──────────────────────────────────────────────────────────
    // Write the audit row BEFORE transitioning to STARTED. The `NoTasks` /
    // `InvalidConfig` guards above ensure malformed pipelines produce zero
    // rows (matching Python: `run_tasks` is only called once tasks are
    // validated upstream). Locked decisions 1 + 13: emit INITIATED at the
    // executor level; no `RunEvent` broadcast.
    watcher.on_pipeline_run_initiated(&run_info).await;

    // ── STARTED ────────────────────────────────────────────────────────────
    run_info.status = PipelineRunStatus::Started;
    watcher
        .on_pipeline(pipeline_id, PipelineStatus::Started { task_count })
        .await;
    watcher.on_pipeline_run_started(&run_info).await;

    // ── Analytics: Pipeline Run Started ─────────────────────────────────
    emit_pipeline_event(
        "Pipeline Run Started",
        user_id,
        &run_info.pipeline_name,
        tenant_id,
        pipeline.telemetry_settings.as_ref(),
    );

    let weights: Vec<u32> = pipeline.tasks.iter().map(|t| t.weight).collect();
    let task_subtokens =
        ctx.progress
            .split(&weights)
            .map_err(|e| ExecutionError::InvalidConfig {
                reason: e.to_string(),
            })?;

    let env = ExecEnv {
        policy: &pipeline.retry_policy,
        default_batch_size: pipeline.batch_size,
        pipeline_id,
        pipeline_name: pipeline.name.as_deref(),
        total_tasks: task_count,
        ctx: &ctx,
        watcher,
        data_id_fn: &pipeline.data_id_fn,
        run_info: &run_info,
        task_subtokens: &task_subtokens,
        rate_limiter: pipeline.rate_limiter.as_ref(),
    };

    let result = if pipeline.concurrency <= 1 {
        execute_items_seq(inputs, pipeline, &ctx, &env).await
    } else {
        execute_items_par(inputs, pipeline, &ctx, &env).await
    };

    match &result {
        Ok(outputs) => {
            run_info.status = PipelineRunStatus::Completed;
            run_info.completed_at = Some(chrono::Utc::now());
            watcher
                .on_pipeline(
                    pipeline_id,
                    PipelineStatus::Succeeded {
                        output_count: outputs.len(),
                    },
                )
                .await;
            watcher
                .on_pipeline_run_completed(&run_info, outputs.len())
                .await;

            // ── Analytics: Pipeline Run Completed ───────────────────────
            emit_pipeline_event(
                "Pipeline Run Completed",
                user_id,
                &run_info.pipeline_name,
                tenant_id,
                pipeline.telemetry_settings.as_ref(),
            );
        }
        Err(ExecutionError::Cancelled) => {
            run_info.status = PipelineRunStatus::Errored;
            run_info.completed_at = Some(chrono::Utc::now());
            watcher
                .on_pipeline(pipeline_id, PipelineStatus::Cancelled)
                .await;
            watcher
                .on_pipeline_run_errored(&run_info, "pipeline was cancelled")
                .await;

            // ── Analytics: Pipeline Run Errored ─────────────────────────
            // No error string on the wire (Python parity, locked decision).
            emit_pipeline_event(
                "Pipeline Run Errored",
                user_id,
                &run_info.pipeline_name,
                tenant_id,
                pipeline.telemetry_settings.as_ref(),
            );
        }
        Err(e) => {
            run_info.status = PipelineRunStatus::Errored;
            run_info.completed_at = Some(chrono::Utc::now());
            let task_index = match e {
                ExecutionError::TaskFailed { task_index, .. } => *task_index,
                _ => 0,
            };
            watcher
                .on_pipeline(
                    pipeline_id,
                    PipelineStatus::Failed {
                        task_index,
                        error: e.to_string(),
                    },
                )
                .await;
            watcher
                .on_pipeline_run_errored(&run_info, &e.to_string())
                .await;

            // ── Analytics: Pipeline Run Errored ─────────────────────────
            // No error string on the wire (Python parity, locked decision).
            emit_pipeline_event(
                "Pipeline Run Errored",
                user_id,
                &run_info.pipeline_name,
                tenant_id,
                pipeline.telemetry_settings.as_ref(),
            );
        }
    }

    result
}
/// Run a single data item through the full task chain, then mark its
/// completion status via `ExecStatusManager`.
async fn execute_one_item<'a>(
    input: Arc<dyn Value>,
    pipeline: &'a Pipeline,
    ctx: &'a Arc<TaskContext>,
    env: &'a ExecEnv<'a>,
) -> Result<Vec<Arc<dyn Value>>, ExecutionError> {
    let data_id = pipeline
        .data_id_fn
        .as_ref()
        .and_then(|f| f(Arc::clone(&input)));

    // Pin `PipelineContext::current_data` to the **original** data item for the
    // whole task chain (Python parity: `PipelineContext(data_item=data_item)` is
    // built once per item in `run_tasks.py` and never rebound per task). Every
    // dispatch site below — `execute_from`, `dispatch_batch`, `process_iter` /
    // `process_stream` — inherits it through this per-item `ExecEnv`.
    let item_ctx = ctx.with_current_data(Arc::clone(&input));
    let item_env = env.with_ctx(&item_ctx);

    let result = execute_from(&pipeline.tasks, input, 0, &item_env).await;

    // Best-effort status marking — don't shadow the pipeline result.
    if let Some(data_id) = &data_id {
        let pipeline_name = pipeline.name.as_deref().unwrap_or("");
        let dataset_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.dataset_id);
        match &result {
            Ok(_) => {
                let _ = ctx
                    .exec_status
                    .mark_completed(data_id, pipeline_name, dataset_id)
                    .await;
            }
            Err(ExecutionError::TaskFailed { source, .. }) => {
                let _ = ctx
                    .exec_status
                    .mark_failed(data_id, pipeline_name, dataset_id, &source.to_string())
                    .await;
            }
            Err(_) => {}
        }
    }

    result
}

/// Sequential execution — items processed one at a time.
async fn execute_items_seq<'a>(
    inputs: Vec<Arc<dyn Value>>,
    pipeline: &'a Pipeline,
    ctx: &'a Arc<TaskContext>,
    env: &'a ExecEnv<'a>,
) -> Result<Vec<Arc<dyn Value>>, ExecutionError> {
    let mut all_outputs = Vec::new();
    for input in inputs {
        all_outputs.append(&mut execute_one_item(input, pipeline, ctx, env).await?);
    }
    Ok(all_outputs)
}

/// Concurrent execution — up to `pipeline.concurrency` items in flight.
///
/// Processes items in chunks of `concurrency` size using `join_all`.
/// Each chunk runs fully before the next chunk starts.
/// **Output order matches chunk order but not necessarily input order
/// within a chunk.**
async fn execute_items_par<'a>(
    inputs: Vec<Arc<dyn Value>>,
    pipeline: &'a Pipeline,
    ctx: &'a Arc<TaskContext>,
    env: &'a ExecEnv<'a>,
) -> Result<Vec<Arc<dyn Value>>, ExecutionError> {
    let mut all_outputs = Vec::new();
    for chunk in inputs.chunks(pipeline.concurrency) {
        let futures: Vec<_> = chunk
            .iter()
            .map(|input| execute_one_item(Arc::clone(input), pipeline, ctx, env))
            .collect();
        let results = futures::future::join_all(futures).await;
        for result in results {
            all_outputs.append(&mut result?);
        }
    }
    Ok(all_outputs)
}
/// Successful output of a task call, with errors already handled / retried.
enum Resolved {
    Single(Arc<dyn Value>),
    Iter(ValueIter),
    Stream(ValueStream),
}

/// Bundle of inputs needed to construct a [`crate::provenance::ProvenanceContext`]
/// at the iter / stream consumption sites. Built once per task in
/// [`execute_from`] and threaded through [`process_iter`] /
/// [`process_stream`] / [`dispatch_batch`] so the per-item stamping
/// loop does not re-walk the input value on every yield.
///
/// See gap 05-06 §4.4 for the design rationale.
#[derive(Clone)]
struct ProvenanceInputs<'a> {
    pipeline_name: &'a str,
    task_name: &'a str,
    user_label: Option<String>,
    input_node_set: Option<String>,
    input_content_hash: Option<String>,
    /// 1-based position of the *emitting* task in the pipeline, written to
    /// `DataPoint.topological_rank`. Python derives this from a deduplicated
    /// task sequence (`run_tasks_base.py:181-190`); Rust pipelines are static
    /// linear arrays, so `first_index + 1` is already the deduplicated
    /// 1-based position and no `task_sequence` equivalent is needed.
    task_rank: i32,
}

impl<'a> ProvenanceInputs<'a> {
    fn ctx(&'a self) -> crate::provenance::ProvenanceContext<'a> {
        crate::provenance::ProvenanceContext {
            pipeline_name: self.pipeline_name,
            task_name: self.task_name,
            user_label: self.user_label.as_deref(),
            node_set: self.input_node_set.as_deref(),
            content_hash: self.input_content_hash.as_deref(),
            task_rank: Some(self.task_rank),
        }
    }
}
/// Parameters that are constant for the entire pipeline run.
/// Bundled into one struct to keep recursive function signatures short.
struct ExecEnv<'a> {
    policy: &'a RetryPolicy,
    /// Pipeline-level default batch size; individual [`TaskInfo`] may override.
    default_batch_size: usize,
    pipeline_id: Uuid,
    pipeline_name: Option<&'a str>,
    total_tasks: usize,
    ctx: &'a Arc<TaskContext>,
    watcher: &'a dyn PipelineWatcher,
    data_id_fn: &'a Option<DataIdFn>,
    /// Rich run info for lifecycle events.
    run_info: &'a PipelineRunInfo,
    /// Per-task progress subtokens, split from the context's progress token.
    task_subtokens: &'a [ProgressToken],
    /// Pipeline-wide rate limiter; per-task limiters override it.
    rate_limiter: Option<&'a Arc<dyn RateLimiter>>,
}

impl<'a> ExecEnv<'a> {
    /// Re-borrow this env against a different [`TaskContext`], keeping every
    /// other (run-constant) field. Used by [`execute_one_item`] to install the
    /// per-item context that carries `PipelineContext::current_data`.
    ///
    /// The returned env has the shorter lifetime `'b` of the borrowed context;
    /// all other `&'a` fields shrink to `'b` by covariance.
    fn with_ctx<'b>(&self, ctx: &'b Arc<TaskContext>) -> ExecEnv<'b>
    where
        'a: 'b,
    {
        ExecEnv {
            policy: self.policy,
            default_batch_size: self.default_batch_size,
            pipeline_id: self.pipeline_id,
            pipeline_name: self.pipeline_name,
            total_tasks: self.total_tasks,
            ctx,
            watcher: self.watcher,
            data_id_fn: self.data_id_fn,
            run_info: self.run_info,
            task_subtokens: self.task_subtokens,
            rate_limiter: self.rate_limiter,
        }
    }
}
/// Depth-first pipeline executor.
///
/// Runs `tasks[0]` on `input`, then:
/// - **Single value** → recurse into `tasks[1..]` with that value.
/// - **Iterator / stream** → collect up to `batch_size` items, dispatch them
///   to the next task (as a batch slice for batch tasks, or individually for
///   non-batch tasks), wait for completion, then pull the next batch.
///
/// The base case (`tasks` is empty) returns `[input]` — the value has
/// already passed through every task.
fn execute_from<'a>(
    tasks: &'a [TaskInfo],
    input: Arc<dyn Value>,
    first_index: usize,
    env: &'a ExecEnv<'a>,
) -> BoxFuture<'a, Result<Vec<Arc<dyn Value>>, ExecutionError>> {
    Box::pin(async move {
        let Some((info, rest)) = tasks.split_first() else {
            // Base case: no more tasks — this value is a final output.
            return Ok(vec![input]);
        };

        if env.ctx.cancellation.is_cancelled() {
            return Err(ExecutionError::Cancelled);
        }

        // ── Incremental dedup: skip items already completed ──────────────
        // Only check at the first task (entire data item enters the pipeline).
        if first_index == 0
            && let Some(id_fn) = env.data_id_fn
            && let Some(data_id) = id_fn(Arc::clone(&input))
        {
            let pipeline_name = env.pipeline_name.unwrap_or("");
            let dataset_id = env.ctx.pipeline_ctx.as_ref().and_then(|p| p.dataset_id);
            let completed = env
                .ctx
                .exec_status
                .is_completed(&data_id, pipeline_name, dataset_id)
                .await
                .map_err(|source| ExecutionError::TaskFailed {
                    task_index: 0,
                    attempts: 0,
                    source,
                })?;
            if completed {
                env.watcher
                    .on_pipeline(env.pipeline_id, PipelineStatus::ItemSkipped { data_id })
                    .await;
                return Ok(vec![]);
            }
        }

        let task_name = info.name.as_deref();
        let task_label = task_name.unwrap_or("");

        env.watcher
            .on_task(
                env.pipeline_id,
                first_index,
                task_name,
                env.total_tasks,
                TaskStatus::Started,
            )
            .await;
        env.watcher
            .on_task_started(env.run_info, task_label, first_index)
            .await;

        // Extract data_id for provenance stamping (re-evaluated here since
        // the value may differ from the dedup check at index 0).
        let data_id = env.data_id_fn.as_ref().and_then(|f| f(Arc::clone(&input)));

        // Build the per-task provenance inputs once. Walks the input
        // value to extract the inherited `node_set` / `content_hash`
        // (Python parity: `_extract_node_set` / `_extract_content_hash`
        // in `run_tasks_base.py`). Reused by `call_with_retry` (Single
        // branch) and by `process_iter` / `process_stream` (eager
        // per-item stamping at consumption — locked decision 8).
        let user_label_owned = env.ctx.pipeline_ctx.as_ref().and_then(|p| p.user_label());
        let prov_inputs = ProvenanceInputs {
            pipeline_name: env.pipeline_name.unwrap_or(""),
            task_name: task_label,
            user_label: user_label_owned,
            input_node_set: crate::provenance::extract_node_set_from_value(input.as_ref()),
            input_content_hash: crate::provenance::extract_content_hash_from_value(input.as_ref()),
            // `first_index` is the 0-based static position of *this* task, so
            // `+ 1` is the 1-based rank Python stamps. Note that
            // `prov_inputs` is later handed to `process_iter` /
            // `process_stream` alongside `first_index + 1` — that argument is
            // the *next* task's index and must not be reused for the rank.
            task_rank: (first_index + 1) as i32,
        };

        // Keep a handle to the original input only for enrichment tasks, so a
        // PassthroughSentinel can forward it unchanged. Cheap Arc clone; skipped
        // entirely for non-enriching tasks.
        let input_passthrough = info.enriches.then(|| Arc::clone(&input));

        let effective_rl = info.rate_limiter.as_ref().or(env.rate_limiter);

        let resolved = call_with_retry(
            &info.task,
            input,
            first_index,
            task_name,
            data_id.as_deref(),
            info.summary_template.as_deref(),
            &prov_inputs,
            effective_rl,
            env,
        )
        .await?;

        env.watcher
            .on_task(
                env.pipeline_id,
                first_index,
                task_name,
                env.total_tasks,
                TaskStatus::Succeeded,
            )
            .await;
        env.watcher
            .on_task_completed(env.run_info, task_label, 1)
            .await;

        // Mark this task's progress as complete.
        env.task_subtokens[first_index].set(1.0);

        // Batch size for accumulating iterator/stream output: the **next** task's
        // per-task override takes priority, falling back to the pipeline default.
        // This matches the Python convention where the consuming task controls
        // how many items it wants to receive at once.
        let batch_size = rest
            .first()
            .and_then(|next| next.batch_size)
            .unwrap_or(env.default_batch_size);

        match resolved {
            Resolved::Single(v) => {
                // Enrichment: a PassthroughSentinel forwards the original input.
                if crate::sentinels::is_passthrough(v.as_ref()) {
                    match input_passthrough {
                        Some(orig) => return execute_from(rest, orig, first_index + 1, env).await,
                        None => {
                            return Err(ExecutionError::TaskFailed {
                                task_index: first_index,
                                attempts: 1,
                                source: "task returned PassthroughSentinel but enriches=false"
                                    .into(),
                            });
                        }
                    }
                }
                // Drop sentinel: discard this item; nothing flows downstream.
                if crate::sentinels::is_dropped(v.as_ref()) {
                    return Ok(vec![]);
                }
                execute_from(rest, v, first_index + 1, env).await
            }
            Resolved::Iter(iter) => {
                process_iter(iter, rest, batch_size, first_index + 1, &prov_inputs, env).await
            }
            Resolved::Stream(stream) => {
                process_stream(stream, rest, batch_size, first_index + 1, &prov_inputs, env).await
            }
        }
    })
}

/// Dispatch an accumulated batch to the tail pipeline.
///
/// - If the next task is a `*Batch` variant, call it directly with the slice.
/// - Otherwise execute each item individually through `execute_from`, collecting
///   all outputs.
///
/// **Design note:** batch-dispatched tasks bypass [`call_with_retry`], and with
/// it *two* executor services — there are no retries and no per-task watcher
/// events. Batch tasks receive pre-accumulated slices and are expected to handle
/// their own error semantics. Only single-value tasks executed via
/// [`execute_from`] get those two.
///
/// Rate limiting, progress, cancellation **and provenance stamping** *are*
/// provided on this path — acquired / scoped / checked / applied here rather
/// than in `call_with_retry`. Stamping uses a [`ProvenanceInputs`] built below
/// for the batch task itself rather than the caller's (which describes the
/// *producer* of the accumulated items) — see `batch_prov`. Because the
/// `DataPoint::topological_rank` write is single-shot, reusing the caller's
/// inputs would permanently stamp the predecessor's rank.
///
/// This doc comment is the code-side summary; the authoritative version, with
/// the reasoning for each service, is the module documentation:
/// [Batch dispatch bypasses executor services](crate::pipeline#batch-dispatch-bypasses-executor-services).
/// Keep the two in sync.
fn dispatch_batch<'a>(
    batch: Vec<Box<dyn Value>>,
    tail: &'a [TaskInfo],
    first_index: usize,
    prov_inputs: &'a ProvenanceInputs<'a>,
    env: &'a ExecEnv<'a>,
) -> BoxFuture<'a, Result<Vec<Arc<dyn Value>>, ExecutionError>> {
    Box::pin(async move {
        let Some((next_info, _)) = tail.split_first() else {
            // No more tasks; each item is a final output.
            return Ok(batch
                .into_iter()
                .map(|item| Arc::from(item) as Arc<dyn Value>)
                .collect());
        };

        if next_info.task.is_batch() {
            // Cancellation: `execute_from`'s check is unreachable for a
            // *terminal* batch task, because `execute_from(rest=[], ..)`
            // returns via its empty-tasks base case before reaching it. Check
            // here so a cancelled run stops before writing another batch
            // rather than draining the producer and reporting success.
            if env.ctx.cancellation.is_cancelled() {
                return Err(ExecutionError::Cancelled);
            }

            // Rate limiting: the batch task's own limiter wins over the
            // pipeline-level one, mirroring `execute_from`'s `effective_rl`.
            // Acquired once per batch call — a batch is one external request.
            if let Some(rl) = next_info.rate_limiter.as_ref().or(env.rate_limiter) {
                rl.acquire().await;

                // The wait above is unbounded — a 1/s limiter parks here for
                // roughly a second — so a cancellation arriving during it would
                // otherwise still let this batch fire. Re-check before calling.
                //
                // Deliberately *not* racing `acquire()` against
                // `cancellation.cancelled()`: `RateLimiter` is a public trait,
                // and dropping a third-party `acquire()` future mid-poll would
                // require a cancel-safety guarantee the trait does not state.
                // (The built-in limiters are cancel-safe — they await a tokio
                // `Semaphore` — but external impls need not be.) Letting the
                // acquisition finish costs a cancelled run at most one limiter
                // slot and never invokes the task.
                if env.ctx.cancellation.is_cancelled() {
                    return Err(ExecutionError::Cancelled);
                }
            }

            // Progress: hand the batch task *its own* subtoken rather than the
            // root token. `ProgressToken::split` zeroes the root's width, so a
            // batch task reporting into the root context was a silent no-op and
            // it could not describe its own progress at all. `with_progress`
            // preserves `pipeline_ctx`, so `current_data` still resolves to the
            // originating item. Completion is set by the caller once the
            // producer is exhausted — `dispatch_batch` runs once per accumulated
            // batch and cannot know which call is the last.
            let batch_ctx = env
                .ctx
                .with_progress(env.task_subtokens[first_index].clone());

            // Provenance for the batch task's *own* outputs. The caller's
            // `prov_inputs` describes the task that produced the accumulated
            // items (`first_index - 1`), so reusing it here would stamp every
            // DataPoint the batch task emits with the predecessor's name and
            // rank — one pipeline column short, and unfixable afterwards
            // because the rank write is single-shot (`None | Some(0)` guard).
            // Python gives every task its own `task_index`
            // (`run_tasks_base.py:181-190`); `first_index` is this batch
            // task's own 0-based position, so `+ 1` is its 1-based rank.
            //
            // `node_set` / `content_hash` are lineage values inherited from
            // the items flowing in, so they carry over from `prov_inputs`
            // rather than being re-extracted (the batch slice holds
            // descendants of the very value they were extracted from).
            //
            // Built after the cancellation / rate-limit gates above so a
            // cancelled run does not pay for the clones.
            let batch_prov = ProvenanceInputs {
                pipeline_name: prov_inputs.pipeline_name,
                task_name: next_info.name.as_deref().unwrap_or(""),
                user_label: prov_inputs.user_label.clone(),
                input_node_set: prov_inputs.input_node_set.clone(),
                input_content_hash: prov_inputs.input_content_hash.clone(),
                task_rank: (first_index + 1) as i32,
            };

            // Call the batch task directly with the accumulated slice.
            // Note: batch tasks bypass `call_with_retry`, so there are no
            // retries and no watcher events (gap 05-06 §8). Items in `batch`
            // were already stamped before being pushed by the upstream
            // `process_iter` / `process_stream`; the shared visited-set makes
            // re-stamping them here a no-op.
            let call = next_info.task.call_batch(&batch, batch_ctx);
            let mut resolved =
                resolve_call(call)
                    .await
                    .map_err(|source| ExecutionError::TaskFailed {
                        task_index: first_index,
                        attempts: 1,
                        source,
                    })?;
            if let Resolved::Single(ref mut v) = resolved {
                stamp_single_output(v, &batch_prov, env);
            }
            // After the batch call resolves, continue through the remaining tail.
            let rest = &tail[1..];
            match resolved {
                Resolved::Single(v) => execute_from(rest, v, first_index + 1, env).await,
                Resolved::Iter(iter) => {
                    let batch_size = rest
                        .first()
                        .and_then(|t| t.batch_size)
                        .unwrap_or(env.default_batch_size);
                    process_iter(iter, rest, batch_size, first_index + 1, &batch_prov, env).await
                }
                Resolved::Stream(stream) => {
                    let batch_size = rest
                        .first()
                        .and_then(|t| t.batch_size)
                        .unwrap_or(env.default_batch_size);
                    process_stream(stream, rest, batch_size, first_index + 1, &batch_prov, env)
                        .await
                }
            }
        } else {
            // Non-batch task: execute each item individually through the
            // remaining pipeline, just like top-level data items.
            let mut all_outputs = Vec::new();
            for item in batch {
                let input = Arc::from(item) as Arc<dyn Value>;
                all_outputs.append(&mut execute_from(tail, input, first_index, env).await?);
            }
            Ok(all_outputs)
        }
    })
}

/// Gather items from a synchronous iterator in `batch_size` chunks, run the
/// tail pipeline on each chunk, and collect all outputs.
///
/// Each item is **eagerly stamped** with provenance before being pushed
/// into the batch (locked decision 8). The visited-set on the
/// `PipelineContext` short-circuits re-stamping a DataPoint that has
/// already been seen by an upstream task in the same run.
async fn process_iter(
    iter: ValueIter,
    tail: &[TaskInfo],
    batch_size: usize,
    first_index: usize,
    prov_inputs: &ProvenanceInputs<'_>,
    env: &ExecEnv<'_>,
) -> Result<Vec<Arc<dyn Value>>, ExecutionError> {
    let mut outputs = Vec::new();
    let mut batch: Vec<Box<dyn Value>> = Vec::with_capacity(batch_size);

    for mut item in iter {
        // Stop pulling from the producer once cancelled. Without this the loop
        // drains the whole iterator and keeps dispatching batches after the
        // cancel request.
        if env.ctx.cancellation.is_cancelled() {
            return Err(ExecutionError::Cancelled);
        }
        // Drop sentinel: discard this item before stamping or accumulating.
        if crate::sentinels::is_dropped(item.as_ref()) {
            continue;
        }
        stamp_boxed_item(&mut item, prov_inputs, env);
        batch.push(item);
        if batch.len() >= batch_size {
            outputs.append(
                &mut dispatch_batch(mem::take(&mut batch), tail, first_index, prov_inputs, env)
                    .await?,
            );
        }
    }

    if !batch.is_empty() {
        outputs.append(&mut dispatch_batch(batch, tail, first_index, prov_inputs, env).await?);
    }

    complete_batch_progress(tail, first_index, env);

    Ok(outputs)
}

/// Gather items from an async stream in `batch_size` chunks, run the tail
/// pipeline on each full chunk (waiting for it to finish) before pulling the
/// next chunk.
///
/// Each item is **eagerly stamped** with provenance before being pushed
/// into the batch (locked decision 8); see [`process_iter`] for the
/// rationale.
///
/// An `Err` item fails the *producer* (`first_index - 1`) without retrying
/// and stops pulling; see [`ValueStream`].
async fn process_stream(
    mut stream: ValueStream,
    tail: &[TaskInfo],
    batch_size: usize,
    first_index: usize,
    prov_inputs: &ProvenanceInputs<'_>,
    env: &ExecEnv<'_>,
) -> Result<Vec<Arc<dyn Value>>, ExecutionError> {
    let mut outputs = Vec::new();
    let mut batch: Vec<Box<dyn Value>> = Vec::with_capacity(batch_size);

    loop {
        // Race the producer against cancellation rather than checking after
        // each item: a slow or stalled stream would otherwise keep a cancelled
        // run alive indefinitely, parked on `next()`. `biased` makes
        // cancellation win deterministically when both are ready, and
        // `StreamExt::next` is cancel-safe, so dropping it mid-poll loses
        // nothing. (`process_iter` cannot do this — a synchronous iterator
        // offers nothing to await, so it checks per item instead.)
        //
        // `cancel_requested`, *not* `cancelled`: the latter also resolves when
        // the `CancellationHandle` is dropped, and every caller drops it
        // (`let (_handle, ctx) = ..`), so that arm would win immediately and
        // abort every streaming pipeline.
        let next = tokio::select! {
            biased;
            () = env.ctx.cancellation.cancel_requested() => {
                return Err(ExecutionError::Cancelled);
            }
            next = stream.next() => next,
        };
        let Some(next) = next else { break };
        // A mid-stream failure fails the *producer* (`first_index` is the
        // consumer's index). Not retried: the stream is partially consumed,
        // and items already dispatched downstream cannot be recalled.
        let mut item = next.map_err(|source| ExecutionError::TaskFailed {
            task_index: first_index - 1,
            attempts: 1,
            source,
        })?;

        // Drop sentinel: discard this item before stamping or accumulating.
        if crate::sentinels::is_dropped(item.as_ref()) {
            continue;
        }
        stamp_boxed_item(&mut item, prov_inputs, env);
        batch.push(item);
        if batch.len() >= batch_size {
            outputs.append(
                &mut dispatch_batch(mem::take(&mut batch), tail, first_index, prov_inputs, env)
                    .await?,
            );
        }
    }

    if !batch.is_empty() {
        outputs.append(&mut dispatch_batch(batch, tail, first_index, prov_inputs, env).await?);
    }

    complete_batch_progress(tail, first_index, env);

    Ok(outputs)
}

/// Mark a batch consumer's progress slice complete once its producer is
/// exhausted.
///
/// `dispatch_batch` runs once per accumulated batch and cannot tell which call
/// is the last, so it hands the batch task its subtoken (letting the task report
/// its own progress) but never completes it. The accumulation loops do know, so
/// completion happens here.
///
/// Only batch consumers need this: a non-batch consumer is reached through
/// `execute_from`, which already sets its subtoken on every invocation.
fn complete_batch_progress(tail: &[TaskInfo], first_index: usize, env: &ExecEnv<'_>) {
    if tail.first().is_some_and(|next| next.task.is_batch()) {
        env.task_subtokens[first_index].set(1.0);
    }
}

/// Eagerly stamp a single boxed iter / stream item using the
/// pipeline's shared visited-set. Mirrors the per-yield call site in
/// Python's `run_tasks_base.py` (locked decision 8).
fn stamp_boxed_item(
    item: &mut Box<dyn Value>,
    prov_inputs: &ProvenanceInputs<'_>,
    env: &ExecEnv<'_>,
) {
    if let Some(pctx) = env.ctx.pipeline_ctx.as_ref() {
        // lock poison is unrecoverable
        let mut visited = pctx.provenance_visited.lock().unwrap();
        let prov_ctx = prov_inputs.ctx();
        let _ = crate::provenance::stamp_tree_dyn(item.as_mut(), &prov_ctx, &mut visited);
    }
}

/// Stamp a `Resolved::Single` task output using the pipeline's shared
/// visited-set (locked decision 8). Shared by [`call_with_retry`] and the
/// batch-dispatch path so both stamp identically.
///
/// Uniquely-owned `Arc` only: a shared `Arc` cannot be mutated, which would
/// mean silently dropping the stamp, so that case is logged instead.
fn stamp_single_output(
    value: &mut Arc<dyn Value>,
    prov_inputs: &ProvenanceInputs<'_>,
    env: &ExecEnv<'_>,
) {
    let Some(pctx) = env.ctx.pipeline_ctx.as_ref() else {
        return;
    };
    let prov_ctx = prov_inputs.ctx();
    // lock poison is unrecoverable
    let mut visited = pctx.provenance_visited.lock().unwrap();
    if let Some(inner) = Arc::get_mut(value) {
        let _ = crate::provenance::stamp_tree_dyn(inner, &prov_ctx, &mut visited);
    } else {
        tracing::warn!(
            "skipping provenance stamping: shared Arc<dyn Value> for task '{}'",
            prov_inputs.task_name
        );
    }
}
/// Call `task` on `input`, retrying on failure according to `env.policy`.
///
/// Retry applies to the task call itself (including awaiting async tasks and
/// setting up iterators / streams).  Individual items emitted by an already-
/// initialised iterator or stream are not retried.
#[allow(clippy::too_many_arguments)]
async fn call_with_retry(
    task: &Task,
    input: Arc<dyn Value>,
    task_index: usize,
    task_name: Option<&str>,
    data_id: Option<&str>,
    #[allow(unused_variables)] summary_template: Option<&str>,
    prov_inputs: &ProvenanceInputs<'_>,
    rate_limiter: Option<&Arc<dyn RateLimiter>>,
    env: &ExecEnv<'_>,
) -> Result<Resolved, ExecutionError> {
    // ── Telemetry span (only when feature is enabled) ───────────────────
    #[cfg(feature = "telemetry")]
    let span = tracing::info_span!(
        "cognee.pipeline.task",
        task.name = task_name.unwrap_or("unknown"),
        task.index = task_index,
        task.result_count = tracing::field::Empty,
        task.result_summary = tracing::field::Empty,
        task.error = tracing::field::Empty,
    );

    let max_attempts = env.policy.max_attempts();
    let mut last_error: Option<TaskError> = None;

    // Inject the task-specific progress subtoken. `current_data` is *not* set
    // here: it is pinned to the original data item once per item in
    // `execute_one_item` (Python's `ctx.data_item`), so overwriting it with this
    // task's input would make it mean "previous task's output" instead.
    let subtoken = env.task_subtokens[task_index].clone();
    let task_ctx = env.ctx.with_progress(subtoken);

    // Resolve identity once (outside the retry loop) — per locked
    // decision 7, task lifecycle events fire once per task, not per
    // attempt.
    let user_id = env.ctx.pipeline_ctx.as_ref().and_then(|p| p.user_id);
    let tenant_id = env.ctx.pipeline_ctx.as_ref().and_then(|p| p.tenant_id);

    // Telemetry first, then watcher (matches `execute()` ordering).
    emit_task_event("Started", task, task_name, user_id, tenant_id);

    for attempt in 1..=max_attempts {
        // Proactive throttle: every attempt is a fresh external call.
        if let Some(rl) = rate_limiter {
            rl.acquire().await;
        }
        let call = task.call(input.clone(), Arc::clone(&task_ctx));
        match resolve_call(call).await {
            Ok(mut resolved) => {
                // ── Telemetry: record result count ──────────────────────
                #[cfg(feature = "telemetry")]
                {
                    let result_count: usize = match &resolved {
                        Resolved::Single(_) => 1,
                        Resolved::Iter(_) | Resolved::Stream(_) => 1,
                    };
                    span.record("task.result_count", result_count);
                    if let Some(template) = summary_template {
                        let summary = template.replace("{n}", &result_count.to_string());
                        span.record("task.result_summary", summary.as_str());
                    }
                }

                // ── Provenance stamping (DataPoint trees) ──────────────
                // Locked decision 8: `Resolved::Single` is stamped here;
                // `Iter` / `Stream` items are stamped eagerly at the
                // consumption site in `process_iter` / `process_stream`.
                // The audit-log call below (locked decision 3) is
                // separate — both coexist.
                if let Resolved::Single(ref mut v) = resolved {
                    stamp_single_output(v, prov_inputs, env);
                }

                // ── Provenance stamping (best-effort) ───────────────────
                if let Some(data_id) = data_id {
                    let pipeline_name = env.pipeline_name.unwrap_or("");
                    let user_id = env.ctx.pipeline_ctx.as_ref().and_then(|p| p.user_id);

                    // Extract node_set from the result if it's a TaggedMeta.
                    let node_set = match &resolved {
                        Resolved::Single(v) => (**v)
                            .as_any()
                            .downcast_ref::<TaggedMeta>()
                            .and_then(|m| m.node_set.clone()),
                        _ => None,
                    };

                    let _ = env
                        .ctx
                        .exec_status
                        .stamp_provenance(
                            data_id,
                            pipeline_name,
                            task_name.unwrap_or(""),
                            user_id,
                            node_set.as_deref(),
                        )
                        .await;
                }

                emit_task_event("Completed", task, task_name, user_id, tenant_id);
                return Ok(resolved);
            }
            Err(e) => {
                let error_str = e.to_string();

                // ── Telemetry: record error ─────────────────────────────
                #[cfg(feature = "telemetry")]
                span.record("task.error", error_str.as_str());

                last_error = Some(e);
                if attempt < max_attempts {
                    env.watcher
                        .on_task(
                            env.pipeline_id,
                            task_index,
                            task_name,
                            env.total_tasks,
                            TaskStatus::Retrying {
                                attempt,
                                error: error_str,
                            },
                        )
                        .await;
                    let retry_index = attempt - 1; // 0-based
                    if let Some(delay) = env.policy.delay(retry_index) {
                        sleep(delay).await;
                    }
                }
            }
        }
    }

    let source = last_error.expect("loop ran at least once");
    let error_str = source.to_string();

    #[cfg(feature = "telemetry")]
    span.record("task.error", error_str.as_str());

    // Telemetry first, then watcher (matches `execute()` ordering).
    emit_task_event("Errored", task, task_name, user_id, tenant_id);

    env.watcher
        .on_task(
            env.pipeline_id,
            task_index,
            task_name,
            env.total_tasks,
            TaskStatus::Failed {
                attempts: max_attempts,
                error: error_str.clone(),
            },
        )
        .await;
    env.watcher
        .on_task_errored(env.run_info, task_name.unwrap_or(""), &error_str)
        .await;

    Err(ExecutionError::TaskFailed {
        task_index,
        attempts: max_attempts,
        source,
    })
}

/// Resolve a [`TaskCall`] into a [`Resolved`] value, awaiting the future for
/// async tasks.
async fn resolve_call(call: TaskCall) -> Result<Resolved, TaskError> {
    match call {
        TaskCall::Sync(r) => r.map(Resolved::Single),
        TaskCall::Async(fut) => fut.await.map(Resolved::Single),
        TaskCall::SyncIter(r) => r.map(Resolved::Iter),
        TaskCall::AsyncStream(r) => r.map(Resolved::Stream),
    }
}
/// The successful output of a pipeline run.
pub struct PipelineRunResult {
    /// The pipeline's ID (matches [`Pipeline::id`]).
    pub run_id: Uuid,
    /// Collected outputs from the final task in the pipeline.
    pub outputs: Vec<Arc<dyn Value>>,
}
/// Handle to a pipeline run spawned in the background via
/// [`execute_in_background`].
///
/// The pipeline continues running even if this handle is dropped (detached).
/// Call [`wait`](PipelineRunHandle::wait) to await its completion, or
/// [`abort`](PipelineRunHandle::abort) to cancel it.
pub struct PipelineRunHandle {
    /// The pipeline's ID.
    pub run_id: Uuid,
    inner: tokio::task::JoinHandle<Result<PipelineRunResult, ExecutionError>>,
}

impl PipelineRunHandle {
    /// Wait for the background pipeline run to complete.
    pub async fn wait(self) -> Result<PipelineRunResult, ExecutionError> {
        match self.inner.await {
            Ok(result) => result,
            Err(join_err) => {
                if join_err.is_cancelled() {
                    Err(ExecutionError::Cancelled)
                } else {
                    // Task panicked — propagate as a generic task failure.
                    Err(ExecutionError::TaskFailed {
                        task_index: 0,
                        attempts: 0,
                        source: join_err.to_string().into(),
                    })
                }
            }
        }
    }

    /// Abort the background pipeline run.
    ///
    /// The spawned task is cancelled at its next `.await` point.
    pub fn abort(&self) {
        self.inner.abort();
    }

    /// Returns `true` if the background task has completed (success or failure).
    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }
}
/// Spawn [`execute`] on the current Tokio runtime and return a
/// [`PipelineRunHandle`] immediately.
///
/// The pipeline, context, and watcher must be owned (`Arc`) since the
/// spawned task is `'static`.  Equivalent to Python's
/// `run_pipeline_as_background_process`.
///
/// ```rust,ignore
/// let handle = execute_in_background(
///     Arc::new(pipeline),
///     vec![input],
///     ctx,
///     Arc::new(NoopWatcher),
/// );
/// // ... do other work ...
/// let result = handle.wait().await?;
/// ```
pub fn execute_in_background(
    pipeline: Arc<Pipeline>,
    inputs: Vec<Arc<dyn Value>>,
    ctx: Arc<TaskContext>,
    watcher: Arc<dyn PipelineWatcher>,
) -> PipelineRunHandle {
    let run_id = pipeline.id;
    // Build the future manually and coerce to a trait object to help the
    // compiler resolve the higher-ranked lifetime on `DataIdFn`.
    let fut = async move {
        let outputs = execute(&pipeline, inputs, ctx, watcher.as_ref()).await?;
        Ok(PipelineRunResult { run_id, outputs })
    };
    let fut: std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>> = Box::pin(fut);
    let inner = tokio::spawn(fut);
    PipelineRunHandle { run_id, inner }
}

/// Run [`execute`] synchronously, blocking the calling thread until the
/// pipeline completes.
///
/// This creates a new single-threaded Tokio runtime internally.  Use this
/// from non-async code (e.g. a CLI main function or a C FFI callback).
/// Equivalent to Python's `run_pipeline_blocking`.
///
/// # Panics
///
/// Panics if called from within an existing Tokio runtime (nested runtimes
/// are not supported).  Use [`execute`] directly in that case.
pub fn execute_blocking(
    pipeline: &Pipeline,
    inputs: Vec<Arc<dyn Value>>,
    ctx: Arc<TaskContext>,
    watcher: &dyn PipelineWatcher,
) -> Result<PipelineRunResult, ExecutionError> {
    let run_id = pipeline.id;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ExecutionError::TaskFailed {
            task_index: 0,
            attempts: 0,
            source: e.into(),
        })?;
    let outputs = rt.block_on(execute(pipeline, inputs, ctx, watcher))?;
    Ok(PipelineRunResult { run_id, outputs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;

    use crate::cancellation::cancellation_pair;
    use crate::exec_status::NoopExecStatusManager;
    use crate::progress::ProgressToken;
    use crate::task::{Task, TaskError, Value};
    use crate::task_context::TaskContext;
    use crate::thread_pool::CpuPool;

    struct StubPool;
    impl CpuPool for StubPool {
        fn spawn_raw(
            &self,
            _task: Box<dyn FnOnce() + Send + 'static>,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::error::CoreError>> + Send + 'static>>
        {
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
            pipeline_watcher: None,
        })
    }

    /// Like [`stub_ctx`], but with a `PipelineContext` attached so tasks can call
    /// `ctx.pipeline()`. Uses the shared
    /// [`PipelineContext::for_test`](crate::task_context::PipelineContext::for_test)
    /// fixture rather than another hand-written struct literal.
    async fn stub_ctx_with_pipeline(pipeline_name: &str) -> Arc<TaskContext> {
        let pipeline_ctx = crate::task_context::PipelineContext::for_test(pipeline_name);
        let ctx = stub_ctx().await;
        Arc::new(TaskContext {
            thread_pool: Arc::clone(&ctx.thread_pool),
            database: Arc::clone(&ctx.database),
            graph_db: Arc::clone(&ctx.graph_db),
            vector_db: Arc::clone(&ctx.vector_db),
            cancellation: ctx.cancellation.clone(),
            progress: ctx.progress.clone(),
            pipeline_ctx: Some(pipeline_ctx),
            exec_status: Arc::clone(&ctx.exec_status),
            pipeline_watcher: None,
        })
    }

    #[test]
    fn pipeline_run_info_elapsed_seconds_returns_none_before_completion() {
        let info = PipelineRunInfo {
            run_id: Uuid::new_v4(),
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "test".to_string(),
            user_id: None,
            tenant_id: None,
            dataset_id: None,
            data_ids: Vec::new(),
            status: PipelineRunStatus::Started,
            started_at: chrono::Utc::now(),
            completed_at: None,
        };
        assert_eq!(info.elapsed_seconds(), None);
    }

    #[test]
    fn pipeline_run_info_elapsed_seconds_returns_positive_after_completion() {
        let now = chrono::Utc::now();
        let started_at = now - chrono::Duration::milliseconds(100);
        let info = PipelineRunInfo {
            run_id: Uuid::new_v4(),
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "test".to_string(),
            user_id: None,
            tenant_id: None,
            dataset_id: None,
            data_ids: Vec::new(),
            status: PipelineRunStatus::Completed,
            started_at,
            completed_at: Some(now),
        };
        let elapsed = info
            .elapsed_seconds()
            .expect("elapsed_seconds should be Some when completed_at is set");
        assert!(elapsed > 0.0, "elapsed should be positive, got {elapsed}");
        assert!(elapsed < 1.0, "elapsed should be < 1s, got {elapsed}");
    }

    #[tokio::test]
    async fn test_execute_retry_on_failure() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let task = Task::Sync(Arc::new(move |input, _ctx| {
            let prev = counter_clone.fetch_add(1, Ordering::SeqCst);
            if prev < 2 {
                // Fail on first two calls (prev == 0 and prev == 1).
                return Err("not yet".into());
            }
            // Third call (prev == 2): succeed with input doubled.
            let val = (*input).as_any().downcast_ref::<i32>().unwrap();
            Ok(Arc::new(*val * 2) as Arc<dyn Value>)
        }));

        let policy = RetryPolicy::Limited {
            max_attempts: std::num::NonZeroU32::new(3).unwrap(),
            delay: RetryDelay::Constant(Duration::from_millis(1)),
        };

        let pipeline = Pipeline::new("retry pipeline")
            .with_retry(policy)
            .with_task(task);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(21_i32)];
        let ctx = stub_ctx().await;
        let watcher = NoopWatcher;

        let outputs = execute(&pipeline, inputs, ctx, &watcher).await.unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 42);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    /// Read `ctx.pipeline().current_data` as an `i32`, panicking if absent.
    ///
    /// Shared by every `current_data_*` test below. Panicking (rather than
    /// returning `Option`) is deliberate: if a refactor threads the root `env`
    /// into a dispatch site, `current_data` becomes `None`, and these tests must
    /// then fail loudly instead of silently skipping their assertion.
    fn observed_current_data(ctx: &Arc<TaskContext>) -> i32 {
        ctx.pipeline()
            .current_data
            .as_ref()
            .and_then(|v| (**v).as_any().downcast_ref::<i32>().copied())
            .expect("current_data is pinned per item by execute_one_item")
    }

    /// `PipelineContext::current_data` must hold the **original** value that
    /// entered the pipeline for this item, not the upstream task's output —
    /// Python's `ctx.data_item` semantics. See `execute_one_item`.
    #[tokio::test]
    async fn current_data_is_the_original_item_not_the_previous_output() {
        use std::sync::Mutex;

        // Records what each task observed in `ctx.pipeline().current_data`.
        let seen: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));

        let record = |seen: Arc<Mutex<Vec<i32>>>| {
            Task::sync_typed(
                move |x: &i32, ctx: Arc<TaskContext>| -> Result<Box<i32>, TaskError> {
                    seen.lock().unwrap().push(observed_current_data(&ctx));
                    Ok(Box::new(*x + 1))
                },
            )
        };

        // Task 0 rewrites the value so a stale `current_data` would be visible
        // to tasks 1 and 2 as `700` / `701` rather than the original `7`.
        let rewrite = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 100))
        });

        let pipeline = Pipeline::new("current_data pipeline")
            .with_task(rewrite)
            .with_task(record(Arc::clone(&seen)))
            .with_task(record(Arc::clone(&seen)));

        let ctx = stub_ctx_with_pipeline("test").await;

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(7_i32)];
        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 1);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![7, 7],
            "tasks at index 1 and 2 must both see the original pipeline input"
        );
    }

    /// Batch tasks are dispatched via `dispatch_batch` with the per-item env, so
    /// they inherit the same `current_data` as single-value tasks.
    #[tokio::test]
    async fn current_data_is_inherited_by_batch_tasks() {
        use std::sync::Mutex;

        let seen: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);

        // Fan the single input out into three items so the next task is reached
        // through `process_iter` → `dispatch_batch`.
        let fan_out = Task::sync_iter_typed(|x: &i32, _ctx| {
            let x = *x;
            Ok((0..3).map(move |i| Box::new(x + i)))
        });
        // Raw `sync_batch` rather than `sync_batch_typed`: this test only needs
        // the item count and the context, so there is nothing to downcast and
        // nothing to gain from the typed variant. (The typed batch constructors
        // are exercised in `task.rs`.)
        let collect = Task::sync_batch(
            move |items: &[Box<dyn Value>],
                  ctx: Arc<TaskContext>|
                  -> Result<Arc<dyn Value>, TaskError> {
                seen_clone.lock().unwrap().push(observed_current_data(&ctx));
                Ok(Arc::new(items.len() as i32) as Arc<dyn Value>)
            },
        );

        let pipeline = Pipeline::new("batch current_data pipeline")
            .with_task(fan_out)
            .with_task(collect);

        let ctx = stub_ctx_with_pipeline("test").await;
        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(7_i32)];
        execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![7],
            "the batch task must see the original pipeline input, not None"
        );
    }

    /// The `process_stream` path must preserve `current_data` too: both the
    /// batch task it dispatches to and the tail task after it see the original
    /// pipeline input, never a streamed item and never `None`.
    #[tokio::test]
    async fn current_data_survives_process_stream_dispatch() {
        use std::sync::Mutex;

        let seen_batch: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_tail: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));

        // Stream out three values *different from* the pipeline input, so a
        // `current_data` sourced from the streamed item would read 70/71/72.
        let fan_out = Task::async_stream(|input: Arc<dyn Value>, _ctx| {
            let x = *(*input).as_any().downcast_ref::<i32>().expect("i32 input");
            let stream = futures::stream::iter(0..3)
                .map(move |i| Ok(Box::new(x * 10 + i) as Box<dyn Value>));
            Ok(Box::pin(stream) as ValueStream)
        });

        // Raw `sync_batch` rather than `sync_batch_typed`: this test only needs
        // the item count and the context, so there is nothing to downcast and
        // nothing to gain from the typed variant. (The typed batch constructors
        // are exercised in `task.rs`.)
        let seen_batch_c = Arc::clone(&seen_batch);
        let collect = Task::sync_batch(
            move |items: &[Box<dyn Value>],
                  ctx: Arc<TaskContext>|
                  -> Result<Arc<dyn Value>, TaskError> {
                seen_batch_c
                    .lock()
                    .unwrap()
                    .push(observed_current_data(&ctx));
                Ok(Arc::new(items.len() as i32) as Arc<dyn Value>)
            },
        );

        let seen_tail_c = Arc::clone(&seen_tail);
        let tail = Task::sync_typed(
            move |n: &i32, ctx: Arc<TaskContext>| -> Result<Box<i32>, TaskError> {
                seen_tail_c
                    .lock()
                    .unwrap()
                    .push(observed_current_data(&ctx));
                Ok(Box::new(*n))
            },
        );

        let pipeline = Pipeline::new("stream current_data pipeline")
            .with_task(fan_out)
            .with_task(collect)
            .with_task(tail);

        let ctx = stub_ctx_with_pipeline("test").await;
        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(7_i32)];
        execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(
            *seen_batch.lock().unwrap(),
            vec![7],
            "a batch task fed by process_stream must see the original pipeline input"
        );
        assert_eq!(
            *seen_tail.lock().unwrap(),
            vec![7],
            "the task after the batch must still see the original pipeline input"
        );
    }

    /// With `concurrency > 1`, several items are in flight against the *same*
    /// task objects. Each task invocation must see **its own** item's original
    /// input — never another item's. This is the case that would misattribute
    /// provenance / ownership across documents.
    #[tokio::test]
    async fn current_data_is_isolated_per_item_under_concurrency() {
        use std::sync::Mutex;

        const ITEMS: i32 = 6;

        // Records (this task's input, observed current_data) pairs.
        let pairs: Arc<Mutex<Vec<(i32, i32)>>> = Arc::new(Mutex::new(Vec::new()));

        // Rewrite the value *and* await, so the items genuinely interleave: the
        // sleeps are in reverse order, so completion order differs from start
        // order and a shared context would cross items over.
        let rewrite = Task::async_fn(|input: Arc<dyn Value>, _ctx| {
            let x = *(*input).as_any().downcast_ref::<i32>().expect("i32 input");
            Box::pin(async move {
                sleep(Duration::from_millis((ITEMS + 1 - x) as u64 * 5)).await;
                Ok(Arc::new(x * 100) as Arc<dyn Value>)
            })
        });

        let pairs_c = Arc::clone(&pairs);
        let record = Task::sync_typed(
            move |x: &i32, ctx: Arc<TaskContext>| -> Result<Box<i32>, TaskError> {
                pairs_c
                    .lock()
                    .unwrap()
                    .push((*x, observed_current_data(&ctx)));
                Ok(Box::new(*x))
            },
        );

        let pipeline = Pipeline::new("per-item current_data pipeline")
            .with_concurrency(4)
            .with_task(rewrite)
            .with_task(record);

        let ctx = stub_ctx_with_pipeline("test").await;
        let inputs: Vec<Arc<dyn Value>> =
            (1..=ITEMS).map(|i| Arc::new(i) as Arc<dyn Value>).collect();
        execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        let recorded = pairs.lock().unwrap().clone();
        assert_eq!(
            recorded.len(),
            ITEMS as usize,
            "every item must be recorded"
        );
        for (task_input, observed) in &recorded {
            assert_eq!(
                *task_input,
                observed * 100,
                "task saw current_data={observed} while processing {task_input}: \
                 that is a different item's original input"
            );
        }

        // And every original input was observed exactly once.
        let mut observed: Vec<i32> = recorded.iter().map(|(_, o)| *o).collect();
        observed.sort_unstable();
        assert_eq!(observed, (1..=ITEMS).collect::<Vec<_>>());
    }

    /// The enrichment re-dispatch path (`PassthroughSentinel` on a
    /// `with_enriches()` task) forwards the original input through a second
    /// `execute_from` call; that call must keep the per-item `current_data`.
    #[tokio::test]
    async fn current_data_survives_enrichment_passthrough() {
        use std::sync::Mutex;

        let seen: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));

        // Rewrite first, so `current_data` (7) is distinguishable from the value
        // the passthrough forwards (700).
        let rewrite = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 100))
        });

        let enrich = TaskInfo::new(Task::Sync(Arc::new(|_input: Arc<dyn Value>, _ctx| {
            Ok(Arc::new(crate::sentinels::PassthroughSentinel) as Arc<dyn Value>)
        })))
        .with_enriches();

        let seen_c = Arc::clone(&seen);
        let record = Task::sync_typed(
            move |x: &i32, ctx: Arc<TaskContext>| -> Result<Box<i32>, TaskError> {
                seen_c.lock().unwrap().push(observed_current_data(&ctx));
                Ok(Box::new(*x))
            },
        );

        let pipeline = Pipeline::new("enrichment current_data pipeline")
            .with_task(rewrite)
            .with_task(enrich)
            .with_task(record);

        let ctx = stub_ctx_with_pipeline("test").await;
        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(7_i32)];
        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        // Sanity: the passthrough really did forward the rewritten value.
        assert_eq!(*(*outputs[0]).as_any().downcast_ref::<i32>().unwrap(), 700);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![7],
            "the task after an enrichment passthrough must see the original input"
        );
    }

    /// `Task::parallel` hands each sub-task `Arc::clone(&ctx)`, so every sub-task
    /// must observe the same per-item `current_data`.
    #[tokio::test]
    async fn current_data_is_visible_to_parallel_subtasks() {
        use std::sync::Mutex;

        let seen: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));

        let rewrite = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 100))
        });

        let record = |seen: Arc<Mutex<Vec<i32>>>| {
            Task::sync_typed(
                move |x: &i32, ctx: Arc<TaskContext>| -> Result<Box<i32>, TaskError> {
                    seen.lock().unwrap().push(observed_current_data(&ctx));
                    Ok(Box::new(*x))
                },
            )
        };

        let par = TaskInfo::parallel(vec![
            TaskInfo::new(record(Arc::clone(&seen))),
            TaskInfo::new(record(Arc::clone(&seen))),
        ]);

        let pipeline = Pipeline::new("parallel current_data pipeline")
            .with_task(rewrite)
            .with_task(par);

        let ctx = stub_ctx_with_pipeline("test").await;
        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(7_i32)];
        execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        let mut observed = seen.lock().unwrap().clone();
        observed.sort_unstable();
        assert_eq!(
            observed,
            vec![7, 7],
            "both parallel sub-tasks must see the original pipeline input"
        );
    }

    #[tokio::test]
    async fn test_execute_single_task_pipeline() {
        let double = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 2))
        });

        let pipeline = Pipeline::new("double pipeline").with_task(double);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(5_i32)];
        let ctx = stub_ctx().await;
        let watcher = NoopWatcher;

        let outputs = execute(&pipeline, inputs, ctx, &watcher).await.unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 10);
    }

    #[tokio::test]
    async fn test_execute_chained_tasks() {
        // task1 doubles an i32, task2 adds 1.
        let double = Task::sync_typed(|x: &i32, _ctx| Ok(Box::new(*x * 2)));
        let add_one = Task::sync_typed(|x: &i32, _ctx| Ok(Box::new(*x + 1)));

        let pipeline = Pipeline::new("chained test")
            .with_task(double)
            .with_task(add_one);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(3_i32)];
        let ctx = stub_ctx().await;
        let watcher = NoopWatcher;

        let outputs = execute(&pipeline, inputs, ctx, &watcher).await.unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        // 3 * 2 = 6, 6 + 1 = 7
        assert_eq!(*result, 7);
    }

    #[tokio::test]
    async fn test_execute_iter_task_batching() {
        // Task 1: SyncIter that yields 5 items (integers 1..=5).
        let iter_task = Task::SyncIter(Arc::new(move |_input, _ctx| {
            let iter = (1..=5).map(|i| Box::new(i) as Box<dyn Value>);
            Ok(Box::new(iter) as Box<dyn Iterator<Item = Box<dyn Value>> + Send>)
        }));

        // Task 2: Sync that doubles each individual item.
        let double_task = Task::sync_typed(|x: &i32, _ctx| Ok(Box::new(*x * 2)));

        let pipeline = Pipeline::new("iter batching test")
            .with_batch_size(2)
            .with_task(iter_task)
            .with_task(double_task);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;
        let watcher = NoopWatcher;

        let outputs = execute(&pipeline, inputs, ctx, &watcher).await.unwrap();

        // Each of the 5 items is individually doubled.
        assert_eq!(outputs.len(), 5);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![2, 4, 6, 8, 10]);
    }

    #[tokio::test]
    async fn test_cancellation_stops_pipeline() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let call_count = Arc::new(AtomicU32::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Task 1: succeeds and signals cancellation via the token.
        let task1 = Task::Async(Arc::new(move |input, ctx| {
            let cc = Arc::clone(&call_count_clone);
            Box::pin(async move {
                cc.fetch_add(1, Ordering::SeqCst);
                ctx.cancellation.cancelled().await; // noop: we cancel synchronously below
                Ok(input)
            })
        }));

        // Task 2: should never run if cancellation is detected between tasks.
        let call_count_clone2 = Arc::clone(&call_count);
        let task2 = Task::Sync(Arc::new(move |input, _ctx| {
            call_count_clone2.fetch_add(1, Ordering::SeqCst);
            Ok(input)
        }));

        let pipeline = Pipeline::new("cancel test")
            .with_task(task1)
            .with_task(task2);

        let db = cognee_database::connect("sqlite::memory:").await.unwrap();
        cognee_database::initialize(&db).await.unwrap();
        let (handle, token) = cancellation_pair();
        let ctx = Arc::new(TaskContext {
            thread_pool: Arc::new(StubPool),
            database: Arc::new(db),
            graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
            vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
            cancellation: token,
            progress: ProgressToken::new(),
            pipeline_ctx: None,
            exec_status: Arc::new(NoopExecStatusManager),
            pipeline_watcher: None,
        });

        // Cancel before execute so the check at execute_from catches it.
        handle.cancel();

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(1_i32)];
        let result = execute(&pipeline, inputs, ctx, &NoopWatcher).await;

        assert!(
            matches!(result, Err(ExecutionError::Cancelled)),
            "expected Cancelled error"
        );
    }

    #[tokio::test]
    async fn test_sync_terminal() {
        let double = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 2))
        });

        let pipeline = Pipeline::new("sync terminal").with_task(double);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(5_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 10);
    }

    #[tokio::test]
    async fn test_async_terminal() {
        let triple = Task::async_fn_typed(|x: &i32, _ctx| {
            let val = *x;
            Box::pin(async move { Ok(Box::new(val * 3)) })
        });

        let pipeline = Pipeline::new("async terminal").with_task(triple);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(4_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 12);
    }

    #[tokio::test]
    async fn test_sync_iter_terminal() {
        use crate::task::ValueIter;

        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![10_i32, 20, 30];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        let pipeline = Pipeline::new("sync iter terminal").with_task(iter_task);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 3);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![10, 20, 30]);
    }

    #[tokio::test]
    async fn test_sync_iter_then_sync() {
        use crate::task::ValueIter;

        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![1_i32, 2, 3, 4];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // Each item is executed individually through the Sync task.
        let double_task = Task::sync_typed(|x: &i32, _ctx| Ok(Box::new(*x * 2)));

        let pipeline = Pipeline::new("sync iter then sync")
            .with_batch_size(2)
            .with_task(iter_task)
            .with_task(double_task);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 4);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![2, 4, 6, 8]);
    }

    #[tokio::test]
    async fn test_sync_iter_then_async() {
        use crate::task::ValueIter;

        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![1_i32, 2, 3];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // Each item is executed individually through the Async task.
        let add_ten = Task::async_fn_typed(|x: &i32, _ctx| {
            let v = *x + 10;
            Box::pin(async move { Ok(Box::new(v)) })
        });

        let pipeline = Pipeline::new("sync iter then async")
            .with_batch_size(3)
            .with_task(iter_task)
            .with_task(add_ten);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 3);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![11, 12, 13]);
    }

    #[tokio::test]
    async fn test_async_stream_terminal() {
        let stream_task = Task::AsyncStream(Arc::new(|_input, _ctx| {
            let items = vec![100_i32, 200, 300];
            Ok(
                Box::pin(futures::stream::iter(items).map(|i| Ok(Box::new(i) as Box<dyn Value>)))
                    as ValueStream,
            )
        }));

        let pipeline = Pipeline::new("async stream terminal").with_task(stream_task);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 3);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![100, 200, 300]);
    }

    /// A stream that fails after it has started yielding fails the run,
    /// attributes the error to the producer, and is not pulled any further.
    #[tokio::test]
    async fn test_async_stream_mid_stream_error_fails_producer() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let pulled_past_error = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&pulled_past_error);
        let stream_task = Task::AsyncStream(Arc::new(move |_input, _ctx| {
            let flag = Arc::clone(&flag);
            let items = futures::stream::iter(0..3).map(move |i| match i {
                0 => Ok(Box::new(1_i32) as Box<dyn Value>),
                1 => Err::<Box<dyn Value>, TaskError>("boom".into()),
                _ => {
                    flag.store(true, Ordering::SeqCst);
                    Ok(Box::new(3_i32) as Box<dyn Value>)
                }
            });
            Ok(Box::pin(items) as ValueStream)
        }));

        let seen = Arc::new(AtomicUsize::new(0));
        let seen_in_task = Arc::clone(&seen);
        let consumer = Task::Sync(Arc::new(move |input, _ctx| {
            seen_in_task.fetch_add(1, Ordering::SeqCst);
            Ok(input)
        }));

        // batch_size 1: the first item is dispatched before the error arrives.
        let pipeline = Pipeline::new("mid-stream error")
            .with_task(stream_task)
            .with_task(consumer)
            .with_batch_size(1);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let Err(err) = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher).await else {
            panic!("a mid-stream error must fail the run");
        };

        match err {
            ExecutionError::TaskFailed {
                task_index,
                attempts,
                source,
            } => {
                assert_eq!(task_index, 0, "the producer, not the consumer, failed");
                assert_eq!(attempts, 1, "mid-stream failures are not retried");
                assert_eq!(source.to_string(), "boom");
            }
            other => panic!("expected TaskFailed, got {other:?}"),
        }
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        assert!(!pulled_past_error.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_async_stream_then_sync() {
        let stream_task = Task::AsyncStream(Arc::new(|_input, _ctx| {
            let items = vec![10_i32, 20, 30, 40];
            Ok(
                Box::pin(futures::stream::iter(items).map(|i| Ok(Box::new(i) as Box<dyn Value>)))
                    as ValueStream,
            )
        }));

        // Each item is executed individually through the Sync task.
        let triple = Task::sync_typed(|x: &i32, _ctx| Ok(Box::new(*x * 3)));

        let pipeline = Pipeline::new("async stream then sync")
            .with_batch_size(2)
            .with_task(stream_task)
            .with_task(triple);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 4);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![30, 60, 90, 120]);
    }

    #[tokio::test]
    async fn test_async_stream_then_async() {
        let stream_task = Task::AsyncStream(Arc::new(|_input, _ctx| {
            let items = vec![5_i32, 15];
            Ok(
                Box::pin(futures::stream::iter(items).map(|i| Ok(Box::new(i) as Box<dyn Value>)))
                    as ValueStream,
            )
        }));

        // Each item is executed individually through the Async task.
        let add_one = Task::async_fn_typed(|x: &i32, _ctx| {
            let v = *x + 1;
            Box::pin(async move { Ok(Box::new(v)) })
        });

        let pipeline = Pipeline::new("async stream then async")
            .with_batch_size(10)
            .with_task(stream_task)
            .with_task(add_one);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 2);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![6, 16]);
    }

    #[tokio::test]
    async fn test_sync_then_sync() {
        let double = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 2))
        });
        let add_one = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x + 1))
        });

        let pipeline = Pipeline::new("sync then sync")
            .with_task(double)
            .with_task(add_one);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(3_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 7); // 3*2=6, 6+1=7
    }

    #[tokio::test]
    async fn test_sync_then_async() {
        let double = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 2))
        });
        let add_ten = Task::async_fn_typed(|x: &i32, _ctx| {
            let v = *x;
            Box::pin(async move { Ok(Box::new(v + 10)) })
        });

        let pipeline = Pipeline::new("sync then async")
            .with_task(double)
            .with_task(add_ten);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(5_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 20); // 5*2=10, 10+10=20
    }

    #[tokio::test]
    async fn test_async_then_sync() {
        let add_hundred = Task::async_fn_typed(|x: &i32, _ctx| {
            let v = *x;
            Box::pin(async move { Ok(Box::new(v + 100)) })
        });
        let double = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 2))
        });

        let pipeline = Pipeline::new("async then sync")
            .with_task(add_hundred)
            .with_task(double);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(3_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 206); // 3+100=103, 103*2=206
    }

    #[tokio::test]
    async fn test_async_then_async() {
        let triple = Task::async_fn_typed(|x: &i32, _ctx| {
            let v = *x;
            Box::pin(async move { Ok(Box::new(v * 3)) })
        });
        let add_one = Task::async_fn_typed(|x: &i32, _ctx| {
            let v = *x;
            Box::pin(async move { Ok(Box::new(v + 1)) })
        });

        let pipeline = Pipeline::new("async then async")
            .with_task(triple)
            .with_task(add_one);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(10_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 31); // 10*3=30, 30+1=31
    }

    #[tokio::test]
    async fn test_sync_iter_then_sync_batch() {
        use crate::task::ValueIter;

        // SyncIter yields [1, 2, 3, 4, 5].
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![1_i32, 2, 3, 4, 5];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // SyncBatch sums items in each batch.
        let sum_batch = Task::SyncBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let sum: i32 = items
                .iter()
                .map(|item| *(**item).as_any().downcast_ref::<i32>().unwrap())
                .sum();
            Ok(Arc::new(sum) as Arc<dyn Value>)
        }));

        let pipeline = Pipeline::new("sync iter then sync batch")
            .with_batch_size(2)
            .with_task(iter_task)
            .with_task(sum_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 3, "expected 3 batches: [1,2], [3,4], [5]");
        let sums: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(sums, vec![3, 7, 5]);
    }

    #[tokio::test]
    async fn test_sync_iter_then_async_batch() {
        use crate::task::ValueIter;

        // SyncIter yields [10, 20, 30].
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![10_i32, 20, 30];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // AsyncBatch returns the count of items in the batch.
        let count_batch = Task::AsyncBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let count = items.len() as i32;
            Box::pin(async move { Ok(Arc::new(count) as Arc<dyn Value>) })
        }));

        let pipeline = Pipeline::new("sync iter then async batch")
            .with_batch_size(2)
            .with_task(iter_task)
            .with_task(count_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 2, "expected 2 batches: [10,20], [30]");
        let counts: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(counts, vec![2, 1]);
    }

    #[tokio::test]
    async fn test_async_stream_then_sync_batch() {
        // AsyncStream yields [5, 10, 15, 20].
        let stream_task = Task::AsyncStream(Arc::new(|_input, _ctx| {
            let stream = futures::stream::iter(vec![5_i32, 10, 15, 20])
                .map(|i| Ok(Box::new(i) as Box<dyn Value>));
            Ok(Box::pin(stream) as ValueStream)
        }));

        // SyncBatch sums items.
        let sum_batch = Task::SyncBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let sum: i32 = items
                .iter()
                .map(|item| *(**item).as_any().downcast_ref::<i32>().unwrap())
                .sum();
            Ok(Arc::new(sum) as Arc<dyn Value>)
        }));

        let pipeline = Pipeline::new("async stream then sync batch")
            .with_batch_size(4)
            .with_task(stream_task)
            .with_task(sum_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 1, "expected 1 batch of all 4 items");
        let sum = *(*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(sum, 50);
    }

    #[tokio::test]
    async fn test_async_stream_then_async_batch() {
        // AsyncStream yields [1, 2, 3].
        let stream_task = Task::AsyncStream(Arc::new(|_input, _ctx| {
            let stream =
                futures::stream::iter(vec![1_i32, 2, 3]).map(|i| Ok(Box::new(i) as Box<dyn Value>));
            Ok(Box::pin(stream) as ValueStream)
        }));

        // AsyncBatch returns the product of items.
        let product_batch = Task::AsyncBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let product: i32 = items
                .iter()
                .map(|item| *(**item).as_any().downcast_ref::<i32>().unwrap())
                .product();
            Box::pin(async move { Ok(Arc::new(product) as Arc<dyn Value>) })
        }));

        let pipeline = Pipeline::new("async stream then async batch")
            .with_batch_size(3)
            .with_task(stream_task)
            .with_task(product_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 1, "expected 1 batch of all 3 items");
        let product = *(*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(product, 6);
    }

    #[tokio::test]
    async fn test_sync_iter_then_sync_iter_batch() {
        use crate::task::ValueIter;

        // SyncIter yields [1, 2, 3, 4].
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![1_i32, 2, 3, 4];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // SyncIterBatch doubles each item in the batch and yields them individually.
        let double_batch = Task::SyncIterBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let doubled: Vec<Box<dyn Value>> = items
                .iter()
                .map(|item| {
                    let val = *(**item).as_any().downcast_ref::<i32>().unwrap();
                    Box::new(val * 2) as Box<dyn Value>
                })
                .collect();
            Ok(Box::new(doubled.into_iter()) as ValueIter)
        }));

        let pipeline = Pipeline::new("sync iter then sync iter batch")
            .with_batch_size(2)
            .with_task(iter_task)
            .with_task(double_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 4);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![2, 4, 6, 8]);
    }

    #[tokio::test]
    async fn test_sync_iter_then_async_stream_batch() {
        use crate::task::ValueIter;

        // SyncIter yields [10, 20, 30].
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![10_i32, 20, 30];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // AsyncStreamBatch returns a stream of each item + 1.
        let add_one_batch = Task::AsyncStreamBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let results: Vec<Box<dyn Value>> = items
                .iter()
                .map(|item| {
                    let val = *(**item).as_any().downcast_ref::<i32>().unwrap();
                    Box::new(val + 1) as Box<dyn Value>
                })
                .collect();
            Ok(Box::pin(futures::stream::iter(results).map(Ok)) as ValueStream)
        }));

        let pipeline = Pipeline::new("sync iter then async stream batch")
            .with_batch_size(3)
            .with_task(iter_task)
            .with_task(add_one_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 3);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![11, 21, 31]);
    }

    #[tokio::test]
    async fn test_async_stream_then_sync_iter_batch() {
        use crate::task::ValueIter;

        // AsyncStream yields [5, 10].
        let stream_task = Task::AsyncStream(Arc::new(|_input, _ctx| {
            let stream =
                futures::stream::iter(vec![5_i32, 10]).map(|i| Ok(Box::new(i) as Box<dyn Value>));
            Ok(Box::pin(stream) as ValueStream)
        }));

        // SyncIterBatch triples each item.
        let triple_batch = Task::SyncIterBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let tripled: Vec<Box<dyn Value>> = items
                .iter()
                .map(|item| {
                    let val = *(**item).as_any().downcast_ref::<i32>().unwrap();
                    Box::new(val * 3) as Box<dyn Value>
                })
                .collect();
            Ok(Box::new(tripled.into_iter()) as ValueIter)
        }));

        let pipeline = Pipeline::new("async stream then sync iter batch")
            .with_batch_size(2)
            .with_task(stream_task)
            .with_task(triple_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 2);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![15, 30]);
    }

    #[tokio::test]
    async fn test_async_stream_then_async_stream_batch() {
        // AsyncStream yields [1, 2, 3].
        let stream_task = Task::AsyncStream(Arc::new(|_input, _ctx| {
            let stream =
                futures::stream::iter(vec![1_i32, 2, 3]).map(|i| Ok(Box::new(i) as Box<dyn Value>));
            Ok(Box::pin(stream) as ValueStream)
        }));

        // AsyncStreamBatch negates each item.
        let negate_batch = Task::AsyncStreamBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let results: Vec<Box<dyn Value>> = items
                .iter()
                .map(|item| {
                    let val = *(**item).as_any().downcast_ref::<i32>().unwrap();
                    Box::new(-val) as Box<dyn Value>
                })
                .collect();
            Ok(Box::pin(futures::stream::iter(results).map(Ok)) as ValueStream)
        }));

        let pipeline = Pipeline::new("async stream then async stream batch")
            .with_batch_size(2)
            .with_task(stream_task)
            .with_task(negate_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx().await;

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 3);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![-1, -2, -3]);
    }

    #[tokio::test]
    async fn test_sync_batch_terminal() {
        use crate::task::ValueIter;

        // SyncIter yields [1, 2, 3]
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![1_i32, 2, 3];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // SyncBatch (terminal) sums items in batch
        let sum_batch = Task::SyncBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let sum: i32 = items
                .iter()
                .map(|item| *(**item).as_any().downcast_ref::<i32>().unwrap())
                .sum();
            Ok(Arc::new(sum) as Arc<dyn Value>)
        }));

        let pipeline = Pipeline::new("sync batch terminal")
            .with_task(iter_task)
            .with_task(TaskInfo::new(sum_batch).with_batch_size(3));

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 6);
    }

    #[tokio::test]
    async fn test_async_batch_terminal() {
        use crate::task::ValueIter;

        // SyncIter yields [10, 20, 30, 40]
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![10_i32, 20, 30, 40];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // AsyncBatch (terminal) returns max of items
        let max_batch = Task::AsyncBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let max_val: i32 = items
                .iter()
                .map(|item| *(**item).as_any().downcast_ref::<i32>().unwrap())
                .max()
                .unwrap();
            Box::pin(async move { Ok(Arc::new(max_val) as Arc<dyn Value>) })
        }));

        let pipeline = Pipeline::new("async batch terminal")
            .with_task(iter_task)
            .with_task(TaskInfo::new(max_batch).with_batch_size(2));

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(outputs.len(), 2);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![20, 40]);
    }

    #[tokio::test]
    async fn test_sync_iter_batch_terminal() {
        use crate::task::ValueIter;

        // SyncIter yields [1, 2, 3]
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![1_i32, 2, 3];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // SyncIterBatch (terminal) doubles each item
        let double_batch = Task::SyncIterBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let doubled: Vec<Box<dyn Value>> = items
                .iter()
                .map(|item| {
                    let val = *(**item).as_any().downcast_ref::<i32>().unwrap();
                    Box::new(val * 2) as Box<dyn Value>
                })
                .collect();
            Ok(Box::new(doubled.into_iter()) as ValueIter)
        }));

        let pipeline = Pipeline::new("sync iter batch terminal")
            .with_task(iter_task)
            .with_task(TaskInfo::new(double_batch).with_batch_size(3));

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(outputs.len(), 3);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![2, 4, 6]);
    }

    #[tokio::test]
    async fn test_async_stream_batch_terminal() {
        use crate::task::ValueIter;

        // SyncIter yields [5, 10]
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let vec = vec![5_i32, 10];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // AsyncStreamBatch (terminal) negates each item
        let negate_batch = Task::AsyncStreamBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let negated: Vec<i32> = items
                .iter()
                .map(|item| {
                    let val = *(**item).as_any().downcast_ref::<i32>().unwrap();
                    -val
                })
                .collect();
            Ok(
                Box::pin(futures::stream::iter(negated).map(|i| Ok(Box::new(i) as Box<dyn Value>)))
                    as ValueStream,
            )
        }));

        let pipeline = Pipeline::new("async stream batch terminal")
            .with_task(iter_task)
            .with_task(TaskInfo::new(negate_batch).with_batch_size(2));

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(outputs.len(), 2);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![-5, -10]);
    }

    #[tokio::test]
    async fn test_sync_then_sync_iter_then_sync_batch() {
        use crate::task::ValueIter;

        // T1: Sync doubles input i32 (5 -> 10)
        let double = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 2))
        });

        // T2: SyncIter receives value and yields [value, value+1, value+2]
        let expand = Task::SyncIter(Arc::new(|input, _ctx| {
            let val = *(*input).as_any().downcast_ref::<i32>().unwrap();
            let vec: Vec<i32> = vec![val, val + 1, val + 2];
            Ok(Box::new(vec.into_iter().map(|i| Box::new(i) as Box<dyn Value>)) as ValueIter)
        }));

        // T3: SyncBatch sums the items in the batch
        let sum_batch = Task::SyncBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let sum: i32 = items
                .iter()
                .map(|item| *(**item).as_any().downcast_ref::<i32>().unwrap())
                .sum();
            Ok(Arc::new(sum) as Arc<dyn Value>)
        }));

        let pipeline = Pipeline::new("sync -> sync_iter -> sync_batch")
            .with_batch_size(2)
            .with_task(double)
            .with_task(expand)
            .with_task(sum_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(5_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        // T1: 5 -> 10
        // T2: 10 -> [10, 11, 12]
        // T3 with batch_size=2: [10,11] -> 21, [12] -> 12
        assert_eq!(outputs.len(), 2);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![21, 12]);
    }

    #[tokio::test]
    async fn test_sync_iter_then_sync_batch_then_sync() {
        use crate::task::ValueIter;

        // T1: SyncIter yields [1, 2, 3, 4]
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let iter = (1..=4).map(|i| Box::new(i) as Box<dyn Value>);
            Ok(Box::new(iter) as ValueIter)
        }));

        // T2: SyncBatch sums items -> single value
        let sum_batch = Task::SyncBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let sum: i32 = items
                .iter()
                .map(|item| *(**item).as_any().downcast_ref::<i32>().unwrap())
                .sum();
            Ok(Arc::new(sum) as Arc<dyn Value>)
        }));

        // T3: Sync doubles the value
        let double = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 2))
        });

        let pipeline = Pipeline::new("sync_iter -> sync_batch -> sync")
            .with_batch_size(2)
            .with_task(iter_task)
            .with_task(sum_batch)
            .with_task(double);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        // T1: [1, 2, 3, 4]
        // T2 with batch_size=2: [1,2] -> sum=3, [3,4] -> sum=7
        // T3: 3 -> 6, 7 -> 14
        assert_eq!(outputs.len(), 2);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![6, 14]);
    }

    #[tokio::test]
    async fn test_sync_iter_then_sync_batch_then_sync_iter() {
        use crate::task::ValueIter;

        // T1: SyncIter yields [1, 2, 3]
        let iter_task = Task::SyncIter(Arc::new(|_input, _ctx| {
            let iter = (1..=3).map(|i| Box::new(i) as Box<dyn Value>);
            Ok(Box::new(iter) as ValueIter)
        }));

        // T2: SyncBatch sums items -> single value
        let sum_batch = Task::SyncBatch(Arc::new(|items: &[Box<dyn Value>], _ctx| {
            let sum: i32 = items
                .iter()
                .map(|item| *(**item).as_any().downcast_ref::<i32>().unwrap())
                .sum();
            Ok(Arc::new(sum) as Arc<dyn Value>)
        }));

        // T3: SyncIter takes sum and yields [sum, sum+1]
        let re_expand = Task::SyncIter(Arc::new(|input, _ctx| {
            let val = *(*input).as_any().downcast_ref::<i32>().unwrap();
            let iter = (0..2).map(move |i| Box::new(val + i) as Box<dyn Value>);
            Ok(Box::new(iter) as ValueIter)
        }));

        let pipeline = Pipeline::new("sync_iter -> sync_batch -> sync_iter")
            .with_batch_size(3)
            .with_task(iter_task)
            .with_task(sum_batch)
            .with_task(re_expand);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        // T1: [1, 2, 3]
        // T2 with batch_size=3: [1,2,3] -> sum=6
        // T3: 6 -> [6, 7]
        assert_eq!(outputs.len(), 2);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![6, 7]);
    }

    #[tokio::test]
    async fn test_pipeline_progress_with_weights() {
        use crate::progress::ProgressToken;
        use crate::task::TaskInfo;

        let progress = ProgressToken::new();
        let (_handle, token) = cancellation_pair();
        let db = cognee_database::connect("sqlite::memory:").await.unwrap();
        cognee_database::initialize(&db).await.unwrap();
        let ctx = Arc::new(TaskContext {
            thread_pool: Arc::new(StubPool),
            database: Arc::new(db),
            graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
            vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
            cancellation: token,
            progress: progress.clone(),
            pipeline_ctx: None,
            exec_status: Arc::new(NoopExecStatusManager),
            pipeline_watcher: None,
        });

        // weight 1 (25%) and weight 3 (75%)
        let task1 = TaskInfo::new(Task::sync_typed(|x: &i32, ctx| {
            ctx.progress.set(0.5);
            Ok(Box::new(*x))
        }))
        .with_weight(1);

        let task2 =
            TaskInfo::new(Task::sync_typed(|x: &i32, _ctx| Ok(Box::new(*x)))).with_weight(3);

        let pipeline = Pipeline::new("progress test")
            .with_task(task1)
            .with_task(task2);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(42_i32)];
        let _ = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        // After completion, both tasks are set to 1.0 by the executor
        assert!((progress.root_fraction() - 1.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_pipeline_builder_typed_chain() {
        // String → usize (len) → String (formatted)
        let t1 = TypedTask::sync(|s: &String, _| Ok(Box::new(s.len())));
        let t2 = TypedTask::sync(|n: &usize, _| Ok(Box::new(format!("len={n}"))));

        let pipeline = PipelineBuilder::new_with_task("typed chain", t1)
            .add_task(t2)
            .build();

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new("hello".to_string())];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(outputs.len(), 1);
        let s = (*outputs[0]).as_any().downcast_ref::<String>().unwrap();
        assert_eq!(s, "len=5");
    }

    #[tokio::test]
    async fn test_pipeline_builder_config_forwarded() {
        let t1 = TypedTask::sync(|x: &i32, _| Ok(Box::new(*x * 2)));
        let pipeline = PipelineBuilder::new_with_task("cfg test", t1)
            .with_name("my pipeline")
            .with_batch_size(8)
            .with_concurrency(2)
            .build();

        assert_eq!(pipeline.name.as_deref(), Some("my pipeline"));
        assert_eq!(pipeline.batch_size, 8);
        assert_eq!(pipeline.concurrency, 2);
    }

    #[test]
    fn test_typed_task_into_task_info() {
        let typed: TypedTask<i32, i32> = TypedTask::sync(|x: &i32, _| Ok(Box::new(*x)));
        let info: TaskInfo = typed.into();
        // Default TaskInfo fields
        assert!(info.name.is_none());
        assert!(info.batch_size.is_none());
        assert_eq!(info.weight, 1);
    }

    #[tokio::test]
    async fn test_typed_task_into_untyped_pipeline() {
        // TypedTask implements Into<TaskInfo>, so it drops into Pipeline::with_task directly.
        let typed: TypedTask<i32, i32> = TypedTask::sync(|x: &i32, _| Ok(Box::new(*x + 10)));
        let pipeline = Pipeline::new("escape hatch").with_task(typed);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(5_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx().await, &NoopWatcher)
            .await
            .unwrap();

        let v = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*v, 15);
    }

    // ── Retention characteristics of the per-item context ────────────────────

    /// A payload that records when it is dropped, so a test can observe
    /// whether the executor is still holding the pipeline input.
    struct TrackedPayload {
        _bytes: Vec<u8>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Drop for TrackedPayload {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Pinning `current_data` per item necessarily keeps the pipeline input
    /// alive for the whole task chain, even once no task needs it any more.
    ///
    /// This is deliberate and matches the reference implementation: Python
    /// builds one `PipelineContext(data_item=data_item, ...)` per item in
    /// `run_tasks.py` and holds it across the entire per-item run, so
    /// `ctx.data_item` is reachable from the last task there too.
    ///
    /// The cost is one extra live copy of the input value per in-flight item
    /// (so × `concurrency`). It matters for pipelines whose *input value*
    /// owns a large buffer rather than a reference to one — e.g.
    /// `DataInput::Binary { data: Vec<u8>, .. }`, which the bindings marshal
    /// into. Such pipelines should stream rather than materialise the payload
    /// in the input value; the executor cannot release it early without
    /// giving up the documented `current_data` semantics.
    #[tokio::test]
    async fn per_item_context_keeps_the_input_alive_for_the_whole_chain() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Task 0 consumes the payload and returns something small, so nothing
        // downstream carries the payload forward. The only remaining strong
        // reference is the one the per-item context holds.
        let consume = TypedTask::<TrackedPayload, i32>::sync(|p: &TrackedPayload, _ctx| {
            Ok(Box::new(p._bytes.len() as i32))
        });

        // Task 1 observes whether the payload is still alive.
        let seen = Arc::new(std::sync::Mutex::new(None::<bool>));
        let seen_in_task = Arc::clone(&seen);
        let dropped_in_task = Arc::clone(&dropped);
        let observe = TypedTask::<i32, i32>::sync(move |n: &i32, ctx| {
            let already_dropped = dropped_in_task.load(std::sync::atomic::Ordering::SeqCst);
            // lock poison is unrecoverable
            *seen_in_task.lock().unwrap() = Some(already_dropped);
            // The value is not merely alive, it is reachable as the original item.
            let cd = ctx
                .pipeline()
                .current_data
                .clone()
                .expect("current_data is pinned per item by execute_one_item");
            assert!(
                (*cd).as_any().downcast_ref::<TrackedPayload>().is_some(),
                "current_data should still be the original TrackedPayload"
            );
            Ok(Box::new(*n))
        });

        let pipeline = PipelineBuilder::new_with_task("retention", consume)
            .add_task(observe)
            .with_name("retention")
            .build();

        let input: Arc<dyn Value> = Arc::new(TrackedPayload {
            _bytes: vec![0_u8; 4096],
            dropped: Arc::clone(&dropped),
        });

        let ctx = stub_ctx_with_pipeline("retention").await;
        let watcher = NoopWatcher;
        execute(&pipeline, vec![input], ctx, &watcher)
            .await
            .unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            Some(false),
            "the input must still be alive while a later task runs — that is \
             what pinning current_data per item means"
        );
    }

    // ── Executor services on the batch-dispatch path ─────────────────────────
    //
    // `dispatch_batch` calls a `*Batch` task directly, bypassing
    // `call_with_retry`. Three services used to be lost with it — rate limiting,
    // progress and cancellation — and are now provided on this path. Each test
    // below fails before the corresponding fix.
    //
    // All three build a pipeline whose *terminal* task is a batch variant,
    // because that is the shape that hid the cancellation bug:
    // `execute_from(rest=[], ..)` returns via its empty-tasks base case before
    // reaching the only `is_cancelled()` check.

    /// Counts `acquire()` calls so a test can assert the batch path throttles.
    struct CountingLimiter(Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait]
    impl crate::rate_limiter::RateLimiter for CountingLimiter {
        async fn acquire(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Build a ctx with a pipeline context, returning the cancellation handle so
    /// the caller can cancel mid-run. `stub_ctx_with_pipeline` drops its handle.
    async fn cancellable_ctx(
        name: &str,
    ) -> (crate::cancellation::CancellationHandle, Arc<TaskContext>) {
        let base = stub_ctx_with_pipeline(name).await;
        let (handle, token) = cancellation_pair();
        let ctx = Arc::new(TaskContext {
            thread_pool: Arc::clone(&base.thread_pool),
            database: Arc::clone(&base.database),
            graph_db: Arc::clone(&base.graph_db),
            vector_db: Arc::clone(&base.vector_db),
            cancellation: token,
            progress: base.progress.clone(),
            pipeline_ctx: base.pipeline_ctx.clone(),
            exec_status: Arc::clone(&base.exec_status),
            pipeline_watcher: None,
        });
        (handle, ctx)
    }

    /// An iterator task that fans `n` items out to a terminal batch task.
    fn fan_out_task(n: i32) -> Task {
        Task::sync_iter_typed(move |_: &i32, _ctx| Ok((0..n).map(Box::new)))
    }

    #[tokio::test]
    async fn cancellation_stops_a_terminal_batch_task() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_in_task = Arc::clone(&calls);
        let handle_slot: Arc<std::sync::Mutex<Option<crate::cancellation::CancellationHandle>>> =
            Arc::new(std::sync::Mutex::new(None));
        let handle_in_task = Arc::clone(&handle_slot);

        // Cancels the run from inside its first invocation, then returns Ok — so
        // nothing but the cancellation check can stop the remaining batches.
        let collect = Task::sync_batch(
            move |items: &[Box<dyn Value>],
                  _ctx: Arc<TaskContext>|
                  -> Result<Arc<dyn Value>, TaskError> {
                calls_in_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // lock poison is unrecoverable
                if let Some(h) = handle_in_task.lock().unwrap().as_ref() {
                    h.cancel();
                }
                Ok(Arc::new(items.len() as i32) as Arc<dyn Value>)
            },
        );

        // 6 items at batch_size 2 → 3 batches if nothing stops it.
        let pipeline = Pipeline::new("cancel-batch")
            .with_name("cancel-batch")
            .with_task(TaskInfo::new(fan_out_task(6)))
            .with_task(TaskInfo::new(collect).with_batch_size(2));

        let (handle, ctx) = cancellable_ctx("cancel-batch").await;
        // lock poison is unrecoverable
        *handle_slot.lock().unwrap() = Some(handle);

        let input: Arc<dyn Value> = Arc::new(0_i32);
        let result = execute(&pipeline, vec![input], ctx, &NoopWatcher).await;

        assert!(
            matches!(result, Err(ExecutionError::Cancelled)),
            "a cancelled run whose terminal task is a batch variant must report \
             Cancelled, not Ok/Completed — got {:?}",
            result.map(|o| o.len())
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the batch task must not be invoked again after cancellation"
        );
    }

    /// A limiter that parks until released, so a test can cancel *during*
    /// acquisition — the window a pre-acquire-only check leaves open.
    struct BlockingLimiter {
        release: Arc<tokio::sync::Notify>,
        entered: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl crate::rate_limiter::RateLimiter for BlockingLimiter {
        async fn acquire(&self) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    /// `with_batch_size` is documented to size the upstream accumulation buffer
    /// whatever kind the consumer is — the doc used to claim it mattered only for
    /// `*Batch` variants, which is why this is pinned.
    ///
    /// A non-batch consumer is invoked once per item, so the buffer size is not
    /// visible in its arguments. It *is* visible in the interleaving: with
    /// `batch_size` N the producer is pulled N times before any consumption. The
    /// pipeline default is 32, so if the per-task override were ignored all six
    /// items would be buffered first and the log would be `PPPPPPCCCCCC`.
    #[tokio::test]
    async fn batch_size_sizes_the_buffer_for_a_non_batch_consumer() {
        let log = Arc::new(std::sync::Mutex::new(String::new()));

        let produce_log = Arc::clone(&log);
        let producer = Task::sync_iter_typed(move |_: &i32, _ctx| {
            let produce_log = Arc::clone(&produce_log);
            Ok((0..6).map(move |i| {
                // lock poison is unrecoverable
                produce_log.lock().unwrap().push('P');
                Box::new(i)
            }))
        });

        let consume_log = Arc::clone(&log);
        let consumer = Task::sync_typed(move |i: &i32, _ctx| {
            // lock poison is unrecoverable
            consume_log.lock().unwrap().push('C');
            Ok(Box::new(*i))
        });

        let pipeline = Pipeline::new("buffer-size")
            .with_name("buffer-size")
            .with_task(TaskInfo::new(producer))
            // Non-batch consumer, per-task override of the 32-item default.
            .with_task(TaskInfo::new(consumer).with_batch_size(2));

        let ctx = stub_ctx_with_pipeline("buffer-size").await;
        let input: Arc<dyn Value> = Arc::new(0_i32);
        execute(&pipeline, vec![input], ctx, &NoopWatcher)
            .await
            .unwrap();

        // lock poison is unrecoverable
        let observed = log.lock().unwrap().clone();
        assert_eq!(
            observed, "PPCCPPCCPPCC",
            "batch_size 2 must buffer two items at a time before handing them to \
             a non-batch consumer; got {observed}"
        );
    }

    #[tokio::test]
    async fn cancellation_during_rate_limiter_wait_stops_the_batch() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_in_task = Arc::clone(&calls);
        let release = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());

        let limiter: Arc<dyn crate::rate_limiter::RateLimiter> = Arc::new(BlockingLimiter {
            release: Arc::clone(&release),
            entered: Arc::clone(&entered),
        });

        let collect = Task::sync_batch(
            move |items: &[Box<dyn Value>],
                  _ctx: Arc<TaskContext>|
                  -> Result<Arc<dyn Value>, TaskError> {
                calls_in_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Arc::new(items.len() as i32) as Arc<dyn Value>)
            },
        );

        let pipeline = Pipeline::new("cancel-in-acquire")
            .with_name("cancel-in-acquire")
            .with_task(TaskInfo::new(fan_out_task(2)))
            .with_task(
                TaskInfo::new(collect)
                    .with_batch_size(2)
                    .with_rate_limiter(limiter),
            );

        let (handle, ctx) = cancellable_ctx("cancel-in-acquire").await;
        let input: Arc<dyn Value> = Arc::new(0_i32);

        // Cancel only once the limiter has parked, so the pre-acquire check has
        // already passed and only a post-acquire check can catch it.
        let canceller = tokio::spawn({
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            async move {
                entered.notified().await;
                handle.cancel();
                release.notify_one();
            }
        });

        let result = execute(&pipeline, vec![input], ctx, &NoopWatcher).await;
        canceller.await.unwrap();

        assert!(
            matches!(result, Err(ExecutionError::Cancelled)),
            "cancellation arriving while the rate limiter was parked must stop \
             the batch — got {:?}",
            result.map(|o| o.len())
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the batch task must not run when cancelled during acquisition"
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_stalled_stream() {
        // A stream that yields one item then never yields again. Without racing
        // cancellation against `next()`, a cancelled run parks here forever.
        let producer = Task::async_stream_typed(|_: &i32, _ctx| {
            let s =
                futures::stream::once(async { Box::new(1_i32) }).chain(futures::stream::pending());
            Ok(s)
        });

        let collect = Task::sync_batch(
            |items: &[Box<dyn Value>],
             _ctx: Arc<TaskContext>|
             -> Result<Arc<dyn Value>, TaskError> {
                Ok(Arc::new(items.len() as i32) as Arc<dyn Value>)
            },
        );

        // batch_size 2 with only one item ever produced: the loop is parked on
        // `next()` when cancellation arrives, having dispatched nothing.
        let pipeline = Pipeline::new("stalled-stream")
            .with_name("stalled-stream")
            .with_task(TaskInfo::new(producer))
            .with_task(TaskInfo::new(collect).with_batch_size(2));

        let (handle, ctx) = cancellable_ctx("stalled-stream").await;
        let input: Arc<dyn Value> = Arc::new(0_i32);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            handle.cancel();
        });

        // The timeout is the assertion: before the fix this never returns.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            execute(&pipeline, vec![input], ctx, &NoopWatcher),
        )
        .await
        .expect("a cancelled run must not hang on a stalled stream");

        assert!(
            matches!(result, Err(ExecutionError::Cancelled)),
            "expected Cancelled, got {:?}",
            result.map(|o| o.len())
        );
    }

    #[tokio::test]
    async fn batch_calls_acquire_the_rate_limiter() {
        let acquires = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let limiter: Arc<dyn crate::rate_limiter::RateLimiter> =
            Arc::new(CountingLimiter(Arc::clone(&acquires)));

        let collect = Task::sync_batch(
            |items: &[Box<dyn Value>],
             _ctx: Arc<TaskContext>|
             -> Result<Arc<dyn Value>, TaskError> {
                Ok(Arc::new(items.len() as i32) as Arc<dyn Value>)
            },
        );

        // Limiter set on the batch task only, so the producer's own acquisition
        // in `call_with_retry` cannot be mistaken for the batch path working.
        // 6 items at batch_size 2 → exactly 3 batch calls.
        let pipeline = Pipeline::new("rl-batch")
            .with_name("rl-batch")
            .with_task(TaskInfo::new(fan_out_task(6)))
            .with_task(
                TaskInfo::new(collect)
                    .with_batch_size(2)
                    .with_rate_limiter(limiter),
            );

        let ctx = stub_ctx_with_pipeline("rl-batch").await;
        let input: Arc<dyn Value> = Arc::new(0_i32);
        execute(&pipeline, vec![input], ctx, &NoopWatcher)
            .await
            .unwrap();

        assert_eq!(
            acquires.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "the batch task's limiter must be acquired once per batch call"
        );
    }

    #[tokio::test]
    async fn batch_task_progress_reaches_completion() {
        let collect = Task::sync_batch(
            |items: &[Box<dyn Value>],
             _ctx: Arc<TaskContext>|
             -> Result<Arc<dyn Value>, TaskError> {
                Ok(Arc::new(items.len() as i32) as Arc<dyn Value>)
            },
        );

        let pipeline = Pipeline::new("progress-batch")
            .with_name("progress-batch")
            .with_task(TaskInfo::new(fan_out_task(6)))
            .with_task(TaskInfo::new(collect).with_batch_size(2));

        let ctx = stub_ctx_with_pipeline("progress-batch").await;
        let root = ctx.progress.clone();
        let input: Arc<dyn Value> = Arc::new(0_i32);
        execute(&pipeline, vec![input], ctx, &NoopWatcher)
            .await
            .unwrap();

        // Two equally-weighted tasks: without the fix the batch task's half is
        // never completed and this sticks at 0.5 after a successful run.
        assert_eq!(
            root.root_fraction(),
            1.0,
            "a successful run must report full progress; the batch consumer's \
             slice was left incomplete"
        );
    }

    #[tokio::test]
    async fn batch_task_gets_its_own_progress_subtoken() {
        // The batch task must receive a *usable* token: reporting into the root
        // context was a silent no-op because `split` zeroes the root's width.
        let observed_width = Arc::new(std::sync::Mutex::new(Vec::<f64>::new()));
        let widths = Arc::clone(&observed_width);

        let collect = Task::sync_batch(
            move |items: &[Box<dyn Value>],
                  ctx: Arc<TaskContext>|
                  -> Result<Arc<dyn Value>, TaskError> {
                // lock poison is unrecoverable
                widths.lock().unwrap().push(ctx.progress.width());
                Ok(Arc::new(items.len() as i32) as Arc<dyn Value>)
            },
        );

        let pipeline = Pipeline::new("subtoken-batch")
            .with_name("subtoken-batch")
            .with_task(TaskInfo::new(fan_out_task(4)))
            .with_task(TaskInfo::new(collect).with_batch_size(2));

        let ctx = stub_ctx_with_pipeline("subtoken-batch").await;
        let input: Arc<dyn Value> = Arc::new(0_i32);
        execute(&pipeline, vec![input], ctx, &NoopWatcher)
            .await
            .unwrap();

        // lock poison is unrecoverable
        let widths = observed_width.lock().unwrap().clone();
        assert!(!widths.is_empty(), "the batch task should have run");
        for w in widths {
            assert!(
                w > 0.0,
                "the batch task was handed a zero-width token, so its own \
                 progress reports cannot land anywhere"
            );
        }
    }
}
