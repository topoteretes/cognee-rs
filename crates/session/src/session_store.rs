use std::collections::HashMap;

use async_trait::async_trait;

use crate::error::SessionError;
use crate::types::{SessionQAEntry, SessionTraceStep, UsedGraphElementIds};

/// Partial update DTO for a QA entry.
///
/// - Outer `None` means "leave field unchanged".
/// - `Some(None)` means "clear the field".
/// - `Some(Some(value))` means "set the field to this value".
///
/// For non-optional fields (`question`, `answer`) the outer `Option` controls
/// whether an update is applied.
#[derive(Debug, Clone, Default)]
pub struct SessionQAUpdate {
    pub question: Option<String>,
    pub answer: Option<String>,
    pub context: Option<Option<String>>,
    pub feedback_text: Option<Option<String>>,
    pub feedback_score: Option<Option<i32>>,
    pub used_graph_element_ids: Option<Option<UsedGraphElementIds>>,
    pub memify_metadata: Option<Option<HashMap<String, bool>>>,
}

/// Abstraction over session Q&A storage backends (SQLite, Redis, filesystem, etc.).
///
/// Analogous to Python's `CacheDBInterface`. All backends implement this trait.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Store a Q&A entry in the session. Returns the generated `qa_id`.
    async fn create_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<String, SessionError>;

    /// Retrieve the most recent `last_n` Q&A entries for a session, ordered oldest-first.
    async fn get_latest_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        last_n: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError>;

    /// Retrieve all Q&A entries for a session, ordered oldest-first.
    async fn get_all_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError>;

    /// Delete all entries for a session. Returns `true` if the session existed.
    async fn delete_session(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<bool, SessionError>;

    /// Delete a single Q&A entry by id. Returns `true` if found and deleted.
    async fn delete_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, SessionError>;

    /// Delete ALL session data across all users and sessions.
    /// Equivalent to Python's `CacheDBInterface.prune()`.
    async fn prune(&self) -> Result<(), SessionError>;

    /// Update fields on a QA entry. Only non-`None` fields in `updates` are applied.
    /// Returns `true` if the entry was found and updated.
    async fn update_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
        updates: SessionQAUpdate,
    ) -> Result<bool, SessionError>;

    /// Return the `qa_id` of the most-recent Q&A entry in the session, or
    /// `None` when the session has no entries yet.
    ///
    /// Used by the search orchestrator to route conversationally-detected
    /// feedback to the previous answer (mirrors Python `session_manager.py:462-469`).
    ///
    /// Default impl loads the latest entry via `get_latest_qa_entries(limit=1)`
    /// and extracts its id. Backends may override with a more efficient query.
    async fn latest_qa_id(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        let entries = self.get_latest_qa_entries(session_id, user_id, 1).await?;
        Ok(entries.into_iter().next().map(|e| e.id.to_string()))
    }

    /// Retrieve the graph knowledge snapshot for a session, or `None`.
    async fn get_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError>;

    /// Store (or overwrite) the graph knowledge snapshot for a session.
    async fn set_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), SessionError>;

    /// Append one agent-trace step to the session's trace list.
    ///
    /// Returns the persisted `trace_id` (caller-provided — `SessionManager`
    /// generates it via UUID4 before invoking).
    ///
    /// Default impl returns `SessionError::StoreError` so backends that have
    /// not been updated to support trace steps still compile. The fs / redis /
    /// sea-orm backends override this with a real implementation.
    async fn save_trace_step(
        &self,
        user_id: &str,
        session_id: &str,
        step: SessionTraceStep,
    ) -> Result<String, SessionError> {
        let _ = (user_id, session_id, step);
        Err(SessionError::StoreError(
            "save_trace_step not implemented for this backend".into(),
        ))
    }

    /// Retrieve agent-trace steps for the given session (oldest-first).
    ///
    /// `SessionManager::get_agent_trace_session` performs `last_n` slicing on
    /// top of the returned list. Default impl returns `SessionError::StoreError`.
    async fn read_trace_steps(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Vec<SessionTraceStep>, SessionError> {
        let _ = (user_id, session_id);
        Err(SessionError::StoreError(
            "read_trace_steps not implemented for this backend".into(),
        ))
    }
}
