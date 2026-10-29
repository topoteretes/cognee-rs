#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
#![cfg(feature = "relational-store")]

use std::sync::Arc;

use cognee_session::{RelationalSessionStore, SessionManager, SessionStore, SessionTraceStep};

async fn setup_store() -> Arc<RelationalSessionStore> {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    // RelationalSessionStore::new creates the session tables
    Arc::new(RelationalSessionStore::new(Arc::new(db)).await.unwrap())
}

#[tokio::test]
async fn create_and_retrieve_qa_entries() {
    let store = setup_store().await;

    store
        .create_qa_entry("s1", None, "What is Rust?", "A language.", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", None, "Tell me more.", "It is fast.", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", None, "And safe?", "Yes, memory safe.", None)
        .await
        .unwrap();

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].question, "What is Rust?");
    assert_eq!(entries[1].question, "Tell me more.");
    assert_eq!(entries[2].question, "And safe?");
}

#[tokio::test]
async fn get_latest_entries_respects_limit() {
    let store = setup_store().await;

    for i in 0..5 {
        store
            .create_qa_entry("s1", None, &format!("q{i}"), &format!("a{i}"), None)
            .await
            .unwrap();
    }

    let entries = store.get_latest_qa_entries("s1", None, 3).await.unwrap();
    assert_eq!(entries.len(), 3);
    // Should be the last 3, oldest-first
    assert_eq!(entries[0].question, "q2");
    assert_eq!(entries[1].question, "q3");
    assert_eq!(entries[2].question, "q4");
}

#[tokio::test]
async fn entries_isolated_by_session_id() {
    let store = setup_store().await;

    store
        .create_qa_entry("session-a", None, "qa", "aa", None)
        .await
        .unwrap();
    store
        .create_qa_entry("session-b", None, "qb", "ab", None)
        .await
        .unwrap();

    let a_entries = store.get_all_qa_entries("session-a", None).await.unwrap();
    let b_entries = store.get_all_qa_entries("session-b", None).await.unwrap();

    assert_eq!(a_entries.len(), 1);
    assert_eq!(a_entries[0].question, "qa");
    assert_eq!(b_entries.len(), 1);
    assert_eq!(b_entries[0].question, "qb");
}

#[tokio::test]
async fn entries_isolated_by_user_id() {
    let store = setup_store().await;

    store
        .create_qa_entry("s1", Some("user-1"), "q1", "a1", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", Some("user-2"), "q2", "a2", None)
        .await
        .unwrap();

    let u1 = store
        .get_all_qa_entries("s1", Some("user-1"))
        .await
        .unwrap();
    let u2 = store
        .get_all_qa_entries("s1", Some("user-2"))
        .await
        .unwrap();

    assert_eq!(u1.len(), 1);
    assert_eq!(u1[0].question, "q1");
    assert_eq!(u2.len(), 1);
    assert_eq!(u2[0].question, "q2");
}

#[tokio::test]
async fn delete_session() {
    let store = setup_store().await;

    store
        .create_qa_entry("s1", None, "q1", "a1", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", None, "q2", "a2", None)
        .await
        .unwrap();

    let deleted = store.delete_session("s1", None).await.unwrap();
    assert!(deleted);

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert!(entries.is_empty());

    // Deleting again returns false
    let deleted_again = store.delete_session("s1", None).await.unwrap();
    assert!(!deleted_again);
}

#[tokio::test]
async fn delete_single_qa_entry() {
    let store = setup_store().await;

    let id1 = store
        .create_qa_entry("s1", None, "q1", "a1", None)
        .await
        .unwrap();
    let _id2 = store
        .create_qa_entry("s1", None, "q2", "a2", None)
        .await
        .unwrap();

    let deleted = store.delete_qa_entry("s1", None, &id1).await.unwrap();
    assert!(deleted);

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].question, "q2");
}

#[tokio::test]
async fn prune_removes_all_sessions() {
    let store = setup_store().await;

    // Create entries across multiple sessions and users
    store
        .create_qa_entry("s1", Some("user-1"), "q1", "a1", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", Some("user-2"), "q2", "a2", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s2", None, "q3", "a3", None)
        .await
        .unwrap();

    // Prune all session data
    store.prune().await.unwrap();

    // Verify all entries are gone
    assert!(
        store
            .get_all_qa_entries("s1", Some("user-1"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .get_all_qa_entries("s1", Some("user-2"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .get_all_qa_entries("s2", None)
            .await
            .unwrap()
            .is_empty()
    );

    // Verify the store is still functional after prune
    store
        .create_qa_entry("s3", None, "q4", "a4", None)
        .await
        .unwrap();
    let entries = store.get_all_qa_entries("s3", None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].question, "q4");
}

#[tokio::test]
async fn prune_on_empty_store_is_ok() {
    let store = setup_store().await;

    // Prune when nothing exists should succeed
    store.prune().await.unwrap();

    // Store remains functional
    store
        .create_qa_entry("s1", None, "q1", "a1", None)
        .await
        .unwrap();
    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries.len(), 1);
}

#[tokio::test]
async fn session_manager_load_history_returns_user_assistant_pairs() {
    let store = setup_store().await;

    store
        .create_qa_entry("s1", None, "Hello?", "Hi there!", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", None, "What is 2+2?", "4", None)
        .await
        .unwrap();

    let sm = SessionManager::new(store as Arc<dyn SessionStore>);
    let messages = sm.load_history_messages(Some("s1"), None).await.unwrap();

    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].content, "Hello?");
    assert_eq!(messages[1].content, "Hi there!");
    assert_eq!(messages[2].content, "What is 2+2?");
    assert_eq!(messages[3].content, "4");
}

#[tokio::test]
async fn session_manager_load_empty_history() {
    let store = setup_store().await;
    let sm = SessionManager::new(store as Arc<dyn SessionStore>);
    let messages = sm
        .load_history_messages(Some("nonexistent"), None)
        .await
        .unwrap();
    assert!(messages.is_empty());
}

#[tokio::test]
async fn session_manager_save_and_load_round_trip() {
    let store = setup_store().await;
    let sm = SessionManager::new(store as Arc<dyn SessionStore>);

    sm.save_qa(Some("s1"), None, "q1", "a1", Some("ctx1"), None)
        .await
        .unwrap();
    sm.save_qa(Some("s1"), None, "q2", "a2", None, None)
        .await
        .unwrap();

    let messages = sm.load_history_messages(Some("s1"), None).await.unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].content, "q1");
    assert_eq!(messages[1].content, "a1");
    assert_eq!(messages[2].content, "q2");
    assert_eq!(messages[3].content, "a2");
}

// -------------------------------------------------------------------------
// Tests for gap-6 session management features
// -------------------------------------------------------------------------

#[tokio::test]
async fn new_entries_have_no_feedback_fields() {
    let store = setup_store().await;

    store
        .create_qa_entry("s1", None, "q1?", "a1", None)
        .await
        .unwrap();

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].feedback_text.is_none());
    assert!(entries[0].feedback_score.is_none());
    assert!(entries[0].used_graph_element_ids.is_none());
    assert!(entries[0].memify_metadata.is_none());
}

#[tokio::test]
async fn update_qa_entry_sets_feedback() {
    use cognee_session::SessionQAUpdate;

    let store = setup_store().await;

    let qa_id = store
        .create_qa_entry("s1", None, "q1?", "a1", None)
        .await
        .unwrap();

    let updated = store
        .update_qa_entry(
            "s1",
            None,
            &qa_id,
            SessionQAUpdate {
                feedback_text: Some(Some("Excellent".to_string())),
                feedback_score: Some(Some(5)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(updated);

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries[0].feedback_text.as_deref(), Some("Excellent"));
    assert_eq!(entries[0].feedback_score, Some(5));
}

#[tokio::test]
async fn update_qa_entry_clears_feedback() {
    use cognee_session::SessionQAUpdate;

    let store = setup_store().await;

    let qa_id = store
        .create_qa_entry("s1", None, "q1?", "a1", None)
        .await
        .unwrap();

    // Set
    store
        .update_qa_entry(
            "s1",
            None,
            &qa_id,
            SessionQAUpdate {
                feedback_text: Some(Some("Good".to_string())),
                feedback_score: Some(Some(4)),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Clear
    store
        .update_qa_entry(
            "s1",
            None,
            &qa_id,
            SessionQAUpdate {
                feedback_text: Some(None),
                feedback_score: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert!(entries[0].feedback_text.is_none());
    assert!(entries[0].feedback_score.is_none());
}

#[tokio::test]
async fn update_qa_entry_not_found() {
    use cognee_session::SessionQAUpdate;

    let store = setup_store().await;

    let result = store
        .update_qa_entry("s1", None, "nonexistent", SessionQAUpdate::default())
        .await
        .unwrap();
    assert!(!result);
}

#[tokio::test]
async fn graph_context_round_trip() {
    let store = setup_store().await;

    // Initially no context
    let ctx = store.get_graph_context("s1", None).await.unwrap();
    assert!(ctx.is_none());

    // Set
    store
        .set_graph_context("s1", None, "Some graph data")
        .await
        .unwrap();

    let ctx = store.get_graph_context("s1", None).await.unwrap();
    assert_eq!(ctx.as_deref(), Some("Some graph data"));

    // Overwrite
    store
        .set_graph_context("s1", None, "Updated graph data")
        .await
        .unwrap();

    let ctx = store.get_graph_context("s1", None).await.unwrap();
    assert_eq!(ctx.as_deref(), Some("Updated graph data"));
}

#[tokio::test]
async fn graph_context_isolated_by_user() {
    let store = setup_store().await;

    store
        .set_graph_context("s1", Some("user-1"), "context for user 1")
        .await
        .unwrap();
    store
        .set_graph_context("s1", Some("user-2"), "context for user 2")
        .await
        .unwrap();

    let ctx1 = store.get_graph_context("s1", Some("user-1")).await.unwrap();
    let ctx2 = store.get_graph_context("s1", Some("user-2")).await.unwrap();

    assert_eq!(ctx1.as_deref(), Some("context for user 1"));
    assert_eq!(ctx2.as_deref(), Some("context for user 2"));
}

#[tokio::test]
async fn delete_session_also_removes_graph_context() {
    let store = setup_store().await;

    store
        .create_qa_entry("s1", None, "q", "a", None)
        .await
        .unwrap();
    store
        .set_graph_context("s1", None, "some context")
        .await
        .unwrap();

    store.delete_session("s1", None).await.unwrap();

    let ctx = store.get_graph_context("s1", None).await.unwrap();
    assert!(ctx.is_none());
}

#[tokio::test]
async fn session_manager_add_and_delete_feedback() {
    let store = setup_store().await;
    let sm = SessionManager::new(store as Arc<dyn SessionStore>);

    let qa_id = sm
        .save_qa(Some("s1"), None, "q1", "a1", None, None)
        .await
        .unwrap();

    // Add feedback
    let ok = sm
        .add_feedback(Some("s1"), None, &qa_id, Some("Great!"), Some(5))
        .await
        .unwrap();
    assert!(ok);

    // Delete feedback
    let ok = sm.delete_feedback(Some("s1"), None, &qa_id).await.unwrap();
    assert!(ok);
}

#[tokio::test]
async fn session_manager_graph_context_round_trip() {
    let store = setup_store().await;
    let sm = SessionManager::new(store as Arc<dyn SessionStore>);

    // No context initially
    let ctx = sm.get_graph_context(Some("s1"), None).await.unwrap();
    assert!(ctx.is_none());

    sm.set_graph_context(Some("s1"), None, "knowledge snapshot")
        .await
        .unwrap();

    let ctx = sm.get_graph_context(Some("s1"), None).await.unwrap();
    assert_eq!(ctx.as_deref(), Some("knowledge snapshot"));
}

// -------------------------------------------------------------------------
// Tests for LIB-02 — agent trace step persistence
// -------------------------------------------------------------------------

fn make_step(trace_id: &str, origin: &str) -> SessionTraceStep {
    SessionTraceStep {
        trace_id: trace_id.to_string(),
        origin_function: origin.to_string(),
        status: "success".to_string(),
        memory_query: "q?".to_string(),
        memory_context: "ctx".to_string(),
        method_params: serde_json::json!({"k": "v"}),
        method_return_value: Some(serde_json::json!([1, 2, 3])),
        error_message: String::new(),
        session_feedback: "fb".to_string(),
    }
}

#[tokio::test]
async fn test_sea_orm_save_and_read_trace_step() {
    let store = setup_store().await;
    let step = make_step("trace-1", "fn.foo");

    let returned = store
        .save_trace_step("user-a", "sess-1", step)
        .await
        .unwrap();
    assert_eq!(returned, "trace-1");

    let read = store.read_trace_steps("user-a", "sess-1").await.unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].trace_id, "trace-1");
    assert_eq!(read[0].origin_function, "fn.foo");
    assert_eq!(read[0].status, "success");
    assert_eq!(read[0].memory_query, "q?");
    assert_eq!(read[0].memory_context, "ctx");
    assert_eq!(read[0].method_params, serde_json::json!({"k": "v"}));
    assert_eq!(
        read[0].method_return_value,
        Some(serde_json::json!([1, 2, 3]))
    );
    assert_eq!(read[0].session_feedback, "fb");
}

#[tokio::test]
async fn test_sea_orm_trace_steps_oldest_first() {
    let store = setup_store().await;

    for i in 0..3 {
        let step = make_step(&format!("trace-{i}"), "fn");
        store
            .save_trace_step("user-a", "sess-1", step)
            .await
            .unwrap();
    }

    let read = store.read_trace_steps("user-a", "sess-1").await.unwrap();
    assert_eq!(read.len(), 3);
    assert_eq!(read[0].trace_id, "trace-0");
    assert_eq!(read[1].trace_id, "trace-1");
    assert_eq!(read[2].trace_id, "trace-2");
}

#[tokio::test]
async fn test_sea_orm_trace_step_isolation() {
    let store = setup_store().await;

    store
        .save_trace_step("user-a", "sess-1", make_step("a-1", "fn"))
        .await
        .unwrap();
    store
        .save_trace_step("user-b", "sess-1", make_step("b-1", "fn"))
        .await
        .unwrap();
    store
        .save_trace_step("user-a", "sess-2", make_step("a-2", "fn"))
        .await
        .unwrap();

    let a1 = store.read_trace_steps("user-a", "sess-1").await.unwrap();
    let b1 = store.read_trace_steps("user-b", "sess-1").await.unwrap();
    let a2 = store.read_trace_steps("user-a", "sess-2").await.unwrap();

    assert_eq!(a1.len(), 1);
    assert_eq!(a1[0].trace_id, "a-1");
    assert_eq!(b1.len(), 1);
    assert_eq!(b1[0].trace_id, "b-1");
    assert_eq!(a2.len(), 1);
    assert_eq!(a2[0].trace_id, "a-2");
}

// ── session-context rows ────────────────────────────────────────────────────

fn ctx_row(value: serde_json::Value) -> cognee_session::SessionContextRow {
    match value {
        serde_json::Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

fn guidance(id: &str, content: &str) -> cognee_session::SessionContextRow {
    ctx_row(serde_json::json!({
        "id": id,
        "kind": "context",
        "section": "rules",
        "content": content,
        "created_at": "2026-10-28T00:00:00+00:00",
    }))
}

#[tokio::test]
async fn context_rows_round_trip_in_insertion_order() {
    let store = setup_store().await;
    store
        .create_session_context_entry("s1", Some("u"), guidance("e1", "first"))
        .await
        .unwrap();
    store
        .create_session_context_entry(
            "s1",
            Some("u"),
            ctx_row(serde_json::json!({"id": "wm", "kind": "wm_state", "done_count": 2})),
        )
        .await
        .unwrap();
    store
        .create_session_context_entry("s1", Some("u"), guidance("e2", "second"))
        .await
        .unwrap();

    let rows = store
        .get_session_context_entries("s1", Some("u"))
        .await
        .unwrap();
    let ids: Vec<_> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["e1", "wm", "e2"]);
    assert_eq!(rows[1]["done_count"], serde_json::json!(2));

    // State rows stay out of the rendered context and out of the Q&A history.
    let active = cognee_session::coerce_active_context_entries(&rows).unwrap();
    assert_eq!(active.len(), 2);
    assert!(
        store
            .get_all_qa_entries("s1", Some("u"))
            .await
            .unwrap()
            .is_empty()
    );
    // Other users and sessions see nothing.
    assert!(
        store
            .get_session_context_entries("s1", Some("other"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .get_session_context_entries("s2", Some("u"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn update_context_row_merges_shallowly() {
    let store = setup_store().await;
    store
        .create_session_context_entry("s1", Some("u"), guidance("e1", "first"))
        .await
        .unwrap();

    let merged = store
        .update_session_context_entry(
            "s1",
            Some("u"),
            "e1",
            ctx_row(serde_json::json!({"helpful_count": 3, "content": "updated"})),
        )
        .await
        .unwrap();
    assert!(merged);
    let rows = store
        .get_session_context_entries("s1", Some("u"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["content"], serde_json::json!("updated"));
    assert_eq!(rows[0]["helpful_count"], serde_json::json!(3));
    assert_eq!(rows[0]["section"], serde_json::json!("rules"));

    let missing = store
        .update_session_context_entry("s1", Some("u"), "nope", guidance("nope", "x"))
        .await
        .unwrap();
    assert!(!missing);
}

#[tokio::test]
async fn watermark_writes_keep_one_row_on_the_store() {
    let store = setup_store().await;
    let manager = SessionManager::new(store.clone());
    let wm = cognee_session::SESSION_PERSIST_WATERMARK;
    wm.write_count(&manager, Some("s1"), Some("u"), 4)
        .await
        .unwrap();
    wm.write_count(&manager, Some("s1"), Some("u"), 7)
        .await
        .unwrap();
    assert_eq!(
        wm.read_count(&manager, Some("s1"), Some("u"))
            .await
            .unwrap(),
        7
    );
    assert_eq!(
        manager
            .get_session_context_entries(Some("s1"), Some("u"))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn delete_session_removes_context_rows() {
    let store = setup_store().await;
    store
        .create_session_context_entry("s1", Some("u"), guidance("e1", "first"))
        .await
        .unwrap();
    store
        .create_session_context_entry("s2", Some("u"), guidance("e1", "other"))
        .await
        .unwrap();

    // A session holding only context rows still existed.
    assert!(store.delete_session("s1", Some("u")).await.unwrap());
    assert!(
        store
            .get_session_context_entries("s1", Some("u"))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .get_session_context_entries("s2", Some("u"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!store.delete_session("s1", Some("u")).await.unwrap());

    store.prune().await.unwrap();
    assert!(
        store
            .get_session_context_entries("s2", Some("u"))
            .await
            .unwrap()
            .is_empty()
    );
}

/// The relational store upserts on the row's id (Python's `SqlCacheAdapter`),
/// so a create racing an update converges on one row.
#[tokio::test]
async fn create_context_row_upserts_on_its_id() {
    let store = setup_store().await;
    store
        .create_session_context_entry("s1", Some("u"), guidance("e1", "first"))
        .await
        .unwrap();
    store
        .create_session_context_entry("s1", Some("u"), guidance("e2", "second"))
        .await
        .unwrap();
    store
        .create_session_context_entry("s1", Some("u"), guidance("e1", "replaced"))
        .await
        .unwrap();

    let rows = store
        .get_session_context_entries("s1", Some("u"))
        .await
        .unwrap();
    let contents: Vec<_> = rows
        .iter()
        .map(|r| r["content"].as_str().unwrap())
        .collect();
    assert_eq!(contents, ["replaced", "second"]);

    // An id-less row is kept (under a synthetic key) and never collides.
    store
        .create_session_context_entry("s1", Some("u"), ctx_row(serde_json::json!({"kind": "x"})))
        .await
        .unwrap();
    store
        .create_session_context_entry("s1", Some("u"), ctx_row(serde_json::json!({"kind": "x"})))
        .await
        .unwrap();
    assert_eq!(
        store
            .get_session_context_entries("s1", Some("u"))
            .await
            .unwrap()
            .len(),
        4
    );
}

// Python `uq_cache_session_context_entry`: a create of an existing entry id
// replaces its payload in place (one row, same position), and concurrent
// creates of one entry converge on one row.
#[tokio::test]
async fn context_create_upserts_on_the_entry_key() {
    let store = setup_store().await;
    for (id, content) in [("e1", "first"), ("e2", "second"), ("e1", "replaced")] {
        store
            .create_session_context_entry("s1", Some("u"), guidance(id, content))
            .await
            .unwrap();
    }
    let rows = store
        .get_session_context_entries("s1", Some("u"))
        .await
        .unwrap();
    let seen: Vec<_> = rows
        .iter()
        .map(|r| (r["id"].as_str().unwrap(), r["content"].as_str().unwrap()))
        .collect();
    assert_eq!(seen, [("e1", "replaced"), ("e2", "second")]);

    let mut writes = tokio::task::JoinSet::new();
    for n in 0..8 {
        let store = Arc::clone(&store);
        writes.spawn(async move {
            store
                .create_session_context_entry("s2", Some("u"), guidance("wm", &n.to_string()))
                .await
        });
    }
    while let Some(result) = writes.join_next().await {
        result.unwrap().unwrap();
    }
    assert_eq!(
        store
            .get_session_context_entries("s2", Some("u"))
            .await
            .unwrap()
            .len(),
        1
    );
}

// The relational store keys a session without a user like the file and Redis
// stores ("default"): it sees only that bucket's rows, never every user's.
#[tokio::test]
async fn context_rows_without_a_user_are_their_own_bucket() {
    let store = setup_store().await;
    store
        .create_session_context_entry("s1", Some("u"), guidance("e1", "mine"))
        .await
        .unwrap();
    assert!(
        store
            .get_session_context_entries("s1", None)
            .await
            .unwrap()
            .is_empty()
    );
    store
        .create_session_context_entry("s1", None, guidance("e1", "anonymous"))
        .await
        .unwrap();
    store
        .create_session_context_entry("s1", None, guidance("e1", "anonymous again"))
        .await
        .unwrap();
    let anonymous = store.get_session_context_entries("s1", None).await.unwrap();
    assert_eq!(anonymous.len(), 1);
    assert_eq!(anonymous[0]["content"], "anonymous again");
    assert!(
        !store
            .update_session_context_entry("s1", None, "e2", guidance("e2", "x"))
            .await
            .unwrap()
    );
    let mine = store
        .get_session_context_entries("s1", Some("u"))
        .await
        .unwrap();
    assert_eq!(mine[0]["content"], "mine");
}

/// `memify_metadata` writes overlay their keys on the stored ones (Python's
/// `update_qa` merge), so one stage's flag survives another's write.
#[tokio::test]
async fn memify_metadata_updates_merge_keys() {
    let store = setup_store().await;
    let qa_id = store
        .create_qa_entry("s1", None, "q1?", "a1", None)
        .await
        .unwrap();
    for (key, value) in [
        ("feedback_weights_applied", true),
        ("preference_turn_counted", true),
        ("feedback_weights_applied", false),
    ] {
        store
            .update_qa_entry(
                "s1",
                None,
                &qa_id,
                cognee_session::SessionQAUpdate {
                    memify_metadata: Some(Some(std::collections::HashMap::from([(
                        key.to_string(),
                        value,
                    )]))),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    let metadata = entries[0].memify_metadata.as_ref().unwrap();
    assert_eq!(metadata.get("feedback_weights_applied"), Some(&false));
    assert_eq!(metadata.get("preference_turn_counted"), Some(&true));

    // `Some(None)` still clears it.
    store
        .update_qa_entry(
            "s1",
            None,
            &qa_id,
            cognee_session::SessionQAUpdate {
                memify_metadata: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert!(entries[0].memify_metadata.is_none());
}
