#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `/api/v1/settings`.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use serial_test::serial;

#[tokio::test]
#[serial]
async fn get_settings_returns_dto_shape() {
    // SAFETY: serial-tested.
    unsafe {
        std::env::set_var("LLM_PROVIDER", "openai");
        std::env::set_var("LLM_MODEL", "gpt-4o-mini");
    }

    let (state, _events) = support::build_auth_test_state().await;
    let app = support::test_router(state).await;
    let resp = support::oneshot_get(app, "/api/v1/settings").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert!(body["llm"].is_object());
    assert!(body["vectorDb"].is_object());
    assert!(body["llm"]["providers"].is_array());
    assert!(body["llm"]["models"].is_object());
}

#[tokio::test]
#[serial]
async fn save_then_get_round_trips_redacted_key() {
    let (state, _events) = support::build_auth_test_state().await;
    let app = support::test_router(state).await;

    let payload = json!({
        "llm": {
            "provider": "openai",
            "model": "gpt-4o",
            "api_key": "sk-1234567890ABC"
        }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .expect("request");
    let resp = app.clone().oneshot_via(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // Body is `null` (Python parity).
    let body = support::body_json(resp).await;
    assert!(body.is_null(), "expected null body, got {body}");

    let resp2 = support::oneshot_get(app, "/api/v1/settings").await;
    assert_eq!(resp2.status(), StatusCode::OK);
    let body2 = support::body_json(resp2).await;
    let api_key = body2["llm"]["apiKey"]
        .as_str()
        .expect("redacted api_key string");
    assert!(api_key.starts_with("sk-1234567"));
    assert!(api_key.contains('*'));
}

#[tokio::test]
#[serial]
async fn save_with_starred_key_does_not_overwrite() {
    let (state, _events) = support::build_auth_test_state().await;
    let app = support::test_router(state).await;

    // First, set a real key.
    let payload = json!({
        "llm": { "provider": "openai", "model": "gpt-4o", "api_key": "sk-real-key-XYZ" }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .expect("request");
    let _ = app.clone().oneshot_via(req).await;

    // Confirm the prefix landed.
    let resp_before = support::oneshot_get(app.clone(), "/api/v1/settings").await;
    let body_before = support::body_json(resp_before).await;
    let key_before = body_before["llm"]["apiKey"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        key_before.starts_with("sk-real-ke"),
        "real key not persisted; got {key_before}"
    );

    // Try to overwrite with a redacted echo containing the literal `*****`.
    let echo = json!({
        "llm": { "provider": "openai", "model": "gpt-4o", "api_key": "sk-prefix*****abc" }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings")
        .header("content-type", "application/json")
        .body(Body::from(echo.to_string()))
        .expect("request");
    let resp = app.clone().oneshot_via(req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // The original key must still be there — echoed `*****` was silently dropped
    // (Python parity per `routers/settings.md §2.2`).
    let resp_after = support::oneshot_get(app, "/api/v1/settings").await;
    let body_after = support::body_json(resp_after).await;
    let key_after = body_after["llm"]["apiKey"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert_eq!(
        key_before, key_after,
        "echoed `*****` must NOT overwrite real key"
    );
}

#[tokio::test]
#[serial]
async fn save_with_unknown_provider_returns_400() {
    let (state, _events) = support::build_auth_test_state().await;
    let app = support::test_router(state).await;

    // `bedrock` is in the GET list but rejected on POST (Python parity per
    // `routers/settings.md §6.4`).
    let payload = json!({
        "llm": { "provider": "bedrock", "model": "anthropic.claude-3-5-sonnet-20240620-v1:0", "api_key": "sk-x" }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .expect("request");
    let resp = app.oneshot_via(req).await;
    // Pydantic-style strict enum rejection → 400 (or 422 from extractor).
    let s = resp.status().as_u16();
    assert!(
        s == 400 || s == 422,
        "expected 400/422 for unknown provider, got {s}"
    );
}

#[tokio::test]
#[serial]
async fn partial_save_leaves_other_subobject_untouched() {
    let (state, _events) = support::build_auth_test_state().await;
    let app = support::test_router(state).await;

    // Snapshot the vector_db section before any LLM-only save.
    let resp = support::oneshot_get(app.clone(), "/api/v1/settings").await;
    let body = support::body_json(resp).await;
    let vector_before = body["vectorDb"].clone();

    // Send LLM-only save.
    let payload = json!({
        "llm": { "provider": "openai", "model": "gpt-4o", "api_key": "sk-AAAAAAAAA-real" }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .expect("request");
    let _ = app.clone().oneshot_via(req).await;

    // vector_db should be unchanged.
    let resp = support::oneshot_get(app, "/api/v1/settings").await;
    let body = support::body_json(resp).await;
    assert_eq!(
        body["vectorDb"], vector_before,
        "partial save must leave vector_db untouched"
    );
}

// Compatibility shim: tower's ServiceExt::oneshot moves the router; we work
// around it for the multi-step test by using a dedicated extension trait via
// support::oneshot_request style. Inline alternative:
trait OneshotVia {
    async fn oneshot_via(self, req: Request<Body>) -> axum::response::Response;
}

impl OneshotVia for axum::Router {
    async fn oneshot_via(self, req: Request<Body>) -> axum::response::Response {
        use tower::ServiceExt;
        self.oneshot(req).await.expect("response")
    }
}
