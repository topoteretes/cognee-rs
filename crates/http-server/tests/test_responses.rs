//! Integration test for `POST /api/v1/responses` against a real OpenAI
//! Responses endpoint.
//!
//! Skip-gated on `OPENAI_URL` + `OPENAI_TOKEN` + `COGNEE_E2E_RESPONSES_ENABLED=1`
//! (the Responses API has cost/quota implications, so the gate is explicit).

mod support;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use cognee_http_server::AppState;
use cognee_http_server::components::ComponentHandles;
use cognee_llm::OpenAIResponsesClient;

use support::{bearer_header, body_json, build_notebooks_state, seed_perm_user, test_router};

fn env_gated() -> Option<(String, String)> {
    if std::env::var("COGNEE_E2E_RESPONSES_ENABLED").unwrap_or_default() != "1" {
        return None;
    }
    let token = std::env::var("OPENAI_TOKEN").ok()?;
    let url = std::env::var("OPENAI_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    Some((url, token))
}

/// Build a state with a real `OpenAIResponsesClient` wired into the
/// `ComponentHandles`. Mirrors the pattern used by other env-gated tests.
async fn build_state_with_responses_client(url: String, token: String) -> AppState {
    let (mut state, _) = build_notebooks_state().await;
    // `state.lib` is `Some(...)` after build_notebooks_state. Clone the
    // existing handles and replace the responses_client slot.
    let existing = state
        .lib
        .as_ref()
        .map(|h| (**h).clone())
        .expect("notebooks state already has component handles");
    let client =
        Arc::new(OpenAIResponsesClient::new(token, Some(url)).expect("build responses client"));
    let handles = ComponentHandles {
        responses_client: Some(client),
        ..existing
    };
    state.lib = Some(Arc::new(handles));
    state
}

#[tokio::test]
async fn responses_real_openai_dispatches_at_least_one_tool() {
    let Some((url, token)) = env_gated() else {
        eprintln!("skipping: set COGNEE_E2E_RESPONSES_ENABLED=1 and OPENAI_TOKEN to run this test");
        return;
    };

    let state = build_state_with_responses_client(url, token).await;
    let user = seed_perm_user(&state, "resp_e2e@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let body = serde_json::json!({
        "model": "cognee-v1",
        "input": "Use the search tool to look up 'Alice'. After the tool call, summarise the result briefly.",
        "tool_choice": "auto",
        "temperature": 0.2,
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/responses")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "expected 200, got {}",
        resp.status()
    );

    let body = body_json(resp).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["model"], "cognee-v1");
    assert!(body["id"].as_str().expect("id").starts_with("resp_"));
    let calls = body["toolCalls"].as_array().expect("toolCalls array");
    assert!(
        !calls.is_empty(),
        "expected at least one tool call, got body {body}"
    );
}
