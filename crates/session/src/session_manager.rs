use std::sync::Arc;

use cognee_llm::Message;
use tracing::debug;

use crate::error::SessionError;
use crate::session_store::SessionStore;
use crate::types::SessionQAEntry;

const DEFAULT_SESSION_ID: &str = "default_session";
const DEFAULT_HISTORY_LIMIT: usize = 10;

/// Orchestrates session operations using an `Arc<dyn SessionStore>`.
///
/// Analogous to Python's `SessionManager`. Loads conversation history as
/// `Vec<Message>` for LLM multi-turn conversations, and saves Q&A entries
/// after each search completion.
pub struct SessionManager {
    store: Arc<dyn SessionStore>,
    default_session_id: String,
    history_limit: usize,
}

impl SessionManager {
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            default_session_id: DEFAULT_SESSION_ID.to_string(),
            history_limit: DEFAULT_HISTORY_LIMIT,
        }
    }

    pub fn with_default_session_id(mut self, id: impl Into<String>) -> Self {
        self.default_session_id = id.into();
        self
    }

    pub fn with_history_limit(mut self, limit: usize) -> Self {
        self.history_limit = limit;
        self
    }

    fn resolve_session_id<'a>(&'a self, session_id: Option<&'a str>) -> &'a str {
        session_id.unwrap_or(&self.default_session_id)
    }

    /// Load conversation history as alternating User/Assistant messages.
    ///
    /// Returns the last `history_limit` Q&A pairs as:
    /// `[User(q1), Assistant(a1), User(q2), Assistant(a2), ...]`
    pub async fn load_history_messages(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<Vec<Message>, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        let entries = self
            .store
            .get_latest_qa_entries(resolved_id, user_id, self.history_limit)
            .await?;

        debug!(
            session_id = resolved_id,
            entries = entries.len(),
            "Loaded session history"
        );

        Ok(entries_to_messages(&entries))
    }

    /// Load history as structured messages AND a formatted string, with a single store round-trip.
    pub async fn load_history_both(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<(Vec<Message>, String), SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        let entries = self
            .store
            .get_latest_qa_entries(resolved_id, user_id, self.history_limit)
            .await?;

        debug!(
            session_id = resolved_id,
            entries = entries.len(),
            "Loaded session history (both forms)"
        );

        let messages = entries_to_messages(&entries);
        let formatted = Self::format_entries(&entries);
        Ok((messages, formatted))
    }

    /// Save a Q&A exchange to the session. Returns the generated `qa_id`.
    pub async fn save_qa(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<String, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store
            .create_qa_entry(resolved_id, user_id, question, answer, context)
            .await
    }

    /// Delete all Q&A entries for a session.
    pub async fn delete_session(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<bool, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store.delete_session(resolved_id, user_id).await
    }

    /// Format Q&A entries as a human-readable string (for debugging / compatibility
    /// with Python's `SessionManager.format_entries`).
    pub fn format_entries(entries: &[SessionQAEntry]) -> String {
        if entries.is_empty() {
            return String::new();
        }
        let mut lines = vec!["Previous conversation:\n\n".to_string()];
        for entry in entries {
            lines.push(format!("[{}]\n", entry.created_at.to_rfc3339()));
            lines.push(format!("QUESTION: {}\n", entry.question));
            lines.push(format!("ANSWER: {}\n\n", entry.answer));
        }
        lines.concat()
    }
}

/// Convert session Q&A entries to alternating User/Assistant LLM messages.
fn entries_to_messages(entries: &[SessionQAEntry]) -> Vec<Message> {
    let mut messages = Vec::with_capacity(entries.len() * 2);
    for entry in entries {
        messages.push(Message::user(&entry.question));
        messages.push(Message::assistant(&entry.answer));
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_to_messages_alternates_roles() {
        let entries = vec![
            SessionQAEntry {
                id: uuid::Uuid::new_v4(),
                session_id: "s1".to_string(),
                user_id: None,
                question: "What is Rust?".to_string(),
                answer: "A systems programming language.".to_string(),
                context: None,
                created_at: chrono::Utc::now(),
            },
            SessionQAEntry {
                id: uuid::Uuid::new_v4(),
                session_id: "s1".to_string(),
                user_id: None,
                question: "Tell me more.".to_string(),
                answer: "It focuses on safety and performance.".to_string(),
                context: None,
                created_at: chrono::Utc::now(),
            },
        ];

        let messages = entries_to_messages(&entries);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, cognee_llm::MessageRole::User);
        assert_eq!(messages[0].content, "What is Rust?");
        assert_eq!(messages[1].role, cognee_llm::MessageRole::Assistant);
        assert_eq!(messages[1].content, "A systems programming language.");
        assert_eq!(messages[2].role, cognee_llm::MessageRole::User);
        assert_eq!(messages[3].role, cognee_llm::MessageRole::Assistant);
    }

    #[test]
    fn format_entries_produces_expected_output() {
        let entries = vec![SessionQAEntry {
            id: uuid::Uuid::new_v4(),
            session_id: "s1".to_string(),
            user_id: None,
            question: "Hello?".to_string(),
            answer: "Hi there!".to_string(),
            context: None,
            created_at: chrono::Utc::now(),
        }];

        let formatted = SessionManager::format_entries(&entries);
        assert!(formatted.contains("Previous conversation:"));
        assert!(formatted.contains("QUESTION: Hello?"));
        assert!(formatted.contains("ANSWER: Hi there!"));
    }

    #[test]
    fn format_entries_empty_returns_empty_string() {
        assert_eq!(SessionManager::format_entries(&[]), "");
    }
}
