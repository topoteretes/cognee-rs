//! `/api/v1/llm` — LLM utility endpoints used by the cognee frontend's
//! "Schema Builder" UI.
//!
//! - `POST /custom-prompt` synthesizes an extraction prompt from a graph model.
//! - `POST /infer-schema` proposes a graph model from sample text.
//!
//! Both endpoints filter `parameters` through `safe_params` before forwarding
//! to the underlying LLM. Error envelope is `{error}` (single field), per
//! Python parity. See `docs/http-server/routers/llm.md` §2.
//!
//! Implementation note: we rely on the type-erased `Llm::create_structured_output_raw`
//! path with an empty schema (`{"type": "string"}`). Python's
//! `LLMGateway.acreate_structured_output(..., response_model=str)` does the same.

use std::collections::HashMap;

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde_json::Value;

use cognee_llm::prompts::render_prompt;
use cognee_llm::types::GenerationOptions;
use cognee_llm::{LlmError, graph_schema_to_graph_model};

use crate::auth::AuthenticatedUser;
use crate::dto::llm::{
    CustomPromptGenerationPayloadDTO, CustomPromptGenerationResponseDTO, InferSchemaPayloadDTO,
    InferSchemaResponseDTO, safe_params,
};
use crate::error::ApiError;
use crate::middleware::validation::Json as ValidatedJson;
use crate::state::AppState;

/// Build the `/api/v1/llm` sub-router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/custom-prompt", post(post_custom_prompt))
        .route("/infer-schema", post(post_infer_schema))
}

// ─── Schemas ──────────────────────────────────────────────────────────────────

/// JSON Schema describing a plain string response.
///
/// Python's `LLMGateway.acreate_structured_output(... response_model=str)` is
/// equivalent — the LLM is instructed to return free-form text and the wire
/// is treated as opaque.
fn string_response_schema() -> serde_json::Value {
    serde_json::json!({"type": "string"})
}

/// Convert filtered LLM kwargs into `GenerationOptions`.
///
/// `safe_params` already restricted `params` to the four allowed keys
/// (`temperature`, `max_tokens`, `top_p`, `seed`). We map the first three onto
/// the corresponding `GenerationOptions` slots; `seed` is not yet plumbed
/// through the `Llm` trait and is silently dropped (Python forwards it as a
/// kwarg). Returns `None` when no recognized keys are present so handlers can
/// pass `None` to adapters that have meaningful defaults.
fn options_from_safe_params(filtered: &Value) -> Option<GenerationOptions> {
    let obj = filtered.as_object()?;
    if obj.is_empty() {
        return None;
    }
    let mut opts = GenerationOptions {
        temperature: None,
        max_tokens: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
    };
    if let Some(t) = obj.get("temperature").and_then(|v| v.as_f64()) {
        opts.temperature = Some(t as f32);
    }
    if let Some(m) = obj.get("max_tokens").and_then(|v| v.as_u64()) {
        opts.max_tokens = Some(m as u32);
    }
    if let Some(tp) = obj.get("top_p").and_then(|v| v.as_f64()) {
        opts.top_p = Some(tp as f32);
    }
    Some(opts)
}

// ─── post_custom_prompt ───────────────────────────────────────────────────────

/// `POST /api/v1/llm/custom-prompt` — synthesize an extraction prompt.
#[utoipa::path(
    post,
    path = "/api/v1/llm/custom-prompt",
    tag = "llm",
    request_body = CustomPromptGenerationPayloadDTO,
    responses(
        (status = 200, description = "custom prompt", body = CustomPromptGenerationResponseDTO),
        (status = 400, description = "value error"),
        (status = 401, description = "unauthorized"),
        (status = 409, description = "catch-all"),
    )
)]
#[tracing::instrument(name = "cognee.api.llm.custom_prompt", skip(state, payload))]
pub async fn post_custom_prompt(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<CustomPromptGenerationPayloadDTO>,
) -> Result<Json<CustomPromptGenerationResponseDTO>, ApiError> {
    let Some(llm) = state.components().and_then(|c| c.llm.clone()) else {
        return Err(ApiError::LlmError(
            StatusCode::CONFLICT,
            "LLM adapter is not wired".to_string(),
        ));
    };

    let parameter_keys: Vec<String> = {
        let mut keys: Vec<String> = payload
            .parameters
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        keys.sort();
        keys
    };
    crate::telemetry::emit(
        "LLM Custom Prompt Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": "POST /v1/llm/custom-prompt",
            "response_model": "str",
            "parameter_keys": parameter_keys,
        }),
    );

    // Render prompts.
    let graph_model_json = serde_json::to_string(&payload.graph_model)
        .map_err(|err| ApiError::LlmError(StatusCode::BAD_REQUEST, err.to_string()))?;
    let mut user_ctx: HashMap<&str, &str> = HashMap::new();
    user_ctx.insert("GRAPH_SCHEMA_JSON", graph_model_json.as_str());
    let user_prompt = render_prompt("custom_prompt_generation_user", &user_ctx)
        .map_err(|err| ApiError::LlmError(StatusCode::BAD_REQUEST, err.to_string()))?;

    let system_ctx = HashMap::new();
    let system_prompt = render_prompt("custom_prompt_generation_system", &system_ctx)
        .map_err(|err| ApiError::LlmError(StatusCode::BAD_REQUEST, err.to_string()))?;

    // Filter parameters and forward to the LLM. Per Python parity the
    // response is treated as opaque text — no JSON-schema enforcement.
    let filtered = safe_params(&payload.parameters);
    let options = options_from_safe_params(&filtered);
    let schema = string_response_schema();
    let raw = llm
        .create_structured_output_raw(&user_prompt, &system_prompt, &schema, options)
        .await
        .map_err(map_llm_error)?;

    let custom_prompt = match raw {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    Ok(Json(CustomPromptGenerationResponseDTO { custom_prompt }))
}

// ─── post_infer_schema ────────────────────────────────────────────────────────

/// `POST /api/v1/llm/infer-schema` — propose a graph model from sample text.
#[utoipa::path(
    post,
    path = "/api/v1/llm/infer-schema",
    tag = "llm",
    request_body = InferSchemaPayloadDTO,
    responses(
        (status = 200, description = "graph schema", body = InferSchemaResponseDTO),
        (status = 401, description = "unauthorized"),
        (status = 409, description = "schema validation failed"),
        (status = 422, description = "LLM output not valid JSON"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.llm.infer_schema",
    skip(state, payload),
    fields(cognee.llm.input.text_len = payload.text.len())
)]
pub async fn post_infer_schema(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<InferSchemaPayloadDTO>,
) -> Result<Json<InferSchemaResponseDTO>, ApiError> {
    let Some(llm) = state.components().and_then(|c| c.llm.clone()) else {
        return Err(ApiError::LlmError(
            StatusCode::CONFLICT,
            "LLM adapter is not wired".to_string(),
        ));
    };

    crate::telemetry::emit(
        "LLM Infer Schema Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": "POST /v1/llm/infer-schema",
            "text_length": payload.text.len(),
        }),
    );

    let mut user_ctx: HashMap<&str, &str> = HashMap::new();
    user_ctx.insert("SAMPLE_TEXT", payload.text.as_str());
    let user_prompt = render_prompt("infer_schema_user", &user_ctx)
        .map_err(|err| ApiError::LlmError(StatusCode::BAD_REQUEST, err.to_string()))?;
    let system_ctx = HashMap::new();
    let system_prompt = render_prompt("infer_schema_system", &system_ctx)
        .map_err(|err| ApiError::LlmError(StatusCode::BAD_REQUEST, err.to_string()))?;

    let filtered = safe_params(&payload.parameters);
    let options = options_from_safe_params(&filtered);
    let schema = string_response_schema();
    let raw = llm
        .create_structured_output_raw(&user_prompt, &system_prompt, &schema, options)
        .await
        .map_err(map_llm_error)?;

    let output_text = match raw {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };

    // Stage 1: JSON parse — 422 on failure (user-fixable).
    let parsed: serde_json::Value = serde_json::from_str(&output_text).map_err(|err| {
        ApiError::LlmError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("LLM output is not valid JSON: {err}"),
        )
    })?;

    // Stage 2: schema-conversion — 409 on failure (system-fixable).
    graph_schema_to_graph_model(&parsed)
        .map_err(|err| ApiError::LlmError(StatusCode::CONFLICT, err.to_string()))?;

    Ok(Json(InferSchemaResponseDTO {
        graph_schema: parsed,
    }))
}

// ─── Error mapping ────────────────────────────────────────────────────────────

/// Map an LLM adapter error to the router's `{error}` envelope.
///
/// Python uses 400 for `ValueError` (malformed schema, prompt-render failure)
/// and 409 for everything else (network, rate limit, OpenAI 5xx).
fn map_llm_error(err: LlmError) -> ApiError {
    match err {
        LlmError::ConfigError(_)
        | LlmError::ContentPolicyViolation(_)
        | LlmError::InvalidResponse(_) => {
            ApiError::LlmError(StatusCode::BAD_REQUEST, err.to_string())
        }
        _ => ApiError::LlmError(StatusCode::CONFLICT, err.to_string()),
    }
}
