//! P4 Step 17 — visualize GET integration tests.

mod support;

use axum::{body::Body, http::Request};
use cognee_database::IngestDb;
use cognee_graph::GraphDBTrait;
use cognee_graph::mock::MockGraphDB;
use std::sync::Arc;
use tower::ServiceExt;

use support::{body_json, build_p4_state};

#[tokio::test]
async fn missing_dataset_id_returns_422_or_400() {
    let graph: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
    let state = build_p4_state(None, None, Some(graph)).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/visualize")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    // axum Query rejection on missing required field — accept 400 or 422.
    let status = resp.status().as_u16();
    assert!(status == 400 || status == 422, "got {status}");
}

#[tokio::test]
async fn unknown_dataset_id_collapses_to_409() {
    // Per Python parity: dataset-not-found is swallowed into 409, NOT 404.
    let graph: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
    let state = build_p4_state(None, None, Some(graph)).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/v1/visualize?dataset_id={}",
            uuid::Uuid::new_v4()
        ))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 409);
    let body = body_json(resp).await;
    assert!(body["error"].is_string());
    assert!(body.get("detail").is_none());
}

#[tokio::test]
async fn happy_path_returns_html_content_type() {
    use cognee_database::AclDb;
    use cognee_models::Dataset;
    use uuid::Uuid;

    // Default authenticated user (when require_authentication=false) — match
    // the value `AuthenticatedUser::default_user_from_state` produces.
    let default_user_id = Uuid::nil();
    let dataset_id = Uuid::new_v4();

    let graph_db = MockGraphDB::new();
    graph_db
        .add_node_raw(serde_json::json!({"id": "n1", "type": "Entity"}))
        .await
        .expect("seed node");
    let graph: Arc<dyn GraphDBTrait> = Arc::new(graph_db);
    let state = build_p4_state(None, None, Some(graph)).await;

    // Seed the dataset and grant the default user `read` on it.
    let components = state.components().expect("components wired");
    let dataset = Dataset::new(
        "test-dataset".to_string(),
        default_user_id,
        None,
        dataset_id,
    );
    IngestDb::create_dataset(components.database.as_ref(), dataset)
        .await
        .expect("create dataset");
    AclDb::grant_permission(
        components.database.as_ref(),
        default_user_id,
        dataset_id,
        "read",
    )
    .await
    .expect("grant read");

    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/visualize?dataset_id={dataset_id}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(
        resp.status(),
        200,
        "expected 200 for authorized read, got {}",
        resp.status()
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/html"),
        "expected text/html content-type, got {ct:?}"
    );
}

#[tokio::test]
async fn dataset_belonging_to_other_user_collapses_to_409() {
    // Per Python parity: permission denied surfaces as 409 (the broad except),
    // NOT 403.
    use cognee_models::Dataset;
    use uuid::Uuid;

    let other_user_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();
    let graph: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
    let state = build_p4_state(None, None, Some(graph)).await;

    let components = state.components().expect("components");
    let dataset = Dataset::new(
        "other-user-dataset".to_string(),
        other_user_id,
        None,
        dataset_id,
    );
    IngestDb::create_dataset(components.database.as_ref(), dataset)
        .await
        .expect("create dataset");
    // Note: NO grant_permission for the default user.

    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/visualize?dataset_id={dataset_id}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 409);
    let body = body_json(resp).await;
    assert!(body["error"].is_string());
    assert!(body.get("detail").is_none());
}
