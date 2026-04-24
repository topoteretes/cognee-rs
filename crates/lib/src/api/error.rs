//! Shared error types for the high-level API functions.

use cognee_database::DatabaseError;
use cognee_delete::DeleteError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DatasetError {
    #[error("permission denied")]
    PermissionDenied,

    #[error("dataset not found")]
    NotFound,

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

    #[error("Memify error: {0}")]
    Memify(String),

    #[error("Improve error: {0}")]
    Improve(String),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("Background task join error: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl From<cognee_delete::DeleteError> for ApiError {
    fn from(e: cognee_delete::DeleteError) -> Self {
        ApiError::DeleteErr(e)
    }
}
