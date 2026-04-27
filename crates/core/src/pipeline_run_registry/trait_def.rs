use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use uuid::Uuid;

use super::types::{
    PipelineFuture, RegistryError, RunEvent, RunHandle, RunOutcome, RunPhase, RunSpec,
};

/// Runtime-agnostic registry for pipeline run lifecycle tracking.
///
/// Implementations hold a map of in-flight and recently-finished run slots,
/// each with a broadcast channel for live event streaming and a durable
/// repository for audit-trail rows.
///
/// # Usage
///
/// ```rust,ignore
/// let handle = registry
///     .register_background(spec, Box::pin(async move { work.await }))
///     .await?;
/// let mut events = registry.subscribe(handle.run_id);
/// while let Some(event) = events.next().await { ... }
/// ```
#[async_trait]
pub trait PipelineRunRegistry: Send + Sync {
    /// Register a new run and run its `work` future inline — the caller
    /// `.await`s to completion. Returns a [`RunOutcome`] describing whether
    /// the run succeeded or errored.
    async fn register_inline(
        &self,
        spec: RunSpec,
        work: PipelineFuture,
    ) -> Result<RunOutcome, RegistryError>;

    /// Register a new run and spawn `work` on the Tokio runtime. Returns
    /// immediately with the handle; use `subscribe(handle.run_id)` to tail
    /// live events.
    async fn register_background(
        &self,
        spec: RunSpec,
        work: PipelineFuture,
    ) -> Result<RunHandle, RegistryError>;

    /// Subscribe to the event stream for a run.
    ///
    /// If the run id is unknown, a placeholder slot is lazily created so a
    /// subscriber that races ahead of the producer still gets a live receiver
    /// (matches Python's `initialize_queue` semantics). The stream ends when
    /// the broadcast sender is dropped (run completed / registry shut down).
    ///
    /// Subscribers that fall behind the channel buffer receive a synthetic
    /// `RunEvent { kind: Errored { message: "subscriber lagged" }, .. }`
    /// so the WebSocket handler can map it to a 1011 close frame.
    fn subscribe(&self, run_id: Uuid) -> Pin<Box<dyn Stream<Item = RunEvent> + Send + 'static>>;

    /// Snapshot the current high-level phase of a run. Returns `None` for
    /// unknown run ids.
    fn snapshot_status(&self, run_id: Uuid) -> Option<RunPhase>;

    /// Abort an in-flight background run. If the run has an abort handle,
    /// it is dropped immediately. When `cfg.abort_writes_errored_row = true`
    /// (the default), writes a `DATASET_PROCESSING_ERRORED` row and publishes
    /// a final `Errored` event so subscribers get a terminal frame.
    async fn abort(&self, run_id: Uuid) -> Result<(), RegistryError>;

    /// Graceful shutdown: abort every in-flight run and drain all channels.
    ///
    /// After this returns, no new runs should be registered. The HTTP server
    /// calls this on SIGTERM before waiting for the shutdown grace period.
    async fn shutdown(&self) -> Result<(), RegistryError>;
}
