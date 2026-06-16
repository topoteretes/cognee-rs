#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests for `SessionManager::add_agent_trace_step` /
//! `SessionManager::get_agent_trace_session` (LIB-02).
//!
//! Uses the `fs` backend as a representative `SessionStore` so the tests
//! run without requiring sea-orm or redis. Behaviour under default-impl
//! `SessionStore` is exercised separately with a stub store.

#![cfg(feature = "fs")]

use std::sync::Arc;

use async_trait::async_trait;
use cognee_session::{
    FsSessionStore, SessionError, SessionManager, SessionQAEntry, SessionQAUpdate, SessionStore,
};

fn fs_manager() -> (SessionManager, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FsSessionStore::new(dir.path()));
    let sm = SessionManager::new(store as Arc<dyn SessionStore>);
    (sm, dir)
}

#[tokio::test]
async fn test_add_agent_trace_step_returns_trace_id() {
    let (sm, _dir) = fs_manager();

    let trace_id = sm
        .add_agent_trace_step(
            "user-a",
            Some("sess-1"),
            "memory.search",
            "success",
            "what?",
            "ctx",
            serde_json::json!({}),
            None,
            "",
            false,
        )
        .await
        .expect("add should succeed");

    // UUID4 string is 36 chars (8-4-4-4-12 with hyphens).
    assert_eq!(trace_id.len(), 36);
    assert_eq!(trace_id.matches('-').count(), 4);

    let read = sm
        .get_agent_trace_session("user-a", Some("sess-1"), None)
        .await
        .expect("read should succeed");
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].trace_id, trace_id);
    assert_eq!(read[0].origin_function, "memory.search");
}

#[tokio::test]
async fn test_trace_step_uuid_uniqueness() {
    let (sm, _dir) = fs_manager();

    let mut seen = std::collections::HashSet::new();
    for _ in 0..100 {
        let id = sm
            .add_agent_trace_step(
                "u",
                Some("s"),
                "fn",
                "success",
                "",
                "",
                serde_json::json!({}),
                None,
                "",
                false,
            )
            .await
            .expect("add should succeed");
        assert!(seen.insert(id), "trace_id collision");
    }
    assert_eq!(seen.len(), 100);
}

#[tokio::test]
async fn test_get_agent_trace_session_last_n() {
    let (sm, _dir) = fs_manager();

    let mut ids = Vec::new();
    for i in 0..5 {
        let id = sm
            .add_agent_trace_step(
                "u",
                Some("s"),
                &format!("fn-{i}"),
                "success",
                "",
                "",
                serde_json::json!({}),
                None,
                "",
                false,
            )
            .await
            .expect("add should succeed");
        ids.push(id);
    }

    let tail = sm
        .get_agent_trace_session("u", Some("s"), Some(3))
        .await
        .expect("read should succeed");
    assert_eq!(tail.len(), 3);
    // Mirrors Python `entries[-last_n:]` — oldest-first slice of the tail.
    assert_eq!(tail[0].trace_id, ids[2]);
    assert_eq!(tail[1].trace_id, ids[3]);
    assert_eq!(tail[2].trace_id, ids[4]);
    assert_eq!(tail[0].origin_function, "fn-2");
    assert_eq!(tail[2].origin_function, "fn-4");
}

#[tokio::test]
async fn test_get_agent_trace_session_default_returns_all() {
    let (sm, _dir) = fs_manager();

    for i in 0..4 {
        sm.add_agent_trace_step(
            "u",
            Some("s"),
            &format!("fn-{i}"),
            "success",
            "",
            "",
            serde_json::json!({}),
            None,
            "",
            false,
        )
        .await
        .expect("add should succeed");
    }

    let all = sm
        .get_agent_trace_session("u", Some("s"), None)
        .await
        .expect("read should succeed");
    assert_eq!(all.len(), 4);
}

/// Stub `SessionStore` that does NOT override the trace methods — exercises
/// the default `SessionError::StoreError` returned by the trait.
struct StubStore;

#[async_trait]
impl SessionStore for StubStore {
    async fn create_qa_entry(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
        _q: &str,
        _a: &str,
        _c: Option<&str>,
    ) -> Result<String, SessionError> {
        Ok("stub".to_string())
    }
    async fn get_latest_qa_entries(
        &self,
        _: &str,
        _: Option<&str>,
        _: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        Ok(vec![])
    }
    async fn get_all_qa_entries(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        Ok(vec![])
    }
    async fn delete_session(&self, _: &str, _: Option<&str>) -> Result<bool, SessionError> {
        Ok(true)
    }
    async fn delete_qa_entry(
        &self,
        _: &str,
        _: Option<&str>,
        _: &str,
    ) -> Result<bool, SessionError> {
        Ok(true)
    }
    async fn prune(&self) -> Result<(), SessionError> {
        Ok(())
    }
    async fn update_qa_entry(
        &self,
        _: &str,
        _: Option<&str>,
        _: &str,
        _: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        Ok(true)
    }
    async fn get_graph_context(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        Ok(None)
    }
    async fn set_graph_context(
        &self,
        _: &str,
        _: Option<&str>,
        _: &str,
    ) -> Result<(), SessionError> {
        Ok(())
    }
}

#[tokio::test]
async fn test_unimplemented_backend_returns_store_error() {
    let sm = SessionManager::new(Arc::new(StubStore) as Arc<dyn SessionStore>);

    let save_err = sm
        .add_agent_trace_step(
            "u",
            Some("s"),
            "fn",
            "success",
            "",
            "",
            serde_json::json!({}),
            None,
            "",
            false,
        )
        .await
        .expect_err("should error");
    match save_err {
        SessionError::StoreError(msg) => assert!(msg.contains("save_trace_step")),
        other => panic!("expected StoreError, got {other:?}"),
    }

    let read_err = sm
        .get_agent_trace_session("u", Some("s"), None)
        .await
        .expect_err("should error");
    match read_err {
        SessionError::StoreError(msg) => assert!(msg.contains("read_trace_steps")),
        other => panic!("expected StoreError, got {other:?}"),
    }
}
