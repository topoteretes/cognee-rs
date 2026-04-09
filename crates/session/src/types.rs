use chrono::{DateTime, Utc};
use cognee_llm::Message;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single question-answer entry stored in a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionQAEntry {
    pub id: Uuid,
    pub session_id: String,
    pub user_id: Option<String>,
    pub question: String,
    pub answer: String,
    pub context: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Session context passed to retrievers: the session ID and any loaded
/// conversation history (as LLM messages).
#[derive(Debug, Clone, Default)]
pub struct SessionContext {
    pub session_id: Option<String>,
    pub history: Vec<Message>,
}
