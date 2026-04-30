//! CLEAN-01 §5.2 — wire-shape regression tests.
//!
//! Per affected endpoint, this file asserts:
//!
//! - A POST with snake_case body fields succeeds (bodies survive aliasing).
//! - A POST with camelCase body fields succeeds.
//! - Response body keys (when the endpoint returns a Decision-10 OutDTO) are
//!   exclusively camelCase at the top level.
//!
//! The `permissions/tenants/select` family is intentionally NOT asserted with
//! a "no underscores in response" check because the response there is a plain
//! Python dict (snake_case literal) — only the request body follows
//! Decision 10.

mod support;

use axum::{body::Body, http::Request};
use cognee_search::types::SearchType;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

use support::{StubRetriever, body_json, build_orchestrator, build_p4_state, build_search_db};

async fn build_search_app() -> axum::Router {
    let db = build_search_db().await;
    let retriever = Arc::new(StubRetriever::text_for(SearchType::GraphCompletion, "ans"));
    let orchestrator = build_orchestrator(db, retriever).await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    cognee_http_server::build_router(state)
        .await
        .expect("router")
}

fn assert_keys_have_no_underscore(value: &serde_json::Value, context: &str) {
    if let Some(map) = value.as_object() {
        for k in map.keys() {
            assert!(
                !k.contains('_'),
                "{context}: response key `{k}` is snake_case; expected camelCase per Decision 10. Full body: {value}"
            );
        }
    }
}

// ─── /api/v1/search ──────────────────────────────────────────────────────────

#[tokio::test]
async fn post_search_accepts_snake_case_body() {
    let app = build_search_app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/search")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "search_type": "GRAPH_COMPLETION",
                "dataset_ids": [],
                "system_prompt": "sys",
                "node_name": ["x"],
                "top_k": 5,
                "only_context": false,
                "query": "hi"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn post_search_accepts_camelcase_body() {
    let app = build_search_app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/search")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "searchType": "GRAPH_COMPLETION",
                "datasetIds": [],
                "systemPrompt": "sys",
                "nodeName": ["x"],
                "topK": 5,
                "onlyContext": false,
                "query": "hi"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn post_search_response_keys_are_camelcase() {
    let app = build_search_app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/search")
        .header("content-type", "application/json")
        .body(Body::from(json!({"query": "hi"}).to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array of SearchResultDTO");
    for entry in arr {
        assert_keys_have_no_underscore(entry, "post_search response item");
    }
}

#[tokio::test]
async fn get_search_history_response_keys_are_camelcase() {
    let app = build_search_app().await;
    // Seed history.
    let post = Request::builder()
        .method("POST")
        .uri("/api/v1/search")
        .header("content-type", "application/json")
        .body(Body::from(json!({"query": "hi"}).to_string()))
        .unwrap();
    let _ = app.clone().oneshot(post).await.expect("seed");

    let get = Request::builder()
        .method("GET")
        .uri("/api/v1/search")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(get).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array of SearchHistoryItemDTO");
    assert!(!arr.is_empty(), "history should have at least one row");
    for entry in arr {
        assert_keys_have_no_underscore(entry, "search history item");
    }
}

// ─── /api/v1/recall ──────────────────────────────────────────────────────────

#[tokio::test]
async fn post_recall_accepts_snake_case_body() {
    let app = build_search_app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "search_type": "GRAPH_COMPLETION",
                "dataset_ids": [],
                "top_k": 3,
                "only_context": false,
                "query": "hi"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn post_recall_accepts_camelcase_body() {
    let app = build_search_app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "searchType": "GRAPH_COMPLETION",
                "datasetIds": [],
                "topK": 3,
                "onlyContext": false,
                "query": "hi"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn post_recall_response_keys_match_python_shape() {
    // E-04 (Decisions 17 + 18) replaced the v1 RecallResultDTO envelope
    // with Python's flat-list-with-`_source` wire shape. The `_source`
    // key is snake_case BY DESIGN — it is the literal Python wire key
    // (`recall.py:208/278/315/495-498`) and must NOT be camelCased.
    // Per-source content keys (e.g. `created_at` for session entries,
    // `trace_id` for traces) likewise mirror Python literally; converting
    // them would break cross-SDK parity. So this test asserts the shape
    // contract, not the no-underscore rule.
    let app = build_search_app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/recall")
        .header("content-type", "application/json")
        .body(Body::from(json!({"query": "hi"}).to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    let arr = body.as_array().expect("array");
    for entry in arr {
        assert!(
            entry["_source"].is_string(),
            "every recall item must have a string `_source`: {entry}"
        );
    }
}

// ─── /api/v1/permissions/tenants/select (camelCase request body) ─────────────

#[tokio::test]
async fn select_tenant_dto_accepts_both_casings() {
    use cognee_http_server::dto::permissions::SelectTenantDTO;
    let snake: SelectTenantDTO =
        serde_json::from_str(r#"{"tenant_id": null}"#).expect("snake parse");
    assert!(snake.tenant_id.is_none());
    let camel: SelectTenantDTO =
        serde_json::from_str(r#"{"tenantId": null}"#).expect("camel parse");
    assert!(camel.tenant_id.is_none());
}
