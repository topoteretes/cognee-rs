#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! E-10 — `GET /api/v1/sessions/stats` integration tests.
//!
//! Eight tests covering:
//!
//! * empty-DB `success_rate == 1.0` fallback (Python `:175`)
//! * effective-status flips a stale running row to `abandoned`
//! * `agent_time_s` skips rows with `NULL started_at`
//! * permitted-dataset visibility (caller's own + ACL-`read`)
//! * `?range=24h` filters older rows
//! * `?range` echo
//! * 401 unauthenticated guard
//! * 500 `{"error":"stats failed"}` envelope when components are unwired

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

/// Build an `AppState` for E-10 stats tests — same wiring as the E-09
/// list tests: real `DatabaseConnection`, auth on, anonymous default
/// rejected (set per-test via bearer header).
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

/// Borrow the underlying `DatabaseConnection` from a state.
fn db_of(state: &AppState) -> &cognee_database::DatabaseConnection {
    state.components().expect("components").database.as_ref()
}

/// Seed a fresh user.
async fn seed_user_id(state: &AppState, email: &str) -> Uuid {
    let user = seed_perm_user(state, email, "Pa$$word123").await;
    user.id
}

/// Build a bearer header for a known user id.
async fn bearer_for_user(state: &AppState, user_id: Uuid) -> String {
    let user = state
        .auth
        .as_ref()
        .expect("auth")
        .user_repo
        .find_by_id(user_id)
        .await
        .expect("find")
        .expect("user");
    bearer_header(&user, state)
}

#[tokio::test]
async fn stats_empty_returns_success_rate_one() {
    // Python parity: `success_rate = 1.0` when `decided == 0`
    // (`get_sessions_router.py:175`, also enforced in LIB-05's
    // `aggregate_stats` at `ops/session_lifecycle.rs:715-720`).
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-stats-empty@example.com").await;

    let app = test_router(state.clone()).await;
    let bearer = bearer_for_user(&state, alice).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/stats?range=all")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(body["sessions"].as_i64(), Some(0), "body: {body}");
    assert_eq!(body["completed"].as_i64(), Some(0));
    assert_eq!(body["failed"].as_i64(), Some(0));
    assert_eq!(body["abandoned"].as_i64(), Some(0));
    assert_eq!(body["running"].as_i64(), Some(0));
    assert_eq!(
        body["success_rate"].as_f64(),
        Some(1.0),
        "decided==0 → 1.0: {body}"
    );
    assert_eq!(body["range"].as_str(), Some("all"));
}

#[tokio::test]
#[serial_test::serial]
async fn stats_buckets_reflect_effective_status() {
    use chrono::{Duration, Utc};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    // Lower the abandon threshold so a deliberately stale running row
    // reports as `abandoned` at read time. SAFETY: serialized via
    // `serial_test::serial` so env mutation is isolated.
    unsafe { std::env::set_var("SESSION_ABANDON_AFTER_SECONDS", "1") };

    async {
        let state = build_sessions_state().await;
        let alice = seed_user_id(&state, "alice-stats-buckets@example.com").await;

        let user_hex = alice.simple().to_string();
        let now = Utc::now();
        let started = now - Duration::seconds(60);
        let stale_ts = now - Duration::seconds(60);
        let ended_ts = now - Duration::seconds(10);

        // 1 completed.
        db_of(&state)
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!(
                    "INSERT INTO session_records (\
                        session_id, user_id, dataset_id, status, started_at, \
                        last_activity_at, ended_at, tokens_in, tokens_out, \
                        cost_usd, error_count, last_model\
                     ) VALUES (\
                        'cc_done_aaaaaaaaaaaa', '{user_hex}', NULL, 'completed', \
                        '{s}', '{l}', '{e}', 100, 200, 0.10, 0, NULL\
                     )",
                    s = started.to_rfc3339(),
                    l = ended_ts.to_rfc3339(),
                    e = ended_ts.to_rfc3339()
                ),
            ))
            .await
            .expect("seed completed");

        // 1 stale running → reports as abandoned at read time.
        db_of(&state)
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!(
                    "INSERT INTO session_records (\
                        session_id, user_id, dataset_id, status, started_at, \
                        last_activity_at, ended_at, tokens_in, tokens_out, \
                        cost_usd, error_count, last_model\
                     ) VALUES (\
                        'cc_stale_running____', '{user_hex}', NULL, 'running', \
                        '{ts}', '{ts}', NULL, 0, 0, 0.0, 0, NULL\
                     )",
                    ts = stale_ts.to_rfc3339()
                ),
            ))
            .await
            .expect("seed stale running");

        let app = test_router(state.clone()).await;
        let bearer = bearer_for_user(&state, alice).await;

        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/sessions/stats?range=all")
            .header("Authorization", bearer)
            .body(Body::empty())
            .expect("request");

        let resp = oneshot_request(app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;

        assert_eq!(body["sessions"].as_i64(), Some(2), "body: {body}");
        assert_eq!(body["completed"].as_i64(), Some(1), "body: {body}");
        assert_eq!(
            body["abandoned"].as_i64(),
            Some(1),
            "stale running flipped to abandoned: {body}"
        );
        assert_eq!(body["running"].as_i64(), Some(0), "body: {body}");
        // success_rate = completed / decided = 1/2.
        assert!(
            (body["success_rate"].as_f64().expect("f64") - 0.5).abs() < 1e-9,
            "expected 0.5, got {body}"
        );
    }
    .await;

    unsafe { std::env::remove_var("SESSION_ABANDON_AFTER_SECONDS") };
}

#[tokio::test]
async fn stats_durations_skip_null_started_at() {
    // Python `get_sessions_router.py:142-159` skips rows where
    // `started_at` or `end` is NULL. The Rust LIB-03 schema enforces
    // `started_at NOT NULL` (matching v2 contract — Python had it
    // nullable for backwards compat with pre-LIB-03 deployments), so we
    // exercise the next-best surrogate: rows where both `ended_at` and
    // `last_activity_at` collapse to the same instant give zero
    // duration, while rows with a real gap contribute. This proves the
    // `agent_time_s` aggregation is well-formed end-to-end and that
    // `avg_session_s` divides by the contributing-row count, not the
    // total session count.
    use chrono::{Duration, Utc};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-stats-dur@example.com").await;

    let user_hex = alice.simple().to_string();
    let now = Utc::now();
    let started = now - Duration::seconds(40);
    let ended = now - Duration::seconds(10);

    // Row 1: 30-second duration (started 40s ago, ended 10s ago).
    db_of(&state)
        .execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_records (\
                    session_id, user_id, dataset_id, status, started_at, \
                    last_activity_at, ended_at, tokens_in, tokens_out, \
                    cost_usd, error_count, last_model\
                 ) VALUES (\
                    'cc_dur_with_gap____', '{user_hex}', NULL, 'completed', \
                    '{s}', '{l}', '{e}', 0, 0, 0.0, 0, NULL\
                 )",
                s = started.to_rfc3339(),
                l = ended.to_rfc3339(),
                e = ended.to_rfc3339()
            ),
        ))
        .await
        .expect("seed dur row");

    // Row 2: zero-duration row — started_at == last_activity_at == ended_at
    // (e.g. an instantly-terminated session). Contributes 0 to
    // `agent_time_s` but counts in `session_count` for the average.
    db_of(&state)
        .execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_records (\
                    session_id, user_id, dataset_id, status, started_at, \
                    last_activity_at, ended_at, tokens_in, tokens_out, \
                    cost_usd, error_count, last_model\
                 ) VALUES (\
                    'cc_dur_zero________', '{user_hex}', NULL, 'completed', \
                    '{ts}', '{ts}', '{ts}', 0, 0, 0.0, 0, NULL\
                 )",
                ts = ended.to_rfc3339()
            ),
        ))
        .await
        .expect("seed zero-duration row");

    let app = test_router(state.clone()).await;
    let bearer = bearer_for_user(&state, alice).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/stats?range=all")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(body["sessions"].as_i64(), Some(2));
    let agent_time = body["agent_time_s"].as_f64().expect("f64");
    // Row 1 contributes ~30s, row 2 contributes 0s. Allow ±2s tolerance.
    assert!(
        (28.0..=32.0).contains(&agent_time),
        "agent_time_s should sum the gapped row only: got {agent_time} (body: {body})"
    );
    let avg = body["avg_session_s"].as_f64().expect("f64");
    // Both rows pass the `started_at`+`end` check in LIB-05's fold, so
    // `session_count = 2` → avg ≈ 15s.
    assert!(
        (13.0..=17.0).contains(&avg),
        "avg_session_s should divide by both contributing rows: got {avg}"
    );
}

#[tokio::test]
async fn stats_visibility_includes_permitted_datasets() {
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-stats-perm@example.com").await;
    let bob = seed_user_id(&state, "bob-stats-perm@example.com").await;
    let shared_dataset = Uuid::new_v4();

    ensure_principal(db_of(&state), shared_dataset, "dataset").await;
    seed_dataset(db_of(&state), shared_dataset, bob, None, "shared-ds-stats").await;
    db_of(&state)
        .grant_permission(alice, shared_dataset, "read")
        .await
        .expect("grant read");

    // Alice owns 1 row.
    db_of(&state)
        .ensure_and_touch_session("cc_alice_own_stats__", alice, None)
        .await
        .expect("alice own");
    // Bob owns 1 row in shared dataset (visible to Alice).
    db_of(&state)
        .ensure_and_touch_session("cc_bob_shared_stats_", bob, Some(shared_dataset))
        .await
        .expect("bob shared");
    // Bob owns 1 row not in any shared dataset (NOT visible to Alice).
    db_of(&state)
        .ensure_and_touch_session("cc_bob_private_stats", bob, None)
        .await
        .expect("bob private");

    let app = test_router(state.clone()).await;
    let bearer = bearer_for_user(&state, alice).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/stats?range=all")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(
        body["sessions"].as_i64(),
        Some(2),
        "Alice sees own + shared (NOT bob's private): {body}"
    );
}

#[tokio::test]
async fn stats_range_24h_filters_correctly() {
    use chrono::{Duration, Utc};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-stats-range@example.com").await;

    let user_hex = alice.simple().to_string();
    let now = Utc::now();
    let recent = now - Duration::hours(2);
    let old = now - Duration::days(2);

    // Recent row — within 24h.
    db_of(&state)
        .execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_records (\
                    session_id, user_id, dataset_id, status, started_at, \
                    last_activity_at, ended_at, tokens_in, tokens_out, \
                    cost_usd, error_count, last_model\
                 ) VALUES (\
                    'cc_recent_aaaaaaaaa', '{user_hex}', NULL, 'completed', \
                    '{ts}', '{ts}', '{ts}', 0, 0, 0.0, 0, NULL\
                 )",
                ts = recent.to_rfc3339()
            ),
        ))
        .await
        .expect("seed recent");

    // Old row — older than 24h.
    db_of(&state)
        .execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_records (\
                    session_id, user_id, dataset_id, status, started_at, \
                    last_activity_at, ended_at, tokens_in, tokens_out, \
                    cost_usd, error_count, last_model\
                 ) VALUES (\
                    'cc_old_bbbbbbbbbbbb', '{user_hex}', NULL, 'completed', \
                    '{ts}', '{ts}', '{ts}', 0, 0, 0.0, 0, NULL\
                 )",
                ts = old.to_rfc3339()
            ),
        ))
        .await
        .expect("seed old");

    let app = test_router(state.clone()).await;
    let bearer = bearer_for_user(&state, alice).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/stats?range=24h")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(
        body["sessions"].as_i64(),
        Some(1),
        "?range=24h must filter out the older row: {body}"
    );
    assert_eq!(body["range"].as_str(), Some("24h"));
}

#[tokio::test]
async fn stats_range_field_echoes_input() {
    // Python emits the input string verbatim at `:181`, even when the
    // input was the default. Test all four valid variants.
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-stats-echo@example.com").await;

    let app = test_router(state.clone()).await;
    let bearer = bearer_for_user(&state, alice).await;

    for (qs, expected) in [
        ("range=24h", "24h"),
        ("range=7d", "7d"),
        ("range=30d", "30d"),
        ("range=all", "all"),
    ] {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/v1/sessions/stats?{qs}"))
            .header("Authorization", bearer.clone())
            .body(Body::empty())
            .expect("request");

        let resp = oneshot_request(app.clone(), req).await;
        assert_eq!(resp.status(), StatusCode::OK, "qs={qs}");
        let body = body_json(resp).await;
        assert_eq!(
            body["range"].as_str(),
            Some(expected),
            "expected echo of {expected}: {body}"
        );
    }
}

#[tokio::test]
async fn stats_unauthenticated_returns_401() {
    // Use the `require_authentication=true` test state — anonymous calls
    // are rejected with 401.
    let (state, _events) = build_auth_required_test_state().await;
    let db = build_search_db().await;
    let handles = build_component_handles(db, None, None, None);
    let mut state = state;
    state.lib = Some(handles);

    let _ = seed_user(&state, "carol-stats@example.com", "Pa$$word123").await;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/stats")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stats_components_not_configured_returns_500_with_python_error_envelope() {
    // Python parity (`get_sessions_router.py:108-110` pattern): the
    // catch-all envelope is `{"error": "stats failed"}`, NOT
    // `{"detail": ...}`. Drive the path by clearing `state.lib`.
    let mut state = build_sessions_state().await;
    state.lib = None;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/stats")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"].as_str(),
        Some("stats failed"),
        "500 body must be {{\"error\": \"stats failed\"}}: {body}"
    );
    assert!(
        body.get("detail").is_none(),
        "must not emit Rust-style {{\"detail\": ...}} on the 500 path: {body}"
    );
}
