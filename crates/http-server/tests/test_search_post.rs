#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! P4 Step 14 — search POST integration tests.

mod support;

use axum::{body::Body, http::Request};
use cognee_search::types::{SearchItem, SearchType};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

use support::{
    RecordingRetriever, StubRetriever, body_json, build_orchestrator,
    build_orchestrator_with_dataset_resolver, build_p4_state, build_search_db,
    default_test_user_id, seed_dataset,
};

async fn make_app_with_text(kind: SearchType, text: &'static str) -> axum::Router {
    let db = build_search_db().await;
    let retriever = Arc::new(StubRetriever::text_for(kind, text));
    let orchestrator = build_orchestrator(db, retriever).await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    cognee_http_server::build_router(state)
        .await
        .expect("router")
}

async fn make_app_with_items(kind: SearchType, items: Vec<SearchItem>) -> axum::Router {
    let db = build_search_db().await;
    let retriever = Arc::new(StubRetriever::items_for(kind, items));
    let orchestrator = build_orchestrator(db, retriever).await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    cognee_http_server::build_router(state)
        .await
        .expect("router")
}

async fn post_search(app: axum::Router, body: serde_json::Value) -> axum::http::Response<Body> {
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/search")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    app.oneshot(req).await.expect("resp")
}

#[tokio::test]
async fn empty_post_body_uses_defaults() {
    let app = make_app_with_text(SearchType::GraphCompletion, "ans").await;
    let resp = post_search(app, json!({})).await;
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["searchResult"], "ans");
}

#[tokio::test]
async fn graph_completion_returns_string_search_result() {
    let app = make_app_with_text(SearchType::GraphCompletion, "ans").await;
    let resp = post_search(
        app,
        json!({"search_type": "GRAPH_COMPLETION", "query": "x"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert!(body[0]["searchResult"].is_string());
}

#[tokio::test]
async fn graph_completion_cot_returns_string() {
    let app = make_app_with_text(SearchType::GraphCompletionCot, "cot").await;
    let resp = post_search(
        app,
        json!({"search_type": "GRAPH_COMPLETION_COT", "query": "x"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(body_json(resp).await[0]["searchResult"].is_string());
}

#[tokio::test]
async fn graph_completion_context_extension_returns_string() {
    let app = make_app_with_text(SearchType::GraphCompletionContextExtension, "ext").await;
    let resp = post_search(
        app,
        json!({"search_type": "GRAPH_COMPLETION_CONTEXT_EXTENSION", "query": "x"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(body_json(resp).await[0]["searchResult"].is_string());
}

#[tokio::test]
async fn graph_summary_completion_returns_string() {
    let app = make_app_with_text(SearchType::GraphSummaryCompletion, "summary").await;
    let resp = post_search(
        app,
        json!({"search_type": "GRAPH_SUMMARY_COMPLETION", "query": "x"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(body_json(resp).await[0]["searchResult"].is_string());
}

#[tokio::test]
async fn rag_completion_returns_string() {
    let app = make_app_with_text(SearchType::RagCompletion, "rag").await;
    let resp = post_search(app, json!({"search_type": "RAG_COMPLETION", "query": "x"})).await;
    assert_eq!(resp.status(), 200);
    assert!(body_json(resp).await[0]["searchResult"].is_string());
}

#[tokio::test]
async fn triplet_completion_returns_array() {
    let items = vec![SearchItem {
        id: None,
        score: Some(0.5),
        payload: json!({"text": "triplet"}),
    }];
    let app = make_app_with_items(SearchType::TripletCompletion, items).await;
    let resp = post_search(
        app,
        json!({"search_type": "TRIPLET_COMPLETION", "query": "x"}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(body_json(resp).await[0]["searchResult"].is_array());
}

#[tokio::test]
async fn chunks_returns_array() {
    let items = vec![SearchItem {
        id: None,
        score: Some(0.9),
        payload: json!({"text": "chunk"}),
    }];
    let app = make_app_with_items(SearchType::Chunks, items).await;
    let resp = post_search(app, json!({"search_type": "CHUNKS", "query": "x"})).await;
    assert_eq!(resp.status(), 200);
    assert!(body_json(resp).await[0]["searchResult"].is_array());
}

#[tokio::test]
async fn summaries_returns_array() {
    let items = vec![SearchItem {
        id: None,
        score: Some(0.5),
        payload: json!({"text": "summary"}),
    }];
    let app = make_app_with_items(SearchType::Summaries, items).await;
    let resp = post_search(app, json!({"search_type": "SUMMARIES", "query": "x"})).await;
    assert_eq!(resp.status(), 200);
    assert!(body_json(resp).await[0]["searchResult"].is_array());
}

#[tokio::test]
async fn temporal_returns_array() {
    let items = vec![SearchItem {
        id: None,
        score: Some(0.5),
        payload: json!({"timestamp": "2024-01-01"}),
    }];
    let app = make_app_with_items(SearchType::Temporal, items).await;
    let resp = post_search(app, json!({"search_type": "TEMPORAL", "query": "x"})).await;
    assert_eq!(resp.status(), 200);
    assert!(body_json(resp).await[0]["searchResult"].is_array());
}

#[tokio::test]
async fn invalid_input_maps_to_422_with_error_envelope() {
    // Use a retriever that always errors (InvalidInput) — orchestrator
    // surfaces the error and the search router maps it to 422.
    let db = build_search_db().await;
    let retriever = Arc::new(StubRetriever::error_for(SearchType::Cypher, "bad query"));
    let orchestrator = build_orchestrator(db, retriever).await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let resp = post_search(app, json!({"search_type": "CYPHER", "query": "BAD"})).await;
    assert_eq!(resp.status(), 422);
    let body = body_json(resp).await;
    // Search uses the {error, detail} envelope, NOT {detail}.
    assert_eq!(body["error"], "Search prerequisites not met");
    assert!(body.get("detail").is_some());
}

async fn make_recording_app(
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

/// `POST /v1/search` always forwarded `dataset_ids`, but nothing checked
/// who owned them: any authenticated caller could read any tenant's rows by
/// UUID. Python answers `403 {"detail": "... [PermissionDeniedError]"}` via
/// the global handler (`get_search_router.py:290-295`, `client.py:220-236`).
#[tokio::test]
async fn foreign_dataset_id_is_forbidden_and_never_searched() {
    let db = build_search_db().await;
    let stranger = uuid::Uuid::new_v4();
    assert_ne!(stranger, default_test_user_id());
    let foreign_dataset_id = seed_dataset(&db, "notes", stranger).await;
    let (app, retriever) = make_recording_app(db).await;

    let resp = post_search(
        app,
        json!({"search_type": "CHUNKS", "query": "x", "dataset_ids": [foreign_dataset_id]}),
    )
    .await;
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

/// The owner check must not break the legitimate case: an owned id is
/// accepted and is what the retriever is scoped to.
#[tokio::test]
async fn owned_dataset_id_reaches_the_retriever() {
    let db = build_search_db().await;
    let dataset_id = seed_dataset(&db, "notes", default_test_user_id()).await;
    let (app, retriever) = make_recording_app(db).await;

    let resp = post_search(
        app,
        json!({"search_type": "CHUNKS", "query": "x", "dataset_ids": [dataset_id]}),
    )
    .await;
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, 200, "got {status}: {body}");

    let seen = retriever.last_params().expect("retriever must run");
    assert_eq!(seen.dataset_ids.as_deref(), Some([dataset_id].as_slice()));
}
