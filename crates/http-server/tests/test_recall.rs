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

use support::{StubRetriever, body_json, build_orchestrator, build_p4_state, build_search_db};

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
