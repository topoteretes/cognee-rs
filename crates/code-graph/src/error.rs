//! Errors of the code graph.

/// Errors raised while running enola, reading its snapshot or writing the
/// code graph.
///
/// The first two mirror Python's `cognee.tasks.code_graph.enola` exceptions;
/// the HTTP status each one carries in Python is
/// [`CodeGraphError::status_code`]. Graph and ledger failures propagate as
/// Python lets the engine's own exception propagate.
#[derive(Debug, thiserror::Error)]
pub enum CodeGraphError {
    /// Python's `EnolaNotInstalledError` (a `CogneeConfigurationError`, HTTP
    /// 500): no enola binary at the configured path or on `PATH`.
    #[error("{0}")]
    EnolaNotInstalled(String),
    /// Python's `EnolaSnapshotError` (a `CogneeSystemError`, HTTP 500): enola
    /// failed, timed out or left no snapshot, or the snapshot is missing,
    /// unreadable, or in a format this reader does not understand.
    #[error("{0}")]
    EnolaSnapshot(String),
    /// A code graph setting is unusable (for example a negative timeout).
    #[error("{0}")]
    InvalidConfig(String),
    /// The graph database failed a read or write.
    #[error("graph database error: {0}")]
    Graph(#[from] cognee_graph::GraphDBError),
    /// The relational provenance ledger failed a write.
    #[error("provenance ledger error: {0}")]
    Ledger(#[from] cognee_database::DatabaseError),
}

impl CodeGraphError {
    /// The Python exception class name this error stands for.
    pub fn name(&self) -> &'static str {
        match self {
            Self::EnolaNotInstalled(_) => "EnolaNotInstalledError",
            Self::EnolaSnapshot(_) => "EnolaSnapshotError",
            Self::InvalidConfig(_) => "CogneeConfigurationError",
            Self::Graph(_) => "GraphDatabaseError",
            Self::Ledger(_) => "RelationalDatabaseError",
        }
    }

    /// The HTTP status Python attaches to this error. Every one of them is a
    /// 500 in Python: `EnolaNotInstalledError` is a configuration error
    /// raised while serving, not a client mistake.
    pub fn status_code(&self) -> u16 {
        500
    }
}

/// Result alias for code graph operations.
pub type CodeGraphResult<T> = Result<T, CodeGraphError>;
