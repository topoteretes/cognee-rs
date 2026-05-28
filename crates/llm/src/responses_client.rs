//! OpenAI Responses API client abstraction.
//!
//! This is a separate surface from the chat-completions [`Llm`](crate::Llm) trait
//! because the Responses API has a meaningfully different shape — `input` /
//! `output` arrays, function-call items in `output`, and a different usage
//! payload (`input_tokens` / `output_tokens` instead of `prompt_tokens` /
//! `completion_tokens`).
//!
//! Used by the HTTP server's `POST /api/v1/responses` handler. The trait
//! deliberately models the Python `client.responses.create(...)` return shape:
//! a JSON `Value`-shaped response with `id`, `output`, and `usage`, plus a
//! best-effort polling hook for stored / async responses.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};
use tracing::{debug, instrument, warn};

use crate::error::{LlmError, LlmResult};

/// Request to the OpenAI Responses API.
#[derive(Debug, Clone)]
pub struct ResponsesRequest {
    /// Model identifier.
    pub model: String,
    /// Free-form input text. Multimodal inputs (file references etc.) are
    /// modelled via `extra_input_items` and merged into the wire payload.
    pub input: String,
    /// Optional `instructions` field (system-prompt analogue).
    pub instructions: Option<String>,
    /// Tools array — typically `DEFAULT_TOOLS`. `None` means do not send a
    /// `tools` field at all.
    pub tools: Option<Vec<Value>>,
    /// Tool selection policy. `"auto"` / `"none"` / `"required"` or an
    /// object. Sent verbatim.
    pub tool_choice: Option<Value>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Optional cap on completion tokens (`max_output_tokens` on the wire).
    pub max_output_tokens: Option<u32>,
    /// Optional end-user identifier forwarded for abuse-tracking.
    pub user: Option<String>,
    /// Extra wire fields merged into the top-level request object. Use
    /// sparingly — exists for forward-compat with new OpenAI fields.
    pub extra_fields: Option<Value>,
}

impl ResponsesRequest {
    /// Build a minimal request with only `model` and `input` set.
    pub fn new(model: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            input: input.into(),
            instructions: None,
            tools: None,
            tool_choice: None,
            temperature: None,
            max_output_tokens: None,
            user: None,
            extra_fields: None,
        }
    }

    /// Render as the JSON body POSTed to `/v1/responses`.
    pub fn to_wire(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("model".into(), Value::String(self.model.clone()));
        obj.insert("input".into(), Value::String(self.input.clone()));
        if let Some(ref s) = self.instructions {
            obj.insert("instructions".into(), Value::String(s.clone()));
        }
        if let Some(ref tools) = self.tools {
            obj.insert("tools".into(), Value::Array(tools.clone()));
        }
        if let Some(ref tc) = self.tool_choice {
            obj.insert("tool_choice".into(), tc.clone());
        }
        if let Some(t) = self.temperature {
            obj.insert(
                "temperature".into(),
                serde_json::Number::from_f64(t as f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
            );
        }
        if let Some(m) = self.max_output_tokens {
            obj.insert("max_output_tokens".into(), Value::Number(m.into()));
        }
        if let Some(ref u) = self.user {
            obj.insert("user".into(), Value::String(u.clone()));
        }
        if let Some(Value::Object(extra)) = self.extra_fields.as_ref() {
            for (k, v) in extra {
                obj.insert(k.clone(), v.clone());
            }
        }
        Value::Object(obj)
    }
}

/// Object-safe trait wrapping the OpenAI Responses API.
///
/// Implementations return the raw `serde_json::Value` from the upstream
/// response so the HTTP-server layer can mirror Python's
/// `response.model_dump()` behaviour exactly without extra structural
/// translation in the LLM crate.
#[async_trait]
pub trait ResponsesClient: Send + Sync {
    /// Create a new response. Mirrors Python's
    /// `client.responses.create(...)`. Returns the raw JSON `Value` from
    /// the upstream API (the caller is responsible for shaping it into
    /// the public `ResponseBodyDTO`).
    async fn create_response(&self, request: &ResponsesRequest) -> LlmResult<Value>;

    /// Retrieve a stored / async response by id. Used to poll until
    /// completion. Mirrors `GET /v1/responses/{id}`.
    async fn retrieve_response(&self, response_id: &str) -> LlmResult<Value>;

    /// Submit tool outputs back for the given response id. Mirrors
    /// `POST /v1/responses/{id}/submit_tool_outputs`. Returns the
    /// updated response.
    ///
    /// `tool_outputs` is an array of `{"tool_call_id": "...", "output": "..."}`
    /// objects (matching the OpenAI wire shape).
    async fn submit_tool_outputs(
        &self,
        response_id: &str,
        tool_outputs: Vec<Value>,
    ) -> LlmResult<Value>;
}

// ─── OpenAI implementation ───────────────────────────────────────────────────

/// OpenAI Responses API client.
///
/// Backed by the same `reqwest` client / retry semantics as
/// [`crate::adapters::OpenAIAdapter`].
#[derive(Clone)]
pub struct OpenAIResponsesClient {
    api_key: String,
    base_url: String,
    client: Client,
    network_retries: usize,
}

impl OpenAIResponsesClient {
    /// Default OpenAI API base URL.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";
    /// Default retry attempts for transient network/server errors.
    pub const DEFAULT_NETWORK_RETRIES: usize = 3;

    /// Construct a new client.
    pub fn new(api_key: impl Into<String>, base_url: Option<String>) -> LlmResult<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| LlmError::ConfigError(format!("Failed to create HTTP client: {}", e)))?;
        Ok(Self {
            api_key: api_key.into(),
            base_url: base_url.unwrap_or_else(|| Self::DEFAULT_BASE_URL.to_string()),
            client,
            network_retries: Self::DEFAULT_NETWORK_RETRIES,
        })
    }

    /// Configure retry attempts for transient network/server errors.
    pub fn with_network_retries(mut self, retries: u32) -> Self {
        self.network_retries = usize::try_from(retries).unwrap_or(usize::MAX);
        self
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    /// POST a JSON body to the given relative URL and parse the response
    /// as JSON. Retries on transient (5xx, 429, network) failures.
    #[instrument(
        name = "responses_api.post",
        level = "info",
        skip(self, body),
        fields(url = tracing::field::Empty),
    )]
    async fn post_json(&self, path: &str, body: Value) -> LlmResult<Value> {
        let url = format!("{}{}", self.base_url, path);
        tracing::Span::current().record("url", url.as_str());
        self.send_with_retries(reqwest::Method::POST, url, Some(body))
            .await
    }

    /// GET a path. Same retry semantics as `post_json`.
    #[instrument(
        name = "responses_api.get",
        level = "info",
        skip(self),
        fields(url = tracing::field::Empty),
    )]
    async fn get_json(&self, path: &str) -> LlmResult<Value> {
        let url = format!("{}{}", self.base_url, path);
        tracing::Span::current().record("url", url.as_str());
        self.send_with_retries(reqwest::Method::GET, url, None)
            .await
    }

    async fn send_with_retries(
        &self,
        method: reqwest::Method,
        url: String,
        body: Option<Value>,
    ) -> LlmResult<Value> {
        let mut last_error = LlmError::NetworkError("No attempt made".to_string());
        for attempt in 0..=self.network_retries {
            debug!(attempt, "Responses API attempt");
            if attempt > 0 {
                let delay_ms = (1_000u64 * 2u64.saturating_pow(attempt as u32 - 1)).min(30_000);
                warn!(
                    attempt,
                    delay_ms,
                    error = %last_error,
                    "Responses API request failed, retrying",
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }

            let mut builder = self
                .client
                .request(method.clone(), &url)
                .header("Authorization", self.auth_header())
                .header("Content-Type", "application/json");
            if let Some(ref b) = body {
                builder = builder.json(b);
            }

            let response = match builder.send().await {
                Ok(r) => r,
                Err(e) => {
                    last_error = LlmError::NetworkError(e.to_string());
                    continue;
                }
            };

            let status = response.status();
            if !status.is_success() {
                let error_body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());
                let err = match status.as_u16() {
                    401 => LlmError::AuthenticationError(error_body),
                    429 => LlmError::RateLimitExceeded(error_body),
                    400 => LlmError::InvalidResponse(format!("Bad request: {}", error_body)),
                    404 => LlmError::ModelNotFound(error_body),
                    _ => LlmError::ApiError(format!("HTTP {}: {}", status, error_body)),
                };
                if matches!(status.as_u16(), 400 | 401 | 404) {
                    return Err(err);
                }
                last_error = err;
                continue;
            }

            let body_text = response.text().await.map_err(|e| {
                LlmError::DeserializationError(format!("Failed to read response body: {}", e))
            })?;
            return serde_json::from_str::<Value>(&body_text).map_err(|e| {
                LlmError::DeserializationError(format!(
                    "Failed to parse response: {}. Raw body: {}",
                    e, body_text
                ))
            });
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Responses API request failed after {} attempt(s): {}",
            self.network_retries + 1,
            last_error
        )))
    }
}

#[async_trait]
impl ResponsesClient for OpenAIResponsesClient {
    async fn create_response(&self, request: &ResponsesRequest) -> LlmResult<Value> {
        self.post_json("/responses", request.to_wire()).await
    }

    async fn retrieve_response(&self, response_id: &str) -> LlmResult<Value> {
        self.get_json(&format!("/responses/{}", response_id)).await
    }

    async fn submit_tool_outputs(
        &self,
        response_id: &str,
        tool_outputs: Vec<Value>,
    ) -> LlmResult<Value> {
        self.post_json(
            &format!("/responses/{}/submit_tool_outputs", response_id),
            json!({ "tool_outputs": tool_outputs }),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_includes_only_set_fields() {
        let req = ResponsesRequest::new("gpt-4o", "hello");
        let wire = req.to_wire();
        assert_eq!(wire["model"], "gpt-4o");
        assert_eq!(wire["input"], "hello");
        assert!(wire.get("temperature").is_none());
        assert!(wire.get("tools").is_none());
        assert!(wire.get("tool_choice").is_none());
        assert!(wire.get("instructions").is_none());
    }

    #[test]
    fn request_wire_serialises_optional_fields() {
        let mut req = ResponsesRequest::new("gpt-4o", "hello");
        req.temperature = Some(0.7);
        req.max_output_tokens = Some(128);
        req.tool_choice = Some(Value::String("auto".into()));
        req.tools = Some(vec![json!({"type":"function","name":"search"})]);
        req.instructions = Some("be terse".into());
        req.user = Some("u-1".into());
        let wire = req.to_wire();
        let t = wire["temperature"]
            .as_f64()
            .expect("temperature is a number");
        assert!((t - 0.7).abs() < 1e-3);
        assert_eq!(wire["max_output_tokens"], 128);
        assert_eq!(wire["tool_choice"], "auto");
        assert_eq!(wire["tools"][0]["name"], "search");
        assert_eq!(wire["instructions"], "be terse");
        assert_eq!(wire["user"], "u-1");
    }

    #[test]
    fn extra_fields_merge_into_top_level() {
        let mut req = ResponsesRequest::new("gpt-4o", "hello");
        req.extra_fields = Some(json!({"reasoning": {"effort": "low"}}));
        let wire = req.to_wire();
        assert_eq!(wire["reasoning"]["effort"], "low");
    }
}
