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

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cognee_utils::pacing::{Pacer, llm_pacer};
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
    /// Minimum HTTP attempts before the request may fail (a floor, not a cap).
    network_retries: usize,
    /// Minimum elapsed time before the request may fail.
    retry_min_elapsed: Duration,
    /// Dispatch pacer; `None` leaves the client unpaced.
    pacer: Option<Arc<Pacer>>,
}

impl OpenAIResponsesClient {
    /// Default OpenAI API base URL.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";
    /// Default retry attempts for transient network/server errors.
    pub const DEFAULT_NETWORK_RETRIES: usize = 3;
    /// Default minimum retry window — Python's `LLM_MIN_RETRY_SECONDS = 240`.
    pub const DEFAULT_MIN_RETRY_ELAPSED: Duration = Duration::from_secs(240);

    /// Construct a new client.
    pub fn new(api_key: impl Into<String>, base_url: Option<String>) -> LlmResult<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            // See `OpenAIAdapter::DEFAULT_CONNECT_TIMEOUT`: without this a
            // black-holed connect consumes the whole request timeout.
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| LlmError::ConfigError(format!("Failed to create HTTP client: {e}")))?;
        Ok(Self {
            api_key: api_key.into(),
            base_url: base_url.unwrap_or_else(|| Self::DEFAULT_BASE_URL.to_string()),
            client,
            network_retries: Self::DEFAULT_NETWORK_RETRIES,
            retry_min_elapsed: Self::DEFAULT_MIN_RETRY_ELAPSED,
            pacer: None,
        })
    }

    /// Configure the minimum attempts for transient network/server errors.
    pub fn with_network_retries(mut self, retries: u32) -> Self {
        self.network_retries = usize::try_from(retries).unwrap_or(usize::MAX);
        self
    }

    /// Configure the minimum time transient failures are retried for.
    /// [`Duration::ZERO`] reduces the stop condition to a plain attempt cap.
    pub fn with_min_retry_elapsed(mut self, min_elapsed: Duration) -> Self {
        self.retry_min_elapsed = min_elapsed;
        self
    }

    /// Attach a dispatch pacer, overriding the process-wide one.
    pub fn with_pacer(mut self, pacer: Arc<Pacer>) -> Self {
        self.pacer = Some(pacer);
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
        let budget = crate::retry::RetryBudget::new(
            u32::try_from(self.network_retries).unwrap_or(u32::MAX),
            self.retry_min_elapsed,
        );
        let pacer = self.pacer.clone().or_else(llm_pacer);
        // Started before the loop, and so before the pacer's admission wait and
        // the in-flight queue inside it, so queueing time counts against the
        // retry budget rather than being invisible to it.
        let started = Instant::now();
        let mut retry_after: Option<Duration> = None;
        let mut attempt: u32 = 0;

        loop {
            debug!(attempt, "Responses API attempt");
            if attempt > 0 {
                let backoff = crate::retry::retry_backoff(attempt);
                // A usable hint replaces the backoff outright, including when it
                // asks for less: the provider knows when its window resets.
                let delay = retry_after.take().unwrap_or(backoff);
                warn!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    error = %last_error,
                    "Responses API request failed, retrying",
                );
                tokio::time::sleep(delay).await;
            }

            // Before the queue below, because this is the only admission that
            // can pace a caller without an in-flight permit in hand.
            let paced_before_queue = match pacer.as_deref() {
                Some(pacer) => pacer.admit().await,
                None => false,
            };

            // See the identical acquisition in the OpenAI adapter: transport-level
            // concurrency ceiling, taken *after* admission and released at the end
            // of the iteration so a permit only ever covers a live socket.
            let _in_flight = crate::in_flight::acquire_in_flight().await;

            // Re-gate immediately before the send: an episode can open while this
            // caller sits in the queue, and without this every caller that
            // cleared the fast path together would still fire one unpaced send at
            // a provider that has just reported overload. Skipped when the
            // admission above already paced this attempt, so an attempt never
            // spends two tokens. See the OpenAI adapter for the full rationale.
            if !paced_before_queue && let Some(pacer) = pacer.as_deref() {
                pacer.admit().await;
            }

            attempt += 1;

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
                    if e.is_timeout()
                        && let Some(pacer) = pacer.as_deref()
                    {
                        pacer.record_overload("timeout");
                    }
                    last_error = LlmError::NetworkError(e.to_string());
                    if budget.is_exhausted(attempt, started.elapsed()) {
                        break;
                    }
                    continue;
                }
            };

            let status = response.status();
            if !status.is_success() {
                let code = status.as_u16();
                let hint = crate::retry::retry_after_hint(response.headers());
                if let Some(reason) = crate::retry::overload_reason(code)
                    && let Some(pacer) = pacer.as_deref()
                {
                    pacer.record_overload(reason);
                }

                let error_body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());

                let quota_exhausted =
                    code == 429 && crate::retry::is_quota_or_billing_error(&error_body);

                let err = match code {
                    401 => LlmError::AuthenticationError(error_body),
                    // 402 was previously mapped to a retryable ApiError, unlike
                    // the other two adapters. Billing failures are terminal.
                    402 => LlmError::PaymentRequired(error_body),
                    429 if quota_exhausted => LlmError::PaymentRequired(error_body),
                    429 => LlmError::RateLimitExceeded(error_body),
                    400 => LlmError::InvalidResponse(format!("Bad request: {error_body}")),
                    404 => LlmError::ModelNotFound(error_body),
                    _ => LlmError::ApiError(format!("HTTP {status}: {error_body}")),
                };
                if matches!(code, 400..=402 | 404) || quota_exhausted {
                    return Err(err);
                }
                retry_after = hint;
                last_error = err;
                if budget.is_exhausted(attempt, started.elapsed()) {
                    break;
                }
                continue;
            }

            let body_text = response.text().await.map_err(|e| {
                LlmError::DeserializationError(format!("Failed to read response body: {e}"))
            })?;
            return serde_json::from_str::<Value>(&body_text).map_err(|e| {
                LlmError::DeserializationError(format!(
                    "Failed to parse response: {e}. Raw body: {body_text}"
                ))
            });
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Responses API request failed after {} attempt(s) over {:.1}s: {}",
            attempt,
            started.elapsed().as_secs_f64(),
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
        self.get_json(&format!("/responses/{response_id}")).await
    }

    async fn submit_tool_outputs(
        &self,
        response_id: &str,
        tool_outputs: Vec<Value>,
    ) -> LlmResult<Value> {
        self.post_json(
            &format!("/responses/{response_id}/submit_tool_outputs"),
            json!({ "tool_outputs": tool_outputs }),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
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
