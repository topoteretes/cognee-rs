//! Anthropic Messages API adapter (native, not OpenAI-compatible).
//!
//! The Anthropic Messages API differs structurally from the OpenAI chat API:
//! the system prompt is a top-level `system` field (not a `system` role inside
//! `messages`), roles are limited to `user`/`assistant`, `max_tokens` is
//! required, and structured output is produced via `tool_use` rather than
//! `response_format`. This adapter hoists system messages into `system`, maps
//! the remaining messages to content, and forces a single tool whose
//! `input_schema` is the response schema so the model returns the structured
//! object as the tool input.
//!
//! It reuses the shared [`Llm`] trait, the `Message` / `GenerationOptions` /
//! `GenerationResponse` types, and the [`LlmError`] enum, and mirrors the
//! transient-retry shape of [`crate::adapters::OpenAIAdapter`]. See issue #17
//! (Tier 2).

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::Llm;
use crate::types::{GenerationOptions, GenerationResponse, Message, MessageRole, TokenUsage};

/// Name of the single forced tool used to carry structured output.
const STRUCTURED_OUTPUT_TOOL: &str = "extract_structured_data";

/// Native adapter for the Anthropic Messages API.
pub struct AnthropicAdapter {
    model: String,
    api_key: String,
    base_url: String,
    anthropic_version: String,
    client: Client,
    structured_output_retries: usize,
    network_retries: usize,
    default_max_tokens: u32,
}

impl AnthropicAdapter {
    /// Default Anthropic API base URL.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.anthropic.com/v1";
    /// Default `anthropic-version` request header.
    pub const DEFAULT_ANTHROPIC_VERSION: &'static str = "2023-06-01";
    /// Default structured-output repair retries (Python instructor parity: 5).
    pub const DEFAULT_STRUCTURED_OUTPUT_RETRIES: usize = 5;
    /// Default transient-network retries.
    pub const DEFAULT_NETWORK_RETRIES: usize = 3;
    /// Anthropic requires `max_tokens`; used when the caller does not set one.
    pub const DEFAULT_MAX_TOKENS: u32 = 4096;

    /// Create a new Anthropic adapter.
    ///
    /// A leading litellm-style `anthropic/` model prefix is stripped for parity
    /// with the Python SDK. `base_url` defaults to the Anthropic API and has any
    /// trailing slash trimmed so request paths never double up.
    pub fn new(
        model: impl Into<String>,
        api_key: impl Into<String>,
        base_url: Option<String>,
    ) -> LlmResult<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| LlmError::ConfigError(format!("Failed to create HTTP client: {e}")))?;

        let model: String = model.into();
        let model = model
            .strip_prefix("anthropic/")
            .map(str::to_string)
            .unwrap_or(model);

        Ok(Self {
            model,
            api_key: api_key.into(),
            base_url: base_url
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or_else(|| Self::DEFAULT_BASE_URL.to_string()),
            anthropic_version: Self::DEFAULT_ANTHROPIC_VERSION.to_string(),
            client,
            structured_output_retries: Self::DEFAULT_STRUCTURED_OUTPUT_RETRIES,
            network_retries: Self::DEFAULT_NETWORK_RETRIES,
            default_max_tokens: Self::DEFAULT_MAX_TOKENS,
        })
    }

    /// Configure retry attempts for structured output extraction (floored at 1).
    pub fn with_structured_output_retries(mut self, retries: u32) -> Self {
        self.structured_output_retries = usize::try_from(retries).unwrap_or(usize::MAX).max(1);
        self
    }

    /// Configure transient network/server retry attempts.
    pub fn with_network_retries(mut self, retries: u32) -> Self {
        self.network_retries = usize::try_from(retries).unwrap_or(usize::MAX);
        self
    }

    /// Override the default `max_tokens` used when a request does not set one.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.default_max_tokens = max_tokens;
        self
    }

    /// Split messages into the top-level `system` string and the `user`/
    /// `assistant` message list. Anthropic has no `system` role inside
    /// `messages`, so all system messages are concatenated into `system`.
    fn split_messages(messages: &[Message]) -> (String, Vec<Value>) {
        let mut system_parts: Vec<&str> = Vec::new();
        let mut converted: Vec<Value> = Vec::new();
        for m in messages {
            match m.role {
                MessageRole::System => system_parts.push(m.content.as_str()),
                MessageRole::User => {
                    converted.push(json!({ "role": "user", "content": m.content }))
                }
                MessageRole::Assistant => {
                    converted.push(json!({ "role": "assistant", "content": m.content }))
                }
            }
        }
        (system_parts.join("\n\n"), converted)
    }

    /// Prepare a schemars-generated schema for use as an Anthropic tool
    /// `input_schema`: drop the `$schema` meta key, which Anthropic does not
    /// expect on a tool schema.
    fn sanitize_tool_schema(schema: &Value) -> Value {
        let mut out = schema.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.remove("$schema");
        }
        out
    }

    /// POST `request_body` to `/messages` with the standard headers and a
    /// transient-retry loop with exponential backoff.
    async fn call_api(&self, request_body: Value) -> LlmResult<AnthropicResponse> {
        let url = format!("{}/messages", self.base_url);
        let debug_enabled = std::env::var("COGNEE_DEBUG_LLM_REQUEST")
            .map(|v| cognee_utils::parse_env_bool(&v))
            .unwrap_or(false);
        if debug_enabled {
            let pretty = serde_json::to_string_pretty(&request_body)
                .unwrap_or_else(|_| request_body.to_string());
            eprintln!("\n[COGNEE_DEBUG_LLM_REQUEST] POST {url}\n{pretty}\n");
        }

        let mut last_error = LlmError::NetworkError("No attempt made".to_string());

        for attempt in 0..=self.network_retries {
            debug!(attempt, "Anthropic API attempt");
            if attempt > 0 {
                let delay_ms = (1_000u64 * 2u64.saturating_pow(attempt as u32 - 1)).min(30_000);
                warn!(
                    attempt,
                    network_retries = self.network_retries,
                    delay_ms,
                    error = %last_error,
                    "Anthropic request failed, retrying",
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }

            let response = match self
                .client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", &self.anthropic_version)
                .header("content-type", "application/json")
                .json(&request_body)
                .send()
                .await
            {
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
                    400 => LlmError::InvalidResponse(format!("Bad request: {error_body}")),
                    404 => LlmError::ModelNotFound(error_body),
                    _ => LlmError::ApiError(format!("HTTP {status}: {error_body}")),
                };
                // Non-retryable: bad request, auth, or unknown model.
                if matches!(status.as_u16(), 400 | 401 | 404) {
                    return Err(err);
                }
                last_error = err;
                continue;
            }

            let body = response.text().await.map_err(|e| {
                LlmError::DeserializationError(format!("Failed to read response body: {e}"))
            })?;
            if debug_enabled {
                eprintln!("\n[COGNEE_DEBUG_LLM_RESPONSE] POST {url}\n{body}\n");
            }
            return serde_json::from_str::<AnthropicResponse>(&body).map_err(|e| {
                LlmError::DeserializationError(format!(
                    "Failed to parse response: {e}. Raw body: {body}"
                ))
            });
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Anthropic request failed after {} attempt(s): {}",
            self.network_retries + 1,
            last_error
        )))
    }

    /// Build the base request body shared by completion and structured output.
    fn base_request(&self, messages: &[Message], opts: &GenerationOptions) -> Value {
        let (system, converted) = Self::split_messages(messages);
        let mut body = json!({
            "model": self.model,
            "max_tokens": opts.max_tokens.unwrap_or(self.default_max_tokens),
            "messages": converted,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if let Some(temp) = opts.temperature {
            body["temperature"] = json!(temp);
        }
        body
    }
}

#[async_trait]
impl Llm for AnthropicAdapter {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let opts = options.unwrap_or_default();
        let mut body = self.base_request(&messages, &opts);
        if let Some(stop) = opts.stop.as_ref()
            && !stop.is_empty()
        {
            body["stop_sequences"] = json!(stop);
        }

        let response = self.call_api(body).await?;
        Ok(GenerationResponse {
            content: response.text(),
            model: response.model,
            finish_reason: response.stop_reason,
            usage: response.usage.map(TokenUsage::from),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let opts = options.unwrap_or_default();
        let mut body = self.base_request(&messages, &opts);
        body["tools"] = json!([{
            "name": STRUCTURED_OUTPUT_TOOL,
            "description": "Return the extracted data in the required schema.",
            "input_schema": Self::sanitize_tool_schema(json_schema),
        }]);
        // Force the model to call exactly this tool, so its `input` is the answer.
        body["tool_choice"] = json!({ "type": "tool", "name": STRUCTURED_OUTPUT_TOOL });

        let mut last_error =
            LlmError::InvalidResponse("No structured-output attempt made".to_string());

        for attempt in 0..self.structured_output_retries {
            if attempt > 0 {
                debug!(attempt, "retrying Anthropic structured output");
            }
            match self.call_api(body.clone()).await {
                Ok(response) => match response.tool_input(STRUCTURED_OUTPUT_TOOL) {
                    Some(input) => return Ok(input),
                    None => {
                        last_error = LlmError::InvalidResponse(
                            "Anthropic response did not contain the forced tool_use block"
                                .to_string(),
                        );
                    }
                },
                // call_api already returns terminal errors (400/401/404) without
                // retrying; surface those immediately rather than burning attempts.
                Err(e @ (LlmError::AuthenticationError(_) | LlmError::ModelNotFound(_))) => {
                    return Err(e);
                }
                Err(e) => last_error = e,
            }
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Anthropic structured output failed after {} attempt(s): {}",
            self.structured_output_retries, last_error
        )))
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn supports_function_calling(&self) -> bool {
        true
    }

    fn max_context_length(&self) -> u32 {
        // Claude 3+ models all support at least a 200k-token context window.
        200_000
    }
}

/// Parsed Anthropic Messages response. `content` is kept as raw blocks so both
/// `text` and `tool_use` block types are handled without an exhaustive enum.
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    model: String,
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

impl AnthropicResponse {
    /// Concatenate the text of every `text` content block.
    fn text(&self) -> String {
        self.content
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")
    }

    /// Return the `input` of the first `tool_use` block named `tool_name`.
    fn tool_input(&self, tool_name: &str) -> Option<Value> {
        self.content.iter().find_map(|b| {
            let is_tool = b.get("type").and_then(Value::as_str) == Some("tool_use")
                && b.get("name").and_then(Value::as_str) == Some(tool_name);
            is_tool.then(|| b.get("input").cloned()).flatten()
        })
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

impl From<AnthropicUsage> for TokenUsage {
    fn from(u: AnthropicUsage) -> Self {
        TokenUsage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn new_strips_anthropic_prefix_and_normalizes_base_url() {
        let adapter = AnthropicAdapter::new(
            "anthropic/claude-3-5-sonnet-20241022",
            "sk-ant-test",
            Some("https://api.anthropic.com/v1/".to_string()),
        )
        .expect("adapter should build");
        assert_eq!(adapter.model(), "claude-3-5-sonnet-20241022");
        assert_eq!(adapter.base_url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn split_messages_hoists_system_and_maps_roles() {
        let (system, converted) = AnthropicAdapter::split_messages(&[
            Message::system("be terse"),
            Message::user("hello"),
            Message::assistant("hi"),
            Message::system("and helpful"),
        ]);
        assert_eq!(system, "be terse\n\nand helpful");
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0]["role"], "user");
        assert_eq!(converted[0]["content"], "hello");
        assert_eq!(converted[1]["role"], "assistant");
    }

    #[test]
    fn sanitize_tool_schema_drops_schema_meta_key() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let out = AnthropicAdapter::sanitize_tool_schema(&schema);
        assert!(out.get("$schema").is_none());
        assert_eq!(out["type"], "object");
        assert_eq!(out["required"], json!(["name"]));
    }

    #[test]
    fn response_extracts_text_blocks() {
        let response: AnthropicResponse = serde_json::from_value(json!({
            "model": "claude-3-5-sonnet-20241022",
            "content": [
                { "type": "text", "text": "Hello " },
                { "type": "text", "text": "world" }
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 10, "output_tokens": 3 }
        }))
        .unwrap();
        assert_eq!(response.text(), "Hello world");
        let usage = TokenUsage::from(response.usage.unwrap());
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 3);
        assert_eq!(usage.total_tokens, 13);
    }

    #[test]
    fn response_extracts_forced_tool_input() {
        let response: AnthropicResponse = serde_json::from_value(json!({
            "model": "claude-3-5-sonnet-20241022",
            "content": [
                { "type": "text", "text": "ignored preamble" },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": STRUCTURED_OUTPUT_TOOL,
                    "input": { "name": "Ada", "age": 36 }
                }
            ],
            "stop_reason": "tool_use"
        }))
        .unwrap();
        let input = response.tool_input(STRUCTURED_OUTPUT_TOOL).unwrap();
        assert_eq!(input["name"], "Ada");
        assert_eq!(input["age"], 36);
        assert!(response.tool_input("other_tool").is_none());
    }
}
