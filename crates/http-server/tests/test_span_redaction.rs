//! Integration: a bearer token planted in a span attribute is redacted
//! before reaching `GET /api/v1/activity/spans`.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cognee_http_server::observability::{RecordedSpan, SpanBufferLayer, SpanStatus};
use tower::ServiceExt;
use tracing::Level;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;

#[tokio::test]
async fn span_attributes_are_redacted_before_spans_endpoint_serves() {
    let state = support::build_test_state().await;

    // Drive a small workload through the layer.
    let layer = SpanBufferLayer::new((*state.spans).clone());
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::span!(
            Level::INFO,
            "request",
            auth = "Authorization: Bearer eyJabc.def.ghi-very-long-jwt-1234567890"
        );
        let _g = span.enter();
    });

    // Also poke a manually-built span with an attribute that is NOT redacted —
    // a sanity check that the redaction is selective.
    state.spans.record(RecordedSpan {
        trace_id: "ff".repeat(16),
        span_id: "ff".repeat(8),
        parent_span_id: None,
        name: "manual".into(),
        start_time_ns: 0,
        end_time_ns: 1,
        duration_ms: 0.0,
        status: SpanStatus::Ok,
        attributes: {
            let mut m = serde_json::Map::new();
            m.insert("safe".into(), serde_json::Value::String("hello".into()));
            m
        },
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

    let mut found_redacted = false;
    let mut found_safe = false;
    for trace in arr {
        for span in trace["spans"].as_array().unwrap_or(&Vec::new()) {
            if let Some(attrs) = span["attributes"].as_object() {
                if let Some(auth) = attrs.get("auth").and_then(|v| v.as_str()) {
                    assert!(auth.contains("***REDACTED***"));
                    assert!(!auth.contains("ghi-very-long-jwt"));
                    found_redacted = true;
                }
                if let Some(safe) = attrs.get("safe").and_then(|v| v.as_str()) {
                    assert_eq!(safe, "hello");
                    found_safe = true;
                }
            }
        }
    }
    assert!(found_redacted, "redacted span attribute present");
    assert!(found_safe, "non-secret leaf untouched");
}
