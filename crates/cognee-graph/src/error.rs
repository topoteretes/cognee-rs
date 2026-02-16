//! Error types for graph database operations.

use thiserror::Error;

/// Result type for graph database operations.
pub type GraphDBResult<T> = Result<T, GraphDBError>;

/// Errors that can occur during graph database operations.
#[derive(Error, Debug)]
pub enum GraphDBError {
    /// Database initialization failed
    #[error("Failed to initialize database: {0}")]
    InitializationError(String),

    /// Query execution failed
    #[error("Query execution failed: {0}")]
    QueryError(String),

    /// Node operation failed
    #[error("Node operation failed: {0}")]
    NodeError(String),

    /// Edge operation failed
    #[error("Edge operation failed: {0}")]
    EdgeError(String),

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// Connection error
    #[error("Connection error: {0}")]
    ConnectionError(String),

    /// Generic database error
    #[error("Database error: {0}")]
    DatabaseError(String),
}
