#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! E-09 — `GET /api/v1/sessions` integration tests.
//!
//! Six tests covering: ACL-scoped visibility, pagination envelope,
//! abandoned-status filter, divergence D-1 (`?order_by=banana` → 400),
//! `?limit=999` validation envelope, and 401 unauthenticated guard.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use cognee_database::{AclDb, SessionLifecycleDb};
use std::sync::Arc;
use uuid::Uuid;

use support::{
    bearer_header, body_json, build_auth_required_test_state, build_component_handles,
    build_search_db, ensure_principal, oneshot_request, seed_dataset, seed_perm_user, seed_user,
    test_router,
};

use cognee_http_server::AppState;

/// Build an `AppState` with auth, components (incl. a real
/// `DatabaseConnection`), and `require_authentication=false`. Returns the
/// state and a `Uuid` of a freshly seeded principal-backed user.
async fn build_sessions_state() -> AppState {
    let db_conn = build_search_db().await;
    let db_for_auth: sea_orm::DatabaseConnection = (*db_conn).clone();

    let user_repo = Arc::new(cognee_database::SeaOrmUserAuthRepository {
        db: db_for_auth.clone(),
    });
    let api_key_repo = Arc::new(cognee_database::SeaOrmApiKeyRepository {
        db: db_for_auth.clone(),
    });

    use cognee_http_server::HttpServerConfig;
    use cognee_http_server::auth::AuthContext;
    use cognee_http_server::auth::mailer::ConsoleMailer;
    use cognee_http_server::config::Environment;
    let cfg = HttpServerConfig {
        require_authentication: false,
        env: Environment::Dev,
        ..HttpServerConfig::default()
    };

    let (mailer, _events) = ConsoleMailer::new();
    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth context");

    let handles = build_component_handles(db_conn.clone(), None, None, None);

    AppState {
        config: std::sync::Arc::new(cfg),
        pipelines: AppState::noop_pipelines(),
        lib: Some(handles),
        auth: Some(std::sync::Arc::new(auth)),
        mailer: std::sync::Arc::new(mailer),
        health: None,
        spans: std::sync::Arc::new(cognee_http_server::observability::SpanBuffer::default()),
        sync: std::sync::Arc::new(cognee_http_server::sync::SyncRegistry::new()),
        #[cfg(feature = "telemetry")]
        telemetry_guard: None,
    }
}

/// Helper: borrow the `DatabaseConnection` from a state.
fn db_of(state: &AppState) -> &cognee_database::DatabaseConnection {
    state.components().expect("components").database.as_ref()
}

/// Build a fresh user with auth. Returns the `Uuid`.
async fn seed_user_id(state: &AppState, email: &str) -> Uuid {
    let user = seed_perm_user(state, email, "Pa$$word123").await;
    user.id
}

#[tokio::test]
async fn list_returns_only_caller_owned_and_permitted_dataset_sessions() {
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice@example.com").await;
    let bob = seed_user_id(&state, "bob@example.com").await;
    let shared_dataset = Uuid::new_v4();

    // Seed a dataset owned by Bob, and grant Alice "read" on it.
    ensure_principal(db_of(&state), shared_dataset, "dataset").await;
    seed_dataset(db_of(&state), shared_dataset, bob, None, "shared-ds").await;
    db_of(&state)
        .grant_permission(alice, shared_dataset, "read")
        .await
        .expect("grant read");

    // Alice owns one session.
    db_of(&state)
        .ensure_and_touch_session("cc_alice_aaaaaaaaaaaa", alice, None)
        .await
        .expect("alice session");
    // Bob owns two sessions: one in shared dataset (visible to Alice), one not.
    db_of(&state)
        .ensure_and_touch_session("cc_bob_shared_111111", bob, Some(shared_dataset))
        .await
        .expect("bob shared session");
    db_of(&state)
        .ensure_and_touch_session("cc_bob_private_22222", bob, None)
        .await
        .expect("bob private session");

    let app = test_router(state.clone()).await;
    // Authenticate as Alice via bearer header.
    let user = state
        .auth
        .as_ref()
        .expect("auth")
        .user_repo
        .find_by_id(alice)
        .await
        .expect("find")
        .expect("user");
    let bearer = bearer_header(&user, &state);

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions?range=all")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let sessions = body["sessions"].as_array().expect("sessions array");
    let ids: Vec<&str> = sessions
        .iter()
        .map(|s| s["session_id"].as_str().unwrap_or(""))
        .collect();

    assert!(
        ids.contains(&"cc_alice_aaaaaaaaaaaa"),
        "Alice's own row visible"
    );
    assert!(
        ids.contains(&"cc_bob_shared_111111"),
        "Bob's shared-dataset row visible to Alice"
    );
    assert!(
        !ids.contains(&"cc_bob_private_22222"),
        "Bob's private row NOT visible to Alice; got {ids:?}"
    );
}

#[tokio::test]
async fn list_pagination_envelope_correct() {
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-page@example.com").await;

    // Seed 75 sessions for Alice. Use small sleeps so last_activity_at
    // is monotonically increasing for stable ordering.
    for i in 0..75u32 {
        let sid = format!("cc_page_{i:020}");
        db_of(&state)
            .ensure_and_touch_session(&sid, alice, None)
            .await
            .expect("seed session");
    }

    let app = test_router(state.clone()).await;
    let user = state
        .auth
        .as_ref()
        .expect("auth")
        .user_repo
        .find_by_id(alice)
        .await
        .expect("find")
        .expect("user");
    let bearer = bearer_header(&user, &state);

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions?range=all&limit=20&offset=40")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(body["total"].as_i64(), Some(75), "total: {body}");
    assert_eq!(body["limit"].as_u64(), Some(20));
    assert_eq!(body["offset"].as_u64(), Some(40));
    assert_eq!(body["has_more"].as_bool(), Some(true), "has_more: {body}");
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 20);
}

#[tokio::test]
#[serial_test::serial]
async fn list_status_filter_includes_abandoned_via_effective_status() {
    use chrono::{Duration, Utc};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    // Lower the abandon threshold so a deliberately stale row reports
    // as `abandoned` at read time. Mirrors LIB-05's repo test pattern.
    // SAFETY: `serial_test::serial` keeps env mutation isolated.
    unsafe { std::env::set_var("SESSION_ABANDON_AFTER_SECONDS", "1") };

    async {
        let state = build_sessions_state().await;
        let alice = seed_user_id(&state, "alice-abandon@example.com").await;

        let user_hex = alice.simple().to_string();
        let stale_sid = "cc_stale_abandoned___";
        let now = Utc::now();
        let old_ts = now - Duration::seconds(60);

        db_of(&state)
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!(
                    "INSERT INTO session_records (\
                        session_id, user_id, dataset_id, status, started_at, \
                        last_activity_at, ended_at, tokens_in, tokens_out, \
                        cost_usd, error_count, last_model\
                     ) VALUES (\
                        '{stale_sid}', '{user_hex}', NULL, 'running', \
                        '{ts}', '{ts}', NULL, 0, 0, 0.0, 0, NULL\
                     )",
                    ts = old_ts.to_rfc3339()
                ),
            ))
            .await
            .expect("seed stale row");

        let app = test_router(state.clone()).await;
        let user = state
            .auth
            .as_ref()
            .expect("auth")
            .user_repo
            .find_by_id(alice)
            .await
            .expect("find")
            .expect("user");
        let bearer = bearer_header(&user, &state);

        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/sessions?range=all&status=abandoned")
            .header("Authorization", bearer)
            .body(Body::empty())
            .expect("request");

        let resp = oneshot_request(app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let sessions = body["sessions"].as_array().expect("sessions array");
        assert_eq!(sessions.len(), 1, "one stale row matches: {body}");
        assert_eq!(sessions[0]["session_id"], stale_sid);
        assert_eq!(
            sessions[0]["effective_status"], "abandoned",
            "effective_status must surface the abandoned status: {body}"
        );
        // Stored `status` column unchanged at read time.
        assert_eq!(sessions[0]["status"], "running");
    }
    .await;

    unsafe { std::env::remove_var("SESSION_ABANDON_AFTER_SECONDS") };
}

#[tokio::test]
async fn list_order_by_invalid_returns_400_with_python_validation_envelope() {
    // Decision 9 / divergence D-1: typed enum rejects `banana` and
    // returns the Python validation envelope. Python silently falls back
    // to `last_activity_at`; this is the Rust-only divergence.
    let state = build_sessions_state().await;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions?order_by=banana")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    let detail = body["detail"].as_array().expect("detail array");
    let entry = &detail[0];
    let loc = entry["loc"].as_array().expect("loc array");
    assert_eq!(loc[0], "query");
    assert_eq!(loc[1], "order_by", "loc should target order_by: {body}");
    let ty = entry["type"].as_str().expect("type str");
    assert!(
        ty.ends_with("value_error"),
        "type ends with value_error: {ty}"
    );
}

#[tokio::test]
async fn list_limit_out_of_range_returns_400_with_python_validation_envelope() {
    // Decision 7: 1..=500 enforced in handler. `?limit=999` returns 400
    // with the Python envelope (loc=["query","limit"]).
    let state = build_sessions_state().await;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions?limit=999")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    let detail = body["detail"].as_array().expect("detail array");
    let entry = &detail[0];
    let loc = entry["loc"].as_array().expect("loc array");
    assert_eq!(loc[0], "query");
    assert_eq!(loc[1], "limit");
    let msg = entry["msg"].as_str().expect("msg str");
    assert!(msg.contains("1..=500"), "msg should mention 1..=500: {msg}");
    let ty = entry["type"].as_str().expect("type str");
    assert!(ty.ends_with("value_error"));
}

#[tokio::test]
async fn list_components_not_configured_returns_500_with_python_error_envelope() {
    // Python parity: `list_sessions` 500 error path returns
    // `JSONResponse(status_code=500, content={"error": "list failed"})`
    // (`get_sessions_router.py:108-110`). Drive that path by clearing
    // `state.lib` so `components()` returns None and assert the wire body
    // is `{"error": "list failed"}` (NOT `{"detail": ...}`).
    let mut state = build_sessions_state().await;
    state.lib = None;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"].as_str(),
        Some("list failed"),
        "500 body must be {{\"error\": \"list failed\"}}: {body}"
    );
    assert!(
        body.get("detail").is_none(),
        "must not emit Rust-style {{\"detail\": ...}} on the 500 path: {body}"
    );
}

#[tokio::test]
async fn list_unauthenticated_returns_401() {
    // Use the `require_authentication=true` test state — anonymous calls
    // are rejected with 401.
    let (state, _events) = build_auth_required_test_state().await;
    // We need to also wire components for the handler to function. Rebuild:
    let db = build_search_db().await;
    let handles = build_component_handles(db, None, None, None);
    let mut state = state;
    state.lib = Some(handles);

    // Seed the user repo with one user but do not include any token.
    let _ = seed_user(&state, "carol@example.com", "Pa$$word123").await;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
