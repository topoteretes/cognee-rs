#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `/api/v1/settings`, focused on the `bedrock` provider
//! surface (`docs/roadmap/bedrock-provider-plan.md` §4 R7 + §5 P2).
//!
//! `HttpServerConfig::default()` leaves `require_authentication = false`, so
//! `AuthenticatedUser` resolves to the synthetic default user and no auth
//! header is needed.
//!
//! **Caution**: the settings store is a process-wide `OnceLock<SettingsStore>`
//! shared by every test in this binary, so the POST cases below mutate state
//! other tests can observe. All assertions here are therefore either on the
//! static `llm_models()` output (which POST never mutates) or on the response
//! status code.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};

/// Python's current bedrock model list (`cognee/modules/settings/get_settings.py`),
/// in Python's order, with Python's values *and* labels.
const EXPECTED_BEDROCK_MODELS: &[(&str, &str)] = &[
    (
        "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "Claude 4.5 Sonnet",
    ),
    (
        "eu.anthropic.claude-haiku-4-5-20251001-v1:0",
        "Claude 4.5 Haiku",
    ),
    ("eu.amazon.nova-lite-v1:0", "Amazon Nova Lite"),
];

/// Build a settings POST request with the given JSON body.
fn settings_post(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/settings")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// `GET /api/v1/settings` advertises exactly Python's three bedrock models,
/// in Python's order, with matching `value` and `label` fields.
#[tokio::test]
async fn test_get_settings_lists_python_bedrock_models() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let resp = support::oneshot_get(app, "/api/v1/settings").await;

    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;

    let models = body["llm"]["models"]["bedrock"]
        .as_array()
        .expect("llm.models.bedrock is an array");

    assert_eq!(
        models.len(),
        EXPECTED_BEDROCK_MODELS.len(),
        "unexpected bedrock model count: {models:?}"
    );

    for (idx, (value, label)) in EXPECTED_BEDROCK_MODELS.iter().enumerate() {
        assert_eq!(
            models[idx]["value"].as_str(),
            Some(*value),
            "bedrock model {idx} value mismatch"
        );
        assert_eq!(
            models[idx]["label"].as_str(),
            Some(*label),
            "bedrock model {idx} label mismatch"
        );
    }
}

/// `POST /api/v1/settings` accepts `provider: "bedrock"` (plan §4 R7). Python's
/// save-side `Literal` does not include it yet — see §5 P1.
///
/// The follow-up `GET` pins the save-side match arm's snapshot string: a 200
/// alone would also be returned if `LlmProvider::Bedrock` were mapped to some
/// other provider name.
#[tokio::test]
async fn test_post_settings_accepts_bedrock_provider() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;

    let req = settings_post(serde_json::json!({
        "llm": {
            "provider": "bedrock",
            "model": "eu.amazon.nova-lite-v1:0",
            "api_key": "test-key",
        }
    }));
    let resp = support::oneshot_request(app, req).await;

    assert_eq!(resp.status(), StatusCode::OK);

    // Read the mutated snapshot back. Safe against the shared process-wide
    // store: this is the only case in this binary that mutates the LLM
    // snapshot (the unknown-provider POST is rejected by the extractor before
    // the handler runs), and the GET case above asserts only on the static
    // `llm_models()` output.
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let body = support::body_json(support::oneshot_get(app, "/api/v1/settings").await).await;

    assert_eq!(body["llm"]["provider"].as_str(), Some("bedrock"));
    assert_eq!(
        body["llm"]["model"].as_str(),
        Some("eu.amazon.nova-lite-v1:0")
    );
}

/// Negative control: the provider enum is still closed, so an unknown value is
/// rejected by axum's `Json` extractor with 422 (what the handler's utoipa
/// annotation documents) rather than being silently accepted.
#[tokio::test]
async fn test_post_settings_rejects_unknown_provider() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;

    let req = settings_post(serde_json::json!({
        "llm": {
            "provider": "not-a-provider",
            "model": "eu.amazon.nova-lite-v1:0",
            "api_key": "test-key",
        }
    }));
    let resp = support::oneshot_request(app, req).await;

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
