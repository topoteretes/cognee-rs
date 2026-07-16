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
use tracing::{debug, instrument, warn};

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::{Llm, StructuredOutputValidator};
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
    /// Output-token ceiling (Python's `llm_max_completion_tokens`). The per-request
    /// `max_tokens` sent to Anthropic is `min(this, the model's documented cap)`.
    max_completion_tokens: u32,
    /// Extra request parameters merged into every Messages request body, lowered
    /// from `LLM_ARGS` (Python `llm_config.llm_args`). Empty = no-op. Explicit
    /// request keys always win, so this only fills gaps (e.g. `top_p`).
    extra_args: serde_json::Map<String, Value>,
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
    /// Default output-token ceiling, matching Python's `llm_max_completion_tokens`.
    /// The per-request value is clamped to the model's documented cap. Aliases the
    /// crate-wide [`crate::DEFAULT_MAX_COMPLETION_TOKENS`] so it moves in lockstep
    /// with the config and `GenerationOptions` defaults.
    pub const DEFAULT_MAX_COMPLETION_TOKENS: u32 = crate::DEFAULT_MAX_COMPLETION_TOKENS;

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
            max_completion_tokens: Self::DEFAULT_MAX_COMPLETION_TOKENS,
            extra_args: serde_json::Map::new(),
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

    /// Set the output-token ceiling (wire from `llm_max_completion_tokens`). The
    /// per-request `max_tokens` is still clamped to the model's documented cap.
    pub fn with_max_completion_tokens(mut self, ceiling: u32) -> Self {
        self.max_completion_tokens = ceiling;
        self
    }

    /// Set extra request parameters (`LLM_ARGS` / Python `llm_config.llm_args`),
    /// merged into each Messages request body. Explicit keys the adapter sets
    /// (model / max_tokens / messages / tools / ...) always win; these only fill
    /// the gaps, matching Python's `{**self.llm_args, **kwargs}`.
    pub fn with_extra_args(mut self, args: serde_json::Map<String, Value>) -> Self {
        self.extra_args = args;
        self
    }

    /// Documented output-token cap per Anthropic model family, mirroring the
    /// `litellm.model_cost[model]["max_tokens"]` lookup the Python adapter uses.
    ///
    /// Substring-matched **in order**, so a broader pattern must never precede a
    /// narrower one it would shadow (`claude-3` after `claude-3-5`; `opus-4`
    /// after `opus-4-5`). Adding a model is a data edit here rather than a new
    /// branch in the lookup.
    ///
    /// Note: this is a hand-maintained table and needs refreshing as Anthropic
    /// ships models. Sourcing it from a real caps dataset (litellm's
    /// `model_cost`) is the durable fix and belongs with the shared `llm::http`
    /// extraction follow-up.
    const MODEL_OUTPUT_CAPS: &'static [(&'static str, u32)] = &[
        // Claude 3.x
        ("claude-3-5", 8_192),
        ("claude-3.5", 8_192),
        ("claude-3-7", 64_000), // 3.7 Sonnet supports 64K extended output
        ("claude-3.7", 64_000),
        ("claude-3", 4_096), // Claude 3 opus / sonnet / haiku
        // Claude 4.x — narrower (point-release) patterns first.
        ("opus-4-5", 64_000),
        ("opus-4.5", 64_000),
        ("opus-4", 32_000), // Opus 4 / 4.1
        ("sonnet-4-6", 128_000),
        ("sonnet-4.6", 128_000),
        ("sonnet-4", 64_000), // Sonnet 4 / 4.5
        ("haiku-4", 64_000),
    ];

    /// Cap applied to models absent from [`MODEL_OUTPUT_CAPS`](Self::MODEL_OUTPUT_CAPS).
    ///
    /// Deliberately conservative: forwarding an operator-raised ceiling unclamped
    /// 400s whenever it exceeds the model's real limit (a terminal error), while
    /// clamping low only reduces the output budget. It does mean a newly released
    /// model under-budgets until the table above is refreshed.
    const UNKNOWN_MODEL_OUTPUT_CAP: u32 = 32_000;

    /// Documented maximum output tokens for `model`.
    ///
    /// Always returns a value, so the configured ceiling is clamped on every
    /// path: an operator-raised `llm_max_completion_tokens` can never be
    /// forwarded above a model's real cap and 400 the request.
    fn model_max_output_tokens(model: &str) -> u32 {
        let m = model.to_ascii_lowercase();
        Self::MODEL_OUTPUT_CAPS
            .iter()
            .find(|(pattern, _)| m.contains(*pattern))
            .map_or(Self::UNKNOWN_MODEL_OUTPUT_CAP, |(_, cap)| *cap)
    }

    /// The `max_tokens` to send: `min(caller value, configured ceiling, model cap)`.
    ///
    /// The configured ceiling (`llm_max_completion_tokens`) is an upper bound on
    /// *every* path, not only when the caller passes `None`:
    /// `GenerationOptions::default()` sets `Some(16384)`, so a default-options
    /// caller would otherwise silently bypass a lower operator-configured ceiling
    /// (an operator setting 4096 to cap cost would still get 8192). Python treats
    /// `llm_max_completion_tokens` as the effective ceiling on all paths. The
    /// model cap is applied last so Anthropic never 400s on
    /// `max_tokens > model limit`.
    fn effective_max_tokens(&self, opts: &GenerationOptions) -> u32 {
        let requested = opts.max_tokens.map_or(self.max_completion_tokens, |v| {
            v.min(self.max_completion_tokens)
        });
        // Floor at 1: Anthropic requires max_tokens >= 1 and 400s on 0, so a
        // misconfigured `llm_max_completion_tokens=0` (the plain u32 parse accepts
        // it) must not hard-fail every call.
        requested
            .min(Self::model_max_output_tokens(&self.model))
            .max(1)
    }

    /// Split messages into the top-level `system` string and the `user`/
    /// `assistant` message list. Anthropic has no `system` role inside
    /// `messages`, so all system messages are concatenated into `system`.
    fn split_messages(messages: &[Message]) -> (String, Vec<Value>) {
        let mut system_parts: Vec<&str> = Vec::new();
        let mut converted: Vec<Value> = Vec::new();
        for m in messages {
            let role = match m.role {
                MessageRole::System => {
                    system_parts.push(m.content.as_str());
                    continue;
                }
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
            };
            converted.push(json!({ "role": role, "content": m.content }));
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
    #[instrument(
        name = "llm.api_call",
        level = "info",
        skip(self, request_body),
        fields(
            url = tracing::field::Empty,
            cognee.llm.model = self.model.as_str(),
            cognee.llm.provider = "anthropic",
        ),
    )]
    async fn call_api(&self, request_body: &Value) -> LlmResult<AnthropicResponse> {
        let url = format!("{}/messages", self.base_url);
        tracing::Span::current().record("url", url.as_str());
        let debug_enabled = std::env::var("COGNEE_DEBUG_LLM_REQUEST")
            .map(|v| cognee_utils::parse_env_bool(&v))
            .unwrap_or(false);
        if debug_enabled {
            let pretty = serde_json::to_string_pretty(request_body)
                .unwrap_or_else(|_| request_body.to_string());
            eprintln!("\n[COGNEE_DEBUG_LLM_REQUEST] POST {url}\n{pretty}\n");
        }

        let mut last_error = LlmError::NetworkError("No attempt made".to_string());

        for attempt in 0..=self.network_retries {
            debug!(attempt, "Anthropic API attempt");
            if attempt > 0 {
                // Shared jittered backoff (equal jitter, issue #19): a batch of
                // concurrent requests that all 429 at once must not retry in
                // lockstep and re-trip the limit. Same helper the OpenAI adapter uses.
                let delay = crate::retry::retry_backoff(attempt as u32);
                warn!(
                    attempt,
                    network_retries = self.network_retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %last_error,
                    "Anthropic request failed, retrying",
                );
                tokio::time::sleep(delay).await;
            }

            let response = match self
                .client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", &self.anthropic_version)
                .header("content-type", "application/json")
                .json(request_body)
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
                    402 => LlmError::PaymentRequired(error_body),
                    429 => LlmError::RateLimitExceeded(error_body),
                    400 => LlmError::InvalidResponse(format!("Bad request: {error_body}")),
                    404 => LlmError::ModelNotFound(error_body),
                    _ => LlmError::ApiError(format!("HTTP {status}: {error_body}")),
                };
                // Non-retryable: bad request, auth, billing, or unknown model.
                // 402 mirrors Python, whose retry policy also excludes
                // LLMPaymentRequiredError: retrying a billing failure can never
                // succeed, it just burns the budget.
                if matches!(status.as_u16(), 400..=402 | 404) {
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
        // Anthropic requires at least one user/assistant message. When the caller
        // passes only system messages, hoisting leaves `converted` empty and the
        // API 400s on an empty `messages` array. Fold the system text into a
        // single user turn instead (the OpenAI backend tolerates system-only
        // input), so the public methods don't hard-fail on it.
        let (system, converted) = if converted.is_empty() && !system.is_empty() {
            (
                String::new(),
                vec![json!({ "role": "user", "content": system })],
            )
        } else {
            (system, converted)
        };
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.effective_max_tokens(opts),
            "messages": converted,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if let Some(temp) = opts.temperature {
            body["temperature"] = json!(temp);
        }
        // Merge LLM_ARGS (Python `{**self.llm_args, **kwargs}`): the explicit keys
        // above always win, so extra_args only fills gaps (e.g. `top_p`). Scoped
        // to the chat/structured path — the vision request builds its own body.
        if let Some(obj) = body.as_object_mut() {
            for (k, v) in &self.extra_args {
                obj.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        body
    }

    /// Build a schema-aware validator for the type-erased raw path (which has no
    /// Rust type to deserialize into).
    ///
    /// Enforces that every property named in the schema's top-level `required`
    /// array is present and non-null, so a tool input omitting a required field
    /// drives a corrective retry instead of being returned as `Ok`. Mirrors the
    /// OpenAI adapter's validator; folding the two into one shared helper belongs
    /// with the `llm::http` extraction follow-up rather than churning `openai.rs`
    /// here.
    fn schema_required_validator(schema: &Value) -> impl Fn(&Value) -> Result<(), String> + '_ {
        move |value: &Value| {
            let Some(required) = schema.get("required").and_then(Value::as_array) else {
                return Ok(());
            };
            let Some(obj) = value.as_object() else {
                return Err("expected a JSON object".to_string());
            };
            for field in required {
                if let Some(name) = field.as_str() {
                    match obj.get(name) {
                        None => return Err(format!("missing required field `{name}`")),
                        Some(Value::Null) => {
                            return Err(format!("required field `{name}` is null"));
                        }
                        _ => {}
                    }
                }
            }
            Ok(())
        }
    }

    /// Append a corrective instruction to the last user turn so the next attempt
    /// carries the failure reason, the way instructor reasks with the validation
    /// error (mirrors `OpenAIAdapter::append_corrective_instruction`).
    fn append_corrective_instruction(body: &mut Value, reason: Option<&str>) {
        let detail = match reason {
            Some(r) => format!("Your previous response failed validation: {r}. "),
            None => "Your previous response could not be parsed into the required structure. "
                .to_string(),
        };
        let instruction = format!(
            "{detail}Call the `{STRUCTURED_OUTPUT_TOOL}` tool again and return ONE complete \
             object that fills in every required field, strictly matching the schema. No extra text."
        );
        let Some(messages) = body["messages"].as_array_mut() else {
            return;
        };
        match messages.last_mut() {
            // Extend the existing user turn in place.
            Some(last) if last["role"] == "user" => {
                let original = last["content"].as_str().unwrap_or("").to_string();
                last["content"] = json!(format!("{original}\n\n{instruction}"));
            }
            // Last turn is assistant, or there is no turn: a bare append would be
            // a no-op and the correction would be silently dropped, so add a new
            // user turn carrying it.
            _ => messages.push(json!({ "role": "user", "content": instruction })),
        }
    }

    /// Shared structured-output loop with instructor-style corrective retries.
    ///
    /// A present-but-unusable tool input (fails `validator`, or was truncated at
    /// `max_tokens` so the object is incomplete) is re-asked with the reason in
    /// context, rather than returned as `Ok` — which would abort the caller at
    /// deserialization. Terminal provider errors short-circuit instead of burning
    /// the retry budget.
    async fn structured_output_impl(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
        validator: Option<StructuredOutputValidator<'_>>,
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
                // Back off between corrective re-asks. `call_api`'s ladder only
                // covers transport retries inside a single attempt, so without
                // this the outer loop would re-ask immediately (Python waits
                // between structured retries via `wait_exponential_jitter`).
                let delay = crate::retry::retry_backoff(attempt as u32);
                debug!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    "retrying Anthropic structured output",
                );
                tokio::time::sleep(delay).await;
            }

            match self.call_api(&body).await {
                Ok(response) => {
                    // `stop_reason == "max_tokens"` means the tool input was cut
                    // off mid-object: present and JSON-parseable, but incomplete.
                    // Re-ask instead of returning a partial object.
                    let truncated = response.stop_reason.as_deref() == Some("max_tokens");
                    match response.tool_input(STRUCTURED_OUTPUT_TOOL) {
                        Some(_) if truncated => {
                            // The tool input was cut off at max_tokens: present and
                            // JSON-parseable, but incomplete. Re-asking with the SAME
                            // budget would truncate again at the same point, so raise
                            // it toward the model's documented cap for the next
                            // attempt. If we are already at that cap a larger budget
                            // is impossible, so fail terminally rather than re-ask
                            // until MaxRetriesExceeded.
                            let model_cap = Self::model_max_output_tokens(&self.model);
                            let current = body["max_tokens"].as_u64().unwrap_or(0) as u32;
                            if current >= model_cap {
                                return Err(LlmError::InvalidResponse(format!(
                                    "Anthropic structured output was truncated at the model's \
                                     {model_cap}-token output cap and cannot be completed within \
                                     that budget"
                                )));
                            }
                            body["max_tokens"] = json!(model_cap);
                            let reason = "the previous answer was cut off at max_tokens before \
                                          the object was complete";
                            last_error = LlmError::InvalidResponse(format!(
                                "Anthropic structured output truncated: {reason}"
                            ));
                            Self::append_corrective_instruction(&mut body, Some(reason));
                        }
                        Some(input) => match validator.map(|v| v(&input)) {
                            None | Some(Ok(())) => return Ok(input),
                            Some(Err(reason)) => {
                                last_error = LlmError::InvalidResponse(format!(
                                    "Anthropic tool input failed validation: {reason}"
                                ));
                                Self::append_corrective_instruction(&mut body, Some(&reason));
                            }
                        },
                        None => {
                            last_error = LlmError::InvalidResponse(
                                "Anthropic response did not contain the forced tool_use block"
                                    .to_string(),
                            );
                            Self::append_corrective_instruction(&mut body, None);
                        }
                    }
                }
                // Terminal: retrying cannot fix auth / billing / unknown-model,
                // and `call_api` has already exhausted its own transport ladder
                // when it returns MaxRetriesExceeded — re-entering it would
                // restart backoff at attempt 0 and hammer a failing endpoint. A
                // 400 stays retryable (Python's tenacity excludes only
                // 401/402/404), which is also the "model rejected the schema"
                // repair case.
                Err(
                    e @ (LlmError::AuthenticationError(_)
                    | LlmError::PaymentRequired(_)
                    | LlmError::ModelNotFound(_)
                    | LlmError::MaxRetriesExceeded(_)),
                ) => return Err(e),
                // A retryable error reaching here is a raw API 400 (InvalidResponse):
                // 429/5xx/network all exhaust inside `call_api` to the terminal
                // MaxRetriesExceeded above. Retrying 400 is Python-faithful
                // (tenacity excludes only 401/402/404), but re-POSTing an identical
                // body just fails the same way — so append the reason so the next
                // attempt differs, the way instructor always reasks with changed
                // content.
                Err(e) => {
                    Self::append_corrective_instruction(&mut body, Some(&e.to_string()));
                    last_error = e;
                }
            }
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Anthropic structured output failed after {} attempt(s): {}",
            self.structured_output_retries, last_error
        )))
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

        let response = self.call_api(&body).await?;
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
        // The raw path has no Rust type to deserialize into, so synthesise a
        // schema-aware validator (same as the OpenAI adapter): a tool input that
        // omits a required field drives a corrective retry instead of returning
        // `Ok` and aborting the caller at deserialization.
        let validator = Self::schema_required_validator(json_schema);
        self.structured_output_impl(messages, json_schema, options, Some(&validator))
            .await
    }

    async fn create_structured_output_with_messages_raw_validated(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
        validator: StructuredOutputValidator<'_>,
    ) -> LlmResult<Value> {
        self.structured_output_impl(messages, json_schema, options, Some(validator))
            .await
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn supports_function_calling(&self) -> bool {
        true
    }

    fn supports_vision(&self) -> bool {
        true
    }

    fn max_context_length(&self) -> u32 {
        // Claude 3+ models all support at least a 200k-token context window.
        200_000
    }

    /// Describe an image via the Messages API. Claude 3+/4 are vision-capable, so
    /// (unlike ollama/mistral/gemini) this is a real override rather than the
    /// trait's `FeatureNotSupported` default — otherwise a dataset containing an
    /// image would abort the whole cognify run under `LLM_PROVIDER=anthropic`.
    async fn transcribe_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        use base64::Engine as _;

        if !mime_type.starts_with("image/") {
            return Err(LlmError::InvalidResponse(format!(
                "Expected image/* MIME type, got: {mime_type}"
            )));
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(image_bytes);
        let max_tokens = options.as_ref().and_then(|o| o.max_tokens).unwrap_or(300);

        // Anthropic vision uses a base64 `image` content block (not OpenAI's
        // `image_url`). Built directly, not via `base_request`, so `LLM_ARGS` do
        // not bleed into the description request (matching the OpenAI adapter).
        let request_body = json!({
            "model": self.model,
            "max_tokens": max_tokens.max(1),
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "What's in this image?" },
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": mime_type,
                            "data": b64,
                        }
                    }
                ]
            }],
        });

        let response = self.call_api(&request_body).await?;
        Ok(response.text())
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
            total_tokens: u.input_tokens.saturating_add(u.output_tokens),
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
    fn max_tokens_is_clamped_to_the_model_output_cap() {
        // Claude 3.5 Sonnet caps output at 8192.
        let sonnet = AnthropicAdapter::new("claude-3-5-sonnet-20241022", "k", None).unwrap();

        // GenerationOptions::default() sets Some(16384); it must clamp to 8192
        // (the model cap), not send 16384 (which 400s) nor a hard 4096.
        assert_eq!(
            sonnet.effective_max_tokens(&GenerationOptions::default()),
            8192
        );
        // An explicit "use the budget" (max_tokens: None) also resolves to the
        // clamped ceiling, not a truncating 4096.
        let unset = GenerationOptions {
            max_tokens: None,
            ..Default::default()
        };
        assert_eq!(sonnet.effective_max_tokens(&unset), 8192);

        // Claude 3 (opus/sonnet/haiku) caps at 4096.
        let opus3 = AnthropicAdapter::new("claude-3-opus-20240229", "k", None).unwrap();
        assert_eq!(
            opus3.effective_max_tokens(&GenerationOptions::default()),
            4096
        );

        // Claude Sonnet 4 caps at 64k, so the default 16384 ceiling passes under it.
        let sonnet4 = AnthropicAdapter::new("claude-sonnet-4-20250514", "k", None).unwrap();
        assert_eq!(
            sonnet4.effective_max_tokens(&GenerationOptions::default()),
            16384
        );
        // A ceiling raised above a model's cap must clamp, not 400. Claude Opus 4
        // caps at 32k: a 40k ceiling clamps to 32k (the regression for the 4.x
        // family — an unclamped passthrough here 400s every request).
        let opus4 = AnthropicAdapter::new("claude-opus-4-20250514", "k", None)
            .unwrap()
            .with_max_completion_tokens(40_000);
        assert_eq!(opus4.effective_max_tokens(&unset), 32_000);
        // Claude Sonnet 4 caps at 64k, so a 40k ceiling is honoured under it.
        let sonnet4_hi = AnthropicAdapter::new("claude-sonnet-4-20250514", "k", None)
            .unwrap()
            .with_max_completion_tokens(40_000);
        assert_eq!(sonnet4_hi.effective_max_tokens(&unset), 40_000);
        // An unknown / future model clamps to the conservative 32k floor.
        let unknown = AnthropicAdapter::new("claude-something-future", "k", None)
            .unwrap()
            .with_max_completion_tokens(40_000);
        assert_eq!(unknown.effective_max_tokens(&unset), 32_000);

        // An explicit ceiling below the model cap is honoured (min wins).
        let capped = AnthropicAdapter::new("claude-3-5-sonnet-20241022", "k", None)
            .unwrap()
            .with_max_completion_tokens(2000);
        assert_eq!(capped.effective_max_tokens(&unset), 2000);
        // ...and it is an upper bound on the default-options path too, not only
        // when the caller passes None: GenerationOptions::default() carries
        // Some(16384), which must not bypass a lower configured ceiling.
        assert_eq!(
            capped.effective_max_tokens(&GenerationOptions::default()),
            2000
        );
    }

    #[test]
    fn model_caps_table_is_ordered_so_broad_patterns_do_not_shadow_narrow_ones() {
        // The table is substring-matched in order, so `claude-3` must not shadow
        // `claude-3-5`/`claude-3-7`, and `opus-4`/`sonnet-4` must not shadow their
        // point releases. This pins that ordering.
        assert_eq!(
            AnthropicAdapter::model_max_output_tokens("claude-3-opus-20240229"),
            4_096
        );
        assert_eq!(
            AnthropicAdapter::model_max_output_tokens("claude-3-5-sonnet-20241022"),
            8_192
        );
        // 3.7 Sonnet supports 64K extended output, not the 3.5 8192.
        assert_eq!(
            AnthropicAdapter::model_max_output_tokens("claude-3-7-sonnet-latest"),
            64_000
        );
        assert_eq!(
            AnthropicAdapter::model_max_output_tokens("claude-opus-4-20250514"),
            32_000
        );
        assert_eq!(
            AnthropicAdapter::model_max_output_tokens("claude-opus-4-5-20251101"),
            64_000
        );
        assert_eq!(
            AnthropicAdapter::model_max_output_tokens("claude-sonnet-4-20250514"),
            64_000
        );
        assert_eq!(
            AnthropicAdapter::model_max_output_tokens("claude-sonnet-4-6-latest"),
            128_000
        );
        // Unknown / future models fall back to the conservative cap.
        assert_eq!(
            AnthropicAdapter::model_max_output_tokens("claude-something-future"),
            AnthropicAdapter::UNKNOWN_MODEL_OUTPUT_CAP
        );
    }

    #[test]
    fn base_request_folds_system_only_into_a_user_turn() {
        // A system-only message list must not send Anthropic an empty `messages`
        // array (a terminal 400); the system text becomes a single user turn.
        let adapter = AnthropicAdapter::new("claude-3-5-sonnet-20241022", "k", None).unwrap();
        let body = adapter.base_request(
            &[Message::system("be terse")],
            &GenerationOptions::default(),
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "system-only input must not send empty messages"
        );
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "be terse");
        assert!(
            body.get("system").is_none(),
            "system text was folded into the user turn, so no top-level system field"
        );
    }

    #[test]
    fn extra_args_fill_gaps_but_explicit_keys_win() {
        let extra = json!({ "top_p": 0.5, "max_tokens": 99 });
        let adapter = AnthropicAdapter::new("claude-3-5-sonnet-20241022", "k", None)
            .unwrap()
            .with_extra_args(extra.as_object().unwrap().clone());
        let body = adapter.base_request(&[Message::user("hi")], &GenerationOptions::default());
        // `top_p` is not set by the adapter, so LLM_ARGS fills that gap.
        assert_eq!(body["top_p"], json!(0.5));
        // `max_tokens` IS set by the adapter, so the explicit value wins over
        // LLM_ARGS (Python `{**llm_args, **kwargs}`).
        assert_ne!(body["max_tokens"], json!(99));
    }

    #[test]
    fn effective_max_tokens_floors_at_one() {
        let unset = GenerationOptions {
            max_tokens: None,
            ..Default::default()
        };
        let zeroed = AnthropicAdapter::new("claude-3-5-sonnet-20241022", "k", None)
            .unwrap()
            .with_max_completion_tokens(0);
        assert_eq!(zeroed.effective_max_tokens(&unset), 1);
    }

    #[test]
    fn corrective_instruction_appends_user_turn_when_last_is_assistant() {
        // A no-op here would silently drop the correction; a new user turn is added.
        let mut body = json!({ "messages": [{ "role": "assistant", "content": "prior" }] });
        AnthropicAdapter::append_corrective_instruction(&mut body, Some("missing field `x`"));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "a new user turn must carry the correction");
        assert_eq!(msgs[1]["role"], "user");
        assert!(
            msgs[1]["content"]
                .as_str()
                .unwrap()
                .contains("missing field `x`")
        );
    }

    #[tokio::test]
    async fn transcribe_image_rejects_non_image_mime() {
        let adapter = AnthropicAdapter::new("claude-3-5-sonnet-20241022", "k", None).unwrap();
        let err = adapter
            .transcribe_image(b"not an image", "text/plain", None)
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(_)));
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
