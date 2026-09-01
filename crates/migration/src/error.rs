//! Errors raised while writing a COGX archive.

use std::path::PathBuf;

/// Failure modes of a COGX export.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// The archive directory could not be created, cleaned, or written to.
    #[error("COGX archive I/O failed at {path}: {source}")]
    Io {
        /// The file or directory the operation was attempting to touch.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// A record could not be serialized to JSON.
    #[error("failed to serialize COGX record: {0}")]
    Serialize(#[from] serde_json::Error),

    /// Reading the graph to export it failed.
    #[error("failed to read graph data for export: {0}")]
    Graph(#[from] cognee_graph::GraphDBError),
}

impl MigrationError {
    /// Attach the offending path to an [`std::io::Error`].
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Result alias for COGX operations.
pub type MigrationResult<T> = Result<T, MigrationError>;
