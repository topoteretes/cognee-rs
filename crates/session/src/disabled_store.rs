//! The session store used while session memory is switched off.

use async_trait::async_trait;

use crate::error::SessionError;
use crate::session_store::{SessionQAUpdate, SessionStore};
use crate::types::{SessionQAEntry, SessionTraceStep};

/// A session store that keeps nothing: the store selected while
/// `session.caching` is off (Python's `CACHING=false`).
///
/// It mirrors Python's session manager without a cache engine
/// (`SessionManager.is_available == False`): every read finds an empty
/// session, updates and deletes find nothing, and the graph-context snapshot
/// is dropped, and [`SessionStore::is_available`] is `false`. The two writes
/// that must hand back an id, a Q&A entry and a trace step, fail with
/// [`SessionError::Disabled`]. Callers handle that as Python does:
///
/// - a search ignores it and answers without session history;
/// - `remember(data, session_id)` logs "session cache not available" and
///   still succeeds, as Python's `_add_to_session` does;
/// - `remember_entry` (a typed Q&A / trace / feedback entry) checks
///   `is_available` before any other work and fails with `Disabled`, as
///   Python's `_dispatch_session_entry` raises "Session cache unavailable";
/// - [`crate::SessionManager::add_agent_trace_step`] checks it too, so no
///   feedback LLM call is made for a step that cannot be stored.
#[derive(Debug, Default, Clone, Copy)]
pub struct DisabledSessionStore;

#[async_trait]
impl SessionStore for DisabledSessionStore {
    fn is_available(&self) -> bool {
        false
    }

    async fn create_qa_entry(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
        _question: &str,
        _answer: &str,
        _context: Option<&str>,
    ) -> Result<String, SessionError> {
        Err(SessionError::Disabled)
    }

    async fn get_latest_qa_entries(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
        _last_n: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        Ok(Vec::new())
    }

    async fn get_all_qa_entries(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        Ok(Vec::new())
    }

    async fn delete_session(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
    ) -> Result<bool, SessionError> {
        Ok(false)
    }

    async fn delete_qa_entry(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
        _qa_id: &str,
    ) -> Result<bool, SessionError> {
        Ok(false)
    }

    async fn prune(&self) -> Result<(), SessionError> {
        Ok(())
    }

    async fn update_qa_entry(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
        _qa_id: &str,
        _updates: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        Ok(false)
    }

    async fn get_graph_context(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        Ok(None)
    }

    async fn set_graph_context(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
        _context: &str,
    ) -> Result<(), SessionError> {
        Ok(())
    }

    async fn save_trace_step(
        &self,
        _user_id: &str,
        _session_id: &str,
        _step: SessionTraceStep,
    ) -> Result<String, SessionError> {
        Err(SessionError::Disabled)
    }

    async fn read_trace_steps(
        &self,
        _user_id: &str,
        _session_id: &str,
    ) -> Result<Vec<SessionTraceStep>, SessionError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_are_empty_and_writes_report_disabled() {
        let store = DisabledSessionStore;
        assert!(!store.is_available());
        assert!(matches!(
            store.create_qa_entry("s", None, "q", "a", None).await,
            Err(SessionError::Disabled)
        ));
        assert!(
            store
                .get_all_qa_entries("s", None)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .get_latest_qa_entries("s", None, 5)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.latest_qa_id("s", None).await.unwrap(), None);
        store.set_graph_context("s", None, "ctx").await.unwrap();
        assert_eq!(store.get_graph_context("s", None).await.unwrap(), None);
        assert!(!store.delete_session("s", None).await.unwrap());
        assert!(store.read_trace_steps("u", "s").await.unwrap().is_empty());
    }
}
