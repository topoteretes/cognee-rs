#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session store error: {0}")]
    StoreError(String),

    #[error("session not found: {0}")]
    NotFound(String),

    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
}
