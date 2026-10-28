use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use cognee_database::DatabaseError;

/// Per-run handle returned by `register_*`. Cheap to clone and share.
#[derive(Clone, Debug)]
pub struct RunHandle {
    pub run_id: Uuid,
    /// `pipeline_runs.id` of the latest row written.
    pub task_run_id: Uuid,
    pub user_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
    pub pipeline_name: String,
    pub started_at: DateTime<Utc>,
}

/// One event in a run's lifecycle emitted on the registry channel.
#[derive(Clone, Debug)]
pub struct RunEvent {
    pub run_id: Uuid,
    pub kind: RunEventKind,
    /// Free-form payload. The HTTP layer fills this for cognify; other
    /// pipelines may leave it `Null`.
    pub payload: serde_json::Value,
    pub at: DateTime<Utc>,
}

/// Discriminant for a [`RunEvent`].
#[derive(Clone, Debug)]
pub enum RunEventKind {
    Started,
    Yield,
    Completed,
    Errored { message: String },
    AlreadyCompleted,
}

/// Snapshot of a run's high-level phase. Cheap to read; never blocks the
/// producer.
#[derive(Clone, Debug, PartialEq)]
pub enum RunPhase {
    Pending,
    Running,
    Completed,
    Errored { message: String },
}

/// Builder-style metadata for a new run.
///
/// Note: intentionally not `Clone` — callers construct one per `register_*`
/// call.
pub struct RunSpec {
    /// `None` → auto-generate UUIDv4 at registration time.
    pub run_id: Option<Uuid>,
    pub pipeline_name: String,
    pub user_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
}

/// Configurable bounds for the in-memory registry.
#[derive(Clone, Debug)]
pub struct RegistryConfig {
    /// Max in-memory active+finished runs. Default: 4096.
    /// Set to `usize::MAX` for unbounded.
    pub max_in_memory_runs: usize,
    /// How long to retain finished runs in memory after their terminal event.
    /// Default: 1 hour.
    pub finished_retention: std::time::Duration,
    /// Per-run event channel capacity. Default: 64. Slow subscribers past
    /// this limit are dropped (they receive a synthetic `Errored` event).
    pub channel_capacity: usize,
    /// Optional yield-event throttle. Default: None (emit every yield).
    pub yield_throttle: Option<std::time::Duration>,
    /// Whether to write `DATASET_PROCESSING_ERRORED` rows on `abort()` during
    /// shutdown. Default: true.
    pub abort_writes_errored_row: bool,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            max_in_memory_runs: 4096,
            finished_retention: std::time::Duration::from_secs(3600),
            channel_capacity: 64,
            yield_throttle: None,
            abort_writes_errored_row: true,
        }
    }
}

/// A boxed, Send pipeline future whose output is a generic `Result`.
///
/// The registry does not require the future to return a meaningful value —
/// only that it reaches a terminal `PipelineWatcher` event.
pub type PipelineFuture = Pin<
    Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'static>,
>;

/// Errors returned by registry operations.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("unknown run id: {0}")]
    UnknownRun(Uuid),

    #[error("run aborted")]
    Aborted,

    #[error("registry shut down")]
    Shutdown,

    #[error("repository error: {0}")]
    Repository(#[from] DatabaseError),

    #[error("registry full and no finished runs to evict")]
    RegistryFull,
}

/// The value returned by `register_inline` once the work future completes.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub run_id: Uuid,
    pub phase: RunPhase,
}
