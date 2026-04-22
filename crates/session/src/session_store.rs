use std::collections::HashMap;

use async_trait::async_trait;

use crate::error::SessionError;
use crate::types::{SessionQAEntry, UsedGraphElementIds};

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
}
