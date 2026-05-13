//! E-12 — `GET /api/v1/sessions/{session_id}` integration tests.
//!
//! Seven tests covering:
//!
//! * 404 `{detail}` envelope when the session id is unknown.
//! * 404 `{detail}` envelope when the row exists but is invisible to the caller.
//! * `label` falls back to first trace's `origin_function` when no QAs.
//! * `label` truncates a long question to 120 *chars* (not bytes).
//! * `qas` and `traces` lists are tail-truncated to 20 entries while
//!   `msg_count` / `tool_calls` reflect the pre-truncation lengths.
//! * Empty `qas` / `traces` when the `SessionManager` slot is unwired
//!   (Python parity for `is_available=False`).
//! * Dataset-grant viewer sees the cache content of someone else's session
//!   (owner-aware cache lookup).
//!
//! Plus the `404 {detail}` envelope is the **only** v2 endpoint that emits
//! the `{detail}` shape — every other catch-all in this router uses
//! `{error}`. See `docs/http-api-v2/README.md §1.1`.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use cognee_database::{AclDb, DatabaseConnection, SessionLifecycleDb};
use cognee_delete::DeleteService;
use cognee_http_server::components::ComponentHandles;
use cognee_ontology::OntologyManager;
use cognee_session::{FsSessionStore, SessionManager, SessionStore};
use cognee_storage::LocalStorage;
use std::sync::Arc;
use uuid::Uuid;

use support::{
    bearer_header, body_json, build_auth_required_test_state, build_component_handles,
    build_search_db, ensure_principal, oneshot_request, seed_dataset, seed_perm_user, seed_user,
    test_router,
};

use cognee_http_server::AppState;

// ─── State builders ──────────────────────────────────────────────────────────

/// Build an `AppState` with auth + components + a real FS-backed
/// `SessionStore` and `SessionManager`. Returns the state and the store
/// (so tests can pre-seed QAs / traces directly via the trait).
async fn build_sessions_detail_state() -> (AppState, Arc<dyn SessionStore>, Arc<SessionManager>) {
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

    let handles = build_handles_with_session(db_conn.clone());
    let store = handles.session_store.clone().expect("session_store wired");
    let sm = handles
        .session_manager
        .clone()
        .expect("session_manager wired");

    let state = AppState {
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
    };
    (state, store, sm)
}

/// Build a `ComponentHandles` with an FS-backed session store + manager.
/// The store/manager share the same `Arc`, so seeding one is observable
/// through the other.
fn build_handles_with_session(db: Arc<DatabaseConnection>) -> Arc<ComponentHandles> {
    let storage_dir = tempfile::tempdir().expect("tmp storage");
    let storage = Arc::new(LocalStorage::new(storage_dir.path().to_path_buf()))
        as Arc<dyn cognee_storage::StorageTrait>;
    Box::leak(Box::new(storage_dir));
    let delete_service = Arc::new(DeleteService::new(
        Arc::clone(&storage),
        db.clone() as Arc<dyn cognee_database::DeleteDb>,
    ));
    let ontology_dir = tempfile::tempdir().expect("tmp ontology");
    let ontology_manager = Arc::new(OntologyManager::new(ontology_dir.path().to_path_buf()));
    Box::leak(Box::new(ontology_dir));

    let session_dir = tempfile::tempdir().expect("tmp session");
    let store: Arc<dyn SessionStore> =
        Arc::new(FsSessionStore::new(session_dir.path().to_path_buf()));
    Box::leak(Box::new(session_dir));
    let session_manager = Arc::new(SessionManager::new(store.clone()));

    let permissions: Option<Arc<dyn cognee_database::permissions::PermissionsRepository>> = Some(
        Arc::new(cognee_database::permissions::SeaOrmPermissionsRepository::new(db.clone())),
    );
    let sync_ops: Option<Arc<dyn cognee_database::SyncOperationRepository>> = Some(Arc::new(
        cognee_database::SeaOrmSyncOperationRepository::new(db.clone()),
    ));

    Arc::new(ComponentHandles {
        database: db,
        storage,
        delete_service,
        ontology_manager,
        search_orchestrator: None,
        llm: None,
        graph_db: None,
        vector_db: None,
        thread_pool: None,
        permissions,
        sync_ops,
        session_store: Some(store),
        session_manager: Some(session_manager),
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn db_of(state: &AppState) -> &DatabaseConnection {
    state.components().expect("components").database.as_ref()
}

async fn seed_user_id(state: &AppState, email: &str) -> Uuid {
    let user = seed_perm_user(state, email, "Pa$$word123").await;
    user.id
}

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

/// Get the canonical owner-hex string used by the session row layer. The
/// repo persists user UUIDs as 32-char hex strings, so the cache must be
/// keyed using the same encoding.
fn owner_hex(user_id: Uuid) -> String {
    user_id.simple().to_string()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn detail_returns_404_for_unknown_session() {
    // No row in the DB. Python returns `404 {"detail":"session not found"}`
    // via `HTTPException(404, ...)` — the only v2 endpoint that emits the
    // `{detail}` envelope.
    let (state, _store, _sm) = build_sessions_detail_state().await;
    let alice = seed_user_id(&state, "alice-detail-404@example.com").await;
    let bearer = bearer_for_user(&state, alice).await;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/cc_unknown_zzzzzzzzz")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(
        body["detail"].as_str(),
        Some("session not found"),
        "404 envelope must use Python `HTTPException` `{{detail}}` shape: {body}"
    );
    assert!(
        body.get("error").is_none(),
        "must not emit `{{error}}` on the 404 path: {body}"
    );
}

#[tokio::test]
async fn detail_returns_404_when_no_visibility() {
    // Row exists owned by Bob with no dataset; Alice is not the owner and
    // has no dataset-grant — the visibility check fails so we get 404
    // (not 403). Mirrors Python's `get_session_row` returning `None`.
    let (state, _store, _sm) = build_sessions_detail_state().await;
    let alice = seed_user_id(&state, "alice-no-vis@example.com").await;
    let bob = seed_user_id(&state, "bob-no-vis@example.com").await;

    db_of(&state)
        .ensure_and_touch_session("cc_bob_private_no_vis", bob, None)
        .await
        .expect("seed bob row");

    let bearer = bearer_for_user(&state, alice).await;
    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/cc_bob_private_no_vis")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["detail"].as_str(), Some("session not found"));
}

#[tokio::test]
async fn detail_label_falls_back_to_origin_function_when_no_qas() {
    // No QA entries; one trace step. Label = trace[0].origin_function.
    let (state, _store, sm) = build_sessions_detail_state().await;
    let alice = seed_user_id(&state, "alice-label-trace@example.com").await;
    let session_id = "cc_alice_label_trace_";

    db_of(&state)
        .ensure_and_touch_session(session_id, alice, None)
        .await
        .expect("seed row");

    sm.add_agent_trace_step(
        &owner_hex(alice),
        Some(session_id),
        "vector_search",
        "success",
        "",
        "",
        serde_json::Value::Null,
        None,
        "",
        "",
    )
    .await
    .expect("seed trace");

    let bearer = bearer_for_user(&state, alice).await;
    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/sessions/{session_id}"))
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["session_id"], session_id);
    assert_eq!(body["msg_count"].as_u64(), Some(0));
    assert_eq!(body["tool_calls"].as_u64(), Some(1));
    assert_eq!(
        body["label"].as_str(),
        Some("vector_search"),
        "label must fall back to first non-empty origin_function: {body}"
    );
}

#[tokio::test]
async fn detail_label_truncates_long_question_to_120_chars() {
    // Python's `str[:120]` slices Unicode code points, not bytes — a
    // multi-byte char like `é` (2 bytes UTF-8) still counts as one
    // position. Use `é * 200` so byte length (400) and char length (200)
    // diverge. Expected label: 120 `é`s (240 bytes).
    let (state, store, _sm) = build_sessions_detail_state().await;
    let alice = seed_user_id(&state, "alice-label-trunc@example.com").await;
    let session_id = "cc_alice_label_trunc";

    db_of(&state)
        .ensure_and_touch_session(session_id, alice, None)
        .await
        .expect("seed row");

    let long_q: String = "é".repeat(200);
    let owner = owner_hex(alice);
    store
        .create_qa_entry(session_id, Some(&owner), &long_q, "ans", None)
        .await
        .expect("seed qa");

    let bearer = bearer_for_user(&state, alice).await;
    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/sessions/{session_id}"))
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let label = body["label"].as_str().expect("label string");
    assert_eq!(
        label.chars().count(),
        120,
        "label must be 120 chars (Python `str[:120]`), got {} chars / {} bytes",
        label.chars().count(),
        label.len()
    );
    assert!(
        label.chars().all(|c| c == 'é'),
        "label must consist only of `é`: {label}"
    );
}

#[tokio::test]
async fn detail_caps_qas_and_traces_at_20_with_pretruncation_counts() {
    // Seed 25 QAs and 25 traces. Wire response truncates each to last 20
    // (oldest of the 20 first), but `msg_count` / `tool_calls` reflect
    // the unbounded length 25 (Python computes those before slicing).
    let (state, store, sm) = build_sessions_detail_state().await;
    let alice = seed_user_id(&state, "alice-cap@example.com").await;
    let session_id = "cc_alice_cap_2020";
    db_of(&state)
        .ensure_and_touch_session(session_id, alice, None)
        .await
        .expect("seed row");

    let owner = owner_hex(alice);
    for i in 0..25u32 {
        store
            .create_qa_entry(
                session_id,
                Some(&owner),
                &format!("q{i:02}"),
                &format!("a{i:02}"),
                None,
            )
            .await
            .expect("seed qa");
        sm.add_agent_trace_step(
            &owner,
            Some(session_id),
            &format!("origin_{i:02}"),
            "success",
            "",
            "",
            serde_json::Value::Null,
            None,
            "",
            "",
        )
        .await
        .expect("seed trace");
    }

    let bearer = bearer_for_user(&state, alice).await;
    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/sessions/{session_id}"))
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["msg_count"].as_u64(),
        Some(25),
        "msg_count is pre-truncation length: {body}"
    );
    assert_eq!(
        body["tool_calls"].as_u64(),
        Some(25),
        "tool_calls is pre-truncation length: {body}"
    );
    let qas = body["qas"].as_array().expect("qas array");
    let traces = body["traces"].as_array().expect("traces array");
    assert_eq!(qas.len(), 20, "qas truncated to last 20: {body}");
    assert_eq!(traces.len(), 20, "traces truncated to last 20: {body}");
    // Oldest of the trailing 20 is index 5 (qa05 / origin_05).
    assert_eq!(qas[0]["question"], "q05");
    assert_eq!(qas[19]["question"], "q24");
    assert_eq!(traces[0]["origin_function"], "origin_05");
    assert_eq!(traces[19]["origin_function"], "origin_24");
    // Label = first non-empty `question` (q00 — full pre-truncation list).
    assert_eq!(body["label"].as_str(), Some("q00"));
}

#[tokio::test]
async fn detail_returns_empty_lists_when_session_manager_unavailable() {
    // The default `build_component_handles` leaves both slots `None`. The
    // row body must still come through with empty `qas` / `traces` and
    // zero counts (Python parity for `is_available=False`).
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

    // Default handles: session_store/session_manager are both None.
    let handles = build_component_handles(db_conn.clone(), None, None, None);

    let state = AppState {
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
    };

    let alice = seed_user_id(&state, "alice-no-sm@example.com").await;
    let session_id = "cc_alice_no_sm______";
    db_of(&state)
        .ensure_and_touch_session(session_id, alice, None)
        .await
        .expect("seed row");

    let bearer = bearer_for_user(&state, alice).await;
    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/sessions/{session_id}"))
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["session_id"], session_id);
    assert_eq!(body["msg_count"].as_u64(), Some(0));
    assert_eq!(body["tool_calls"].as_u64(), Some(0));
    assert_eq!(body["qas"].as_array().expect("qas").len(), 0);
    assert_eq!(body["traces"].as_array().expect("traces").len(), 0);
    assert!(
        body["label"].is_null(),
        "label must be null when no qas/traces: {body}"
    );
}

#[tokio::test]
async fn detail_dataset_grant_views_other_users_session() {
    // Bob owns a session attached to a dataset; Alice has `read` on that
    // dataset. Alice can hit the endpoint and see the row, AND the cache
    // content under Bob's user_id (owner-aware lookup).
    let (state, store, _sm) = build_sessions_detail_state().await;
    let alice = seed_user_id(&state, "alice-grant-detail@example.com").await;
    let bob = seed_user_id(&state, "bob-grant-detail@example.com").await;
    let shared_dataset = Uuid::new_v4();

    ensure_principal(db_of(&state), shared_dataset, "dataset").await;
    seed_dataset(db_of(&state), shared_dataset, bob, None, "shared-detail").await;
    db_of(&state)
        .grant_permission(alice, shared_dataset, "read")
        .await
        .expect("grant read");

    let session_id = "cc_bob_shared_detail";
    db_of(&state)
        .ensure_and_touch_session(session_id, bob, Some(shared_dataset))
        .await
        .expect("seed row");

    // Cache content keyed by Bob's user_id (the row owner).
    let bob_owner = owner_hex(bob);
    store
        .create_qa_entry(
            session_id,
            Some(&bob_owner),
            "what is the meaning of life?",
            "42",
            None,
        )
        .await
        .expect("seed qa");

    let bearer = bearer_for_user(&state, alice).await;
    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/sessions/{session_id}"))
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["session_id"], session_id);
    assert_eq!(body["user_id"].as_str(), Some(bob_owner.as_str()));
    assert_eq!(body["msg_count"].as_u64(), Some(1));
    let qas = body["qas"].as_array().expect("qas");
    assert_eq!(qas.len(), 1);
    assert_eq!(qas[0]["question"], "what is the meaning of life?");
    assert_eq!(qas[0]["answer"], "42");
    assert_eq!(
        body["label"].as_str(),
        Some("what is the meaning of life?"),
        "label = first qa.question: {body}"
    );
}

#[tokio::test]
async fn detail_unauthenticated_returns_401() {
    // Use the `require_authentication=true` test state — anonymous calls
    // are rejected with 401 before the handler runs.
    let (state, _events) = build_auth_required_test_state().await;
    let db = build_search_db().await;
    let handles = build_component_handles(db, None, None, None);
    let mut state = state;
    state.lib = Some(handles);
    let _ = seed_user(&state, "carol-detail@example.com", "Pa$$word123").await;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/cc_anything")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn detail_components_not_configured_returns_500_with_python_error_envelope() {
    // Python parity: 500 path returns `{"error":"session detail failed"}`
    // (matches the catch-all envelope used by the three sibling endpoints).
    // Drive that path by clearing `state.lib` so `components()` returns None.
    let (mut state, _store, _sm) = build_sessions_detail_state().await;
    state.lib = None;

    let app = test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sessions/cc_anything")
        .body(Body::empty())
        .expect("request");

    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"].as_str(),
        Some("session detail failed"),
        "500 body must be {{\"error\": \"session detail failed\"}}: {body}"
    );
    assert!(
        body.get("detail").is_none(),
        "500 path must not emit Rust-style {{\"detail\": ...}}: {body}"
    );
}
