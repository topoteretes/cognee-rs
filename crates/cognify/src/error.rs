//! Error types for the cognify pipeline.

use thiserror::Error;

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
}

/// Convert GraphDBError to CognifyError
impl From<cognee_graph::GraphDBError> for CognifyError {
    fn from(err: cognee_graph::GraphDBError) -> Self {
        CognifyError::GraphDatabaseError(err.to_string())
    }
}

/// Convert a graph-backend failure into a cognify error.
///
/// Folded into the existing [`CognifyError::GraphExtractionError`] rather than
/// given a variant of its own: to everything above the seam a backend failure
/// *is* a graph-extraction failure, and the backend name is already in the
/// message.
impl From<crate::graph_backend::GraphBackendError> for CognifyError {
    fn from(err: crate::graph_backend::GraphBackendError) -> Self {
        CognifyError::GraphExtractionError(err.to_string())
    }
}

/// Convert cognee_database::DatabaseError to CognifyError
impl From<cognee_database::DatabaseError> for CognifyError {
    fn from(err: cognee_database::DatabaseError) -> Self {
        CognifyError::DatabaseError(err.to_string())
    }
}
