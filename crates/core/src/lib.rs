//! Core runtime primitives for the cognee pipeline.
//!
//! This crate provides:
//!
//! - [`AsyncRuntime`] — a thin wrapper around a Tokio [`tokio::runtime::Runtime`].
//! - [`RayonThreadPool`] / [`CpuPool`] / [`CpuPoolExt`] — a Rayon-backed CPU thread pool
//!   with an async interface for offloading CPU-intensive work from Tokio workers.
//! - [`CancellationHandle`] / [`CancellationToken`] — cooperative task cancellation via
//!   a `tokio::sync::watch` channel pair.
//! - [`ProgressToken`] — a lock-free, clone-able progress counter.
//! - [`TaskContext`] / [`TaskContextBuilder`] — a bundle of all runtime services
//!   (thread pool, databases, cancellation, progress) passed into pipeline tasks.
//! - [`Task`] / [`Pipeline`] — composable unit of work and ordered executor with
//!   fan-out batching and retry support.

pub mod cancellation;
pub mod error;
pub mod exec_status;
pub mod pipeline;
pub mod progress;
pub mod provenance;
pub mod runtime;
pub mod sentinels;
pub mod task;
pub mod task_context;
pub mod thread_pool;

#[cfg(feature = "pipeline-run-registry")]
pub mod pipeline_run_registry;

pub use cancellation::{CancellationHandle, CancellationToken, cancellation_pair};
pub use error::CoreError;
pub use exec_status::{ExecStatusManager, NoopExecStatusManager};
pub use pipeline::{
    DataIdFn, ExecutionError, NoopWatcher, Pipeline, PipelineBuilder, PipelineRunHandle,
    PipelineRunInfo, PipelineRunResult, PipelineRunStatus, PipelineStatus, PipelineWatcher,
    RetryDelay, RetryPolicy, TaskStatus, execute, execute_blocking, execute_in_background,
};
pub use progress::ProgressToken;
pub use provenance::{
    HasDataPoint, ProvenanceContext, extract_content_hash_from_value, extract_node_set_from_value,
    stamp_tree, stamp_tree_dyn,
};
pub use runtime::AsyncRuntime;
pub use sentinels::{DroppedSentinel, is_dropped};
pub use task::{
    AsyncBatchFn, AsyncFn, AsyncStreamBatchFn, AsyncStreamFn, SyncBatchFn, SyncFn, SyncIterBatchFn,
    SyncIterFn, Tagged, TaggedMeta, Task, TaskCall, TaskError, TaskInfo, TypedTask, Value,
    ValueIter, ValueStream, downcast_value, extract_node_set,
};
pub use task_context::{PipelineContext, TaskContext, TaskContextBuilder};
pub use thread_pool::{CpuPool, CpuPoolExt, RayonThreadPool};

#[cfg(feature = "pipeline-run-registry")]
pub use pipeline_run_registry::{
    DbPipelineWatcher, DefaultPipelineRunRegistry, PipelineFuture, PipelineRunRegistry,
    RegistryConfig, RegistryError, RunEvent, RunEventKind, RunHandle, RunOutcome, RunPhase,
    RunSpec, ScopedRunWatcher,
};

// Re-export the repository trait from cognee-database for ergonomics.
// This is unconditional — the trait costs nothing if unused.
pub use cognee_database::PipelineRunRepository;
