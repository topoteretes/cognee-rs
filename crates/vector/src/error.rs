use thiserror::Error;

/// Error type for vector database operations.
#[derive(Error, Debug)]
pub enum VectorDBError {
    /// The requested collection does not exist.
    #[error("Collection not found: {0}")]
    CollectionNotFound(String),

    /// A collection with the given name already exists.
    #[error("Collection already exists: {0}")]
    CollectionExists(String),

    /// The vector dimension does not match the collection's expected dimension.
    #[error("Dimension mismatch in collection '{collection}': expected {expected}, got {actual}")]
    DimensionMismatch {
        /// Collection name.
        collection: String,
        /// Expected vector dimension.
        expected: usize,
        /// Actual vector dimension provided.
        actual: usize,
    },

    /// A generic storage-level error.
    #[error("Storage error: {0}")]
    StorageError(String),

    /// An I/O error occurred.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// A serialization or deserialization error occurred.
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
}

/// Convenience `Result` alias for vector database operations.
pub type VectorDBResult<T> = Result<T, VectorDBError>;
