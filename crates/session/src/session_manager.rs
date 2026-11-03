use std::collections::HashMap;
use std::sync::Arc;

use cognee_llm::Llm;
use cognee_llm::Message;
use tracing::debug;

use crate::context_entry::SessionContextRow;
use crate::error::SessionError;
use crate::feedback;
use crate::session_store::{SessionQAUpdate, SessionStore};
use crate::types::{SessionQAEntry, SessionTraceStep, UsedGraphElementIds};

const DEFAULT_SESSION_ID: &str = "default_session";
const DEFAULT_HISTORY_LIMIT: usize = 10;

/// Orchestrates session operations using an `Arc<dyn SessionStore>`.
///
/// Analogous to Python's `SessionManager`. Loads conversation history as
/// `Vec<Message>` for LLM multi-turn conversations, and saves Q&A entries
/// after each search completion.
#[derive(Clone)]
pub struct SessionManager {
    store: Arc<dyn SessionStore>,
    default_session_id: String,
    history_limit: usize,
    llm: Option<Arc<dyn Llm>>,
    feedback_llm_timeout: std::time::Duration,
}

impl SessionManager {
    /// Maximum Q&A pairs included in completion history.
    pub fn history_limit(&self) -> usize {
        self.history_limit
    }
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            default_session_id: DEFAULT_SESSION_ID.to_string(),
            history_limit: DEFAULT_HISTORY_LIMIT,
            llm: None,
            feedback_llm_timeout: feedback::DEFAULT_FEEDBACK_LLM_TIMEOUT,
        }
    }

    /// Whether the store keeps anything (Python's `is_available`); `false`
    /// while session memory is switched off (`session.caching`).
    pub fn is_available(&self) -> bool {
        self.store.is_available()
    }

    /// How long the LLM call that writes a trace step's feedback may take
    /// before the step is stored with the deterministic fallback
    /// (`session.feedback_llm_timeout_ms`; default 8 s).
    #[must_use]
    pub fn with_feedback_llm_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.feedback_llm_timeout = timeout;
        self
    }

    pub fn with_llm(mut self, llm: Arc<dyn Llm>) -> Self {
        self.llm = Some(llm);
        self
    }

    pub fn with_default_session_id(mut self, id: impl Into<String>) -> Self {
        self.default_session_id = id.into();
        self
    }

    pub fn with_history_limit(mut self, limit: usize) -> Self {
        self.history_limit = limit;
        self
    }

    /// The session a search that names none writes to (Python's
    /// `resolve_session_id(None)`): `{default}_{dataset_id}` when the search is
    /// scoped to one dataset, so two datasets can never mix their turns, and
    /// the plain default otherwise. The default id is fixed
    /// ([`Self::with_default_session_id`]); Python has no setting for it.
    pub fn default_session_id_for(&self, dataset_id: Option<uuid::Uuid>) -> String {
        match dataset_id {
            Some(id) => format!("{}_{id}", self.default_session_id),
            None => self.default_session_id.clone(),
        }
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
    ///
    /// `used_graph_element_ids` carries the node/edge IDs that were consulted
    /// during retrieval so the memify pipeline can trace which graph elements
    /// produced the answer (mirrors Python `session_manager.py:492-525`,
    /// `add_qa(..., used_graph_element_ids=used_graph_element_ids)`).
    ///
    /// `used_session_context_ids` carries the session-context entry ids
    /// served into the answer's guidance block, so the next turn's analysis
    /// can rate them (Python's `used_session_context_ids`). An answered turn
    /// passes the served ids, an acknowledgement turn `None`.
    #[allow(clippy::too_many_arguments)]
    pub async fn save_qa(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
        used_graph_element_ids: Option<UsedGraphElementIds>,
        used_session_context_ids: Option<Vec<String>>,
    ) -> Result<String, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        let qa_id = self
            .store
            .create_qa_entry(resolved_id, user_id, question, answer, context)
            .await?;

        // Write the lineage ids if provided. An empty served-id list stores
        // nothing, as in Python (`served_ids or None`).
        let used_session_context_ids = used_session_context_ids.filter(|ids| !ids.is_empty());
        if (used_graph_element_ids.is_some() || used_session_context_ids.is_some())
            && let Err(e) = self
                .store
                .update_qa_entry(
                    resolved_id,
                    user_id,
                    &qa_id,
                    SessionQAUpdate {
                        used_graph_element_ids: used_graph_element_ids.map(Some),
                        used_session_context_ids: used_session_context_ids.map(Some),
                        ..Default::default()
                    },
                )
                .await
        {
            tracing::warn!(
                qa_id = %qa_id,
                "save_qa: failed to persist used_graph_element_ids / used_session_context_ids (non-fatal): {e}"
            );
        }

        // Mirrors Python `send_telemetry("cognee.session.add_qa", ...)` from
        // cognee/memory/session_manager.py:171.
        #[cfg(feature = "telemetry")]
        {
            let data_size_bytes =
                question.len() + answer.len() + context.map(|c| c.len()).unwrap_or(0);
            cognee_telemetry::send_telemetry(
                "cognee.session.add_qa",
                user_id.unwrap_or("sdk"),
                Some(serde_json::json!({
                    "session_id": resolved_id,
                    "data_size_bytes": data_size_bytes,
                    "has_feedback": false,
                    "has_graph_elements": false,
                })),
            );
        }

        Ok(qa_id)
    }

    /// Every stored session-context row of the session (guidance entries,
    /// feedback records and internal state rows), oldest first. Use
    /// [`crate::coerce_active_context_entries`] to keep only the rows that
    /// may be rendered.
    ///
    /// Python's `SessionManager.get_session_context_entries`. Unlike Python
    /// (fail-open unless `raise_on_error`) a failed read is returned as an
    /// error, so a caller never mistakes an unreadable session for an empty
    /// one; an unavailable store reads as empty.
    pub async fn get_session_context_entries(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionContextRow>, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store
            .get_session_context_entries(resolved_id, user_id)
            .await
    }

    /// Append one session-context row (a JSON object carrying a `"kind"`
    /// field). Python's `SessionManager.create_session_context_entry`.
    pub async fn create_session_context_entry(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        entry: SessionContextRow,
    ) -> Result<(), SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store
            .create_session_context_entry(resolved_id, user_id, entry)
            .await
    }

    /// Shallow-merge `merge` into the session-context row whose `"id"` is
    /// `entry_id`; `false` when there is none. Python's
    /// `SessionManager.update_session_context_entry`.
    pub async fn update_session_context_entry(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        entry_id: &str,
        merge: SessionContextRow,
    ) -> Result<bool, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store
            .update_session_context_entry(resolved_id, user_id, entry_id, merge)
            .await
    }

    /// Every Q&A entry of a session, oldest first (Python's
    /// `SessionManager.get_session` without `last_n`).
    pub async fn get_session(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store.get_all_qa_entries(resolved_id, user_id).await
    }

    /// Most recent Q&A entries, oldest first, without loading the whole session.
    pub async fn get_latest_session_entries(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        last_n: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store
            .get_latest_qa_entries(resolved_id, user_id, last_n)
            .await
    }

    /// Delete all entries for a session, its session-context rows included.
    ///
    /// Waits for the session's turn lock first ([`crate::lock_session_turn`]),
    /// so a turn in flight finishes before its session disappears (Python's
    /// `delete_session` runs under `session_turn_lock`). Must not be called
    /// while holding that lock.
    pub async fn delete_session(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<bool, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        // Python's `session_turn_lock` does not lock without a user or session.
        let _turn = match user_id.filter(|user| !user.is_empty()) {
            Some(user) if !resolved_id.is_empty() => {
                Some(crate::lock_session_turn(user, resolved_id).await)
            }
            _ => None,
        };
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

    /// Append one agent-trace step to the session and return the generated
    /// `trace_id` (UUID4).
    ///
    /// Mirrors Python's `SessionManager.add_agent_trace_step`.
    ///
    /// When `generate_feedback` is `true`, this method attempts to use the
    /// configured LLM (`with_llm`) to summarize `method_return_value`; if no
    /// LLM is wired or generation fails, it falls back to deterministic
    /// feedback (`<origin> succeeded/failed`).
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
        generate_feedback: bool,
    ) -> Result<String, SessionError> {
        // Nothing can be stored: fail before the (paid) feedback LLM call.
        if !self.store.is_available() {
            return Err(SessionError::Disabled);
        }
        let resolved_id = self.resolve_session_id(session_id);
        let trace_id = uuid::Uuid::new_v4().to_string();
        let session_feedback = if generate_feedback {
            if let Some(llm) = self.llm.as_ref() {
                feedback::generate_session_feedback(
                    llm.as_ref(),
                    self.feedback_llm_timeout,
                    origin_function,
                    status,
                    method_return_value.as_ref(),
                    error_message,
                )
                .await
            } else {
                tracing::warn!(
                    origin_function,
                    session_id = resolved_id,
                    "add_agent_trace_step: generate_feedback=true but no LLM wired; using deterministic fallback"
                );
                feedback::fallback_feedback(origin_function, status, error_message)
            }
        } else {
            feedback::fallback_feedback(origin_function, status, error_message)
        };

        let step = SessionTraceStep {
            trace_id: trace_id.clone(),
            origin_function: origin_function.to_string(),
            status: status.to_string(),
            memory_query: memory_query.to_string(),
            memory_context: memory_context.to_string(),
            method_params,
            method_return_value,
            error_message: error_message.to_string(),
            session_feedback,
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
    use crate::DisabledSessionStore;

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
            used_session_context_ids: None,
            memify_metadata: None,
        }
    }

    #[test]
    fn the_default_session_is_scoped_to_a_dataset() {
        let manager = SessionManager::new(Arc::new(DisabledSessionStore));
        let dataset = uuid::Uuid::from_u128(7);
        assert_eq!(manager.default_session_id_for(None), "default_session");
        assert_eq!(
            manager.default_session_id_for(Some(dataset)),
            format!("default_session_{dataset}")
        );
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
