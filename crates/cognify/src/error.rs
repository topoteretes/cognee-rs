//! Error types for the cognify pipeline.

use thiserror::Error;

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

/// Convert cognee_database::DatabaseError to CognifyError
impl From<cognee_database::DatabaseError> for CognifyError {
    fn from(err: cognee_database::DatabaseError) -> Self {
        CognifyError::DatabaseError(err.to_string())
    }
}
