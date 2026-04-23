//! Shared error type for the high-level API functions.

use thiserror::Error;

/// Unified error type for top-level API operations (forget, update, prune,
/// recall, remember, improve).
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Delete error: {0}")]
    Delete(#[from] cognee_delete::DeleteError),

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

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
}
