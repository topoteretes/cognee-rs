//! Public session management API — thin wrappers over `SessionManager`.
//!
//! Mirrors the Python `cognee/api/v1/session/session.py` functions:
//! `get_session`, `add_feedback`, `delete_feedback`, `get_graph_context`,
//! `set_graph_context`.

pub use cognee_session::{
    SessionContext, SessionError, SessionManager, SessionQAEntry, SessionQAUpdate, SessionStore,
    UsedGraphElementIds,
};

/// Retrieve Q&A history from a session.
///
/// If `last_n` is `Some(n)`, only the most recent `n` entries are returned.
/// Otherwise all entries are returned. Delegates to the underlying store
/// via the manager, using the explicit `session_id` (not the manager's default).
pub async fn get_session(
    store: &dyn SessionStore,
    session_id: &str,
    user_id: Option<&str>,
    last_n: Option<usize>,
) -> Result<Vec<SessionQAEntry>, SessionError> {
    if let Some(n) = last_n {
        store.get_latest_qa_entries(session_id, user_id, n).await
    } else {
        store.get_all_qa_entries(session_id, user_id).await
    }
}

/// Add feedback (text and/or score) to a Q&A entry.
///
/// Returns `true` if the entry was found and updated.
pub async fn add_feedback(
    manager: &SessionManager,
    session_id: &str,
    qa_id: &str,
    user_id: Option<&str>,
    feedback_text: Option<&str>,
    feedback_score: Option<i32>,
) -> Result<bool, SessionError> {
    manager
        .add_feedback(
            Some(session_id),
            user_id,
            qa_id,
            feedback_text,
            feedback_score,
        )
        .await
}

/// Clear feedback from a Q&A entry.
///
/// Returns `true` if the entry was found and updated.
pub async fn delete_feedback(
    manager: &SessionManager,
    session_id: &str,
    qa_id: &str,
    user_id: Option<&str>,
) -> Result<bool, SessionError> {
    manager
        .delete_feedback(Some(session_id), user_id, qa_id)
        .await
}

/// Retrieve the graph knowledge snapshot for a session.
pub async fn get_graph_context(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
) -> Result<Option<String>, SessionError> {
    manager.get_graph_context(Some(session_id), user_id).await
}

/// Store (or overwrite) the graph knowledge snapshot for a session.
pub async fn set_graph_context(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
    context: &str,
) -> Result<(), SessionError> {
    manager
        .set_graph_context(Some(session_id), user_id, context)
        .await
}
