use std::collections::HashMap;
use std::sync::Arc;

use cognee_llm::Message;
use tracing::debug;

use crate::error::SessionError;
use crate::session_store::{SessionQAUpdate, SessionStore};
use crate::types::{SessionQAEntry, SessionTraceStep};

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
    ///
    /// When `include_context` is `true`, the context field is included between
    /// QUESTION and ANSWER (matching the Python `include_context` parameter).
    pub fn format_entries(entries: &[SessionQAEntry]) -> String {
        Self::format_entries_with_context(entries, false)
    }

    /// Format Q&A entries, optionally including context.
    pub fn format_entries_with_context(
        entries: &[SessionQAEntry],
        include_context: bool,
    ) -> String {
        if entries.is_empty() {
            return String::new();
        }
        let mut lines = vec!["Previous conversation:\n\n".to_string()];
        for entry in entries {
            lines.push(format!("[{}]\n", entry.created_at.to_rfc3339()));
            lines.push(format!("QUESTION: {}\n", entry.question));
            if include_context && let Some(ref ctx) = entry.context {
                lines.push(format!("CONTEXT: {ctx}\n"));
            }
            lines.push(format!("ANSWER: {}\n\n", entry.answer));
        }
        lines.concat()
    }

    /// Update arbitrary fields on a QA entry.
    pub async fn update_qa(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        qa_id: &str,
        updates: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store
            .update_qa_entry(resolved_id, user_id, qa_id, updates)
            .await
    }

    /// Add or update feedback on a QA entry (convenience over `update_qa`).
    ///
    /// Resets `memify_metadata.feedback_weights_applied` to `false` so that the
    /// memify pipeline will re-apply weights on the next run.
    pub async fn add_feedback(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        qa_id: &str,
        feedback_text: Option<&str>,
        feedback_score: Option<i32>,
    ) -> Result<bool, SessionError> {
        if let Some(score) = feedback_score
            && !(1..=5).contains(&score)
        {
            return Err(SessionError::InvalidParameter(format!(
                "feedback_score must be between 1 and 5, got {score}"
            )));
        }

        let mut memify = HashMap::new();
        memify.insert("feedback_weights_applied".to_string(), false);

        self.update_qa(
            session_id,
            user_id,
            qa_id,
            SessionQAUpdate {
                feedback_text: Some(feedback_text.map(|s| s.to_string())),
                feedback_score: Some(feedback_score),
                memify_metadata: Some(Some(memify)),
                ..Default::default()
            },
        )
        .await
    }

    /// Clear feedback from a QA entry.
    pub async fn delete_feedback(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, SessionError> {
        self.update_qa(
            session_id,
            user_id,
            qa_id,
            SessionQAUpdate {
                feedback_text: Some(None),
                feedback_score: Some(None),
                ..Default::default()
            },
        )
        .await
    }

    /// Retrieve graph knowledge snapshot for a session.
    pub async fn get_graph_context(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store.get_graph_context(resolved_id, user_id).await
    }

    /// Store graph knowledge snapshot for a session.
    pub async fn set_graph_context(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store
            .set_graph_context(resolved_id, user_id, context)
            .await
    }

    /// Append one agent-trace step to the session and return the generated
    /// `trace_id` (UUID4).
    ///
    /// Mirrors Python's `SessionManager.add_agent_trace_step`. The Python
    /// version also generates `session_feedback` via an LLM call before
    /// persisting; that responsibility belongs to LIB-01 (`remember_entry`).
    /// In this task callers pass `session_feedback` directly (default `""`).
    #[allow(clippy::too_many_arguments)]
    pub async fn add_agent_trace_step(
        &self,
        user_id: &str,
        session_id: Option<&str>,
        origin_function: &str,
        status: &str,
        memory_query: &str,
        memory_context: &str,
        method_params: serde_json::Value,
        method_return_value: Option<serde_json::Value>,
        error_message: &str,
        session_feedback: &str,
    ) -> Result<String, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        let trace_id = uuid::Uuid::new_v4().to_string();
        let step = SessionTraceStep {
            trace_id: trace_id.clone(),
            origin_function: origin_function.to_string(),
            status: status.to_string(),
            memory_query: memory_query.to_string(),
            memory_context: memory_context.to_string(),
            method_params,
            method_return_value,
            error_message: error_message.to_string(),
            session_feedback: session_feedback.to_string(),
        };
        self.store.save_trace_step(user_id, resolved_id, step).await
    }

    /// Retrieve agent-trace steps for a session, oldest-first.
    ///
    /// If `last_n` is `Some(n)`, the trailing `n` entries are returned
    /// (mirrors Python's `entries[-last_n:]`).
    pub async fn get_agent_trace_session(
        &self,
        user_id: &str,
        session_id: Option<&str>,
        last_n: Option<usize>,
    ) -> Result<Vec<SessionTraceStep>, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        let mut entries = self.store.read_trace_steps(user_id, resolved_id).await?;
        if let Some(n) = last_n {
            let drop = entries.len().saturating_sub(n);
            entries = entries.split_off(drop);
        }
        Ok(entries)
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

    fn make_entry(question: &str, answer: &str) -> SessionQAEntry {
        SessionQAEntry {
            id: uuid::Uuid::new_v4(),
            session_id: "s1".to_string(),
            user_id: None,
            question: question.to_string(),
            answer: answer.to_string(),
            context: None,
            created_at: chrono::Utc::now(),
            feedback_text: None,
            feedback_score: None,
            used_graph_element_ids: None,
            memify_metadata: None,
        }
    }

    #[test]
    fn entries_to_messages_alternates_roles() {
        let entries = vec![
            make_entry("What is Rust?", "A systems programming language."),
            make_entry("Tell me more.", "It focuses on safety and performance."),
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
        let entries = vec![make_entry("Hello?", "Hi there!")];

        let formatted = SessionManager::format_entries(&entries);
        assert!(formatted.contains("Previous conversation:"));
        assert!(formatted.contains("QUESTION: Hello?"));
        assert!(formatted.contains("ANSWER: Hi there!"));
    }

    #[test]
    fn format_entries_empty_returns_empty_string() {
        assert_eq!(SessionManager::format_entries(&[]), "");
    }

    #[test]
    fn format_entries_with_context_includes_context() {
        let mut entry = make_entry("Hello?", "Hi there!");
        entry.context = Some("Some context here".to_string());
        let entries = vec![entry];

        let formatted = SessionManager::format_entries_with_context(&entries, true);
        assert!(formatted.contains("CONTEXT: Some context here"));
    }

    #[test]
    fn format_entries_with_context_false_omits_context() {
        let mut entry = make_entry("Hello?", "Hi there!");
        entry.context = Some("Some context here".to_string());
        let entries = vec![entry];

        let formatted = SessionManager::format_entries_with_context(&entries, false);
        assert!(!formatted.contains("CONTEXT:"));
    }
}
