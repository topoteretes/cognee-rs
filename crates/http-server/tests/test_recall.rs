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
    assert_eq!(body[0]["search_result"], "ans");
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
