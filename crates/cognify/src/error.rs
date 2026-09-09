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
    /// The message names the remedy because the most common way to reach it is
    /// not a concurrent run at all: a killed process cannot release its claim,
    /// so the row outlives it and refuses every later run on the pair until it
    /// ages out a day later (SDK-616). An operator who has just had a cognify
    /// OOM needs to be told that clearing it is possible, and told where.
    #[error(
        "pipeline {pipeline_name} for dataset {dataset_id} is already running. If the previous \
         run was killed rather than finishing, its claim is still held; inspect it with \
         `cognee-cli pipeline-claim -d <dataset> --pipeline {pipeline_name}`"
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
