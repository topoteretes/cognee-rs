//! `/api/v1/responses` — OpenAI-compatible Responses API (Stage B).
//!
//! Mirrors Python's [`cognee.api.v1.responses.routers.get_responses_router`].
//! The handler:
//!
//! 1. Validates and authenticates the request (handled by extractors).
//! 2. Calls the configured `ResponsesClient` (OpenAI Responses API).
//! 3. Walks the upstream `output` array; for each `function_call` item,
//!    dispatches the call to the in-process tool dispatcher and folds the
//!    result into a `ResponseToolCallDTO`.
//! 4. Returns a `ResponseBodyDTO` shaped to match the Python wire contract.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::Value;

use cognee_llm::ResponsesRequest;

use crate::auth::AuthenticatedUser;
use crate::dto::responses::{
    ChatUsageDTO, FunctionCallDTO, ResponseBodyDTO, ResponseRequestDTO, ResponseToolCallDTO,
    ToolCallOutputDTO,
};
use crate::error::ApiError;
use crate::middleware::validation::Json as ValidatedJson;
use crate::responses_dispatch::{
    ComponentHandlesDispatcher, ToolDispatcher, default_tools, dispatch_one, extract_tool_calls,
};
use crate::state::AppState;

// ─── Router ───────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", post(create_response))
}

// ─── POST / ───────────────────────────────────────────────────────────────────

/// `POST /api/v1/responses` — create a response (OpenAI-compatible).
///
/// Auth and request-body validation are enforced by the extractors. Errors
/// surfaced from missing wiring use the canonical `{"detail": "..."}` envelope.
#[utoipa::path(
    post,
    path = "/api/v1/responses",
    tag = "responses",
    operation_id = "create_response",
    request_body = ResponseRequestDTO,
    responses(
        (status = 200, description = "OpenAI-compatible response with dispatched tool calls", body = ResponseBodyDTO),
        (status = 400, description = "validation error"),
        (status = 401, description = "unauthorized"),
        (status = 500, description = "internal server error"),
    ),
)]
#[tracing::instrument(
    name = "cognee.api.responses.create",
    skip(state, req),
    fields(cognee.user.id = %user.id)
)]
async fn create_response(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<ResponseRequestDTO>,
) -> Result<Response, ApiError> {
    let components = state
        .components()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("server components are not wired")))?;
    let responses_client = components
        .responses_client
        .clone()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("responses client is not wired")))?;

    // Wrap the components in an Arc<ComponentHandles> for the dispatcher.
    // `state.components()` returns a borrow, so reconstruct the Arc from the
    // `state.lib` slot (cheap clone).
    let components_arc = state
        .lib
        .clone()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("server components are not wired")))?;
    let dispatcher: Arc<dyn ToolDispatcher> =
        Arc::new(ComponentHandlesDispatcher::new(components_arc));

    let upstream = create_response_with(&user, req, responses_client.as_ref(), dispatcher.as_ref())
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    Ok((StatusCode::OK, Json(upstream)).into_response())
}

/// Shared implementation that is independent of axum extractors so it can be
/// unit-tested with a mock `ResponsesClient` + mock dispatcher.
pub(crate) async fn create_response_with(
    user: &AuthenticatedUser,
    req: ResponseRequestDTO,
    client: &dyn cognee_llm::ResponsesClient,
    dispatcher: &dyn ToolDispatcher,
) -> Result<ResponseBodyDTO, anyhow::Error> {
    // ── Build upstream request ────────────────────────────────────────────────
    let model_label = match req.model {
        crate::dto::responses::CogneeModelDTO::CogneeV1 => "cognee-v1".to_string(),
    };
    // Python parity: the actual upstream model is hard-coded to gpt-4o for the
    // single advertised "cognee-v1" alias (see Python `_get_model_client` /
    // `call_openai_api_for_model`).
    let upstream_model = "gpt-4o".to_string();

    // Tools: caller-supplied or DEFAULT_TOOLS.
    let tools: Vec<Value> = match req.tools {
        Some(ref dto_tools) if !dto_tools.is_empty() => dto_tools
            .iter()
            .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
            .collect(),
        _ => default_tools(),
    };

    let upstream_request = ResponsesRequest {
        model: upstream_model,
        input: req.input.clone(),
        instructions: None,
        tools: Some(tools),
        tool_choice: Some(req.tool_choice.clone()),
        temperature: Some(req.temperature),
        max_output_tokens: req.max_completion_tokens,
        user: req.user.clone(),
        extra_fields: None,
    };

    // ── Call upstream ─────────────────────────────────────────────────────────
    //
    // Log the verbose upstream error internally; surface a generic message to
    // the caller so we never leak OpenAI response bodies (which may include
    // API keys or other sensitive context) through the canonical
    // `{"detail": "..."}` envelope.
    let upstream = client
        .create_response(&upstream_request)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "responses upstream call failed");
            anyhow::anyhow!("responses upstream call failed")
        })?;

    // ── Extract id, output, usage ────────────────────────────────────────────
    let response_id = upstream
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));
    let output = upstream.get("output").cloned().unwrap_or(Value::Null);
    let usage = upstream.get("usage").cloned().unwrap_or(Value::Null);

    // ── Dispatch each function_call item ──────────────────────────────────────
    let tool_calls = extract_tool_calls(&output);
    let mut processed: Vec<ResponseToolCallDTO> = Vec::with_capacity(tool_calls.len());
    for call in tool_calls {
        let result = dispatch_one(&call, dispatcher, user).await;
        let data_obj = match result.data {
            Value::Object(m) => Some(m.into_iter().collect()),
            other => {
                let mut map = std::collections::HashMap::new();
                map.insert("result".to_string(), other);
                Some(map)
            }
        };
        processed.push(ResponseToolCallDTO {
            id: call.id,
            kind: "function".to_string(),
            function: FunctionCallDTO {
                name: call.name,
                arguments: call.arguments,
            },
            output: Some(ToolCallOutputDTO {
                status: result.status,
                data: data_obj,
            }),
        });
    }

    // ── Assemble usage ────────────────────────────────────────────────────────
    let chat_usage = ChatUsageDTO {
        // Responses API uses input_tokens/output_tokens; map them onto
        // prompt_tokens/completion_tokens for Python wire parity.
        prompt_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        completion_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    };

    Ok(ResponseBodyDTO {
        id: response_id,
        created: chrono::Utc::now().timestamp(),
        model: model_label,
        object: "response".to_string(),
        status: "completed".to_string(),
        tool_calls: processed,
        usage: Some(chat_usage),
        metadata: None,
    })
}

// ─── Inline tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use cognee_llm::{LlmError, ResponsesClient as TraitResponsesClient};
    use serde_json::json;
    use tower::ServiceExt;
    use uuid::Uuid;

    // ── Mock ResponsesClient ─────────────────────────────────────────────────

    #[derive(Clone)]
    struct MockResponsesClient {
        canned_create: Value,
    }

    #[async_trait]
    impl TraitResponsesClient for MockResponsesClient {
        async fn create_response(&self, _request: &ResponsesRequest) -> Result<Value, LlmError> {
            Ok(self.canned_create.clone())
        }
        async fn retrieve_response(&self, _: &str) -> Result<Value, LlmError> {
            Ok(json!({"status": "completed"}))
        }
        async fn submit_tool_outputs(&self, _: &str, _: Vec<Value>) -> Result<Value, LlmError> {
            Ok(json!({"status": "completed"}))
        }
    }

    /// Stub dispatcher that always succeeds with a fixed payload — used to
    /// exercise the loop without touching real backends.
    struct CapturingDispatcher;

    #[async_trait]
    impl ToolDispatcher for CapturingDispatcher {
        async fn dispatch_search(
            &self,
            arguments: &Value,
            _user: &AuthenticatedUser,
        ) -> crate::responses_dispatch::ToolDispatchResult {
            crate::responses_dispatch::ToolDispatchResult {
                status: "success".into(),
                data: json!({"result": format!("search:{}", arguments.get("search_query").and_then(Value::as_str).unwrap_or(""))}),
            }
        }
        async fn dispatch_cognify(
            &self,
            arguments: &Value,
            _user: &AuthenticatedUser,
        ) -> crate::responses_dispatch::ToolDispatchResult {
            crate::responses_dispatch::ToolDispatchResult {
                status: "success".into(),
                data: json!({"result": format!("cognify:{}", arguments.get("text").and_then(Value::as_str).unwrap_or(""))}),
            }
        }
    }

    fn fake_user() -> AuthenticatedUser {
        AuthenticatedUser {
            id: Uuid::new_v4(),
            email: "u@example.com".into(),
            is_superuser: false,
            is_verified: true,
            is_active: true,
            tenant_id: Some(Uuid::new_v4()),
            auth_method: crate::auth::AuthMethod::DefaultUser,
        }
    }

    fn sample_request() -> ResponseRequestDTO {
        ResponseRequestDTO {
            model: crate::dto::responses::CogneeModelDTO::CogneeV1,
            input: "Find facts about Alice".into(),
            tools: None,
            tool_choice: json!("auto"),
            user: None,
            temperature: 1.0,
            max_completion_tokens: None,
        }
    }

    // ── create_response_with — happy path ─────────────────────────────────────

    #[tokio::test]
    async fn create_response_with_dispatches_search_tool_call() {
        let upstream = json!({
            "id": "resp_xyz",
            "output": [
                {
                    "type": "function_call",
                    "name": "search",
                    "arguments": "{\"search_query\":\"Alice\"}",
                    "call_id": "call_1"
                }
            ],
            "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7}
        });
        let client = MockResponsesClient {
            canned_create: upstream,
        };
        let dispatcher = CapturingDispatcher;
        let user = fake_user();

        let body = create_response_with(&user, sample_request(), &client, &dispatcher)
            .await
            .expect("ok");

        assert_eq!(body.id, "resp_xyz");
        assert_eq!(body.model, "cognee-v1");
        assert_eq!(body.status, "completed");
        assert_eq!(body.tool_calls.len(), 1);
        let tc = &body.tool_calls[0];
        assert_eq!(tc.function.name, "search");
        assert_eq!(tc.id, "call_1");
        let out = tc.output.as_ref().expect("output");
        assert_eq!(out.status, "success");
        let data = out.data.as_ref().expect("data");
        assert!(
            data["result"].as_str().expect("result").contains("Alice"),
            "expected search dispatch to echo 'Alice', got: {data:?}"
        );
        let usage = body.usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 5);
        assert_eq!(usage.completion_tokens, 2);
        assert_eq!(usage.total_tokens, 7);
    }

    #[tokio::test]
    async fn create_response_with_dispatches_cognify_tool_call() {
        let upstream = json!({
            "id": "resp_q",
            "output": [
                {
                    "type": "function_call",
                    "name": "cognify",
                    "arguments": "{\"text\":\"facts about Alice\"}",
                    "call_id": "call_c1"
                }
            ],
            "usage": {}
        });
        let client = MockResponsesClient {
            canned_create: upstream,
        };
        let dispatcher = CapturingDispatcher;
        let user = fake_user();
        let body = create_response_with(&user, sample_request(), &client, &dispatcher)
            .await
            .expect("ok");
        assert_eq!(body.tool_calls.len(), 1);
        let out = body.tool_calls[0].output.as_ref().expect("output");
        assert_eq!(out.status, "success");
        let data = out.data.as_ref().expect("data");
        assert!(
            data["result"]
                .as_str()
                .expect("result")
                .contains("facts about Alice")
        );
    }

    #[tokio::test]
    async fn create_response_with_no_tool_calls_returns_empty_list() {
        let upstream = json!({
            "id": "resp_noop",
            "output": [
                {"type": "message", "content": "hello"}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        });
        let client = MockResponsesClient {
            canned_create: upstream,
        };
        let dispatcher = CapturingDispatcher;
        let user = fake_user();
        let body = create_response_with(&user, sample_request(), &client, &dispatcher)
            .await
            .expect("ok");
        assert!(body.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn create_response_with_synthesises_id_if_upstream_omits_it() {
        let upstream = json!({
            "output": [],
            "usage": {}
        });
        let client = MockResponsesClient {
            canned_create: upstream,
        };
        let dispatcher = CapturingDispatcher;
        let user = fake_user();
        let body = create_response_with(&user, sample_request(), &client, &dispatcher)
            .await
            .expect("ok");
        assert!(body.id.starts_with("resp_"));
    }

    // ── Inline 501-regression-guard ──────────────────────────────────────────

    /// Tier-3 headline check: posting the minimum payload must NOT return 501.
    ///
    /// When components are unwired the handler returns 500 (per spec) with a
    /// `{"detail": "..."}` body; never 501. This is the inverse of the old
    /// Stage-A stub.
    #[tokio::test]
    async fn responses_no_longer_returns_501() {
        let state = AppState::build(crate::config::HttpServerConfig::default())
            .await
            .expect("build state");
        let app = Router::new()
            .route("/", post(create_response_no_auth))
            .with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"model":"cognee-v1","input":"hello"}"#))
            .expect("request");
        let resp = app.oneshot(req).await.expect("response");
        assert_ne!(
            resp.status(),
            StatusCode::NOT_IMPLEMENTED,
            "Stage B handler must not return 501"
        );
    }

    /// Helper that bypasses the auth extractor so the test can drive the
    /// handler directly. Mirrors the pattern used in `routers/cognify.rs`.
    async fn create_response_no_auth(
        State(state): State<AppState>,
        ValidatedJson(req): ValidatedJson<ResponseRequestDTO>,
    ) -> Result<Response, ApiError> {
        let user = fake_user();
        create_response(user, State(state), ValidatedJson(req)).await
    }
}
