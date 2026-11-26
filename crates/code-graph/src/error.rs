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
    /// A code repo data item cannot be loaded: its stored manifest is
    /// unreadable, or the repository directory it points at is gone. Python
    /// raises a `ValueError` (`extract_code_repo_graph`).
    #[error("{0}")]
    InvalidRepoItem(String),
    /// A per-file code data item cannot be loaded: it is not stamped as a
    /// code file, or its stored text is not UTF-8. Python raises a
    /// `ValueError` (`extract_code_files_graph`; a `UnicodeDecodeError` is
    /// one).
    #[error("{0}")]
    InvalidCodeItem(String),
    /// Staging a code file for its enola run failed on the filesystem.
    /// Python lets the `OSError` propagate (`_stage_code_file`).
    #[error("{0}")]
    Staging(String),
    /// The graph database failed a read or write.
    #[error("graph database error: {0}")]
    Graph(#[from] cognee_graph::GraphDBError),
    /// The relational provenance ledger failed a write.
    #[error("provenance ledger error: {0}")]
    Ledger(#[from] cognee_database::DatabaseError),
    /// The embedding engine failed while indexing the code graph's vectors
    /// (`index_vectors`).
    #[error("embedding error: {0}")]
    Embedding(String),
    /// The vector store failed while indexing the code graph's vectors
    /// (`index_vectors`).
    #[error("vector database error: {0}")]
    Vector(String),
    /// Reading a dataset's code graph for a CODE search failed. Carries the
    /// underlying error's text: one load is shared by every concurrent
    /// caller ([`crate::query::CodeGraphSnapshotCache`]).
    #[error("code graph snapshot load failed: {0}")]
    SnapshotLoad(String),
}

impl CodeGraphError {
    /// The Python exception class name this error stands for.
    pub fn name(&self) -> &'static str {
        match self {
            Self::EnolaNotInstalled(_) => "EnolaNotInstalledError",
            Self::EnolaSnapshot(_) => "EnolaSnapshotError",
            Self::InvalidConfig(_) => "CogneeConfigurationError",
            Self::InvalidRepoItem(_) | Self::InvalidCodeItem(_) => "ValueError",
            Self::Staging(_) => "OSError",
            Self::Graph(_) | Self::SnapshotLoad(_) => "GraphDatabaseError",
            Self::Ledger(_) => "RelationalDatabaseError",
            Self::Embedding(_) => "EmbeddingException",
            Self::Vector(_) => "VectorDatabaseError",
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
