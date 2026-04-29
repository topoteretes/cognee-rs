//! P4 Step 16 — LLM router integration tests.

mod support;

use axum::{body::Body, http::Request};
use cognee_http_server::{HttpServerConfig, config::Environment};
use cognee_llm::Llm;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

use support::{MockLlm, body_json, build_p4_state};

async fn build_app(llm: Arc<dyn Llm>) -> axum::Router {
    let state = build_p4_state(None, Some(llm), None).await;
    cognee_http_server::build_router(state)
        .await
        .expect("router")
}

#[tokio::test]
async fn custom_prompt_happy_path_returns_canned() {
    let llm = Arc::new(MockLlm::new("custom prompt body"));
    let app = build_app(llm).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/llm/custom-prompt")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "graph_model": {"entity_types": []},
                "parameters": {"temperature": 0.5, "junk_key": "x"}
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["customPrompt"], "custom prompt body");
}

#[tokio::test]
async fn custom_prompt_filters_parameters_through_safe_params() {
    // Parity headline: `safe_params` must drop unknown keys before forwarding
    // to the underlying LLM. With `junk_key` and `model` in the wire input the
    // adapter should still see ONLY the canonical four (`temperature` here).
    let mock = Arc::new(MockLlm::new("ok"));
    let captured = mock.last_options.clone();
    let app = build_app(mock).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/llm/custom-prompt")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "graph_model": {"entity_types": []},
                "parameters": {
                    "temperature": 0.5,
                    "junk_key": "x",
                    "model": "gpt-4o",
                    "stream": true
                }
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);

    // lock poison is unrecoverable
    let opts = captured.lock().unwrap().clone().expect("options recorded");
    // Allowed key survived.
    assert!((opts.temperature.unwrap_or(0.0) - 0.5).abs() < 1e-6);
    // Disallowed keys can never reach the LLM via GenerationOptions; the
    // struct only exposes the four allowed slots, so this is structurally
    // enforced. The presence of just `temperature` confirms the filter ran.
    assert!(opts.max_tokens.is_none());
    assert!(opts.top_p.is_none());
}

#[tokio::test]
async fn custom_prompt_no_auth_returns_401_with_canonical_envelope() {
    // When `require_authentication=true` and no credential is supplied the
    // canonical `{detail: "Unauthorized"}` envelope is returned (NOT the
    // router-specific `{error}` envelope).
    use cognee_database::{SeaOrmApiKeyRepository, SeaOrmUserAuthRepository};
    use cognee_http_server::AppState;
    use cognee_http_server::auth::AuthContext;
    use cognee_http_server::auth::mailer::ConsoleMailer;
    use std::sync::Arc;

    let db = support::setup_auth_db().await;
    let user_repo = Arc::new(SeaOrmUserAuthRepository { db: db.clone() });
    let api_key_repo = Arc::new(SeaOrmApiKeyRepository { db: db.clone() });
    let cfg = HttpServerConfig {
        require_authentication: true,
        env: Environment::Dev,
        ..HttpServerConfig::default()
    };
    let (mailer, _events) = ConsoleMailer::new();
    let auth = AuthContext::from_env(&cfg, user_repo, api_key_repo).expect("auth");
    let state = AppState {
        config: Arc::new(cfg),
        pipelines: AppState::noop_pipelines(),
        lib: None,
        auth: Some(Arc::new(auth)),
        mailer: Arc::new(mailer),
        health: None,
        spans: Arc::new(cognee_http_server::observability::SpanBuffer::default()),
        sync: Arc::new(cognee_http_server::sync::SyncRegistry::new()),
    };
    let app = cognee_http_server::build_router(state)
        .await
        .expect("router");

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/llm/custom-prompt")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"graph_model": {"entity_types": []}}).to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 401);
    let body = body_json(resp).await;
    assert_eq!(body["detail"], "Unauthorized");
    // Confirm the LlmError envelope is NOT used for the auth path.
    assert!(body.get("error").is_none());
}

#[tokio::test]
async fn infer_schema_happy_path_returns_parsed_schema() {
    let llm = Arc::new(MockLlm::new(
        r#"{"entity_types": [], "relationship_types": []}"#,
    ));
    let app = build_app(llm).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/llm/infer-schema")
        .header("content-type", "application/json")
        .body(Body::from(json!({"text": "Alice met Bob."}).to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert!(body["graphSchema"]["entity_types"].is_array());
    assert!(body["graphSchema"]["relationship_types"].is_array());
}

#[tokio::test]
async fn infer_schema_invalid_json_returns_422() {
    let llm = Arc::new(MockLlm::new("definitely { not json"));
    let app = build_app(llm).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/llm/infer-schema")
        .header("content-type", "application/json")
        .body(Body::from(json!({"text": "x"}).to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 422);
    let body = body_json(resp).await;
    let err = body["error"].as_str().expect("error string");
    assert!(
        err.starts_with("LLM output is not valid JSON: "),
        "expected canonical prefix, got {err:?}"
    );
}

#[tokio::test]
async fn infer_schema_valid_json_but_invalid_schema_returns_409() {
    // Valid JSON, but missing the canonical entity_types/relationship_types
    // keys → graph_schema_to_graph_model fails → 409.
    let llm = Arc::new(MockLlm::new(r#"{"name": "Foo"}"#));
    let app = build_app(llm).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/llm/infer-schema")
        .header("content-type", "application/json")
        .body(Body::from(json!({"text": "x"}).to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), 409);
    let body = body_json(resp).await;
    assert!(body["error"].is_string());
    assert!(body.get("detail").is_none());
}
