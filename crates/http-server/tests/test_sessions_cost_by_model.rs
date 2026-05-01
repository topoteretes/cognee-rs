//! E-11 — `GET /api/v1/sessions/cost-by-model` integration tests.
//!
//! Nine tests covering:
//!
//! * single-model session yields one row
//! * mixed-model session splits across rows (`session_count == 1` per row)
//! * null/empty model falls back to `"unknown"` (LIB-05's repo applies the
//!   fallback at `crates/database/src/ops/session_lifecycle.rs:822`)
//! * `?range=24h` filters older rows through the `JOIN session_records` predicate
//! * permitted-dataset visibility (caller's own + ACL-`read`)
//! * ordered by total `cost_usd` descending
//! * empty response is `[]` (a JSON array, not `null`)
//! * 401 unauthenticated guard
//! * 500 `{"error":"cost-by-model failed"}` envelope when components are unwired

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

/// Build an `AppState` for E-11 cost-by-model tests — same wiring as the
/// E-09 list / E-10 stats tests: real `DatabaseConnection`, auth on,
/// anonymous default rejected (set per-test via bearer header).
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

/// Issue a `GET /api/v1/sessions/cost-by-model` and return the parsed JSON
/// body. Used by every test that drives the happy path.
async fn cost_by_model_json(
    state: AppState,
    user_id: Uuid,
    qs: &str,
) -> (StatusCode, serde_json::Value) {
    let app = test_router(state.clone()).await;
    let bearer = bearer_for_user(&state, user_id).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/sessions/cost-by-model?{qs}"))
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    let status = resp.status();
    let body = body_json(resp).await;
    (status, body)
}

#[tokio::test]
async fn single_model_session_yields_one_row() {
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-cbm-single@example.com").await;

    db_of(&state)
        .ensure_and_touch_session("cc_cbm_single_______", alice, None)
        .await
        .expect("ensure");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_single_______",
            alice,
            Some("gpt-4o-mini"),
            100,
            200,
            0.5,
            false,
        )
        .await
        .expect("accumulate");

    let (status, body) = cost_by_model_json(state, alice, "range=all").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected one row: {body}");
    let row = &arr[0];
    assert_eq!(row["model"].as_str(), Some("gpt-4o-mini"));
    assert_eq!(row["session_count"].as_i64(), Some(1));
    assert_eq!(row["tokens_in"].as_i64(), Some(100));
    assert_eq!(row["tokens_out"].as_i64(), Some(200));
    assert!(
        (row["cost_usd"].as_f64().expect("f64") - 0.5).abs() < 1e-9,
        "row: {row}"
    );
}

#[tokio::test]
async fn mixed_model_session_splits_correctly() {
    // One session emitting usage for two distinct models → two rows,
    // each with `session_count == 1`.
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-cbm-mixed@example.com").await;

    db_of(&state)
        .ensure_and_touch_session("cc_cbm_mixed________", alice, None)
        .await
        .expect("ensure");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_mixed________",
            alice,
            Some("gpt-4o-mini"),
            10,
            20,
            0.10,
            false,
        )
        .await
        .expect("acc model A");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_mixed________",
            alice,
            Some("gpt-4o"),
            30,
            40,
            0.40,
            false,
        )
        .await
        .expect("acc model B");

    let (status, body) = cost_by_model_json(state, alice, "range=all").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 2, "expected two rows: {body}");
    for row in arr {
        assert_eq!(
            row["session_count"].as_i64(),
            Some(1),
            "each model row counts the session once: {row}"
        );
    }
    // Sorted by SUM(cost_usd) DESC — gpt-4o (0.40) before gpt-4o-mini (0.10).
    assert_eq!(arr[0]["model"].as_str(), Some("gpt-4o"));
    assert_eq!(arr[1]["model"].as_str(), Some("gpt-4o-mini"));
}

#[tokio::test]
async fn null_model_falls_back_to_unknown() {
    // The LIB-05 repository folds null/empty-model rows into a single
    // `"unknown"` bucket via `r.model.unwrap_or_else(|| "unknown")` at
    // `crates/database/src/ops/session_lifecycle.rs:822`, mirroring
    // Python's `row.model or "unknown"` (`get_sessions_router.py:244`).
    //
    // The LIB-03 schema declares `session_model_usage.model` NOT NULL +
    // part of the composite PK (`(session_id, user_id, model)`), so a
    // straight INSERT can't produce NULL. To exercise the fallback we
    // recreate the table without the NOT NULL constraint and seed a NULL
    // row — verifying the handler renders the literal string `"unknown"`.
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-cbm-null@example.com").await;

    db_of(&state)
        .ensure_and_touch_session("cc_cbm_null_________", alice, None)
        .await
        .expect("ensure");

    // Drop the NOT NULL constraint on `model` so we can seed a NULL row.
    // SQLite has no `ALTER COLUMN DROP NOT NULL`, so we drop+recreate the
    // table (only this test cares; `accumulate_usage` always passes a
    // non-null model in production).
    for sql in [
        "DROP TABLE session_model_usage",
        "CREATE TABLE session_model_usage (\
            session_id TEXT NOT NULL,\
            user_id TEXT NOT NULL,\
            model TEXT,\
            tokens_in INTEGER NOT NULL DEFAULT 0,\
            tokens_out INTEGER NOT NULL DEFAULT 0,\
            cost_usd REAL NOT NULL DEFAULT 0.0,\
            updated_at TEXT NOT NULL,\
            PRIMARY KEY (session_id, user_id)\
         )",
    ] {
        db_of(&state)
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                sql.to_string(),
            ))
            .await
            .expect("recreate session_model_usage without NOT NULL on model");
    }

    let user_hex = alice.simple().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    db_of(&state)
        .execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_model_usage (\
                    session_id, user_id, model, tokens_in, tokens_out, cost_usd, updated_at\
                 ) VALUES (\
                    'cc_cbm_null_________', '{user_hex}', NULL, 5, 7, 0.05, '{now}'\
                 )"
            ),
        ))
        .await
        .expect("seed null-model row");

    let (status, body) = cost_by_model_json(state, alice, "range=all").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected one row: {body}");
    assert_eq!(
        arr[0]["model"].as_str(),
        Some("unknown"),
        "null model must fold to the literal string \"unknown\": {body}"
    );
    assert_eq!(arr[0]["tokens_in"].as_i64(), Some(5));
    assert_eq!(arr[0]["tokens_out"].as_i64(), Some(7));
}

#[tokio::test]
async fn range_24h_filters_through_join() {
    // Two sessions: one recent (within 24h) and one old (> 24h). The
    // `JOIN session_records sr ON ... WHERE sr.last_activity_at >= since`
    // predicate must filter out the old session. Each session has a
    // different model so we can identify which row was kept.
    use chrono::{Duration, Utc};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-cbm-range@example.com").await;

    let user_hex = alice.simple().to_string();
    let now = Utc::now();
    let recent = now - Duration::hours(2);
    let old = now - Duration::days(2);

    // Recent row + recent usage on `gpt-4o-mini`.
    db_of(&state)
        .execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_records (\
                    session_id, user_id, dataset_id, status, started_at, \
                    last_activity_at, ended_at, tokens_in, tokens_out, \
                    cost_usd, error_count, last_model\
                 ) VALUES (\
                    'cc_cbm_recent_______', '{user_hex}', NULL, 'completed', \
                    '{ts}', '{ts}', '{ts}', 0, 0, 0.0, 0, NULL\
                 )",
                ts = recent.to_rfc3339()
            ),
        ))
        .await
        .expect("seed recent");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_recent_______",
            alice,
            Some("gpt-4o-mini"),
            10,
            20,
            0.10,
            false,
        )
        .await
        .expect("acc recent");

    // Old row + old usage on `gpt-4o`.
    db_of(&state)
        .execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_records (\
                    session_id, user_id, dataset_id, status, started_at, \
                    last_activity_at, ended_at, tokens_in, tokens_out, \
                    cost_usd, error_count, last_model\
                 ) VALUES (\
                    'cc_cbm_old__________', '{user_hex}', NULL, 'completed', \
                    '{ts}', '{ts}', '{ts}', 0, 0, 0.0, 0, NULL\
                 )",
                ts = old.to_rfc3339()
            ),
        ))
        .await
        .expect("seed old");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_old__________",
            alice,
            Some("gpt-4o"),
            999,
            999,
            9.99,
            false,
        )
        .await
        .expect("acc old");

    let (status, body) = cost_by_model_json(state, alice, "range=24h").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("array");
    assert_eq!(
        arr.len(),
        1,
        "?range=24h must filter out the old session via the JOIN: {body}"
    );
    assert_eq!(arr[0]["model"].as_str(), Some("gpt-4o-mini"));
}

#[tokio::test]
async fn visibility_through_dataset_permissions() {
    // Alice owns a session; Bob owns one in a shared dataset (alice has
    // read) and one in a private dataset (alice does NOT). Alice's
    // /cost-by-model must include her own + bob's shared, but NOT
    // bob's private session.
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-cbm-perm@example.com").await;
    let bob = seed_user_id(&state, "bob-cbm-perm@example.com").await;
    let shared_dataset = Uuid::new_v4();

    ensure_principal(db_of(&state), shared_dataset, "dataset").await;
    seed_dataset(db_of(&state), shared_dataset, bob, None, "shared-ds-cbm").await;
    db_of(&state)
        .grant_permission(alice, shared_dataset, "read")
        .await
        .expect("grant read");

    // Alice's own (model A).
    db_of(&state)
        .ensure_and_touch_session("cc_cbm_alice_own____", alice, None)
        .await
        .expect("alice own");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_alice_own____",
            alice,
            Some("model-a"),
            1,
            1,
            0.10,
            false,
        )
        .await
        .expect("acc alice");
    // Bob's shared (model B) — visible to alice via dataset permission.
    db_of(&state)
        .ensure_and_touch_session("cc_cbm_bob_shared___", bob, Some(shared_dataset))
        .await
        .expect("bob shared");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_bob_shared___",
            bob,
            Some("model-b"),
            2,
            2,
            0.20,
            false,
        )
        .await
        .expect("acc bob shared");
    // Bob's private (model C) — NOT visible to alice.
    db_of(&state)
        .ensure_and_touch_session("cc_cbm_bob_private__", bob, None)
        .await
        .expect("bob private");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_bob_private__",
            bob,
            Some("model-c"),
            3,
            3,
            0.30,
            false,
        )
        .await
        .expect("acc bob private");

    let (status, body) = cost_by_model_json(state, alice, "range=all").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("array");
    let models: Vec<&str> = arr
        .iter()
        .filter_map(|r| r["model"].as_str())
        .collect::<Vec<_>>();
    assert!(
        models.contains(&"model-a"),
        "alice's own session: {models:?}"
    );
    assert!(
        models.contains(&"model-b"),
        "bob's shared session via permitted dataset: {models:?}"
    );
    assert!(
        !models.contains(&"model-c"),
        "bob's private session must be hidden: {models:?}"
    );
}

#[tokio::test]
async fn ordered_by_total_cost_desc() {
    // Three rows with distinct cost totals. Repository orders by
    // `SUM(smu.cost_usd) DESC` (`ops/session_lifecycle.rs:811`).
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-cbm-order@example.com").await;

    db_of(&state)
        .ensure_and_touch_session("cc_cbm_order_______1", alice, None)
        .await
        .expect("ensure 1");
    db_of(&state)
        .ensure_and_touch_session("cc_cbm_order_______2", alice, None)
        .await
        .expect("ensure 2");
    db_of(&state)
        .ensure_and_touch_session("cc_cbm_order_______3", alice, None)
        .await
        .expect("ensure 3");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_order_______1",
            alice,
            Some("model-cheap"),
            1,
            1,
            0.10,
            false,
        )
        .await
        .expect("acc cheap");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_order_______2",
            alice,
            Some("model-mid"),
            1,
            1,
            0.50,
            false,
        )
        .await
        .expect("acc mid");
    db_of(&state)
        .accumulate_usage(
            "cc_cbm_order_______3",
            alice,
            Some("model-expensive"),
            1,
            1,
            5.00,
            false,
        )
        .await
        .expect("acc expensive");

    let (status, body) = cost_by_model_json(state, alice, "range=all").await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 3, "expected three rows: {body}");
    let models: Vec<&str> = arr.iter().filter_map(|r| r["model"].as_str()).collect();
    assert_eq!(
        models,
        vec!["model-expensive", "model-mid", "model-cheap"],
        "rows must be ordered by SUM(cost_usd) DESC: {body}"
    );
}

#[tokio::test]
async fn empty_response_is_array_not_null() {
    // Fresh DB → no rows. Python returns `[]` via `jsonable_encoder([])`;
    // Rust must do the same (a JSON array, never `null`).
    let state = build_sessions_state().await;
    let alice = seed_user_id(&state, "alice-cbm-empty@example.com").await;

    let (status, body) = cost_by_model_json(state, alice, "range=all").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array(), "empty response must be an array: {body}");
    assert_eq!(
        body.as_array().expect("array").len(),
        0,
        "expected empty array: {body}"
    );
}

#[tokio::test]
async fn unauthenticated_returns_401() {
    // Use the `require_authentication=true` test state — anonymous calls
    // are rejected with 401.
    let (state, _events) = build_auth_required_test_state().await;
    let db = build_search_db().await;
    let handles = build_component_handles(db, None, None, None);
    let mut state = state;
    state.lib = Some(handles);

    let _ = seed_user(&state, "carol-cbm@example.com", "Pa$$word123").await;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/cost-by-model")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn internal_error_returns_envelope() {
    // Python parity (`get_sessions_router.py:108-110` pattern): the
    // catch-all envelope is `{"error": "cost-by-model failed"}`, NOT
    // `{"detail": ...}`. Drive the path by clearing `state.lib`.
    let mut state = build_sessions_state().await;
    state.lib = None;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/cost-by-model")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"].as_str(),
        Some("cost-by-model failed"),
        "500 body must be {{\"error\": \"cost-by-model failed\"}}: {body}"
    );
    assert!(
        body.get("detail").is_none(),
        "must not emit Rust-style {{\"detail\": ...}} on the 500 path: {body}"
    );
}
