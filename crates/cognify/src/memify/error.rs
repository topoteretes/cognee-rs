use thiserror::Error;
use uuid::Uuid;

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

    /// Returned when the qualification gate finds an in-flight pipeline run
    /// for the same `(pipeline_name, dataset_id)` pair (latest status =
    /// `STARTED`). Caller should not start a second run concurrently. See
    /// doc 08 §13 / 08-08 §4.4.
    /// Same wording as the cognify variant, and for the same reason: memify
    /// takes a claim under `memify_pipeline` and leaves the same two records
    /// behind when killed, so an operator wedged on memify needs the same
    /// pointer a wedged cognify gets (SDK-616).
    #[error(
        "pipeline {pipeline_name} for dataset {dataset_id:?} is already running. If the \
         previous run was killed rather than finishing, it left both an unfinished run record \
         and a claim behind, and both must be cleared before another run can start (the CLI \
         exposes this as `pipeline-unblock`)"
    )]
    PipelineAlreadyRunning {
        pipeline_name: String,
        dataset_id: Option<Uuid>,
    },

    /// Returned by the underlying database when the qualification check
    /// reads the latest `pipeline_runs` row. Distinct from `Execute` so
    /// callers can tell whether the gate or the executor failed.
    #[error("Database error: {0}")]
    Database(String),
}
