#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session store error: {0}")]
    StoreError(String),

    #[error("session not found: {0}")]
    NotFound(String),

    #[error("invalid parameter: {0}")]
    InvalidParameter(String),

    /// A stored session-context row does not validate as a
    /// [`crate::SessionContextEntry`].
    #[error("invalid session-context entry: {0}")]
    InvalidContextEntry(String),

    /// Session memory is switched off (`session.caching`, Python's
    /// `CACHING=false`); see [`crate::DisabledSessionStore`].
    #[error("session memory is disabled; set CACHING=true (session.caching) to enable it")]
    Disabled,
}
