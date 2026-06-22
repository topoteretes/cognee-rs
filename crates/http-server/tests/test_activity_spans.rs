#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration test for `GET /api/v1/activity/spans`.
//!
//! The handler reads `state.spans` (an `Arc<SpanBuffer>`); we directly poke a
//! span into the buffer to avoid coupling the test to the live tracing
//! subscriber installation.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cognee_http_server::observability::{RecordedSpan, SpanStatus};
use tower::ServiceExt;

#[tokio::test]
async fn spans_endpoint_returns_buffered_traces() {
    let state = support::build_test_state().await;

    let parent_id = "0000000000000001".to_string();
    let child_id = "0000000000000002".to_string();
    let trace_id = "00000000000000000000000000000001".to_string();

    state.spans.record(RecordedSpan {
        trace_id: trace_id.clone(),
        span_id: parent_id.clone(),
        parent_span_id: None,
        name: "cognee.api.test_root".into(),
        start_time_ns: 100,
        end_time_ns: 200,
        duration_ms: 0.0001,
        status: SpanStatus::Ok,
        attributes: serde_json::Map::new(),
    });
    state.spans.record(RecordedSpan {
        trace_id: trace_id.clone(),
        span_id: child_id.clone(),
        parent_span_id: Some(parent_id.clone()),
        name: "cognee.api.test_child".into(),
        start_time_ns: 110,
        end_time_ns: 190,
        duration_ms: 0.00008,
        status: SpanStatus::Ok,
        attributes: serde_json::Map::new(),
    });

    let app = support::test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/activity/spans")
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["trace_id"], trace_id);
    assert_eq!(arr[0]["span_count"], 2);
    let spans = arr[0]["spans"].as_array().expect("spans array");
    assert_eq!(spans.len(), 2);
    let child = spans
        .iter()
        .find(|s| s["span_id"] == child_id)
        .expect("child span present");
    assert_eq!(child["parent_span_id"], parent_id);
    assert_eq!(child["status"], "OK");
}
