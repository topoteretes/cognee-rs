//! Stage 4 integration tests — `sync_graph_to_session`.
#![cfg(feature = "testing")]

use std::sync::Arc;

use chrono::{Duration, Utc};
use cognee_cognify::memify::sync_graph_session::{DEFAULT_MAX_LINES, sync_graph_to_session};
use cognee_database::ops::{datasets, graph_storage};
use cognee_database::{
    DatabaseConnection, GraphEdge, GraphNode, SeaOrmCheckpointStore, connect, initialize,
};
use cognee_models::Dataset;
use cognee_session::{FsSessionStore, SessionManager, SessionStore};
use serde_json::json;
use uuid::Uuid;

async fn setup_db() -> (tempfile::TempDir, Arc<DatabaseConnection>) {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("cognee.db");
    std::fs::File::create(&db_path).unwrap();
    let url = format!("sqlite://{}", db_path.display());
    let db = connect(&url).await.unwrap();
    initialize(&db).await.unwrap();
    (temp, Arc::new(db))
}

fn make_node(dataset_id: Uuid, owner_id: Uuid, label: &str, node_type: &str) -> GraphNode {
    GraphNode {
        id: Uuid::new_v4(),
        slug: Uuid::new_v4(),
        user_id: owner_id,
        data_id: Uuid::new_v4(),
        dataset_id,
        label: Some(label.to_string()),
        node_type: node_type.to_string(),
        indexed_fields: json!({}),
        attributes: Some(json!({"description": format!("{label} described")})),
        created_at: Utc::now(),
    }
}

fn make_edge(
    dataset_id: Uuid,
    owner_id: Uuid,
    src: Uuid,
    dst: Uuid,
    rel: &str,
    created_at: chrono::DateTime<Utc>,
) -> GraphEdge {
    GraphEdge {
        id: Uuid::new_v4(),
        slug: Uuid::new_v4(),
        user_id: owner_id,
        data_id: Uuid::new_v4(),
        dataset_id,
        source_node_id: src,
        destination_node_id: dst,
        relationship_name: rel.to_string(),
        label: None,
        attributes: None,
        created_at,
    }
}

fn make_session_manager() -> (
    tempfile::TempDir,
    Arc<SessionManager>,
    Arc<dyn SessionStore>,
) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new(dir.path()));
    let mgr = Arc::new(SessionManager::new(Arc::clone(&store)));
    (dir, mgr, store)
}

#[tokio::test]
async fn sync_first_run_merges_all_edges() {
    let (_db_dir, db) = setup_db().await;
    let (_sess_dir, mgr, _store) = make_session_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-1";
    let dataset_id = Uuid::new_v4();
    datasets::create_dataset(
        &db,
        Dataset::new("ds1".to_string(), owner, None, dataset_id),
    )
    .await
    .unwrap();

    // Seed 3 edges with 2 nodes.
    let alice = make_node(dataset_id, owner, "Alice", "Person");
    let bob = make_node(dataset_id, owner, "Bob", "Person");
    graph_storage::upsert_nodes(&db, &[alice.clone(), bob.clone()])
        .await
        .unwrap();

    let now = Utc::now();
    let edges = vec![
        make_edge(
            dataset_id,
            owner,
            alice.id,
            bob.id,
            "knows",
            now - Duration::minutes(3),
        ),
        make_edge(
            dataset_id,
            owner,
            alice.id,
            bob.id,
            "works_with",
            now - Duration::minutes(2),
        ),
        make_edge(
            dataset_id,
            owner,
            bob.id,
            alice.id,
            "friend_of",
            now - Duration::minutes(1),
        ),
    ];
    graph_storage::upsert_edges(&db, &edges).await.unwrap();

    let ckstore = SeaOrmCheckpointStore::new(Arc::clone(&db));
    let result = sync_graph_to_session(
        &user_id,
        session_id,
        dataset_id,
        db.as_ref(),
        mgr.as_ref(),
        &ckstore,
        DEFAULT_MAX_LINES,
    )
    .await
    .unwrap();
    assert_eq!(result.synced, 3);
    assert_eq!(result.total, 3);

    // Graph context should have 3 JSON-lines.
    let ctx = mgr
        .get_graph_context(Some(session_id), Some(&user_id))
        .await
        .unwrap()
        .unwrap();
    let lines: Vec<&str> = ctx.split('\n').filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 3);

    // First line has source/relationship/target keys.
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(parsed["source"]["label"].is_string());
    assert!(parsed["relationship"].is_string());
    assert!(parsed["target"]["label"].is_string());
}

#[tokio::test]
async fn sync_second_run_picks_up_new_edges_only() {
    let (_db_dir, db) = setup_db().await;
    let (_sess_dir, mgr, _store) = make_session_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-incr";
    let dataset_id = Uuid::new_v4();
    datasets::create_dataset(
        &db,
        Dataset::new("ds-incr".to_string(), owner, None, dataset_id),
    )
    .await
    .unwrap();

    let alice = make_node(dataset_id, owner, "Alice", "Person");
    let bob = make_node(dataset_id, owner, "Bob", "Person");
    graph_storage::upsert_nodes(&db, &[alice.clone(), bob.clone()])
        .await
        .unwrap();

    let base = Utc::now() - Duration::hours(1);
    // 2 initial edges
    let edges = vec![
        make_edge(dataset_id, owner, alice.id, bob.id, "r1", base),
        make_edge(
            dataset_id,
            owner,
            alice.id,
            bob.id,
            "r2",
            base + Duration::minutes(1),
        ),
    ];
    graph_storage::upsert_edges(&db, &edges).await.unwrap();

    let ckstore = SeaOrmCheckpointStore::new(Arc::clone(&db));
    let r1 = sync_graph_to_session(
        &user_id,
        session_id,
        dataset_id,
        db.as_ref(),
        mgr.as_ref(),
        &ckstore,
        DEFAULT_MAX_LINES,
    )
    .await
    .unwrap();
    assert_eq!(r1.synced, 2);

    // Add 3 more edges later.
    let more = vec![
        make_edge(
            dataset_id,
            owner,
            alice.id,
            bob.id,
            "r3",
            base + Duration::minutes(10),
        ),
        make_edge(
            dataset_id,
            owner,
            alice.id,
            bob.id,
            "r4",
            base + Duration::minutes(11),
        ),
        make_edge(
            dataset_id,
            owner,
            alice.id,
            bob.id,
            "r5",
            base + Duration::minutes(12),
        ),
    ];
    graph_storage::upsert_edges(&db, &more).await.unwrap();

    let r2 = sync_graph_to_session(
        &user_id,
        session_id,
        dataset_id,
        db.as_ref(),
        mgr.as_ref(),
        &ckstore,
        DEFAULT_MAX_LINES,
    )
    .await
    .unwrap();
    assert_eq!(r2.synced, 3);
    assert_eq!(r2.total, 5);

    // Checkpoint advanced.
    let ctx = mgr
        .get_graph_context(Some(session_id), Some(&user_id))
        .await
        .unwrap()
        .unwrap();
    let lines: Vec<&str> = ctx.split('\n').filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 5);
}

#[tokio::test]
async fn sync_caps_at_max_lines() {
    let (_db_dir, db) = setup_db().await;
    let (_sess_dir, mgr, _store) = make_session_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-cap";
    let dataset_id = Uuid::new_v4();
    datasets::create_dataset(
        &db,
        Dataset::new("ds-cap".to_string(), owner, None, dataset_id),
    )
    .await
    .unwrap();

    let alice = make_node(dataset_id, owner, "Alice", "Person");
    let bob = make_node(dataset_id, owner, "Bob", "Person");
    graph_storage::upsert_nodes(&db, &[alice.clone(), bob.clone()])
        .await
        .unwrap();

    // Seed 25 edges, cap at 10
    let cap = 10usize;
    let base = Utc::now() - Duration::hours(1);
    let mut edges = Vec::new();
    for i in 0..25 {
        edges.push(make_edge(
            dataset_id,
            owner,
            alice.id,
            bob.id,
            &format!("rel_{i}"),
            base + Duration::seconds(i as i64),
        ));
    }
    graph_storage::upsert_edges(&db, &edges).await.unwrap();

    let ckstore = SeaOrmCheckpointStore::new(Arc::clone(&db));
    let res = sync_graph_to_session(
        &user_id,
        session_id,
        dataset_id,
        db.as_ref(),
        mgr.as_ref(),
        &ckstore,
        cap,
    )
    .await
    .unwrap();
    assert_eq!(res.synced, 25);
    // After cap, total should equal cap.
    assert_eq!(res.total, cap);

    let ctx = mgr
        .get_graph_context(Some(session_id), Some(&user_id))
        .await
        .unwrap()
        .unwrap();
    let lines: Vec<&str> = ctx.split('\n').filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), cap);

    // The newest edges (rel_15 .. rel_24) should be kept.
    let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(last["relationship"], serde_json::json!("rel_24"));
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["relationship"], serde_json::json!("rel_15"));
}

#[tokio::test]
async fn sync_empty_when_no_edges() {
    let (_db_dir, db) = setup_db().await;
    let (_sess_dir, mgr, _store) = make_session_manager();
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess-empty";
    let dataset_id = Uuid::new_v4();

    let ckstore = SeaOrmCheckpointStore::new(Arc::clone(&db));
    let res = sync_graph_to_session(
        &user_id,
        session_id,
        dataset_id,
        db.as_ref(),
        mgr.as_ref(),
        &ckstore,
        DEFAULT_MAX_LINES,
    )
    .await
    .unwrap();
    assert_eq!(res.synced, 0);
    assert_eq!(res.total, 0);

    // No graph_context should have been stored.
    let ctx = mgr
        .get_graph_context(Some(session_id), Some(&user_id))
        .await
        .unwrap();
    assert!(ctx.is_none());
}
