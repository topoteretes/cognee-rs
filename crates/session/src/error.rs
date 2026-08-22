#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session store is read-only")]
    ReadOnly,

    #[error("session store error: {0}")]
    StoreError(String),

    #[error("session not found: {0}")]
    NotFound(String),

    #[error("invalid parameter: {0}")]
    InvalidParameter(String),

    #[error("external event conflict for '{event_id}': {reason}")]
    ExternalEventConflict { event_id: String, reason: String },
}
