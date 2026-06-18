#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `GET /api/v1/datasets/{id}/graph`.
//!
//! The endpoint returns the canonical graph snapshot
//! `{"nodes": [{...}], "edges": [{...}]}` shaped by `get_formatted_graph_data`.
//! When the graph backend is not wired we still return the same shape with
//! empty arrays (matches the WS frame's wire contract — clients should not
//! need to distinguish "no graph yet" from "graph DB not configured").

mod support;

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use cognee_database::AclDb;
use cognee_graph::{GraphDBTrait, MockGraphDB};
use serde_json::json;
use tower::ServiceExt;

use support::{
    bearer_header, build_auth_required_test_state, build_component_handles,
    build_permissions_state, build_search_db, ensure_principal, permissions_db, seed_dataset,
    seed_perm_user, test_router,
};

/// No auth → 401.
#[tokio::test]
async fn test_get_graph_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/graph"))
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Regression guard: when no graph backend is wired but auth + DB are, the
/// response must still be `200 OK` with the canonical empty-graph shape —
/// `{"nodes": [...], "edges": [...]}`, not a 501 or error envelope.
#[tokio::test]
async fn test_get_graph_no_graph_db_returns_empty_shape() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "graph_empty@example.com", "Str0ng!Pass#1").await;

    // Seed a dataset owned by the user and grant a read ACL so the permission
    // gate passes through to the formatter (which then returns the empty
    // shape because `graph_db` is None on the test state).
    let dataset_id = uuid::Uuid::new_v4();
    seed_dataset(
        permissions_db(&state),
        dataset_id,
        user.id,
        None,
        "empty-ds",
    )
    .await;
    let db = state.components().expect("components").database.as_ref();
    ensure_principal(db, user.id, "user").await;
    AclDb::grant_permission(db, user.id, dataset_id, "read")
        .await
        .expect("grant read");

    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/graph"))
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "graph endpoint must return 200 even when graph_db is None"
    );

    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("bytes");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert!(v.get("nodes").and_then(|n| n.as_array()).is_some());
    assert!(v.get("edges").and_then(|e| e.as_array()).is_some());
    assert!(v["nodes"].as_array().unwrap().is_empty());
    assert!(v["edges"].as_array().unwrap().is_empty());
}

/// With a wired graph DB containing real nodes + edges, the response must
/// surface a non-empty snapshot in the canonical Python-parity shape.
#[tokio::test]
async fn test_get_graph_with_wired_graph_db_returns_populated_snapshot() {
    use cognee_database::{SeaOrmApiKeyRepository, SeaOrmUserAuthRepository};
    use cognee_http_server::AppState;
    use cognee_http_server::auth::AuthContext;
    use cognee_http_server::auth::mailer::ConsoleMailer;
    use cognee_http_server::config::{Environment, HttpServerConfig};

    // Build a graph DB seeded with two nodes + one edge.
    let mock_graph = MockGraphDB::new();
    mock_graph
        .add_node_raw(json!({
            "id": "n1",
            "type": "Entity",
            "name": "Alice",
        }))
        .await
        .expect("add n1");
    mock_graph
        .add_node_raw(json!({
            "id": "n2",
            "type": "Entity",
            "name": "Bob",
        }))
        .await
        .expect("add n2");
    mock_graph
        .add_edge(
            "n1",
            "n2",
            "KNOWS",
            Some(HashMap::from([(Cow::Borrowed("weight"), json!(1.0))])),
        )
        .await
        .expect("add edge");
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(mock_graph);

    // Build a fully-migrated DB + ComponentHandles wired with the graph_db.
    let db = build_search_db().await;
    let handles = build_component_handles(db.clone(), None, None, Some(graph_db));

    let db_for_auth: sea_orm::DatabaseConnection = (*db).clone();
    let user_repo = Arc::new(SeaOrmUserAuthRepository {
        db: db_for_auth.clone(),
    });
    let api_key_repo = Arc::new(SeaOrmApiKeyRepository {
        db: db_for_auth.clone(),
    });

    let cfg = HttpServerConfig {
        require_authentication: false,
        env: Environment::Dev,
        ..HttpServerConfig::default()
    };
    let (mailer, _events) = ConsoleMailer::new();
    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth context");

    let state = AppState {
        config: Arc::new(cfg),
        pipelines: AppState::noop_pipelines(),
        lib: Some(handles),
        auth: Some(Arc::new(auth)),
        mailer: Arc::new(mailer),
        health: None,
        spans: Arc::new(cognee_http_server::observability::SpanBuffer::default()),
        sync: Arc::new(cognee_http_server::sync::SyncRegistry::new()),
        #[cfg(feature = "telemetry")]
        telemetry_guard: None,
    };

    let user = seed_perm_user(&state, "wired_graph@example.com", "Str0ng!Pass#1").await;
    let dataset_id = uuid::Uuid::new_v4();
    seed_dataset(permissions_db(&state), dataset_id, user.id, None, "ds-w").await;
    ensure_principal(permissions_db(&state), user.id, "user").await;
    AclDb::grant_permission(permissions_db(&state), user.id, dataset_id, "read")
        .await
        .expect("grant read");

    let auth_header = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/graph"))
        .header("Authorization", auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("bytes");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let nodes = v["nodes"].as_array().expect("nodes array");
    let edges = v["edges"].as_array().expect("edges array");
    assert_eq!(nodes.len(), 2, "expected 2 nodes, got {}", nodes.len());
    assert_eq!(edges.len(), 1, "expected 1 edge, got {}", edges.len());

    // Verify field shapes (id, label, type, properties) per Python parity.
    for n in nodes {
        let obj = n.as_object().expect("node object");
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("label"));
        assert!(obj.contains_key("type"));
        assert!(obj.contains_key("properties"));
    }
    for e in edges {
        let obj = e.as_object().expect("edge object");
        assert!(obj.contains_key("source"));
        assert!(obj.contains_key("target"));
        assert!(obj.contains_key("label"));
    }
}
