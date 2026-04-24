//! Stage 1 integration tests — `apply_feedback_weights_pipeline`.
#![cfg(feature = "testing")]

use std::sync::Arc;

use cognee_cognify::memify::feedback_weights::{
    EDGE_ID_DELIMITER, FEEDBACK_WEIGHTS_APPLIED_KEY, apply_feedback_weights_pipeline,
};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_session::{
    FsSessionStore, SessionManager, SessionQAUpdate, SessionStore, UsedGraphElementIds,
};
use serde_json::json;
use uuid::Uuid;

async fn add_node_with_weight(db: &MockGraphDB, id: &str, weight: f64) {
    db.add_node_raw(json!({
        "id": id,
        "name": id,
        "type": "Entity",
        "feedback_weight": weight,
    }))
    .await
    .unwrap();
}

fn make_store_and_manager() -> (
    tempfile::TempDir,
    Arc<dyn SessionStore>,
    Arc<SessionManager>,
) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new(dir.path()));
    let manager = Arc::new(SessionManager::new(Arc::clone(&store)));
    (dir, store, manager)
}

async fn create_qa_with_feedback(
    store: &dyn SessionStore,
    manager: &SessionManager,
    session_id: &str,
    user_id: &str,
    node_ids: Vec<String>,
    edge_ids: Vec<String>,
    feedback_score: i32,
) -> String {
    let qa_id = store
        .create_qa_entry(session_id, Some(user_id), "Q", "A", None)
        .await
        .unwrap();
    manager
        .update_qa(
            Some(session_id),
            Some(user_id),
            &qa_id,
            SessionQAUpdate {
                feedback_score: Some(Some(feedback_score)),
                used_graph_element_ids: Some(Some(UsedGraphElementIds { node_ids, edge_ids })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    qa_id
}

#[tokio::test]
async fn feedback_high_score_pushes_weights_up() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-high";

    let graph = MockGraphDB::new();
    add_node_with_weight(&graph, "n1", 0.5).await;
    add_node_with_weight(&graph, "n2", 0.5).await;

    create_qa_with_feedback(
        store.as_ref(),
        manager.as_ref(),
        session_id,
        &user_id,
        vec!["n1".to_string(), "n2".to_string()],
        vec![],
        5,
    )
    .await;

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.processed, 1);
    assert_eq!(res.applied, 1);
    assert_eq!(res.skipped, 0);

    // Node weights should have moved towards 1.0. stream_update_weight(0.5, 1.0, 0.1) = 0.55
    let w1 = graph
        .get_node_feedback_weights(&["n1".to_string(), "n2".to_string()])
        .await
        .unwrap();
    assert_eq!(w1.len(), 2);
    for (_, v) in w1 {
        assert!((v - 0.55).abs() < 1e-9, "expected 0.55, got {v}");
    }
}

#[tokio::test]
async fn feedback_low_score_pushes_weights_down() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-low";

    let graph = MockGraphDB::new();
    add_node_with_weight(&graph, "n1", 0.5).await;

    create_qa_with_feedback(
        store.as_ref(),
        manager.as_ref(),
        session_id,
        &user_id,
        vec!["n1".to_string()],
        vec![],
        1,
    )
    .await;

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.applied, 1);

    let w = graph
        .get_node_feedback_weights(&["n1".to_string()])
        .await
        .unwrap();
    let v = *w.get("n1").unwrap();
    // stream_update_weight(0.5, 0.0, 0.1) = 0.45
    assert!((v - 0.45).abs() < 1e-9, "expected 0.45, got {v}");
}

#[tokio::test]
async fn feedback_idempotent_second_run_skips() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-idem";

    let graph = MockGraphDB::new();
    add_node_with_weight(&graph, "n1", 0.5).await;

    create_qa_with_feedback(
        store.as_ref(),
        manager.as_ref(),
        session_id,
        &user_id,
        vec!["n1".to_string()],
        vec![],
        5,
    )
    .await;

    let first = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(first.processed, 1);

    // Verify the memify_metadata was set.
    let entries = store
        .get_all_qa_entries(session_id, Some(&user_id))
        .await
        .unwrap();
    let meta = entries[0].memify_metadata.as_ref().expect("metadata set");
    assert_eq!(meta.get(FEEDBACK_WEIGHTS_APPLIED_KEY).copied(), Some(true));

    // Second run should skip.
    let second = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(second.processed, 0);
    assert_eq!(second.skipped, 1);
}

#[tokio::test]
async fn feedback_skips_entries_without_graph_ids() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-noids";

    let graph = MockGraphDB::new();

    // Entry without used_graph_element_ids (not eligible)
    let _qa_id = store
        .create_qa_entry(session_id, Some(&user_id), "Q", "A", None)
        .await
        .unwrap();
    // Note: we left feedback_score and used_graph_element_ids as None
    // via create_qa_entry.

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.processed, 0);
    assert_eq!(res.skipped, 1);
}

#[tokio::test]
async fn feedback_skips_entries_with_invalid_score() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-invalid";

    let graph = MockGraphDB::new();

    // Score 0 is invalid (must be 1-5)
    let qa_id = store
        .create_qa_entry(session_id, Some(&user_id), "Q", "A", None)
        .await
        .unwrap();
    manager
        .update_qa(
            Some(session_id),
            Some(&user_id),
            &qa_id,
            SessionQAUpdate {
                feedback_score: Some(Some(0)),
                used_graph_element_ids: Some(Some(UsedGraphElementIds {
                    node_ids: vec!["n1".into()],
                    edge_ids: vec![],
                })),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.processed, 0);
    assert_eq!(res.skipped, 1);
}

#[tokio::test]
async fn feedback_rejects_bad_alpha() {
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let graph = MockGraphDB::new();

    let r1 = apply_feedback_weights_pipeline(
        &["s".to_string()],
        owner,
        0.0,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await;
    assert!(r1.is_err());

    let r2 = apply_feedback_weights_pipeline(
        &["s".to_string()],
        owner,
        1.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await;
    assert!(r2.is_err());
}

#[tokio::test]
async fn feedback_handles_edge_ids_on_mock() {
    // MockGraphDB's default `get_edge_feedback_weights` returns an empty
    // map and the default `update_edge_property` is a warning no-op — so
    // the pipeline should report the entry as processed but not applied
    // when there are edge-only ids (node_success true, edge_success false
    // because the in-map reading returns nothing).
    let (_dir, store, manager) = make_store_and_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-edge";

    let graph = MockGraphDB::new();
    add_node_with_weight(&graph, "n1", 0.5).await;

    let edge_id = format!("src{EDGE_ID_DELIMITER}tgt{EDGE_ID_DELIMITER}rel");
    create_qa_with_feedback(
        store.as_ref(),
        manager.as_ref(),
        session_id,
        &user_id,
        vec!["n1".to_string()],
        vec![edge_id],
        5,
    )
    .await;

    let res = apply_feedback_weights_pipeline(
        &[session_id.to_string()],
        owner,
        0.1,
        &graph,
        Arc::clone(&store),
        Arc::clone(&manager),
    )
    .await
    .unwrap();
    assert_eq!(res.processed, 1);
    // Edge update returns empty weights from MockGraphDB default, so
    // applied=0.
    assert_eq!(res.applied, 0);
}
