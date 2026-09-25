#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! P4 Step 15 — recall integration tests.

mod support;

use axum::{body::Body, http::Request};
use cognee_search::types::SearchType;
use std::sync::Arc;
use tower::ServiceExt;

use support::{
    RecordingRetriever, StubRetriever, body_json, build_orchestrator,
    build_orchestrator_with_dataset_resolver, build_p4_state, build_search_db,
    default_test_user_id, seed_dataset,
};

async fn build_app_with(
    retriever: Arc<dyn cognee_search::retrievers::SearchRetriever>,
) -> axum::Router {
    let db = build_search_db().await;
    let orchestrator = build_orchestrator(db, retriever).await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    cognee_http_server::build_router(state)
        .await
        .expect("router")
}

#[tokio::test]
async fn post_recall_returns_search_results() {
    // Default scope (auto, no session_id) -> graph-only. The graph branch
    // returns `SearchOutput::Text("ans")` which becomes a `RecallItem` with
    // a string content; the wire shape wraps it as
    // `{"text": "ans", "_source": "graph"}`.
    let retriever = Arc::new(StubRetriever::text_for(SearchType::GraphCompletion, "ans"));
    let app = build_app_with(retriever).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"search_type":"GRAPH_COMPLETION","query":"hi"}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert!(body.is_array(), "expected flat array, got {body}");
    assert_eq!(body[0]["text"], "ans");
    assert_eq!(body[0]["_source"], "graph");
}

#[tokio::test]
async fn post_recall_passes_session_id() {
    // session_id alone -> auto resolves to [Session, Graph] with
    // auto_fallthrough=true. session_store is None so search_session
    // returns []; graph runs and the result tags _source=graph. The body
    // never errors -- session_id is plumbed through without crashing.
    let retriever = Arc::new(StubRetriever::text_for(SearchType::GraphCompletion, "ans"));
    let app = build_app_with(retriever).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"query":"hi","sessionId":"s1","scope":"graph"}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert_eq!(body[0]["_source"], "graph");
}

#[tokio::test]
async fn post_recall_scope_graph_only() {
    // Explicit scope=graph -> only graph runs.
    let retriever = Arc::new(StubRetriever::text_for(
        SearchType::GraphCompletion,
        "g-ans",
    ));
    let app = build_app_with(retriever).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"query":"q","scope":"graph"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert!(body.is_array());
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["_source"], "graph");
    assert_eq!(arr[0]["text"], "g-ans");
}

#[tokio::test]
async fn post_recall_scope_all_runs_four_sources() {
    // scope=all expands to [Graph, Session, Trace, GraphContext]. Without
    // session_store/session_manager wired, three of the four sources
    // return empty and only graph contributes — but the request itself
    // must succeed end-to-end, proving the four-source iteration runs.
    let retriever = Arc::new(StubRetriever::text_for(
        SearchType::GraphCompletion,
        "all-ans",
    ));
    let app = build_app_with(retriever).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"query":"q","scope":"all"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    // Only graph contributes (session/trace/graph_context handles None).
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["_source"], "graph");
}

#[tokio::test]
async fn post_recall_unknown_scope_returns_400_with_validation_envelope() {
    // Unknown scope value goes through `normalize_scope` which returns
    // SearchError::InvalidInput with a Python-parity message. The DTO's
    // custom `deserialize_with` surfaces it via `serde::de::Error::custom`,
    // and `ValidatedJson` maps that to the 400 validation envelope.
    let retriever = Arc::new(StubRetriever::text_for(SearchType::GraphCompletion, "ans"));
    let app = build_app_with(retriever).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"query":"hi","scope":"foo"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 400);
    let body = body_json(resp).await;
    let detail = body["detail"].as_array().expect("detail array");
    assert_eq!(detail.len(), 1);
    assert_eq!(detail[0]["loc"], serde_json::json!(["body"]));
    assert_eq!(detail[0]["type"], "value_error.json_parse");
    let msg = detail[0]["msg"].as_str().expect("msg string");
    assert!(
        msg.contains("Unknown recall scope(s)"),
        "msg should contain 'Unknown recall scope(s)': {msg}"
    );
    // The raw input body is echoed under top-level `body` (Python parity).
    assert!(body["body"].is_object(), "body echo missing: {body}");
    assert_eq!(body["body"]["scope"], "foo");
}

#[tokio::test]
async fn post_recall_response_emits_underscore_source_per_item() {
    // Every item in the response carries an `_source` field tagged with
    // its origin (graph, session, trace, graph_context).
    let retriever = Arc::new(StubRetriever::text_for(SearchType::GraphCompletion, "x"));
    let app = build_app_with(retriever).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"query":"hi"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    for item in body.as_array().expect("array") {
        assert!(
            item["_source"].is_string(),
            "every item must have _source string: {item}"
        );
    }
}

#[tokio::test]
async fn post_recall_prerequisite_error_returns_422_with_hint() {
    // InvalidInput from the retriever maps to the 422 prerequisite envelope.
    let retriever = Arc::new(StubRetriever::error_for(
        SearchType::GraphCompletion,
        "missing prereq",
    ));
    let app = build_app_with(retriever).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"query":"hi"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 422);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "Recall prerequisites not met");
    assert!(body["hint"].is_string());
    // Recall uses {error, hint}, NOT {error, detail}.
    assert!(body.get("detail").is_none());
}

#[tokio::test]
async fn get_recall_history_no_orchestrator_returns_500_single_field_envelope() {
    // Build a state without a search orchestrator wired — the GET handler
    // returns 500 with the single-field {error} envelope, NOT {error, detail}.
    let state = build_p4_state(None, None, None).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/recall")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 500);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"],
        "An error occurred while fetching recall history."
    );
    assert!(body.get("detail").is_none());
    assert!(body.get("hint").is_none());
}

#[tokio::test]
async fn post_recall_no_orchestrator_returns_409_catch_all() {
    // No orchestrator wired → 409 with the JustError envelope per parity.
    let state = build_p4_state(None, None, None).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"query":"x"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 409);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "An error occurred during recall.");
    assert!(body.get("hint").is_none());
}

#[tokio::test]
async fn get_recall_returns_same_history_as_get_search() {
    // Pin the "shared history rows" contract from routers/recall.md §2.1.
    let retriever = Arc::new(StubRetriever::text_for(SearchType::GraphCompletion, "ans"));
    let app = build_app_with(retriever).await;

    // Run a search to seed history.
    let post = Request::builder()
        .method("POST")
        .uri("/api/v1/search")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"query":"hi"}"#))
        .unwrap();
    let _ = app.clone().oneshot(post).await.expect("seed search");

    let search_req = Request::builder()
        .method("GET")
        .uri("/api/v1/search")
        .body(Body::empty())
        .unwrap();
    let recall_req = Request::builder()
        .method("GET")
        .uri("/api/v1/recall")
        .body(Body::empty())
        .unwrap();
    let s = body_json(app.clone().oneshot(search_req).await.expect("search")).await;
    let r = body_json(app.oneshot(recall_req).await.expect("recall")).await;
    assert_eq!(s, r, "search and recall histories must match");
}

/// Regression guard for #197 (issue #198): `POST /v1/recall` with a
/// `datasets` *name* filter must run dataset resolution as the caller.
///
/// The orchestrator resolves names owner-scoped and refuses to guess an owner
/// (`dataset name filter requires SearchRequest.user_id to identify the
/// owner`), so the router has to hand `run_graph` the authenticated
/// `user.id`. Before #197 it passed `None` and every name filter died with
/// that error; the lib `recall()` and CLI paths are pinned elsewhere, this is
/// the HTTP layer's copy.
///
/// The assertion is deliberately stronger than "200": the recording
/// retriever must have been reached with `dataset_ids == [<seeded id>]`.
/// That can only happen if the name resolved against the caller's owner id,
/// so the case goes red as soon as the router stops forwarding the user
/// (the orchestrator then returns 422 and the retriever is never invoked).
#[tokio::test]
async fn post_recall_dataset_name_filter_resolves_as_the_caller() {
    let db = build_search_db().await;
    let dataset_id = seed_dataset(&db, "notes", default_test_user_id()).await;
    let retriever = Arc::new(RecordingRetriever::new(SearchType::Chunks));
    let orchestrator = build_orchestrator_with_dataset_resolver(
        db,
        Arc::clone(&retriever) as Arc<dyn cognee_search::retrievers::SearchRetriever>,
    )
    .await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"query":"hi","searchType":"CHUNKS","scope":"graph","datasets":["notes"]}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(
        status, 200,
        "dataset name must resolve for its owner, got {status} with body {body}"
    );

    let seen = retriever
        .last_params()
        .expect("retriever must have been invoked after the dataset name resolved");
    assert_eq!(
        seen.dataset_ids.as_deref(),
        Some([dataset_id].as_slice()),
        "the caller's dataset id must reach the retriever"
    );
}

/// Complement to the guard above: resolution stays scoped to the caller.
/// A same-named dataset owned by someone else is invisible, so the request
/// surfaces the 422 prerequisites envelope (DatasetNotFound) rather than
/// silently widening to another owner's rows. Note this case cannot by
/// itself detect a dropped `user_id` — both failures map to 422 — which is
/// why the positive case above carries the #197 regression.
#[tokio::test]
async fn post_recall_dataset_name_filter_does_not_widen_to_another_owner() {
    let db = build_search_db().await;
    let stranger = uuid::Uuid::new_v4();
    assert_ne!(stranger, default_test_user_id());
    let _foreign_dataset_id = seed_dataset(&db, "notes", stranger).await;
    let retriever = Arc::new(RecordingRetriever::new(SearchType::Chunks));
    let orchestrator = build_orchestrator_with_dataset_resolver(
        db,
        Arc::clone(&retriever) as Arc<dyn cognee_search::retrievers::SearchRetriever>,
    )
    .await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"query":"hi","searchType":"CHUNKS","scope":"graph","datasets":["notes"]}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 422);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "Recall prerequisites not met");
    assert!(body["hint"].is_string());
    assert!(
        retriever.last_params().is_none(),
        "retriever must not run when the name does not resolve for the caller"
    );
}

/// Shared setup for the `dataset_ids` cases: a recording retriever behind an
/// orchestrator with the dataset resolver wired, plus the DB so a test can
/// seed whatever ownership it needs first.
async fn build_recording_app(
    db: Arc<cognee_database::DatabaseConnection>,
) -> (axum::Router, Arc<RecordingRetriever>) {
    let retriever = Arc::new(RecordingRetriever::new(SearchType::Chunks));
    let orchestrator = build_orchestrator_with_dataset_resolver(
        db,
        Arc::clone(&retriever) as Arc<dyn cognee_search::retrievers::SearchRetriever>,
    )
    .await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");
    (app, retriever)
}

fn post_recall_request(body: String) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

/// `POST /v1/recall` deserialized `datasetIds` and then never forwarded it —
/// `run_graph` hard-coded `dataset_ids: None` — so an id filter came back
/// `200` with unfiltered results. Python forwards it
/// (`get_recall_router.py:285`) and `_run_graph` hands it to
/// `authorized_search` (`recall.py:655-657, 767-770`).
///
/// Like the name-filter guard above, the assertion is on what reached the
/// retriever, not on the status: `SearchParams.dataset_ids` must equal the
/// requested id.
#[tokio::test]
async fn post_recall_dataset_ids_filter_reaches_the_retriever() {
    let db = build_search_db().await;
    let dataset_id = seed_dataset(&db, "notes", default_test_user_id()).await;
    let (app, retriever) = build_recording_app(db).await;

    let resp = app
        .oneshot(post_recall_request(format!(
            r#"{{"query":"hi","searchType":"CHUNKS","scope":"graph","datasetIds":["{dataset_id}"]}}"#
        )))
        .await
        .expect("resp");
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(
        status, 200,
        "an owned dataset id must be accepted, got {status} with body {body}"
    );

    let seen = retriever
        .last_params()
        .expect("retriever must have been invoked for an owned dataset id");
    assert_eq!(
        seen.dataset_ids.as_deref(),
        Some([dataset_id].as_slice()),
        "the requested dataset id must reach the retriever"
    );
}

/// The security case: an id owned by someone else must not widen access.
/// Python raises `PermissionDeniedError` from
/// `get_specific_user_permission_datasets` (`:30-38`) and the recall router
/// re-raises it into the global handler (`get_recall_router.py:322-325`), so
/// the wire answer is `403 {"detail": "... [PermissionDeniedError]"}` — not
/// an empty `200`, and not the other tenant's rows. The retriever must never
/// run.
#[tokio::test]
async fn post_recall_foreign_dataset_id_is_forbidden_and_never_searched() {
    let db = build_search_db().await;
    let stranger = uuid::Uuid::new_v4();
    assert_ne!(stranger, default_test_user_id());
    let foreign_dataset_id = seed_dataset(&db, "notes", stranger).await;
    let (app, retriever) = build_recording_app(db).await;

    let resp = app
        .oneshot(post_recall_request(format!(
            r#"{{"query":"hi","searchType":"CHUNKS","scope":"graph","datasetIds":["{foreign_dataset_id}"]}}"#
        )))
        .await
        .expect("resp");
    assert_eq!(resp.status(), 403);
    let body = body_json(resp).await;
    let detail = body["detail"]
        .as_str()
        .expect("Python-shaped {detail} body");
    assert!(
        detail.contains("[PermissionDeniedError]"),
        "detail must carry Python's exception name: {detail}"
    );
    assert!(
        retriever.last_params().is_none(),
        "retriever must not run for a dataset the caller does not own"
    );
}

/// A well-formed id that names nothing gets the same 403 as a foreign one
/// (Python's check is `len(permitted) != len(requested)`, whatever the
/// reason), so the endpoint cannot be used to probe which ids exist.
#[tokio::test]
async fn post_recall_unknown_dataset_id_is_forbidden_like_a_foreign_one() {
    let db = build_search_db().await;
    let (app, retriever) = build_recording_app(db).await;

    let resp = app
        .oneshot(post_recall_request(format!(
            r#"{{"query":"hi","searchType":"CHUNKS","scope":"graph","datasetIds":["{}"]}}"#,
            uuid::Uuid::new_v4()
        )))
        .await
        .expect("resp");
    assert_eq!(resp.status(), 403);
    assert!(retriever.last_params().is_none());
}

/// Precedence when both fields are sent: ids win and the names are never
/// resolved (`get_recall_router.py:47-48`, `recall.py:655-657`). The name
/// here is deliberately one the caller does *not* own — if names were
/// consulted the request would 422 instead of reaching the retriever with
/// the id.
#[tokio::test]
async fn post_recall_dataset_ids_take_precedence_over_dataset_names() {
    let db = build_search_db().await;
    let owned_id = seed_dataset(&db, "mine", default_test_user_id()).await;
    let _foreign = seed_dataset(&db, "theirs", uuid::Uuid::new_v4()).await;
    let (app, retriever) = build_recording_app(db).await;

    let resp = app
        .oneshot(post_recall_request(format!(
            r#"{{"query":"hi","searchType":"CHUNKS","scope":"graph","datasets":["theirs"],"datasetIds":["{owned_id}"]}}"#
        )))
        .await
        .expect("resp");
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, 200, "ids must win over names, got {status}: {body}");

    let seen = retriever.last_params().expect("retriever must run");
    assert_eq!(
        seen.dataset_ids.as_deref(),
        Some([owned_id].as_slice()),
        "only the explicit id may reach the retriever"
    );
}

/// `datasetIds: []` is "no id filter", not "match nothing": Python's
/// `dataset_ids or None` (`recall.py:656`) makes it fall through to the
/// names, and the HTTP search router's `if not payload.dataset_ids` does the
/// same. So the name must resolve exactly as if the field were omitted.
#[tokio::test]
async fn post_recall_empty_dataset_ids_is_no_filter_and_names_still_resolve() {
    let db = build_search_db().await;
    let dataset_id = seed_dataset(&db, "notes", default_test_user_id()).await;
    let (app, retriever) = build_recording_app(db).await;

    let resp = app
        .oneshot(post_recall_request(
            r#"{"query":"hi","searchType":"CHUNKS","scope":"graph","datasets":["notes"],"datasetIds":[]}"#
                .to_string(),
        ))
        .await
        .expect("resp");
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, 200, "got {status}: {body}");

    let seen = retriever.last_params().expect("retriever must run");
    assert_eq!(
        seen.dataset_ids.as_deref(),
        Some([dataset_id].as_slice()),
        "the name must resolve when the id list is empty"
    );
}
