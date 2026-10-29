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

    /// An LLM call made on the session's behalf failed (agent-context
    /// extraction).
    #[error("LLM error: {0}")]
    Llm(cognee_llm::LlmError),
}

impl SessionError {
    /// The LLM budget is exhausted (HTTP 402): every later call would fail
    /// the same way.
    pub fn is_budget_exhausted(&self) -> bool {
        matches!(self, Self::Llm(cognee_llm::LlmError::PaymentRequired(_)))
    }
}
