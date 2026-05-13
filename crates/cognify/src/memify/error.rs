use thiserror::Error;

#[derive(Error, Debug)]
pub enum MemifyError {
    #[error("Graph DB error: {0}")]
    GraphDBError(String),

    #[error("Vector DB error: {0}")]
    VectorDBError(String),

    #[error("Embedding error: {0}")]
    EmbeddingError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// `TaskContextBuilder::build()` failed (a required backend handle was
    /// missing). Surfaced by the executor-routed `memify` convenience
    /// function. See LIB-06-02 §4.5.
    #[error("Task context build failed: {0}")]
    Context(String),

    /// `cognee_core::pipeline::execute` returned an `ExecutionError`.
    #[error("Pipeline execution failed: {0}")]
    Execute(String),

    /// The pipeline's typed output could not be downcast to the expected
    /// concrete type (`IndexResult`). Indicates a programmer error in the
    /// pipeline shape. See LIB-06-02 Decision 9.
    #[error("Pipeline output type mismatch: expected {expected}, got {actual}")]
    OutputTypeMismatch {
        expected: &'static str,
        actual: &'static str,
    },
}
