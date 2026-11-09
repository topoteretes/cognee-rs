#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `collect_session_events` end to end over SQLite: the lifecycle listing
//! (dataset scoping, owner only) and the session entries it reads. Port of
//! the collection cases of Python's `test_session_event_scoping.py`.
#![cfg(all(feature = "session-events", feature = "relational-store"))]

use std::sync::Arc;

use cognee_database::{DatabaseConnection, SessionLifecycleDb};
use cognee_session::{
    DisabledSessionStore, RelationalSessionStore, SessionManager, UsedGraphElementIds,
    collect_session_events,
};
use serde_json::Value;
use uuid::Uuid;

const DATASET_A: Uuid = Uuid::from_u128(0xaaaaaaaa_1111_4111_8111_1111111111ab);
const DATASET_B: Uuid = Uuid::from_u128(0xbbbbbbbb_2222_4222_8222_2222222222cd);
const USER: Uuid = Uuid::from_u128(0xcccccccc_3333_4333_8333_3333333333ef);

async fn setup() -> (DatabaseConnection, SessionManager) {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
    let store = RelationalSessionStore::new(Arc::new(db.clone()))
        .await
        .unwrap();
    (db, SessionManager::new(Arc::new(store)))
}

/// A lifecycle row plus one QA entry asking `q-{session_id}`.
async fn session(
    db: &DatabaseConnection,
    manager: &SessionManager,
    session_id: &str,
    dataset_id: Option<Uuid>,
    user: Uuid,
) -> String {
    db.ensure_and_touch_session(session_id, user, dataset_id)
        .await
        .unwrap();
    manager
        .save_qa(
            Some(session_id),
            Some(&user.to_string()),
            &format!("q-{session_id}"),
            "a",
            Some(""),
            Some(UsedGraphElementIds {
                node_ids: vec!["n1".to_string()],
                edge_ids: vec![],
            }),
            None,
        )
        .await
        .unwrap()
}

fn questions(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|e| e["question"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn explicit_session_ids_bypass_the_dataset_filter() {
    let (db, manager) = setup().await;
    session(&db, &manager, "session-b", Some(DATASET_B), USER).await;

    let ids = vec!["session-b".to_string()];
    let events = collect_session_events(&db, &manager, USER, Some(DATASET_A), Some(&ids)).await;
    assert_eq!(questions(&events), ["q-session-b"]);
}

#[tokio::test]
async fn listed_sessions_are_the_callers_own_on_that_dataset() {
    let (db, manager) = setup().await;
    session(&db, &manager, "session-a", Some(DATASET_A), USER).await;
    session(&db, &manager, "session-b", Some(DATASET_B), USER).await;
    session(&db, &manager, "opaque", None, USER).await;
    session(
        &db,
        &manager,
        &format!("default_session_{DATASET_A}"),
        None,
        USER,
    )
    .await;
    // Another user's session on the same dataset, suffix and all.
    let other = Uuid::new_v4();
    session(&db, &manager, "theirs", Some(DATASET_A), other).await;

    let mut scoped =
        questions(&collect_session_events(&db, &manager, USER, Some(DATASET_A), None).await)
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
    scoped.sort();
    assert_eq!(
        scoped,
        [
            format!("q-default_session_{DATASET_A}"),
            "q-session-a".to_string()
        ]
    );

    let all = collect_session_events(&db, &manager, USER, None, None).await;
    assert_eq!(all.len(), 4, "every own session, no one else's");
    assert!(!questions(&all).contains(&"q-theirs"));
    // Sorted by time.
    let times: Vec<&str> = all.iter().map(|e| e["time"].as_str().unwrap()).collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted);
}

#[tokio::test]
async fn rated_entries_emit_improve_events() {
    let (db, manager) = setup().await;
    let qa_id = session(&db, &manager, "rated", Some(DATASET_A), USER).await;
    manager
        .add_feedback(
            Some("rated"),
            Some(&USER.to_string()),
            &qa_id,
            Some("good"),
            Some(5),
        )
        .await
        .unwrap();
    let events = collect_session_events(&db, &manager, USER, Some(DATASET_A), None).await;
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["search", "improve"]);
    assert_eq!(events[1]["rating"], 5);
    assert_eq!(events[1]["applied"], false);
}

#[tokio::test]
async fn an_unavailable_layer_yields_no_events() {
    // Session memory switched off.
    let (db, manager) = setup().await;
    session(&db, &manager, "s", None, USER).await;
    let disabled = SessionManager::new(Arc::new(DisabledSessionStore));
    assert!(
        collect_session_events(&db, &disabled, USER, None, None)
            .await
            .is_empty()
    );

    // No lifecycle table: the listing fails, collection returns [].
    let bare = cognee_database::connect("sqlite::memory:").await.unwrap();
    assert!(
        collect_session_events(&bare, &manager, USER, None, None)
            .await
            .is_empty()
    );
}
