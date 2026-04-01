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
use crate::task::{TaggedMeta, Task, TaskCall, TaskError, TaskInfo, Value, ValueIter, ValueStream};
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
    /// Dataset being processed.
    pub dataset_id: Option<Uuid>,
    /// Current run status.
    pub status: PipelineRunStatus,
    /// When the run was initiated.
    pub started_at: chrono::DateTime<chrono::Utc>,
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
    let dataset_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.dataset_id);
    let pipeline_id = deterministic_pipeline_id(pipeline.name.as_deref(), user_id, dataset_id)
        .unwrap_or(pipeline.id);

    let mut run_info = PipelineRunInfo {
        run_id,
        pipeline_id,
        pipeline_name: pipeline.name.clone().unwrap_or_default(),
        user_id,
        dataset_id,
        status: PipelineRunStatus::Started,
        started_at: chrono::Utc::now(),
    };

    watcher
        .on_pipeline(pipeline_id, PipelineStatus::Started { task_count })
        .await;
    watcher.on_pipeline_run_started(&run_info).await;

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
    };

    let result = if pipeline.concurrency <= 1 {
        execute_items_seq(inputs, pipeline, &ctx, &env).await
    } else {
        execute_items_par(inputs, pipeline, &ctx, &env).await
    };

    match &result {
        Ok(outputs) => {
            run_info.status = PipelineRunStatus::Completed;
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
        }
        Err(ExecutionError::Cancelled) => {
            run_info.status = PipelineRunStatus::Errored;
            watcher
                .on_pipeline(pipeline_id, PipelineStatus::Cancelled)
                .await;
            watcher
                .on_pipeline_run_errored(&run_info, "pipeline was cancelled")
                .await;
        }
        Err(e) => {
            run_info.status = PipelineRunStatus::Errored;
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

    let result = execute_from(&pipeline.tasks, input, 0, env).await;

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

        let resolved = call_with_retry(
            &info.task,
            input,
            first_index,
            task_name,
            data_id.as_deref(),
            info.summary_template.as_deref(),
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
            Resolved::Single(v) => execute_from(rest, v, first_index + 1, env).await,
            Resolved::Iter(iter) => {
                process_iter(iter, rest, batch_size, first_index + 1, env).await
            }
            Resolved::Stream(stream) => {
                process_stream(stream, rest, batch_size, first_index + 1, env).await
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
/// **Design note:** batch-dispatched tasks bypass [`call_with_retry`] — there
/// are no retries, no per-task watcher events, and no provenance stamping.
/// Batch tasks receive pre-accumulated slices and are expected to handle their
/// own error semantics. Only single-value tasks executed via [`execute_from`]
/// get the full retry / watcher / provenance treatment.
fn dispatch_batch<'a>(
    batch: Vec<Box<dyn Value>>,
    tail: &'a [TaskInfo],
    first_index: usize,
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
            // Call the batch task directly with the accumulated slice.
            let call = next_info.task.call_batch(&batch, env.ctx.clone());
            let resolved =
                resolve_call(call)
                    .await
                    .map_err(|source| ExecutionError::TaskFailed {
                        task_index: first_index,
                        attempts: 1,
                        source,
                    })?;
            // After the batch call resolves, continue through the remaining tail.
            let rest = &tail[1..];
            match resolved {
                Resolved::Single(v) => execute_from(rest, v, first_index + 1, env).await,
                Resolved::Iter(iter) => {
                    let batch_size = rest
                        .first()
                        .and_then(|t| t.batch_size)
                        .unwrap_or(env.default_batch_size);
                    process_iter(iter, rest, batch_size, first_index + 1, env).await
                }
                Resolved::Stream(stream) => {
                    let batch_size = rest
                        .first()
                        .and_then(|t| t.batch_size)
                        .unwrap_or(env.default_batch_size);
                    process_stream(stream, rest, batch_size, first_index + 1, env).await
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
async fn process_iter(
    iter: ValueIter,
    tail: &[TaskInfo],
    batch_size: usize,
    first_index: usize,
    env: &ExecEnv<'_>,
) -> Result<Vec<Arc<dyn Value>>, ExecutionError> {
    let mut outputs = Vec::new();
    let mut batch: Vec<Box<dyn Value>> = Vec::with_capacity(batch_size);

    for item in iter {
        batch.push(item);
        if batch.len() >= batch_size {
            outputs
                .append(&mut dispatch_batch(mem::take(&mut batch), tail, first_index, env).await?);
        }
    }

    if !batch.is_empty() {
        outputs.append(&mut dispatch_batch(batch, tail, first_index, env).await?);
    }

    Ok(outputs)
}

/// Gather items from an async stream in `batch_size` chunks, run the tail
/// pipeline on each full chunk (waiting for it to finish) before pulling the
/// next chunk.
async fn process_stream(
    mut stream: ValueStream,
    tail: &[TaskInfo],
    batch_size: usize,
    first_index: usize,
    env: &ExecEnv<'_>,
) -> Result<Vec<Arc<dyn Value>>, ExecutionError> {
    let mut outputs = Vec::new();
    let mut batch: Vec<Box<dyn Value>> = Vec::with_capacity(batch_size);

    while let Some(item) = stream.next().await {
        batch.push(item);
        if batch.len() >= batch_size {
            outputs
                .append(&mut dispatch_batch(mem::take(&mut batch), tail, first_index, env).await?);
        }
    }

    if !batch.is_empty() {
        outputs.append(&mut dispatch_batch(batch, tail, first_index, env).await?);
    }

    Ok(outputs)
}
/// Call `task` on `input`, retrying on failure according to `env.policy`.
///
/// Retry applies to the task call itself (including awaiting async tasks and
/// setting up iterators / streams).  Individual items emitted by an already-
/// initialised iterator or stream are not retried.
async fn call_with_retry(
    task: &Task,
    input: Arc<dyn Value>,
    task_index: usize,
    task_name: Option<&str>,
    data_id: Option<&str>,
    #[allow(unused_variables)] summary_template: Option<&str>,
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

    // Inject the task-specific progress subtoken and current data.
    let subtoken = env.task_subtokens[task_index].clone();
    let scoped_ctx = env.ctx.with_progress(subtoken);
    let task_ctx = scoped_ctx.with_current_data(input.clone());

    for attempt in 1..=max_attempts {
        let call = task.call(input.clone(), Arc::clone(&task_ctx));
        match resolve_call(call).await {
            Ok(resolved) => {
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

    fn stub_ctx() -> Arc<TaskContext> {
        let (_handle, token) = cancellation_pair();
        Arc::new(TaskContext {
            thread_pool: Arc::new(StubPool),
            database: Arc::new(cognee_database::MockDatabase::new()),
            graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
            vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
            cancellation: token,
            progress: ProgressToken::new(),
            pipeline_ctx: None,
            exec_status: Arc::new(NoopExecStatusManager),
        })
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
        let ctx = stub_ctx();
        let watcher = NoopWatcher;

        let outputs = execute(&pipeline, inputs, ctx, &watcher).await.unwrap();

        assert_eq!(outputs.len(), 1);
        let result = (*outputs[0]).as_any().downcast_ref::<i32>().unwrap();
        assert_eq!(*result, 42);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_execute_single_task_pipeline() {
        let double = Task::sync_typed(|x: &i32, _ctx| -> Result<Box<i32>, TaskError> {
            Ok(Box::new(*x * 2))
        });

        let pipeline = Pipeline::new("double pipeline").with_task(double);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(5_i32)];
        let ctx = stub_ctx();
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
        let ctx = stub_ctx();
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
        let ctx = stub_ctx();
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

        let (handle, token) = cancellation_pair();
        let ctx = Arc::new(TaskContext {
            thread_pool: Arc::new(StubPool),
            database: Arc::new(cognee_database::MockDatabase::new()),
            graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
            vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
            cancellation: token,
            progress: ProgressToken::new(),
            pipeline_ctx: None,
            exec_status: Arc::new(NoopExecStatusManager),
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
        let ctx = stub_ctx();

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
        let ctx = stub_ctx();

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
        let ctx = stub_ctx();

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
        let ctx = stub_ctx();

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
        let ctx = stub_ctx();

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
                Box::pin(futures::stream::iter(items).map(|i| Box::new(i) as Box<dyn Value>))
                    as ValueStream,
            )
        }));

        let pipeline = Pipeline::new("async stream terminal").with_task(stream_task);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx();

        let outputs = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();

        assert_eq!(outputs.len(), 3);
        let values: Vec<i32> = outputs
            .iter()
            .map(|v| *(**v).as_any().downcast_ref::<i32>().unwrap())
            .collect();
        assert_eq!(values, vec![100, 200, 300]);
    }

    #[tokio::test]
    async fn test_async_stream_then_sync() {
        let stream_task = Task::AsyncStream(Arc::new(|_input, _ctx| {
            let items = vec![10_i32, 20, 30, 40];
            Ok(
                Box::pin(futures::stream::iter(items).map(|i| Box::new(i) as Box<dyn Value>))
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
        let ctx = stub_ctx();

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
                Box::pin(futures::stream::iter(items).map(|i| Box::new(i) as Box<dyn Value>))
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
        let ctx = stub_ctx();

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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let ctx = stub_ctx();

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
        let ctx = stub_ctx();

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
                .map(|i| Box::new(i) as Box<dyn Value>);
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
        let ctx = stub_ctx();

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
                futures::stream::iter(vec![1_i32, 2, 3]).map(|i| Box::new(i) as Box<dyn Value>);
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
        let ctx = stub_ctx();

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
        let ctx = stub_ctx();

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
            Ok(Box::pin(futures::stream::iter(results)) as ValueStream)
        }));

        let pipeline = Pipeline::new("sync iter then async stream batch")
            .with_batch_size(3)
            .with_task(iter_task)
            .with_task(add_one_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx();

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
                futures::stream::iter(vec![5_i32, 10]).map(|i| Box::new(i) as Box<dyn Value>);
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
        let ctx = stub_ctx();

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
                futures::stream::iter(vec![1_i32, 2, 3]).map(|i| Box::new(i) as Box<dyn Value>);
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
            Ok(Box::pin(futures::stream::iter(results)) as ValueStream)
        }));

        let pipeline = Pipeline::new("async stream then async stream batch")
            .with_batch_size(2)
            .with_task(stream_task)
            .with_task(negate_batch);

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let ctx = stub_ctx();

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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
                Box::pin(futures::stream::iter(negated).map(|i| Box::new(i) as Box<dyn Value>))
                    as ValueStream,
            )
        }));

        let pipeline = Pipeline::new("async stream batch terminal")
            .with_task(iter_task)
            .with_task(TaskInfo::new(negate_batch).with_batch_size(2));

        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(0_i32)];
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let outputs = execute(&pipeline, inputs, stub_ctx(), &NoopWatcher)
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
        let ctx = Arc::new(TaskContext {
            thread_pool: Arc::new(StubPool),
            database: Arc::new(cognee_database::MockDatabase::new()),
            graph_db: Arc::new(cognee_graph::MockGraphDB::new()),
            vector_db: Arc::new(cognee_vector::MockVectorDB::new()),
            cancellation: token,
            progress: progress.clone(),
            pipeline_ctx: None,
            exec_status: Arc::new(NoopExecStatusManager),
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
}
