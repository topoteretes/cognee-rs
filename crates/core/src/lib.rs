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
pub mod runtime;
pub mod task;
pub mod task_context;
pub mod thread_pool;

pub use cancellation::{CancellationHandle, CancellationToken, cancellation_pair};
pub use error::CoreError;
pub use exec_status::{ExecStatusManager, NoopExecStatusManager};
pub use pipeline::{
    DataIdFn, ExecutionError, NoopWatcher, Pipeline, PipelineRunHandle, PipelineRunInfo,
    PipelineRunResult, PipelineRunStatus, PipelineStatus, PipelineWatcher, RetryDelay, RetryPolicy,
    TaskStatus, execute, execute_blocking, execute_in_background,
};
pub use progress::ProgressToken;
pub use runtime::AsyncRuntime;
pub use task::{
    AsyncBatchFn, AsyncFn, AsyncStreamBatchFn, AsyncStreamFn, SyncBatchFn, SyncFn, SyncIterBatchFn,
    SyncIterFn, Tagged, TaggedMeta, Task, TaskCall, TaskError, TaskInfo, Value, ValueIter,
    ValueStream, downcast_value, extract_node_set,
};
pub use task_context::{PipelineContext, TaskContext, TaskContextBuilder};
pub use thread_pool::{CpuPool, CpuPoolExt, RayonThreadPool};
