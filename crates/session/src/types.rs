use std::collections::HashMap;

use chrono::{DateTime, Utc};
use cognee_llm::Message;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Graph element IDs that were used to produce the answer for a Q&A entry.
///
/// Matches the Python `used_graph_element_ids` dict: `{"node_ids": [...], "edge_ids": [...]}`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsedGraphElementIds {
    #[serde(default)]
    pub node_ids: Vec<String>,
    #[serde(default)]
    pub edge_ids: Vec<String>,
}

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
    /// User-provided feedback text for this Q&A entry.
    #[serde(default)]
    pub feedback_text: Option<String>,
    /// User-provided feedback score (1-5 rating, validated on write).
    #[serde(default)]
    pub feedback_score: Option<i32>,
    /// Graph node/edge IDs that were used to produce the answer.
    #[serde(default)]
    pub used_graph_element_ids: Option<UsedGraphElementIds>,
    /// Metadata tracking for the memify pipeline (e.g. `{"feedback_weights_applied": true}`).
    #[serde(default)]
    pub memify_metadata: Option<HashMap<String, bool>>,
}

/// Session context passed to retrievers: the session ID and any loaded
/// conversation history (as LLM messages).
#[derive(Debug, Clone, Default)]
pub struct SessionContext {
    pub session_id: Option<String>,
    pub history: Vec<Message>,
    pub formatted_history: String,
}
