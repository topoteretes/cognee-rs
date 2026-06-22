#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! P4 Step 14 — search history integration tests.

mod support;

use axum::{body::Body, http::Request};
use cognee_search::types::SearchType;
use std::sync::Arc;
use tower::ServiceExt;

use support::{StubRetriever, body_json, build_orchestrator, build_p4_state, build_search_db};

#[tokio::test]
async fn fresh_user_history_is_empty_array() {
    let db = build_search_db().await;
    let retriever = Arc::new(StubRetriever::text_for(SearchType::GraphCompletion, "ans"));
    let orchestrator = build_orchestrator(db, retriever).await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/search")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert_eq!(body, serde_json::json!([]));
}

#[tokio::test]
async fn post_then_get_returns_query_and_result_rows() {
    let db = build_search_db().await;
    let retriever = Arc::new(StubRetriever::text_for(SearchType::GraphCompletion, "ans"));
    let orchestrator = build_orchestrator(db, retriever).await;
    let state = build_p4_state(Some(orchestrator), None, None).await;
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let post_req = Request::builder()
        .method("POST")
        .uri("/api/v1/search")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"search_type":"GRAPH_COMPLETION","query":"hi"}"#,
        ))
        .unwrap();
    let post_resp = app.clone().oneshot(post_req).await.expect("post");
    assert_eq!(post_resp.status(), 200);

    let get_req = Request::builder()
        .method("GET")
        .uri("/api/v1/search")
        .body(Body::empty())
        .unwrap();
    let get_resp = app.oneshot(get_req).await.expect("get");
    assert_eq!(get_resp.status(), 200);
    let body = body_json(get_resp).await;
    let arr = body.as_array().expect("array");
    assert!(
        arr.len() >= 2,
        "expected at least one query and one result row, got {arr:?}"
    );
    let users: Vec<&str> = arr
        .iter()
        .map(|v| v["user"].as_str().unwrap_or(""))
        .collect();
    assert!(users.contains(&"user"), "missing query row");
    assert!(users.contains(&"system"), "missing result row");

    // Decision 6: every wire-visible `DateTime<Utc>` field must serialize with
    // an explicit `+00:00` offset (Python `datetime.isoformat()` parity), not
    // chrono's default `Z` suffix.
    let created_at = arr[0]["createdAt"]
        .as_str()
        .expect("createdAt must be a string");
    assert!(
        created_at.ends_with("+00:00"),
        "createdAt must end with +00:00 offset, got: {created_at}"
    );
}
