use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("Runtime error: {0}")]
    Runtime(String),

    #[error("Thread pool build error: {0}")]
    ThreadPoolBuild(String),

    /// Returned when waiting for a spawned CPU task and its sender was dropped
    /// (e.g. the task panicked or the pool was shut down).
    #[error("CPU task aborted: {reason}")]
    TaskAborted { reason: String },

    #[error("Required TaskContext field is missing: {field}")]
    MissingContextField { field: &'static str },

    #[error("invalid progress split: {reason}")]
    InvalidProgressSplit { reason: String },
}
