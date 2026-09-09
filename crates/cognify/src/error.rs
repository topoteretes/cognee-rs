//! Error types for the cognify pipeline.

use thiserror::Error;
use uuid::Uuid;

use crate::failure::FailureReport;

#[derive(Debug, Error)]
pub enum CognifyError {
    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Chunking error: {0}")]
    ChunkingError(String),

    #[error("Graph extraction error: {0}")]
    GraphExtractionError(String),

    #[error("Summarization error: {0}")]
    SummarizationError(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("LLM error: {0}")]
    LlmError(String),

    #[error("Fact extraction error: {0}")]
    FactExtractionError(String),

    #[error("Graph database query failed: {0}")]
    GraphDatabaseError(String),

    #[error("Failed to store graph: {0}")]
    GraphStorageError(String),

    #[error("Embedding generation error: {0}")]
    EmbeddingError(String),

    #[error("Vector database error: {0}")]
    VectorDBError(String),

    #[error("Dataset resolution error: {0}")]
    DatasetResolutionError(String),

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Unsupported document type: {0}")]
    UnsupportedDocumentType(String),

    #[error("Task context build failed: {0}")]
    ContextBuild(String),

    #[error("Pipeline execution failed: {0}")]
    Execute(String),

    /// The run collected one or more stage failures and the configured
    /// failure policy judged them fatal. The report carries which chunks and
    /// which files failed, which stage produced each failure, and the error
    /// text — plus a total count when the entry list was capped.
    ///
    /// Boxed to keep [`CognifyError`] small (clippy's `result_large_err`).
    #[error("cognify run failed: {report}")]
    RunFailed { report: Box<FailureReport> },

    #[error("Output type mismatch: expected {expected}, got {actual}")]
    OutputTypeMismatch {
        expected: &'static str,
        actual: &'static str,
    },

    /// Returned when the qualification gate finds an in-flight pipeline run
    /// for the same `(pipeline_name, dataset_id)` pair (latest status =
    /// `STARTED`). Caller should not start a second run concurrently.
    ///
    /// Python parity: Python's `check_pipeline_run_qualification` returns
    /// `False` (skip silently) in this case; the Rust port surfaces it as an
    /// error so callers can distinguish the "rejected" path from the
    /// short-circuit "already completed" path. See doc 08 §13 / 08-08 §4.3.
    /// The message names the cause because the most common way to reach it is
    /// not a concurrent run at all (SDK-616). A killed process leaves *two*
    /// records behind: its unfinished `pipeline_runs` row — which is what this
    /// error is raised from on the qualification path, and which never expires
    /// outside an HTTP-server restart sweep — and its claim, which does expire,
    /// a day later. Both have to go before a re-run is admitted.
    ///
    /// Deliberately stated without prescribing a binary. This error reaches
    /// HTTP and binding callers that have no CLI, and naming only the claim
    /// would misdescribe the blocker on the path that raises it most.
    #[error(
        "pipeline {pipeline_name} for dataset {dataset_id} is already running. If the previous \
         run was killed rather than finishing, it left both an unfinished run record and a \
         claim behind, and both must be cleared before another run can start (the CLI exposes \
         this as `pipeline-unblock`)"
    )]
    PipelineAlreadyRunning {
        pipeline_name: String,
        dataset_id: Uuid,
    },
}

/// Convert GraphDBError to CognifyError
impl From<cognee_graph::GraphDBError> for CognifyError {
    fn from(err: cognee_graph::GraphDBError) -> Self {
        CognifyError::GraphDatabaseError(err.to_string())
    }
}

/// Convert cognee_database::DatabaseError to CognifyError
impl From<cognee_database::DatabaseError> for CognifyError {
    fn from(err: cognee_database::DatabaseError) -> Self {
        CognifyError::DatabaseError(err.to_string())
    }
}
