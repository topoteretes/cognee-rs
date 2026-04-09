use async_trait::async_trait;

use crate::error::SessionError;
use crate::types::SessionQAEntry;

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
}
