//! Errors of the code graph.

/// Errors raised while reading an enola snapshot.
///
/// Mirrors Python's `cognee.tasks.code_graph.enola` exceptions; the HTTP
/// status each one carries in Python is [`CodeGraphError::status_code`].
#[derive(Debug, thiserror::Error)]
pub enum CodeGraphError {
    /// Python's `EnolaSnapshotError` (a `CogneeSystemError`, HTTP 500): the
    /// snapshot is missing, unreadable, or in a format this reader does not
    /// understand.
    #[error("{0}")]
    EnolaSnapshot(String),
}

impl CodeGraphError {
    /// The Python exception class name this error stands for.
    pub fn name(&self) -> &'static str {
        match self {
            Self::EnolaSnapshot(_) => "EnolaSnapshotError",
        }
    }

    /// The HTTP status Python attaches to this error.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::EnolaSnapshot(_) => 500,
        }
    }
}

/// Result alias for code graph operations.
pub type CodeGraphResult<T> = Result<T, CodeGraphError>;
