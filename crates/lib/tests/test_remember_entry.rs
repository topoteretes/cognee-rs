#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! LIB-01 integration tests for `remember_entry()` typed-entry dispatch.
//!
//! Covers the six §5 cases from `docs/http-api-v2/tasks/lib-01-remember-entry-facade.md`:
//!   - QA dispatch returns the qa_id and `entry_type == "qa"`.
//!   - QA with optional fields persists via `update_qa`.
//!   - Trace dispatch returns the trace_id and `entry_type == "trace"`.
//!   - Feedback dispatch returns the qa_id on `Ok(true)`.
//!   - Feedback `Ok(false)` produces `Errored` + populated `error`.
//!   - Empty `session_id` returns `ApiError::InvalidArgument`.
//!
//! Inline `InMemorySessionStore` mirrors the prior-art pattern at
//! `crates/delete/src/lib.rs:3565-3580` (no MockSessionStore exists in
//! `cognee-test-utils`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use cognee_lib::api::error::ApiError;
use cognee_lib::api::remember::{RememberStatus, remember_entry};
use cognee_models::{FeedbackEntry, MemoryEntry, QAEntry, TraceEntry};
use cognee_session::{
    SessionError, SessionManager, SessionQAEntry, SessionQAUpdate, SessionStore, SessionTraceStep,
    UsedGraphElementIds,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// In-memory SessionStore for the dispatch tests.
//
// Records all create_qa_entry / update_qa_entry / save_trace_step calls so
// the assertions can verify the dispatch reached the expected method with
// the expected arguments. `add_feedback` returns whether to simulate the
// "qa not found" path.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct InMemorySessionStore {
    qa: Mutex<HashMap<String, SessionQAEntry>>,
    qa_updates: Mutex<Vec<(String, SessionQAUpdate)>>,
    trace_steps: Mutex<Vec<(String, String, SessionTraceStep)>>,
    /// When `false`, `update_qa_entry` returns `Ok(false)` (simulates
    /// "QA not found"). Defaults to `true`.
    update_qa_succeeds: Mutex<bool>,
}

impl InMemorySessionStore {
    fn new() -> Self {
        Self {
            qa: Mutex::new(HashMap::new()),
            qa_updates: Mutex::new(Vec::new()),
            trace_steps: Mutex::new(Vec::new()),
            update_qa_succeeds: Mutex::new(true),
        }
    }

    fn set_update_qa_succeeds(&self, ok: bool) {
        *self
            .update_qa_succeeds
            .lock()
            .expect("lock poison is unrecoverable") = ok;
    }

    fn qa_updates_count(&self) -> usize {
        self.qa_updates
            .lock()
            .expect("lock poison is unrecoverable")
            .len()
    }

    fn last_qa_update(&self) -> Option<(String, SessionQAUpdate)> {
        self.qa_updates
            .lock()
            .expect("lock poison is unrecoverable")
            .last()
            .cloned()
    }

    fn last_trace_step(&self) -> Option<(String, String, SessionTraceStep)> {
        self.trace_steps
            .lock()
            .expect("lock poison is unrecoverable")
            .last()
            .cloned()
    }
}

// `SessionQAUpdate` does derive `Clone` (`Default + Clone` per
// `crates/session/src/session_store.rs:17`). The store records each
// update via `clone_update` for symmetry with `SessionTraceStep`'s
// straight `.clone()`.
fn clone_update(u: &SessionQAUpdate) -> SessionQAUpdate {
    u.clone()
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn create_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<String, SessionError> {
        let qa_id = Uuid::new_v4().to_string();
        let entry = SessionQAEntry {
            id: Uuid::new_v4(),
            session_id: session_id.to_string(),
            user_id: user_id.map(|s| s.to_string()),
            question: question.to_string(),
            answer: answer.to_string(),
            context: context.map(|s| s.to_string()),
            created_at: Utc::now(),
            feedback_text: None,
            feedback_score: None,
            used_graph_element_ids: None,
            memify_metadata: None,
        };
        self.qa
            .lock()
            .expect("lock poison is unrecoverable")
            .insert(qa_id.clone(), entry);
        Ok(qa_id)
    }

    async fn get_latest_qa_entries(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
        _last_n: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        Ok(vec![])
    }

    async fn get_all_qa_entries(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        Ok(vec![])
    }

    async fn delete_session(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
    ) -> Result<bool, SessionError> {
        Ok(true)
    }

    async fn delete_qa_entry(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
        _qa_id: &str,
    ) -> Result<bool, SessionError> {
        Ok(true)
    }

    async fn prune(&self) -> Result<(), SessionError> {
        Ok(())
    }

    async fn update_qa_entry(
        &self,
        _session_id: &str,
        _user_id: Option<&str>,
        qa_id: &str,
        updates: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        let succeeds = *self
            .update_qa_succeeds
            .lock()
            .expect("lock poison is unrecoverable");
        self.qa_updates
            .lock()
            .expect("lock poison is unrecoverable")
            .push((qa_id.to_string(), clone_update(&updates)));
        Ok(succeeds)
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
        user_id: &str,
        session_id: &str,
        step: SessionTraceStep,
    ) -> Result<String, SessionError> {
        let trace_id = step.trace_id.clone();
        self.trace_steps
            .lock()
            .expect("lock poison is unrecoverable")
            .push((user_id.to_string(), session_id.to_string(), step));
        Ok(trace_id)
    }

    async fn read_trace_steps(
        &self,
        _user_id: &str,
        _session_id: &str,
    ) -> Result<Vec<SessionTraceStep>, SessionError> {
        Ok(self
            .trace_steps
            .lock()
            .expect("lock poison is unrecoverable")
            .iter()
            .map(|(_, _, step)| step.clone())
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn make_sm() -> (Arc<InMemorySessionStore>, Arc<SessionManager>) {
    let store = Arc::new(InMemorySessionStore::new());
    let sm = Arc::new(SessionManager::new(store.clone() as Arc<dyn SessionStore>));
    (store, sm)
}

#[tokio::test]
async fn test_qa_entry_dispatch_returns_qa_id() {
    let (store, sm) = make_sm();
    let owner = Uuid::new_v4();

    let entry = MemoryEntry::Qa(QAEntry {
        question: "What is Rust?".into(),
        answer: "A systems programming language.".into(),
        context: "".into(),
        feedback_text: None,
        feedback_score: None,
        used_graph_element_ids: None,
    });

    let result = remember_entry(
        entry,
        "main_dataset",
        "session-A",
        owner,
        None,
        None,
        Some(store.clone() as Arc<dyn SessionStore>),
        Some(sm),
        None,
    )
    .await
    .expect("dispatch must succeed");

    assert_eq!(result.status, RememberStatus::SessionStored);
    assert_eq!(result.entry_type.as_deref(), Some("qa"));
    assert!(
        result.entry_id.is_some(),
        "entry_id must be populated for QA path"
    );
    assert_eq!(
        result.session_ids.as_deref(),
        Some(&["session-A".to_string()][..])
    );
    assert!(result.elapsed_seconds.is_some());
    assert_eq!(result.dataset_name, "main_dataset");
    // No optional fields set → no follow-up update_qa.
    assert_eq!(
        store.qa_updates_count(),
        0,
        "no update_qa expected when no optional fields are set"
    );
}

#[tokio::test]
async fn test_qa_entry_with_optional_fields_persists_via_update_qa() {
    let (store, sm) = make_sm();
    let owner = Uuid::new_v4();

    let used = serde_json::json!({"node_ids": ["n1", "n2"], "edge_ids": ["e1"]});
    let entry = MemoryEntry::Qa(QAEntry {
        question: "Q".into(),
        answer: "A".into(),
        context: "ctx".into(),
        feedback_text: Some("nice".into()),
        feedback_score: Some(4),
        used_graph_element_ids: Some(used),
    });

    let result = remember_entry(
        entry,
        "ds",
        "sess-1",
        owner,
        None,
        None,
        Some(store.clone() as Arc<dyn SessionStore>),
        Some(sm),
        None,
    )
    .await
    .expect("dispatch must succeed");

    assert_eq!(result.status, RememberStatus::SessionStored);
    assert_eq!(result.entry_type.as_deref(), Some("qa"));

    // Exactly one update_qa call, carrying all three optional fields.
    assert_eq!(store.qa_updates_count(), 1);
    let (_qa_id, update) = store.last_qa_update().expect("update must be recorded");
    assert_eq!(
        update.feedback_text.as_ref().and_then(|o| o.as_deref()),
        Some("nice")
    );
    assert_eq!(update.feedback_score.flatten(), Some(4));
    let used_after = update.used_graph_element_ids.flatten();
    let used_after = used_after.expect("used_graph_element_ids must be set");
    assert_eq!(
        used_after.node_ids,
        vec!["n1".to_string(), "n2".to_string()]
    );
    assert_eq!(used_after.edge_ids, vec!["e1".to_string()]);
}

#[tokio::test]
async fn test_trace_entry_dispatch() {
    let (store, sm) = make_sm();
    let owner = Uuid::new_v4();

    let entry = MemoryEntry::Trace(TraceEntry {
        origin_function: "search".into(),
        status: "success".into(),
        method_params: Some(serde_json::json!({"q": "hello"})),
        method_return_value: Some(serde_json::json!({"hits": 3})),
        memory_query: "what?".into(),
        memory_context: "ctx".into(),
        error_message: "".into(),
        generate_feedback_with_llm: false,
    });

    let result = remember_entry(
        entry,
        "ds",
        "sess-trace",
        owner,
        None,
        None,
        Some(store.clone() as Arc<dyn SessionStore>),
        Some(sm),
        None,
    )
    .await
    .expect("dispatch must succeed");

    assert_eq!(result.status, RememberStatus::SessionStored);
    assert_eq!(result.entry_type.as_deref(), Some("trace"));
    let trace_id = result.entry_id.as_ref().expect("entry_id");
    assert!(
        Uuid::parse_str(trace_id).is_ok(),
        "trace_id should be a UUID4 string, got {trace_id:?}"
    );

    let (uid, sid, step) = store.last_trace_step().expect("trace step recorded");
    assert_eq!(uid, owner.to_string());
    assert_eq!(sid, "sess-trace");
    assert_eq!(step.origin_function, "search");
    assert_eq!(step.status, "success");
    assert_eq!(step.memory_query, "what?");
    assert_eq!(step.memory_context, "ctx");
    // Gap 07 parity bump: deterministic fallback is recorded even when
    // `generate_feedback_with_llm` is false (matches Python's
    // `_fallback_agent_trace_feedback`).
    assert_eq!(step.session_feedback, "search succeeded.");
    // method_params dispatched as a json value.
    assert_eq!(step.method_params, serde_json::json!({"q": "hello"}));
}

#[tokio::test]
async fn test_feedback_entry_dispatch_returns_qa_id_on_success() {
    let (store, sm) = make_sm();
    let owner = Uuid::new_v4();

    // Default — update_qa_entry returns Ok(true).
    let entry = MemoryEntry::Feedback(FeedbackEntry {
        qa_id: "qa-existing".into(),
        feedback_text: Some("good".into()),
        feedback_score: Some(5),
    });

    let result = remember_entry(
        entry,
        "ds",
        "sess-fb",
        owner,
        None,
        None,
        Some(store.clone() as Arc<dyn SessionStore>),
        Some(sm),
        None,
    )
    .await
    .expect("dispatch must succeed");

    assert_eq!(result.status, RememberStatus::SessionStored);
    assert_eq!(result.entry_type.as_deref(), Some("feedback"));
    assert_eq!(result.entry_id.as_deref(), Some("qa-existing"));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn test_feedback_entry_returns_errored_when_qa_missing() {
    let (store, sm) = make_sm();
    let owner = Uuid::new_v4();

    // Force update_qa_entry to return Ok(false) — simulates QA not found.
    store.set_update_qa_succeeds(false);

    let entry = MemoryEntry::Feedback(FeedbackEntry {
        qa_id: "qa-missing".into(),
        feedback_text: Some("oops".into()),
        feedback_score: Some(3),
    });

    let result = remember_entry(
        entry,
        "ds",
        "sess-fb",
        owner,
        None,
        None,
        Some(store.clone() as Arc<dyn SessionStore>),
        Some(sm),
        None,
    )
    .await
    .expect("dispatch must surface as Ok with Errored status");

    assert_eq!(result.status, RememberStatus::Errored);
    assert_eq!(result.entry_type.as_deref(), Some("feedback"));
    // Python parity: entry_id is set to the input qa_id even on not-found.
    assert_eq!(result.entry_id.as_deref(), Some("qa-missing"));
    let err = result.error.as_deref().expect("error must be set");
    assert!(err.contains("qa-missing"));
    assert!(err.contains("sess-fb"));
}

#[tokio::test]
async fn test_missing_session_id_returns_error() {
    let (store, sm) = make_sm();
    let owner = Uuid::new_v4();

    let entry = MemoryEntry::Qa(QAEntry {
        question: "q".into(),
        answer: "a".into(),
        context: "".into(),
        feedback_text: None,
        feedback_score: None,
        used_graph_element_ids: None,
    });

    let err = remember_entry(
        entry,
        "ds",
        "", // empty session_id → 400-class error
        owner,
        None,
        None,
        Some(store as Arc<dyn SessionStore>),
        Some(sm),
        None,
    )
    .await
    .expect_err("empty session_id must return Err");

    match err {
        ApiError::InvalidArgument(msg) => {
            assert!(
                msg.contains("session_id"),
                "expected session_id message, got {msg:?}"
            );
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Compile-time sanity: the public types from cognee_models are reachable.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn _ensure_types_reachable() {
    let _q: QAEntry = QAEntry {
        question: "".into(),
        answer: "".into(),
        context: "".into(),
        feedback_text: None,
        feedback_score: None,
        used_graph_element_ids: None,
    };
    let _t: TraceEntry = TraceEntry {
        origin_function: "".into(),
        status: "success".into(),
        method_params: None,
        method_return_value: None,
        memory_query: "".into(),
        memory_context: "".into(),
        error_message: "".into(),
        generate_feedback_with_llm: false,
    };
    let _f: FeedbackEntry = FeedbackEntry {
        qa_id: "".into(),
        feedback_text: None,
        feedback_score: None,
    };
    // Suppress unused — this fn is intentionally not called.
    let _used: &dyn Fn() = &|| {
        let _ = UsedGraphElementIds::default();
    };
}
