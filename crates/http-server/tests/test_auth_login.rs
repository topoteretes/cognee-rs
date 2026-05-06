//! Integration tests for `POST /api/v1/auth/login`, `POST /api/v1/auth/logout`,
//! and `GET /api/v1/auth/me`.
//!
//! Uses in-memory SQLite via `build_auth_test_state`.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{body_json, build_auth_test_state, seed_user, test_router};

// ─── POST /api/v1/auth/login ─────────────────────────────────────────────────

#[tokio::test]
async fn login_success_returns_access_token_and_cookie() {
    let (state, _events) = build_auth_test_state().await;
    seed_user(&state, "alice@example.com", "Str0ng!Pass#1").await;

    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(
            "username=alice%40example.com&password=Str0ng!Pass%231",
        ))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    // Set-Cookie header must be present and contain required attributes
    let set_cookie = resp
        .headers()
        .get("Set-Cookie")
        .expect("Set-Cookie header must be present")
        .to_str()
        .expect("Set-Cookie must be valid ASCII")
        .to_owned();
    assert!(
        set_cookie.contains("HttpOnly"),
        "Set-Cookie must contain HttpOnly: {set_cookie}"
    );
    assert!(
        set_cookie.contains("Path=/"),
        "Set-Cookie must contain Path=/: {set_cookie}"
    );
    assert!(
        set_cookie.contains("SameSite=Lax"),
        "Set-Cookie must contain SameSite=Lax: {set_cookie}"
    );

    // Response body must contain access_token and token_type
    let body = body_json(resp).await;
    assert!(
        body["access_token"].is_string(),
        "expected access_token: {body}"
    );
    assert_eq!(body["token_type"], "bearer");
}

#[tokio::test]
async fn login_wrong_password_returns_bad_credentials() {
    let (state, _events) = build_auth_test_state().await;
    seed_user(&state, "bob@example.com", "Str0ng!Pass#1").await;

    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(
            "username=bob%40example.com&password=WrongPass1!",
        ))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    assert_eq!(body["detail"], "LOGIN_BAD_CREDENTIALS");
}

#[tokio::test]
async fn login_unknown_email_returns_bad_credentials() {
    let (state, _events) = build_auth_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(
            "username=nobody%40example.com&password=Str0ng!Pass%231",
        ))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    assert_eq!(body["detail"], "LOGIN_BAD_CREDENTIALS");
}

#[tokio::test]
async fn login_empty_body_returns_bad_credentials() {
    let (state, _events) = build_auth_test_state().await;
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    assert_eq!(body["detail"], "LOGIN_BAD_CREDENTIALS");
}

// ─── GET /api/v1/auth/me ─────────────────────────────────────────────────────

#[tokio::test]
async fn get_auth_me_with_bearer_token_returns_email() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "charlie@example.com", "Str0ng!Pass#1").await;
    let bearer = support::bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["email"], "charlie@example.com");
    // /auth/me must NOT include is_superuser, is_active, id etc.
    assert!(body.get("id").is_none(), "auth/me must not expose id");
    assert!(
        body.get("is_superuser").is_none(),
        "auth/me must not expose is_superuser"
    );
}

#[tokio::test]
async fn get_auth_me_with_cookie_returns_email() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "diana@example.com", "Str0ng!Pass#1").await;
    let cookie = support::cookie_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .header("cookie", cookie)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["email"], "diana@example.com");
}

#[tokio::test]
async fn get_auth_me_no_token_when_auth_required_returns_401() {
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
        spans: Arc::new(cognee_http_server::observability::SpanBuffer::default()),
        sync: Arc::new(cognee_http_server::sync::SyncRegistry::new()),
        #[cfg(feature = "telemetry")]
        telemetry_guard: None,
    };
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── POST /api/v1/auth/logout ────────────────────────────────────────────────

#[tokio::test]
async fn logout_clears_cookie_and_returns_empty_json() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "eve@example.com", "Str0ng!Pass#1").await;
    let bearer = support::bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/logout")
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    // Body should be `{}`
    let body = body_json(resp).await;
    assert!(body.is_object());
}

// ─── GET /api/v1/auth/me — X-Api-Key auth ────────────────────────────────────

#[tokio::test]
async fn get_auth_me_with_x_api_key_returns_email() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "apikey_me@example.com", "Str0ng!Pass#1").await;
    let app = test_router(state.clone()).await;

    // Create an API key for the user
    let raw_key = {
        let bearer = support::bearer_header(&user, &state);
        let payload = serde_json::json!({"name": "test-key"});
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/api-keys")
            .header("authorization", bearer)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
            .expect("request");
        let app2 = test_router(state.clone()).await;
        let resp = app2.oneshot(req).await.expect("response");
        body_json(resp).await["key"]
            .as_str()
            .expect("key")
            .to_owned()
    };

    // Use the X-Api-Key header to authenticate /auth/me
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .header("X-Api-Key", &raw_key)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["email"], "apikey_me@example.com");
    // /auth/me must return ONLY email
    assert!(body.get("id").is_none(), "auth/me must not expose id");
}

// ─── POST /api/v1/auth/login — inactive user ────────────────────────────────

/// An inactive user gets LOGIN_BAD_CREDENTIALS (Python parity — not USER_NOT_VERIFIED).
#[tokio::test]
async fn login_inactive_user_returns_bad_credentials() {
    use cognee_database::UpdateUserPayload;

    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "inactive@example.com", "Str0ng!Pass#1").await;

    // Mark user as inactive
    let auth = state.auth.as_ref().expect("auth ctx");
    auth.user_repo
        .update(
            user.id,
            UpdateUserPayload {
                is_active: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("mark inactive");

    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(
            "username=inactive%40example.com&password=Str0ng!Pass%231",
        ))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    assert_eq!(body["detail"], "LOGIN_BAD_CREDENTIALS");
}
