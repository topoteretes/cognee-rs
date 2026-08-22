#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    dead_code,
    reason = "test code — panics are acceptable failures"
)]

use std::sync::Arc;

use cognee_session::{SessionError, SessionStore, SessionTraceStep, external_event_entry_id};

const SESSION: &str = "session-a";
const USER: &str = "user-a";
const EVENT: &str = "evt-123";

#[test]
fn external_event_entry_ids_use_the_documented_uuid_v5_namespace() {
    assert_eq!(
        external_event_entry_id(EVENT),
        "739236c9-9bd6-53f3-97ac-e5ee095ad964"
    );
    assert_eq!(
        external_event_entry_id(EVENT),
        external_event_entry_id(EVENT)
    );
    assert_ne!(
        external_event_entry_id(EVENT),
        external_event_entry_id("evt-124")
    );
}

async fn exercise_qa_exact_once(store: Arc<dyn SessionStore>) {
    let entry_id = external_event_entry_id(EVENT);
    let first = store
        .create_qa_entry_with_id(
            &entry_id,
            SESSION,
            Some(USER),
            "question",
            "answer",
            Some("context"),
            Some(EVENT),
        )
        .await
        .expect("first save");
    let replay = store
        .create_qa_entry_with_id(
            &entry_id,
            SESSION,
            Some(USER),
            "question",
            "answer",
            Some("context"),
            Some(EVENT),
        )
        .await
        .expect("identical replay");

    assert_eq!(first, replay);
    assert_eq!(first, entry_id);
    let entries = store
        .get_all_qa_entries(SESSION, Some(USER))
        .await
        .expect("entries");
    assert_eq!(entries.len(), 1, "an identical replay must not append");
    assert_eq!(entries[0].external_event_id.as_deref(), Some(EVENT));
    assert!(
        store
            .contains_external_event(SESSION, Some(USER), EVENT)
            .await
            .expect("contains")
    );

    let conflict = store
        .create_qa_entry_with_id(
            &entry_id,
            SESSION,
            Some(USER),
            "question",
            "different answer",
            Some("context"),
            Some(EVENT),
        )
        .await
        .expect_err("same event with different content must conflict");
    assert!(matches!(
        conflict,
        SessionError::ExternalEventConflict { .. }
    ));

    let legacy_a = store
        .create_qa_entry(SESSION, Some(USER), "same", "same", None)
        .await
        .expect("legacy save a");
    let legacy_b = store
        .create_qa_entry(SESSION, Some(USER), "same", "same", None)
        .await
        .expect("legacy save b");
    assert_ne!(legacy_a, legacy_b, "no-key callers retain UUID4 behavior");
}

fn trace(event_id: &str, status: &str) -> SessionTraceStep {
    SessionTraceStep {
        trace_id: external_event_entry_id(event_id),
        external_event_id: Some(event_id.to_string()),
        origin_function: "tool".to_string(),
        status: status.to_string(),
        memory_query: "query".to_string(),
        memory_context: "context".to_string(),
        method_params: serde_json::json!({"path":"/tmp/example"}),
        method_return_value: Some(serde_json::json!({"ok":true})),
        error_message: String::new(),
        session_feedback: "tool succeeded.".to_string(),
    }
}

async fn exercise_trace_exact_once(store: Arc<dyn SessionStore>) {
    let first = store
        .save_trace_step(USER, SESSION, trace("evt-trace", "success"))
        .await
        .expect("first trace");
    let replay = store
        .save_trace_step(USER, SESSION, trace("evt-trace", "success"))
        .await
        .expect("trace replay");
    assert_eq!(first, replay);

    let steps = store
        .read_trace_steps(USER, SESSION)
        .await
        .expect("trace steps");
    assert_eq!(steps.len(), 1, "an identical trace replay must not append");
    assert_eq!(steps[0].external_event_id.as_deref(), Some("evt-trace"));

    let conflict = store
        .save_trace_step(USER, SESSION, trace("evt-trace", "error"))
        .await
        .expect_err("same trace event with different content must conflict");
    assert!(matches!(
        conflict,
        SessionError::ExternalEventConflict { .. }
    ));
}

#[cfg(feature = "fs")]
#[tokio::test]
async fn filesystem_store_is_exact_once_for_qa_and_trace_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn SessionStore> = Arc::new(cognee_session::FsSessionStore::new(dir.path()));
    exercise_qa_exact_once(Arc::clone(&store)).await;
    exercise_trace_exact_once(store).await;
}

#[cfg(feature = "sea-orm-store")]
#[tokio::test]
async fn sea_orm_store_is_exact_once_for_qa_and_trace_entries() {
    let db = sea_orm::Database::connect("sqlite::memory:")
        .await
        .expect("connect");
    let store: Arc<dyn SessionStore> = Arc::new(
        cognee_session::SeaOrmSessionStore::new(Arc::new(db))
            .await
            .expect("store"),
    );
    exercise_qa_exact_once(Arc::clone(&store)).await;
    exercise_trace_exact_once(store).await;
}

#[cfg(feature = "redis")]
#[tokio::test]
async fn redis_store_is_exact_once_when_a_test_backend_is_available() {
    let Ok(url) = std::env::var("COGNEE_TEST_REDIS_URL") else {
        return;
    };
    let store: Arc<dyn SessionStore> = Arc::new(
        cognee_session::RedisSessionStore::new(&url)
            .await
            .expect("redis store"),
    );
    store.prune().await.expect("clean redis fixture");
    exercise_qa_exact_once(Arc::clone(&store)).await;
    exercise_trace_exact_once(store).await;
}
