//! P4 Step 17 — visualize POST /multi integration tests.

mod support;

use axum::{body::Body, http::Request};
use cognee_graph::GraphDBTrait;
use cognee_graph::mock::MockGraphDB;
use std::sync::Arc;
use tower::ServiceExt;

use support::{body_json, build_p4_state};

#[tokio::test]
async fn empty_array_with_no_auth_falls_through_default_user() {
    // Default user is a superuser when require_authentication=false (see
    // AuthenticatedUser::default_user_from_state). An empty array yields an
    // empty multi-user HTML.
    let graph: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
    let state = build_p4_state(None, None, Some(graph)).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/visualize/multi")
        .header("content-type", "application/json")
        .body(Body::from("[]"))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/html"));
}

#[tokio::test]
async fn non_superuser_returns_403_with_error_envelope() {
    // Headline parity invariant: the SuperuserOnly extractor emits the
    // visualize-specific `{error}` envelope, NOT the canonical `{detail}`.
    use cognee_database::{SeaOrmApiKeyRepository, SeaOrmUserAuthRepository};
    use cognee_http_server::{
        AppState, HttpServerConfig,
        auth::{AuthContext, mailer::ConsoleMailer},
        config::Environment,
    };
    use std::sync::Arc;
    use support::{bearer_header, seed_user, setup_auth_db};

    // Build an auth-enabled state where the request will resolve to a NON-
    // superuser via Bearer.
    let db = setup_auth_db().await;
    let user_repo = Arc::new(SeaOrmUserAuthRepository { db: db.clone() });
    let api_key_repo = Arc::new(SeaOrmApiKeyRepository { db: db.clone() });
    let cfg = HttpServerConfig {
        require_authentication: false,
        env: Environment::Dev,
        ..HttpServerConfig::default()
    };
    let (mailer, _events) = ConsoleMailer::new();
    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth");
    let state = AppState {
        config: Arc::new(cfg),
        pipelines: AppState::noop_pipelines(),
        lib: None,
        auth: Some(Arc::new(auth)),
        mailer: Arc::new(mailer),
        health: None,
        spans: Arc::new(cognee_http_server::observability::SpanBuffer::default()),
        sync: Arc::new(cognee_http_server::sync::SyncRegistry::new()),
    };

    let regular = seed_user(&state, "user@example.com", "passw0rd!").await;
    let bearer = bearer_header(&regular, &state);
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/visualize/multi")
        .header("content-type", "application/json")
        .header("authorization", bearer)
        .body(Body::from("[]"))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 403);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"],
        "Superuser privileges required for multi-user visualization"
    );
    // NOT the canonical {detail} envelope.
    assert!(body.get("detail").is_none());
}

#[tokio::test]
async fn unknown_dataset_collapses_to_409() {
    let graph: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
    let state = build_p4_state(None, None, Some(graph)).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let body = serde_json::json!([
        {"user_id": uuid::Uuid::new_v4(), "dataset_id": uuid::Uuid::new_v4()}
    ])
    .to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/visualize/multi")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 409);
    let body = body_json(resp).await;
    assert!(body["error"].is_string());
}
