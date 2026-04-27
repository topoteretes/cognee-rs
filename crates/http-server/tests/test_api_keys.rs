//! Integration tests for the `/api/v1/auth/api-keys` router.
//!
//! Covers: list (GET /), create (POST /), delete (DELETE /{id}).
//! Error envelope for this router is `{"error": {"message": "..."}}`.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use uuid::Uuid;

use support::{bearer_header, body_json, build_auth_test_state, seed_user, test_router};

// ─── GET /api/v1/auth/api-keys ───────────────────────────────────────────────

#[tokio::test]
async fn list_keys_empty_for_new_user() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "keylist@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/api-keys")
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert!(body.is_array());
    assert_eq!(body.as_array().expect("array").len(), 0);
}

#[tokio::test]
async fn list_keys_requires_auth() {
    // Build a state with require_authentication=true baked into the AuthContext.
    use cognee_database::{SeaOrmApiKeyRepository, SeaOrmUserAuthRepository};
    use cognee_http_server::auth::context::AuthContext;
    use cognee_http_server::config::Environment;
    use std::sync::Arc;

    let db = support::setup_auth_db().await;
    let user_repo = Arc::new(SeaOrmUserAuthRepository { db: db.clone() });
    let api_key_repo = Arc::new(SeaOrmApiKeyRepository { db: db.clone() });

    // Dev env so "super_secret" is accepted without env var mutation
    let cfg = cognee_http_server::HttpServerConfig {
        require_authentication: true,
        env: Environment::Dev,
        ..Default::default()
    };

    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth ctx");

    use cognee_http_server::auth::mailer::ConsoleMailer;
    let (mailer, _events) = ConsoleMailer::new();

    let state = cognee_http_server::AppState {
        config: Arc::new(cfg),
        pipelines: cognee_http_server::AppState::noop_pipelines(),
        lib: None,
        auth: Some(Arc::new(auth)),
        mailer: Arc::new(mailer),
        health: None,
        spans: None,
        sync: None,
    };
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/api-keys")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── POST /api/v1/auth/api-keys ──────────────────────────────────────────────

#[tokio::test]
async fn create_key_returns_raw_key_and_label() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "keycreate@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    let payload = serde_json::json!({"name": "my-key"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/api-keys")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    // Response must include a raw key and id
    assert!(body["key"].is_string(), "expected key: {body}");
    assert!(body["id"].is_string(), "expected id: {body}");
    assert!(body["label"].is_string(), "expected label: {body}");
}

#[tokio::test]
async fn create_key_no_name_still_succeeds() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "keynoname@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    let payload = serde_json::json!({});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/api-keys")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn created_key_appears_in_list() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "keylistafter@example.com", "Str0ng!Pass#1").await;

    // Create
    {
        let bearer = bearer_header(&user, &state);
        let app = test_router(state.clone()).await;
        let payload = serde_json::json!({"name": "test-key"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/api-keys")
            .header("authorization", bearer)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // List — should now have 1 key
    {
        let bearer = bearer_header(&user, &state);
        let app = test_router(state).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/auth/api-keys")
            .header("authorization", bearer)
            .body(Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body.as_array().expect("array").len(), 1);
    }
}

// ─── DELETE /api/v1/auth/api-keys{id} ───────────────────────────────────────

#[tokio::test]
async fn delete_existing_key_returns_200_null() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "keydelete@example.com", "Str0ng!Pass#1").await;

    // Create a key
    let key_id: Uuid = {
        let bearer = bearer_header(&user, &state);
        let app = test_router(state.clone()).await;
        let payload = serde_json::json!({"name": "to-delete"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/api-keys")
            .header("authorization", bearer)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        let body = body_json(resp).await;
        let id_str = body["id"].as_str().expect("id string");
        Uuid::parse_str(id_str).expect("parse uuid")
    };

    // Delete
    {
        let bearer = bearer_header(&user, &state);
        let app = test_router(state).await;
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/auth/api-keys/{key_id}"))
            .header("authorization", bearer)
            .body(Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body.is_null());
    }
}

/// Python quirk: deleting a non-existent key returns 500.
#[tokio::test]
async fn delete_missing_key_returns_500() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "keymissing@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    let random_id = Uuid::new_v4();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/auth/api-keys/{random_id}"))
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// When `hash_api_key=true`, listed keys show `"************"` instead of the raw key.
#[tokio::test]
async fn list_keys_masked_when_hash_api_key_enabled() {
    use cognee_database::{SeaOrmApiKeyRepository, SeaOrmUserAuthRepository};
    use cognee_http_server::auth::context::AuthContext;
    use std::sync::Arc;

    let db = support::setup_auth_db().await;
    let user_repo = Arc::new(SeaOrmUserAuthRepository { db: db.clone() });
    let api_key_repo = Arc::new(SeaOrmApiKeyRepository { db: db.clone() });

    use cognee_http_server::config::Environment;
    // Dev env so "super_secret" is accepted without env var mutation.
    // HASH_API_KEY is read by from_env; set it only for the duration of this call.
    unsafe { std::env::set_var("HASH_API_KEY", "true") };
    let cfg = cognee_http_server::HttpServerConfig {
        require_authentication: false,
        env: Environment::Dev,
        ..Default::default()
    };
    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth ctx");
    unsafe { std::env::remove_var("HASH_API_KEY") };

    use cognee_http_server::auth::mailer::ConsoleMailer;
    let (mailer, _events) = ConsoleMailer::new();

    let state = cognee_http_server::AppState {
        config: Arc::new(cfg),
        pipelines: cognee_http_server::AppState::noop_pipelines(),
        lib: None,
        auth: Some(Arc::new(auth)),
        mailer: Arc::new(mailer),
        health: None,
        spans: None,
        sync: None,
    };

    let user = seed_user(&state, "hashedkey@example.com", "Str0ng!Pass#1").await;

    // Create a key
    {
        let bearer = bearer_header(&user, &state);
        let app = test_router(state.clone()).await;
        let payload = serde_json::json!({"name": "hashed"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/api-keys")
            .header("authorization", bearer)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // List — key must be masked
    {
        let bearer = bearer_header(&user, &state);
        let app = test_router(state).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/auth/api-keys")
            .header("authorization", bearer)
            .body(Body::empty())
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let items = body.as_array().expect("array");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["key"], "************", "key must be masked");
    }
}

/// Creating an 11th key when the 10-key limit is reached returns the unique
/// `{"error": {"message": "..."}}` envelope — not `{"detail": "..."}`.
#[tokio::test]
async fn create_key_at_cap_returns_api_key_envelope() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "keycap@example.com", "Str0ng!Pass#1").await;

    // Create 10 keys
    for i in 0..10u8 {
        let bearer = bearer_header(&user, &state);
        let app = test_router(state.clone()).await;
        let payload = serde_json::json!({"name": format!("key-{i}")});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/api-keys")
            .header("authorization", bearer)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK, "key {i} should succeed");
    }

    // 11th key must fail with the api-key-specific envelope
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;
    let payload = serde_json::json!({"name": "key-overflow"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/api-keys")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "11th key must return 400"
    );
    let body = body_json(resp).await;
    // Must use the api-keys-specific error envelope ({"error": {"message": "..."}})
    assert!(
        body["error"]["message"].is_string(),
        "expected api-key error envelope: {body}"
    );
    assert_eq!(
        body["error"]["message"], "You have reached the maximum number of API keys.",
        "exact message must match"
    );
    // Must NOT use the standard detail envelope
    assert!(
        body.get("detail").is_none(),
        "api-key errors must not use detail envelope: {body}"
    );
}

/// Cross-user delete: User A cannot delete User B's key — returns 500 (Python quirk).
#[tokio::test]
async fn delete_other_users_key_returns_500() {
    let (state, _events) = build_auth_test_state().await;
    let user_a = seed_user(&state, "keya@example.com", "Str0ng!Pass#1").await;
    let user_b = seed_user(&state, "keyb@example.com", "Str0ng!Pass#1").await;

    // User B creates a key
    let key_id: uuid::Uuid = {
        let bearer = bearer_header(&user_b, &state);
        let app = test_router(state.clone()).await;
        let payload = serde_json::json!({"name": "b-key"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/api-keys")
            .header("authorization", bearer)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        let body = body_json(resp).await;
        uuid::Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid")
    };

    // User A tries to delete User B's key — must fail with 500 (Python quirk)
    let bearer = bearer_header(&user_a, &state);
    let app = test_router(state).await;
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/auth/api-keys/{key_id}"))
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "cross-user delete must return 500 (Python parity)"
    );
}
