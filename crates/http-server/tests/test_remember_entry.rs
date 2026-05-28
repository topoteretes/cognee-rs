//! E-02 — `POST /api/v1/remember/entry` integration tests.
//!
//! Covers the seven test cases enumerated in
//! [`docs/http-api-v2/tasks/e-02-remember-entry.md`](../../../docs/http-api-v2/tasks/e-02-remember-entry.md)
//! §5:
//! 1. `qa_entry_returns_session_stored_with_entry_type_qa`
//! 2. `trace_entry_returns_session_stored_with_entry_type_trace`
//! 3. `feedback_entry_success_returns_session_stored_with_qa_id`
//! 4. `feedback_entry_qa_not_found_returns_errored_with_qa_id`
//! 5. `missing_session_id_returns_400_with_validation_envelope`
//! 6. `unknown_entry_type_returns_400_with_validation_envelope`
//! 7. `session_cache_unavailable_returns_503`

mod support;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_delete::DeleteService;
use cognee_http_server::components::ComponentHandles;
use cognee_http_server::{AppState, HttpServerConfig, build_router};
use cognee_ontology::OntologyManager;
use cognee_session::{FsSessionStore, SessionManager};
use cognee_storage::LocalStorage;
use tower::ServiceExt;

use support::body_json;

// ─── test scaffolding ─────────────────────────────────────────────────────────

async fn build_search_db_local() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("in-memory sqlite");
    initialize(&db).await.expect("init schema");
    Arc::new(db)
}

/// Build a `ComponentHandles` wired with an FS-backed `SessionStore` +
/// `SessionManager`. Used by the success-path tests.
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

    // FS-backed session store under a leaked tempdir so it stays alive for the
    // duration of the test.
    let session_dir = tempfile::tempdir().expect("tmp session");
    let store: Arc<dyn cognee_session::SessionStore> =
        Arc::new(FsSessionStore::new(session_dir.path().to_path_buf()));
    Box::leak(Box::new(session_dir));
    let session_manager = Arc::new(SessionManager::new(store.clone()));

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
        embedding_engine: None,
        ontology_resolver: None,
        permissions: None,
        sync_ops: None,
        session_store: Some(store),
        session_manager: Some(session_manager),
    })
}

/// Build a `ComponentHandles` **without** a session manager — used to verify
/// the 503 branch.
fn build_handles_without_session(db: Arc<DatabaseConnection>) -> Arc<ComponentHandles> {
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
        embedding_engine: None,
        ontology_resolver: None,
        permissions: None,
        sync_ops: None,
        session_store: None,
        session_manager: None,
    })
}

async fn build_app_with_session() -> axum::Router {
    let db = build_search_db_local().await;
    let handles = build_handles_with_session(db);
    let cfg = HttpServerConfig::default();
    let mut state = AppState::build(cfg).await.expect("build state");
    state.lib = Some(handles);
    build_router(state).await.expect("router")
}

async fn build_app_without_session() -> axum::Router {
    let db = build_search_db_local().await;
    let handles = build_handles_without_session(db);
    let cfg = HttpServerConfig::default();
    let mut state = AppState::build(cfg).await.expect("build state");
    state.lib = Some(handles);
    build_router(state).await.expect("router")
}

fn json_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/remember/entry")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .expect("request")
}

// ─── 1. QA entry ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn qa_entry_returns_session_stored_with_entry_type_qa() {
    let app = build_app_with_session().await;

    let raw = r#"{
        "entry": {"type": "qa", "question": "Q?", "answer": "A."},
        "datasetName": "main_dataset",
        "sessionId": "s-qa"
    }"#;
    let resp = app.oneshot(json_request(raw)).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);

    let v = body_json(resp).await;
    let obj = v.as_object().expect("object");
    assert_eq!(obj["status"], "session_stored");
    assert_eq!(obj["entry_type"], "qa");
    assert!(obj["entry_id"].is_string());
    assert!(!obj["entry_id"].as_str().unwrap().is_empty());
    assert_eq!(obj["session_ids"][0], "s-qa");
    // file/text-only fields are absent on the typed-entry path
    assert!(!obj.contains_key("content_hash"));
    assert!(!obj.contains_key("items"));
}

// ─── 2. Trace entry ───────────────────────────────────────────────────────────

#[tokio::test]
async fn trace_entry_returns_session_stored_with_entry_type_trace() {
    let app = build_app_with_session().await;

    let raw = r#"{
        "entry": {
            "type": "trace",
            "originFunction": "search",
            "status": "success",
            "memoryQuery": "what?",
            "memoryContext": "ctx"
        },
        "sessionId": "s-trace"
    }"#;
    let resp = app.oneshot(json_request(raw)).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);

    let v = body_json(resp).await;
    let obj = v.as_object().expect("object");
    assert_eq!(obj["status"], "session_stored");
    assert_eq!(obj["entry_type"], "trace");
    let id = obj["entry_id"].as_str().expect("entry_id string");
    // SessionManager generates UUID4 trace ids — assert non-empty.
    assert!(!id.is_empty());
    // Default dataset name parity check (Python: `dataset_name = "main_dataset"`).
    assert_eq!(obj["dataset_name"], "main_dataset");
}

// ─── 3. Feedback entry — success path ─────────────────────────────────────────

#[tokio::test]
async fn feedback_entry_success_returns_session_stored_with_qa_id() {
    let app = build_app_with_session().await;

    // Step 1 — create a QA so the feedback can attach to it.
    let session_id = "s-fb-ok";
    let qa_payload = format!(
        r#"{{
            "entry": {{"type": "qa", "question": "Q", "answer": "A"}},
            "sessionId": "{session_id}"
        }}"#
    );
    let resp = app
        .clone()
        .oneshot(json_request(&qa_payload))
        .await
        .expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let qa_id = v["entry_id"].as_str().expect("qa_id").to_string();

    // Step 2 — attach feedback.
    let fb_payload = format!(
        r#"{{
            "entry": {{
                "type": "feedback",
                "qaId": "{qa_id}",
                "feedbackText": "great",
                "feedbackScore": 5
            }},
            "sessionId": "{session_id}"
        }}"#
    );
    let resp = app.oneshot(json_request(&fb_payload)).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let obj = v.as_object().expect("object");
    assert_eq!(obj["status"], "session_stored");
    assert_eq!(obj["entry_type"], "feedback");
    // Python parity: entry_id == qa_id (the input qa_id).
    assert_eq!(obj["entry_id"], qa_id);
    // No error key when feedback succeeded.
    assert!(!obj.contains_key("error"));
}

// ─── 4. Feedback entry — qa not found ─────────────────────────────────────────

#[tokio::test]
async fn feedback_entry_qa_not_found_returns_errored_with_qa_id() {
    let app = build_app_with_session().await;

    let raw = r#"{
        "entry": {
            "type": "feedback",
            "qaId": "nonexistent-qa",
            "feedbackText": "missing"
        },
        "sessionId": "s-fb-miss"
    }"#;
    let resp = app.oneshot(json_request(raw)).await.expect("resp");
    // Python returns 200 with status=errored — not a 4xx.
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let obj = v.as_object().expect("object");
    assert_eq!(obj["status"], "errored");
    assert_eq!(obj["entry_type"], "feedback");
    // Python parity (remember.py:307): entry_id == input qa_id even on miss.
    assert_eq!(obj["entry_id"], "nonexistent-qa");
    // The error message documents the missing-qa condition (no PII).
    let err = obj["error"].as_str().expect("error string");
    assert!(err.contains("nonexistent-qa"), "error: {err}");
    assert!(err.contains("s-fb-miss"), "error: {err}");
}

// ─── 5. Missing session_id ────────────────────────────────────────────────────

#[tokio::test]
async fn missing_session_id_returns_400_with_validation_envelope() {
    let app = build_app_with_session().await;

    // No `sessionId` at all — serde rejects with a JSON-parse value_error
    // envelope. The wire-shape requirements (Decision 7) are: status 400,
    // body.detail is an array, the first entry's `loc` lists `"body"`.
    let raw = r#"{"entry": {"type": "qa", "question": "x", "answer": "y"}}"#;
    let resp = app.clone().oneshot(json_request(raw)).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    let detail = v["detail"].as_array().expect("detail array");
    assert!(!detail.is_empty(), "detail must be non-empty");
    let loc = detail[0]["loc"].as_array().expect("loc array");
    assert_eq!(loc[0].as_str(), Some("body"));
    let typ = detail[0]["type"].as_str().expect("type string");
    assert!(
        typ.contains("value_error"),
        "type should contain value_error, got {typ}"
    );

    // Empty-string `sessionId` — the handler's pre-validator rejects with
    // the more precise envelope (`loc == ["body","session_id"]`).
    let raw_empty = r#"{
        "entry": {"type": "qa", "question": "x", "answer": "y"},
        "sessionId": ""
    }"#;
    let resp = app.oneshot(json_request(raw_empty)).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    let detail = v["detail"].as_array().expect("detail array");
    assert_eq!(detail.len(), 1);
    let loc = detail[0]["loc"].as_array().expect("loc array");
    assert_eq!(loc[0].as_str(), Some("body"));
    assert_eq!(loc[1].as_str(), Some("session_id"));
    assert_eq!(detail[0]["type"], "value_error");
}

// ─── 6. Unknown entry type ────────────────────────────────────────────────────

#[tokio::test]
async fn unknown_entry_type_returns_400_with_validation_envelope() {
    let app = build_app_with_session().await;

    let raw = r#"{
        "entry": {"type": "bogus"},
        "sessionId": "s"
    }"#;
    let resp = app.oneshot(json_request(raw)).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    let detail = v["detail"].as_array().expect("detail array");
    assert!(!detail.is_empty(), "detail must be non-empty");
    // ValidatedJson surfaces serde failures under `loc == ["body"]` with a
    // `value_error.json_parse` type — the v1-envelope shape gap is
    // documented in IMPLEMENTATION-PROMPT.md (Decision 7) and accepted.
    let loc = detail[0]["loc"].as_array().expect("loc array");
    assert_eq!(loc[0].as_str(), Some("body"));
    let typ = detail[0]["type"].as_str().expect("type string");
    assert!(
        typ.contains("value_error"),
        "type should contain value_error, got {typ}"
    );
}

// ─── 7. Session cache unavailable ─────────────────────────────────────────────

#[tokio::test]
async fn session_cache_unavailable_returns_503() {
    let app = build_app_without_session().await;

    let raw = r#"{
        "entry": {"type": "qa", "question": "Q", "answer": "A"},
        "sessionId": "s-503"
    }"#;
    let resp = app.oneshot(json_request(raw)).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(resp).await;
    // Python uses the `{"error": "..."}` envelope (NOT `{"detail": "..."}`).
    let err = v["error"].as_str().expect("error key");
    assert!(
        err.contains("Session cache"),
        "error message should mention session cache, got {err}"
    );
    assert!(v.get("detail").is_none(), "must not use 'detail' key");
}
