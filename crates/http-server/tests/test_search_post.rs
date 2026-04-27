//! P4 Step 14 — search POST integration tests.

mod support;

use axum::{body::Body, http::Request};
use cognee_search::types::{SearchItem, SearchType};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

use support::{StubRetriever, body_json, build_orchestrator, build_p4_state, build_search_db};

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
    assert_eq!(arr[0]["search_result"], "ans");
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
    assert!(body[0]["search_result"].is_string());
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
    assert!(body_json(resp).await[0]["search_result"].is_string());
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
    assert!(body_json(resp).await[0]["search_result"].is_string());
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
    assert!(body_json(resp).await[0]["search_result"].is_string());
}

#[tokio::test]
async fn rag_completion_returns_string() {
    let app = make_app_with_text(SearchType::RagCompletion, "rag").await;
    let resp = post_search(app, json!({"search_type": "RAG_COMPLETION", "query": "x"})).await;
    assert_eq!(resp.status(), 200);
    assert!(body_json(resp).await[0]["search_result"].is_string());
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
    assert!(body_json(resp).await[0]["search_result"].is_array());
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
    assert!(body_json(resp).await[0]["search_result"].is_array());
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
    assert!(body_json(resp).await[0]["search_result"].is_array());
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
    assert!(body_json(resp).await[0]["search_result"].is_array());
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
