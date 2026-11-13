//! Shared error types for the high-level API functions.

use cognee_database::DatabaseError;
use cognee_delete::DeleteError;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum DatasetError {
    #[error("permission denied")]
    PermissionDenied,

    #[error("dataset not found")]
    NotFound,

    #[error("ACL not configured: call with_acl() before create_authorized_dataset()")]
    AclNotConfigured,

    #[error("database error: {0}")]
    Database(#[from] DatabaseError),

    #[error("delete error: {0}")]
    Delete(#[from] DeleteError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Unified error type for top-level API operations (forget, update, prune,
/// recall, remember, improve).
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Delete error: {0}")]
    DeleteErr(cognee_delete::DeleteError),

    #[error("Ingestion error: {0}")]
    Ingestion(String),

    #[error("Cognify error: {0}")]
    Cognify(String),

    #[error("Search error: {0}")]
    Search(String),

    #[error("Session error: {0}")]
    Session(#[from] cognee_session::SessionError),

    #[error("Storage error: {0}")]
    Storage(#[from] cognee_storage::StorageError),

    #[error("Graph error: {0}")]
    Graph(#[from] cognee_graph::GraphDBError),

    #[error("Vector error: {0}")]
    Vector(#[from] cognee_vector::VectorDBError),

    #[error("Improve error: {0}")]
    Improve(String),

    /// The fatal `persist_session_qa` stage errored and stopped the run.
    /// Carries the partial result (the stages before the abort, the rest
    /// skipped) as Python attaches it to the raised error. `budget_exhausted`
    /// says the stage failed on an exhausted LLM budget, which the HTTP layer
    /// answers with 402 instead of 409.
    #[error("{}", improve_aborted_message(result, *budget_exhausted))]
    ImproveAborted {
        result: Box<crate::api::improve::ImproveResult>,
        budget_exhausted: bool,
    },

    /// `update()` was called with a data id that resolves to no document:
    /// neither an exact row id nor a recorded pre-fork `legacy_id` matched in
    /// the dataset. Python's `UpdateTargetNotFoundError` (404); `add()` is the
    /// path for new documents.
    #[error(
        "No document found to update: data_id '{data_id}' does not exist in dataset \
         '{dataset_id}' (neither as a document id nor as a pre-fork legacy id). \
         Use add() to create new documents."
    )]
    UpdateTargetNotFound { data_id: Uuid, dataset_id: Uuid },

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    /// A per-call `embedding_config` whose model is not as wide as the vector
    /// store's, refused before anything searches (Python's
    /// `EmbeddingDimensionMismatchError`, HTTP 409).
    #[error("{0}")]
    EmbeddingDimensionMismatch(#[from] cognee_search::EmbeddingDimensionMismatch),

    /// The caller lacks the permission on the dataset in its tenant. "No
    /// grant" and "a grant only on another tenant's dataset" both answer this
    /// variant, like Python's single `PermissionDeniedError` (HTTP 403,
    /// `cognee/modules/users/exceptions/exceptions.py:42-51@37f788bd2`).
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// A component could not be built: the engines of a per-call
    /// `llm_config` / `embedding_config`, or the configuration they are
    /// built over.
    #[error("Component error: {0}")]
    Component(#[from] crate::error::ComponentError),

    #[error("Background task join error: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// Python's `_abort_run` message for a fatal stage that errored.
fn improve_aborted_message(
    result: &crate::api::improve::ImproveResult,
    budget_exhausted: bool,
) -> String {
    let stage = crate::api::improve::StageKind::PersistSessionQa.name();
    let error = result.error.as_deref().unwrap_or("unknown error");
    if budget_exhausted {
        format!("improve: fatal stage '{stage}' stopped on an exhausted LLM budget: {error}")
    } else {
        format!("improve: fatal stage '{stage}' errored: {error}")
    }
}

impl From<cognee_delete::DeleteError> for ApiError {
    fn from(e: cognee_delete::DeleteError) -> Self {
        ApiError::DeleteErr(e)
    }
}
