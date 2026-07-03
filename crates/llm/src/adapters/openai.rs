//! OpenAI API adapter with JSON schema support for structured outputs.
//!
//! This adapter uses OpenAI's function calling or JSON mode to generate
//! structured outputs based on JSON schemas derived from Rust types.

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, instrument, warn};

#[allow(unused_imports)]
use cognee_utils::tracing_keys::{COGNEE_LLM_MODEL, COGNEE_LLM_PROVIDER};

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::Llm;
use crate::transcriber::{Transcriber, TranscriptionOutput, validate_audio_format};
use crate::types::{GenerationOptions, GenerationResponse, Message, MessageRole, TokenUsage};

/// OpenAI API adapter.
///
/// Supports structured output generation via:
/// - Strict JSON schema mode (response_format with type: "json_schema")
/// - Function calling (for GPT-4 and GPT-3.5-turbo)
/// - JSON mode (response_format with type: "json_object")
/// - JSON schema validation (via function parameters)
///
/// # Example
/// ```ignore
/// use cognee_llm::adapters::OpenAIAdapter;
/// use cognee_llm::Llm;
///
/// let adapter = OpenAIAdapter::new(
///     "gpt-4-turbo-preview",
///     "sk-...",
///     None, // Use default base URL
/// )?;
///
/// let result: MyStruct = adapter.create_structured_output(
///     "Extract information from this text",
///     "You are a helpful assistant",
///     None,
/// ).await?;
/// ```
#[derive(Clone)]
pub struct OpenAIAdapter {
    model: String,
    api_key: String,
    base_url: String,
    client: Client,
    structured_output_retries: usize,
    /// Number of times to retry the HTTP request on transient network/server errors.
    network_retries: usize,
    /// Model name for audio transcription (e.g. `"whisper-1"`).
    transcription_model: String,
}

impl OpenAIAdapter {
    /// Default OpenAI API base URL
    pub const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";
    /// Default retry attempts for structured output parsing paths.
    ///
    /// Python parity: instructor's `acreate_structured_output` retries up to
    /// `MAX_RETRIES = 5` times on a parse/validation failure. We match that
    /// count so transient malformed responses get the same number of repair
    /// chances before the cognify pipeline gives up.
    pub const DEFAULT_STRUCTURED_OUTPUT_RETRIES: usize = 5;
    /// Default retry attempts for transient network/server errors.
    pub const DEFAULT_NETWORK_RETRIES: usize = 3;

    /// Create a new OpenAI adapter.
    ///
    /// # Arguments
    /// * `model` - Model identifier (e.g., "gpt-4", "gpt-3.5-turbo")
    /// * `api_key` - OpenAI API key
    /// * `base_url` - Optional custom base URL (defaults to OpenAI's API)
    ///
    /// # Returns
    /// A new OpenAI adapter instance
    pub fn new(
        model: impl Into<String>,
        api_key: impl Into<String>,
        base_url: Option<String>,
    ) -> LlmResult<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| LlmError::ConfigError(format!("Failed to create HTTP client: {e}")))?;

        let transcription_model =
            std::env::var("TRANSCRIPTION_MODEL").unwrap_or_else(|_| "whisper-1".to_string());

        // Strip a leading litellm-style "openai/" provider prefix. Python's
        // litellm accepts provider-qualified names (e.g. "openai/gpt-5-mini")
        // and strips the provider before calling the OpenAI-native API, which
        // itself rejects the prefix. Strip it here for parity so a
        // provider-qualified config value works against real OpenAI.
        let model: String = model.into();
        let model = model
            .strip_prefix("openai/")
            .map(str::to_string)
            .unwrap_or(model);

        Ok(Self {
            model,
            api_key: api_key.into(),
            // Normalise a trailing slash so request URLs built as
            // `{base_url}/chat/completions` never produce a double slash. The
            // Gemini OpenAI-compat base ends in `/v1beta/openai/`, and a
            // user-supplied endpoint may too; both would otherwise 404.
            base_url: base_url
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or_else(|| Self::DEFAULT_BASE_URL.to_string()),
            client,
            structured_output_retries: Self::DEFAULT_STRUCTURED_OUTPUT_RETRIES,
            network_retries: Self::DEFAULT_NETWORK_RETRIES,
            transcription_model,
        })
    }

    /// Configure retry attempts for structured output extraction.
    ///
    /// Values lower than 1 are coerced to 1.
    pub fn with_structured_output_retries(mut self, retries: u32) -> Self {
        let retries = usize::try_from(retries).unwrap_or(usize::MAX);
        self.structured_output_retries = retries.max(1);
        self
    }

    /// Configure retry attempts for transient network and server errors (HTTP 429, 5xx).
    ///
    /// Each retry uses exponential backoff starting at 1 s, doubling up to 30 s.
    pub fn with_network_retries(mut self, retries: u32) -> Self {
        self.network_retries = usize::try_from(retries).unwrap_or(usize::MAX);
        self
    }

    /// Configure the model used for audio transcription (default: `"whisper-1"`).
    pub fn with_transcription_model(mut self, model: impl Into<String>) -> Self {
        self.transcription_model = model.into();
        self
    }

    /// Build the authorization header value
    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    /// Whether to request non-thinking mode for local Qwen OpenAI-compatible endpoints.
    fn should_disable_thinking(&self) -> bool {
        self.model.to_lowercase().starts_with("qwen") && !self.base_url.contains("api.openai.com")
    }

    /// True for OpenAI reasoning-model families (`gpt-5*`, `o1*`, `o3*`, `o4*`)
    /// that reject `temperature`/`top_p`/`frequency_penalty`/`presence_penalty`
    /// overrides and require `max_completion_tokens` in place of `max_tokens`.
    ///
    /// Gated on the official `api.openai.com` base URL so custom OpenAI-compatible
    /// proxies (Ollama, vLLM, …) keep accepting legacy parameters even when the
    /// configured model name happens to match a reasoning-family prefix.
    fn is_reasoning_model(&self) -> bool {
        if !self.base_url.contains("api.openai.com") {
            return false;
        }
        let m = self.model.to_lowercase();
        m.starts_with("gpt-5") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
    }

    /// Insert `max_tokens` (or `max_completion_tokens` on reasoning models) into a
    /// request body if `value` is `Some`.
    fn write_max_tokens(&self, body: &mut Value, value: Option<u32>) {
        if let Some(v) = value {
            let key = if self.is_reasoning_model() {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body[key] = json!(v);
        }
    }

    /// Call the OpenAI chat completions API, retrying on transient network/server errors.
    ///
    /// Retries up to `self.network_retries` times with exponential backoff (1 s, 2 s, 4 s …
    /// capped at 30 s) on:
    /// - Network-level failures (connection refused, timeout, etc.)
    /// - HTTP 429 (rate limit exceeded)
    /// - HTTP 5xx (server errors)
    ///
    /// Errors on HTTP 400 and 401 are returned immediately without retrying.
    #[instrument(
        name = "llm.api_call",
        level = "info",
        skip(self, request_body),
        fields(
            url = tracing::field::Empty,
            cognee.llm.model = self.model.as_str(),
            cognee.llm.provider = "openai",
        ),
    )]
    async fn call_api(&self, request_body: Value) -> LlmResult<OpenAIResponse> {
        let url = format!("{}/chat/completions", self.base_url);
        tracing::Span::current().record("url", url.as_str());
        let debug_enabled = std::env::var("COGNEE_DEBUG_LLM_REQUEST")
            .map(|v| cognee_utils::parse_env_bool(&v))
            .unwrap_or(false);

        if debug_enabled {
            let pretty_request = serde_json::to_string_pretty(&request_body)
                .unwrap_or_else(|_| request_body.to_string());
            eprintln!("\n[COGNEE_DEBUG_LLM_REQUEST] POST {url}\n{pretty_request}\n");
        }

        let mut last_error = LlmError::NetworkError("No attempt made".to_string());

        for attempt in 0..=self.network_retries {
            debug!(attempt, "LLM API attempt");
            if attempt > 0 {
                let delay_ms = (1_000u64 * 2u64.saturating_pow(attempt as u32 - 1)).min(30_000);
                warn!(
                    attempt,
                    network_retries = self.network_retries,
                    delay_ms,
                    error = %last_error,
                    "LLM request failed, retrying",
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }

            let response = match self
                .client
                .post(&url)
                .header("Authorization", self.auth_header())
                .header("Content-Type", "application/json")
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
                    _ => LlmError::ApiError(format!("HTTP {status}: {error_body}")),
                };

                // Non-retryable: bad request or authentication failure.
                if matches!(status.as_u16(), 400 | 401) {
                    return Err(err);
                }

                last_error = err;
                continue;
            }

            let response_body = response.text().await.map_err(|e| {
                LlmError::DeserializationError(format!("Failed to read response body: {e}"))
            })?;

            if debug_enabled {
                eprintln!("\n[COGNEE_DEBUG_LLM_RESPONSE] POST {url}\n{response_body}\n");
            }

            return serde_json::from_str::<OpenAIResponse>(&response_body).map_err(|e| {
                LlmError::DeserializationError(format!(
                    "Failed to parse response: {e}. Raw body: {response_body}"
                ))
            });
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "LLM request failed after {} attempt(s): {}",
            self.network_retries + 1,
            last_error
        )))
    }

    /// Convert our Message type to OpenAI's format
    fn convert_messages(messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .map(|msg| {
                json!({
                    "role": match msg.role {
                        MessageRole::System => "system",
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                    },
                    "content": msg.content
                })
            })
            .collect()
    }

    /// Convert JSON Schema to an example JSON with placeholder values
    /// This is clearer for LLMs than showing the full schema
    fn schema_to_example(schema: &Value) -> String {
        fn create_example(value: &Value, definitions: Option<&Value>) -> Value {
            match value {
                Value::Object(obj) => {
                    // Handle $ref references
                    if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str())
                        && let Some(def_name) = ref_str.strip_prefix("#/definitions/")
                        && let Some(defs) = definitions
                        && let Some(def) = defs.get(def_name)
                    {
                        return create_example(def, definitions);
                    }

                    // Get the type of this field
                    let type_val = obj.get("type");

                    // Handle arrays
                    if let Some(Value::String(t)) = type_val
                        && t == "array"
                    {
                        if let Some(items) = obj.get("items") {
                            // Return array with one example item
                            return json!([create_example(items, definitions)]);
                        }
                        return json!([]);
                    }

                    // Handle objects with properties
                    if let Some(props) = obj.get("properties")
                        && let Value::Object(props_obj) = props
                    {
                        let mut result = serde_json::Map::new();
                        for (key, val) in props_obj {
                            result.insert(key.clone(), create_example(val, definitions));
                        }
                        return Value::Object(result);
                    }

                    // Handle primitive types
                    if let Some(Value::String(t)) = type_val {
                        return match t.as_str() {
                            "string" => json!("example"),
                            "number" | "integer" => json!(0),
                            "boolean" => json!(false),
                            _ => json!(null),
                        };
                    }

                    // Handle union types (e.g., ["string", "null"])
                    if let Some(Value::Array(types)) = type_val {
                        for t in types {
                            if let Value::String(type_str) = t
                                && type_str != "null"
                            {
                                return match type_str.as_str() {
                                    "string" => json!("example"),
                                    "number" | "integer" => json!(0),
                                    "boolean" => json!(false),
                                    _ => json!(null),
                                };
                            }
                        }
                    }

                    json!(null)
                }
                _ => value.clone(),
            }
        }

        let definitions = schema.get("definitions");
        let example = create_example(schema, definitions);

        serde_json::to_string_pretty(&example).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Rewrite a `schemars`-generated JSON schema so it satisfies OpenAI's
/// **strict** structured-output requirements.
///
/// OpenAI's `response_format: {type: "json_schema", strict: true}` rejects any
/// schema where an object lacks `"additionalProperties": false` or whose
/// `"required"` array does not list *every* declared property. `schemars`
/// (0.8, draft-07) emits neither guarantee — optional (`Option<T>`) fields are
/// omitted from `required` and `additionalProperties` is left unset. When the
/// strict request 400s, [`OpenAIAdapter::create_structured_output_with_messages_raw`]
/// silently falls back to lenient JSON mode, where the model is free to drop
/// required fields (e.g. a `Node` without its `type`), causing downstream
/// deserialization failures.
///
/// This walks the schema (including `definitions`/`$defs`, `properties`,
/// `items`, and the `anyOf`/`allOf`/`oneOf` combinators) and, for every object
/// that declares `properties`, forces `additionalProperties: false` and sets
/// `required` to the full set of property keys. The `Value` is cloned and
/// returned unchanged for non-object schemas.
fn to_strict_schema(schema: &Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(obj) => {
                if let Some(Value::Object(props)) = obj.get("properties") {
                    // Every declared property must be required under strict mode.
                    let keys: Vec<Value> = props.keys().map(|k| Value::String(k.clone())).collect();
                    obj.insert("required".to_string(), Value::Array(keys));
                    obj.insert("additionalProperties".to_string(), Value::Bool(false));
                }
                for (_k, v) in obj.iter_mut() {
                    walk(v);
                }
            }
            Value::Array(items) => {
                for v in items.iter_mut() {
                    walk(v);
                }
            }
            _ => {}
        }
    }

    let mut out = schema.clone();
    walk(&mut out);
    out
}

#[async_trait]
impl Llm for OpenAIAdapter {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let opts = options.unwrap_or_default();

        let mut request_body = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
        });

        // Add optional parameters. Reasoning models (gpt-5*/o1*/o3*/o4*)
        // reject sampling overrides and only accept `max_completion_tokens`.
        if !self.is_reasoning_model() {
            if let Some(temp) = opts.temperature {
                request_body["temperature"] = json!(temp);
            }
            if let Some(top_p) = opts.top_p {
                request_body["top_p"] = json!(top_p);
            }
            if let Some(freq_penalty) = opts.frequency_penalty {
                request_body["frequency_penalty"] = json!(freq_penalty);
            }
            if let Some(pres_penalty) = opts.presence_penalty {
                request_body["presence_penalty"] = json!(pres_penalty);
            }
        }
        self.write_max_tokens(&mut request_body, opts.max_tokens);
        if let Some(stop) = opts.stop
            && !stop.is_empty()
        {
            request_body["stop"] = json!(stop);
        }

        if self.should_disable_thinking() {
            request_body["think"] = json!(false);
            request_body["reasoning"] = json!({"effort": "none"});
        }

        let response = self.call_api(request_body).await?;

        // Extract the first choice
        let choice = response
            .choices
            .first()
            .ok_or_else(|| LlmError::InvalidResponse("No choices in response".to_string()))?;

        Ok(GenerationResponse {
            content: choice.message.content.clone().unwrap_or_default(),
            model: response.model,
            finish_reason: choice.finish_reason.clone(),
            usage: response.usage.map(|u| TokenUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let is_empty_or_non_json = |raw: &str| {
            let trimmed = raw.trim();
            trimmed.is_empty() || serde_json::from_str::<Value>(trimmed).is_err()
        };

        let parse_json =
            |raw: &str| -> Result<Value, serde_json::Error> { serde_json::from_str(raw) };

        let opts = options.unwrap_or_default();
        let schema = json_schema;

        // OpenAI strict mode requires `additionalProperties: false` and that
        // every property appear in `required` on every object; the raw
        // schemars schema satisfies neither, which would 400 and silently
        // drop us into lenient mode (where required fields can go missing).
        let strict_schema = to_strict_schema(schema);

        // Try strict JSON schema mode first (new OpenAI API behavior).
        let mut strict_schema_request = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "extract_structured_data",
                    "schema": strict_schema,
                    "strict": true
                }
            }
        });

        if !self.is_reasoning_model()
            && let Some(temp) = opts.temperature
        {
            strict_schema_request["temperature"] = json!(temp);
        }
        self.write_max_tokens(&mut strict_schema_request, opts.max_tokens);
        if self.should_disable_thinking() {
            strict_schema_request["think"] = json!(false);
            strict_schema_request["reasoning"] = json!({"effort": "none"});
        }

        for attempt in 0..self.structured_output_retries {
            match self.call_api(strict_schema_request.clone()).await {
                Ok(strict_response) => {
                    let strict_choice = strict_response.choices.first().ok_or_else(|| {
                        LlmError::InvalidResponse(
                            "No choices in strict schema response".to_string(),
                        )
                    })?;

                    if let Some(function_call) = &strict_choice.message.function_call {
                        match parse_json(&function_call.arguments) {
                            Ok(parsed) => return Ok(parsed),
                            Err(e) => {
                                if attempt + 1 < self.structured_output_retries
                                    && is_empty_or_non_json(&function_call.arguments)
                                {
                                    continue;
                                }
                                if !is_empty_or_non_json(&function_call.arguments) {
                                    return Err(LlmError::DeserializationError(format!(
                                        "Failed to deserialize strict function call arguments: {}. Raw: {}",
                                        e, function_call.arguments
                                    )));
                                }
                                break;
                            }
                        }
                    }

                    if let Some(content) = strict_choice.message.content.as_ref() {
                        match parse_json(content) {
                            Ok(parsed) => return Ok(parsed),
                            Err(e) => {
                                if attempt + 1 < self.structured_output_retries
                                    && is_empty_or_non_json(content)
                                {
                                    continue;
                                }
                                if !is_empty_or_non_json(content) {
                                    return Err(LlmError::DeserializationError(format!(
                                        "Failed to deserialize strict JSON content: {e}. Raw: {content}"
                                    )));
                                }
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    // Strict json_schema mode is unsupported by this
                    // model/endpoint (or the schema was rejected). Fall back to
                    // function calling / JSON mode below, but make the reason
                    // visible — a silent fallback is how required fields end up
                    // missing from the model's output.
                    warn!(error = %e, "strict json_schema request failed; falling back to function/JSON mode");
                    break;
                }
            }
        }

        // Try function calling first (works with OpenAI)
        let mut request_body = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
            "functions": [{
                "name": "extract_structured_data",
                "description": "Extract structured data from the input",
                "parameters": schema
            }],
            "function_call": {"name": "extract_structured_data"}
        });

        if !self.is_reasoning_model()
            && let Some(temp) = opts.temperature
        {
            request_body["temperature"] = json!(temp);
        }
        self.write_max_tokens(&mut request_body, opts.max_tokens);
        if self.should_disable_thinking() {
            request_body["think"] = json!(false);
            request_body["reasoning"] = json!({"effort": "none"});
        }

        for attempt in 0..self.structured_output_retries {
            let response = self.call_api(request_body.clone()).await?;

            let choice = response
                .choices
                .first()
                .ok_or_else(|| LlmError::InvalidResponse("No choices in response".to_string()))?;

            if let Some(function_call) = &choice.message.function_call {
                match parse_json(&function_call.arguments) {
                    Ok(parsed) => return Ok(parsed),
                    Err(e) => {
                        if attempt + 1 < self.structured_output_retries
                            && is_empty_or_non_json(&function_call.arguments)
                        {
                            continue;
                        }
                        if !is_empty_or_non_json(&function_call.arguments) {
                            return Err(LlmError::DeserializationError(format!(
                                "Failed to deserialize function call arguments: {}. Raw: {}",
                                e, function_call.arguments
                            )));
                        }
                        break;
                    }
                }
            }

            break;
        }

        // Fallback to JSON mode (works with Ollama and other providers)
        let mut json_messages = Self::convert_messages(&messages);

        let example = Self::schema_to_example(schema);

        if let Some(last_msg) = json_messages.last_mut()
            && last_msg["role"] == "user"
        {
            let original_content = last_msg["content"].as_str().unwrap_or("");
            last_msg["content"] = json!(format!(
                "{}\n\n\
                    Extract the information from the text above and return it as JSON.\n\
                    Use this structure as your template (but with actual data from the text):\n\
                    {}",
                original_content, example
            ));
        }

        let mut json_request = json!({
            "model": self.model,
            "messages": json_messages,
            "response_format": {"type": "json_object"}
        });

        if !self.is_reasoning_model()
            && let Some(temp) = opts.temperature
        {
            json_request["temperature"] = json!(temp);
        }
        self.write_max_tokens(&mut json_request, opts.max_tokens);
        if self.should_disable_thinking() {
            json_request["think"] = json!(false);
            json_request["reasoning"] = json!({"effort": "none"});
        }

        for attempt in 0..self.structured_output_retries {
            let mut request_for_attempt = json_request.clone();

            if attempt > 0 {
                if let Some(messages) = request_for_attempt["messages"].as_array_mut()
                    && let Some(last_msg) = messages.last_mut()
                    && last_msg["role"] == "user"
                {
                    let original_content = last_msg["content"].as_str().unwrap_or("");
                    last_msg["content"] = json!(format!(
                        "{}\n\n/no_think\nReturn ONLY one valid JSON object matching the required schema. No reasoning, no markdown, no extra text.",
                        original_content
                    ));
                }

                if !self.is_reasoning_model() {
                    request_for_attempt["temperature"] = json!(0.0);
                }
            }

            let json_response = self.call_api(request_for_attempt).await?;

            let json_choice = json_response.choices.first().ok_or_else(|| {
                LlmError::InvalidResponse("No choices in JSON mode response".to_string())
            })?;

            let content = json_choice.message.content.as_ref().ok_or_else(|| {
                LlmError::InvalidResponse("No content in JSON mode response".to_string())
            })?;

            match parse_json(content) {
                Ok(parsed) => return Ok(parsed),
                Err(e) => {
                    if attempt + 1 < self.structured_output_retries && is_empty_or_non_json(content)
                    {
                        continue;
                    }
                    return Err(LlmError::DeserializationError(format!(
                        "Failed to deserialize JSON content: {e}. Raw: {content}"
                    )));
                }
            }
        }

        Err(LlmError::InvalidResponse(
            "Structured output retries exhausted without a parseable response".to_string(),
        ))
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_function_calling(&self) -> bool {
        true
    }

    fn max_context_length(&self) -> u32 {
        // Context lengths for common OpenAI models
        match self.model.as_str() {
            m if m.starts_with("gpt-4-turbo") => 128_000,
            m if m.starts_with("gpt-4-32k") => 32_768,
            m if m.starts_with("gpt-4") => 8_192,
            m if m.starts_with("gpt-3.5-turbo-16k") => 16_384,
            m if m.starts_with("gpt-3.5-turbo") => 4_096,
            _ => 4_096, // Conservative default
        }
    }

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
        let data_uri = format!("data:{mime_type};base64,{b64}");

        let vision_model = std::env::var("LLM_VISION_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.model.clone());

        let max_tokens = options.as_ref().and_then(|o| o.max_tokens).unwrap_or(300);

        let mut request_body = json!({
            "model": vision_model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "What's in this image?" },
                    { "type": "image_url", "image_url": { "url": data_uri } }
                ]
            }],
        });
        self.write_max_tokens(&mut request_body, Some(max_tokens));

        let response = self.call_api(request_body).await?;

        let choice = response.choices.first().ok_or_else(|| {
            LlmError::InvalidResponse("No choices in vision response".to_string())
        })?;

        choice.message.content.clone().ok_or_else(|| {
            LlmError::InvalidResponse("Vision response contained no content".to_string())
        })
    }

    fn supports_vision(&self) -> bool {
        let m = self.model.to_lowercase();
        m.contains("gpt-4")
            || m.contains("gpt-5")
            || m.contains("vision")
            || m.contains("o1")
            || m.contains("o3")
            || m.contains("o4")
            || m.contains("llava")
            || m.contains("moondream")
            || m.contains("llama-3.2-vision")
            || m.contains("gemma3")
    }
}

// ---------------------------------------------------------------------------
// Whisper transcription support
// ---------------------------------------------------------------------------

/// Response from the OpenAI Whisper `verbose_json` endpoint.
#[derive(Debug, Deserialize)]
struct WhisperResponse {
    text: String,
    language: Option<String>,
    duration: Option<f32>,
}

/// Map a validated audio format extension to its MIME type.
fn audio_mime_type(format: &str) -> &'static str {
    match format {
        "mp3" | "mpeg" | "mpga" => "audio/mpeg",
        "mp4" | "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "webm" => "audio/webm",
        // validate_audio_format ensures only the above values reach here
        _ => "application/octet-stream",
    }
}

impl OpenAIAdapter {
    /// Call the Whisper transcription API with the same retry logic as `call_api`.
    #[instrument(
        name = "llm.transcription_api_call",
        level = "info",
        skip(self, form),
        fields(
            url = tracing::field::Empty,
            cognee.llm.model = self.transcription_model.as_str(),
            cognee.llm.provider = "openai",
        ),
    )]
    async fn call_transcription_api(
        &self,
        form: reqwest::multipart::Form,
    ) -> LlmResult<WhisperResponse> {
        let url = format!("{}/audio/transcriptions", self.base_url);
        tracing::Span::current().record("url", url.as_str());

        // We cannot clone a multipart Form, so the first attempt uses the
        // original form and retries are not possible for the multipart body.
        // However, we keep the retry loop for network errors that occur
        // *before* the body is consumed (connection refused, DNS failure).
        // For simplicity and matching the guide's design, we rebuild the form
        // if needed by storing the bytes. But since `Form` doesn't support
        // Clone, we perform a single attempt with the form and rely on the
        // caller to retry externally if needed.
        //
        // Actually, the simplest approach is to send the form once and
        // handle retries at a higher level. But the guide says to mirror
        // call_api's retry. Since reqwest::multipart::Form is not Clone,
        // we accept `form` by value and do a single-shot request here,
        // while the `transcribe_audio` impl handles retry by rebuilding
        // the form on each attempt.

        let response = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .multipart(form)
            .send()
            .await
            .map_err(|e| LlmError::NetworkError(e.to_string()))?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            return Err(match status.as_u16() {
                401 => LlmError::AuthenticationError(error_body),
                429 => LlmError::RateLimitExceeded(error_body),
                400 => LlmError::InvalidResponse(format!("Bad request: {error_body}")),
                _ => LlmError::ApiError(format!("HTTP {status}: {error_body}")),
            });
        }

        let response_body = response.text().await.map_err(|e| {
            LlmError::DeserializationError(format!("Failed to read response body: {e}"))
        })?;

        serde_json::from_str::<WhisperResponse>(&response_body).map_err(|e| {
            LlmError::DeserializationError(format!(
                "Failed to parse Whisper response: {e}. Raw body: {response_body}"
            ))
        })
    }

    /// Build a `reqwest::multipart::Form` for a Whisper transcription request.
    fn build_transcription_form(
        &self,
        audio: &[u8],
        format: &str,
        language_hint: Option<&str>,
        prompt_hint: Option<&str>,
    ) -> LlmResult<reqwest::multipart::Form> {
        let mime = audio_mime_type(format);
        let filename = format!("audio.{format}");

        let file_part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(filename)
            .mime_str(mime)
            .map_err(|e| {
                LlmError::ConfigError(format!("Failed to set MIME type on multipart part: {e}"))
            })?;

        let mut form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .text("model", self.transcription_model.clone())
            .text("response_format", "verbose_json");

        if let Some(lang) = language_hint {
            form = form.text("language", lang.to_string());
        }
        if let Some(prompt) = prompt_hint {
            form = form.text("prompt", prompt.to_string());
        }

        Ok(form)
    }
}

#[async_trait]
impl Transcriber for OpenAIAdapter {
    async fn transcribe_audio(
        &self,
        audio: &[u8],
        format: &str,
        language_hint: Option<&str>,
        prompt_hint: Option<&str>,
    ) -> LlmResult<TranscriptionOutput> {
        // Normalize and validate before any network I/O.
        let format_lower = format.to_ascii_lowercase();
        validate_audio_format(&format_lower)?;

        let mut last_error = LlmError::NetworkError("No attempt made".to_string());

        for attempt in 0..=self.network_retries {
            debug!(attempt, "Transcription API attempt");
            if attempt > 0 {
                let delay_ms = (1_000u64 * 2u64.saturating_pow(attempt as u32 - 1)).min(30_000);
                warn!(
                    attempt,
                    network_retries = self.network_retries,
                    delay_ms,
                    error = %last_error,
                    "Transcription request failed, retrying",
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }

            let form =
                self.build_transcription_form(audio, &format_lower, language_hint, prompt_hint)?;

            match self.call_transcription_api(form).await {
                Ok(resp) => {
                    return Ok(TranscriptionOutput {
                        text: resp.text,
                        language: resp.language,
                        duration: resp.duration,
                    });
                }
                Err(e) => {
                    // Non-retryable errors: bad request or authentication failure.
                    if matches!(
                        e,
                        LlmError::InvalidResponse(_) | LlmError::AuthenticationError(_)
                    ) {
                        return Err(e);
                    }
                    last_error = e;
                    continue;
                }
            }
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Transcription request failed after {} attempt(s): {}",
            self.network_retries + 1,
            last_error
        )))
    }

    fn transcription_model(&self) -> &str {
        &self.transcription_model
    }
}

// OpenAI API response types
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponse {
    id: String,
    object: String,
    created: i64,
    model: String,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIChoice {
    index: u32,
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIMessage {
    role: String,
    content: Option<String>,
    reasoning: Option<String>,
    function_call: Option<OpenAIFunctionCall>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
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
    fn test_openai_provider_prefix_is_stripped() {
        // litellm-style "openai/<model>" must be sent as bare "<model>".
        let adapter = OpenAIAdapter::new("openai/gpt-5-mini", "test-key", None).unwrap();
        assert_eq!(adapter.model(), "gpt-5-mini");
        // Non-openai provider prefixes (custom endpoints) are left intact.
        let adapter = OpenAIAdapter::new("ollama/llama3", "test-key", None).unwrap();
        assert_eq!(adapter.model(), "ollama/llama3");
    }

    #[test]
    fn test_openai_adapter_creation() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None);
        assert!(adapter.is_ok());

        let adapter = adapter.unwrap();
        assert_eq!(adapter.model(), "gpt-4");
        assert_eq!(adapter.base_url, OpenAIAdapter::DEFAULT_BASE_URL);
        assert_eq!(
            adapter.structured_output_retries,
            OpenAIAdapter::DEFAULT_STRUCTURED_OUTPUT_RETRIES
        );
    }

    #[test]
    fn test_configurable_structured_output_retries() {
        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None)
            .unwrap()
            .with_structured_output_retries(5);
        assert_eq!(adapter.structured_output_retries, 5);

        let adapter = OpenAIAdapter::new("gpt-4", "test-key", None)
            .unwrap()
            .with_structured_output_retries(0);
        assert_eq!(adapter.structured_output_retries, 1);
    }

    #[test]
    fn test_openai_adapter_custom_base_url() {
        let adapter = OpenAIAdapter::new(
            "gpt-4",
            "test-key",
            Some("https://custom.api.com/v1".to_string()),
        );
        assert!(adapter.is_ok());

        let adapter = adapter.unwrap();
        assert_eq!(adapter.base_url, "https://custom.api.com/v1");
    }

    #[test]
    fn test_base_url_trailing_slash_is_normalized() {
        // The Gemini OpenAI-compat base ends in `/`; without normalisation the
        // request URL would be `.../openai//chat/completions` and 404.
        let adapter = OpenAIAdapter::new(
            "gemini-2.0-flash",
            "test-key",
            Some("https://generativelanguage.googleapis.com/v1beta/openai/".to_string()),
        )
        .unwrap();
        assert_eq!(
            adapter.base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
    }

    #[test]
    fn test_is_reasoning_model_matches_openai_families() {
        let cases = [
            ("gpt-5", true),
            ("gpt-5-mini", true),
            ("gpt-5-2025-06-01", true),
            ("o1", true),
            ("o1-mini", true),
            ("o3", true),
            ("o3-mini", true),
            ("o4-mini", true),
            ("GPT-5-Mini", true),
            ("gpt-4o-mini", false),
            ("gpt-4-turbo", false),
            ("gpt-3.5-turbo", false),
            ("o-foo", false),
        ];
        for (model, expected) in cases {
            let adapter = OpenAIAdapter::new(model, "test-key", None).unwrap();
            assert_eq!(
                adapter.is_reasoning_model(),
                expected,
                "is_reasoning_model({model})"
            );
        }
    }

    #[test]
    fn test_is_reasoning_model_skipped_for_custom_base_url() {
        // Custom OpenAI-compatible endpoints (Ollama, vLLM, …) may have
        // model names that look like reasoning families but still accept
        // legacy sampling parameters — the gate is conservative.
        let adapter = OpenAIAdapter::new(
            "gpt-5-mini",
            "test-key",
            Some("http://localhost:11434/v1".to_string()),
        )
        .unwrap();
        assert!(!adapter.is_reasoning_model());
    }

    #[test]
    fn test_write_max_tokens_renames_key_for_reasoning_models() {
        let mut body = json!({"model": "gpt-5-mini"});
        let reasoning = OpenAIAdapter::new("gpt-5-mini", "test-key", None).unwrap();
        reasoning.write_max_tokens(&mut body, Some(2048));
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 2048);

        let mut body = json!({"model": "gpt-4o-mini"});
        let classic = OpenAIAdapter::new("gpt-4o-mini", "test-key", None).unwrap();
        classic.write_max_tokens(&mut body, Some(2048));
        assert_eq!(body["max_tokens"], 2048);
        assert!(body.get("max_completion_tokens").is_none());

        // None leaves body untouched.
        let mut body = json!({"model": "gpt-5-mini"});
        reasoning.write_max_tokens(&mut body, None);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn test_message_conversion() {
        let messages = vec![
            Message {
                role: MessageRole::System,
                content: "You are helpful".to_string(),
            },
            Message {
                role: MessageRole::User,
                content: "Hello".to_string(),
            },
        ];

        let converted = OpenAIAdapter::convert_messages(&messages);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0]["role"], "system");
        assert_eq!(converted[0]["content"], "You are helpful");
        assert_eq!(converted[1]["role"], "user");
        assert_eq!(converted[1]["content"], "Hello");
    }

    #[test]
    fn test_context_length() {
        let adapter = OpenAIAdapter::new("gpt-4-turbo-preview", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 128_000);

        let adapter = OpenAIAdapter::new("gpt-4", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 8_192);

        let adapter = OpenAIAdapter::new("gpt-3.5-turbo-16k", "key", None).unwrap();
        assert_eq!(adapter.max_context_length(), 16_384);
    }

    #[test]
    fn test_supports_vision_gpt4o() {
        let adapter = OpenAIAdapter::new("gpt-4o", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gpt4_turbo() {
        let adapter = OpenAIAdapter::new("gpt-4-turbo", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gpt4o_mini() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gpt35_is_false() {
        let adapter = OpenAIAdapter::new("gpt-3.5-turbo", "key", None).unwrap();
        assert!(!adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_llava() {
        let adapter = OpenAIAdapter::new("llava:13b", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_o1() {
        let adapter = OpenAIAdapter::new("o1-preview", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[test]
    fn test_supports_vision_gemma3() {
        let adapter = OpenAIAdapter::new("gemma3:12b", "key", None).unwrap();
        assert!(adapter.supports_vision());
    }

    #[tokio::test]
    async fn transcribe_image_rejects_non_image_mime() {
        let adapter = OpenAIAdapter::new("gpt-4o", "fake-key", None).unwrap();
        let result = adapter
            .transcribe_image(b"not-an-image", "text/plain", None)
            .await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), LlmError::InvalidResponse(_)),
            "Expected InvalidResponse for non-image MIME type"
        );
    }

    #[test]
    fn test_transcription_model_default() {
        // Clear the env var to test the default value.
        // SAFETY: This test is single-threaded and no other thread reads
        // TRANSCRIPTION_MODEL concurrently.
        unsafe { std::env::remove_var("TRANSCRIPTION_MODEL") };
        let adapter = OpenAIAdapter::new("gpt-4", "key", None).unwrap();
        assert_eq!(adapter.transcription_model(), "whisper-1");
    }

    #[test]
    fn test_transcription_model_custom() {
        let adapter = OpenAIAdapter::new("gpt-4", "key", None)
            .unwrap()
            .with_transcription_model("whisper-large-v3");
        assert_eq!(adapter.transcription_model(), "whisper-large-v3");
    }

    #[test]
    fn test_audio_mime_type_mapping() {
        assert_eq!(audio_mime_type("mp3"), "audio/mpeg");
        assert_eq!(audio_mime_type("mpeg"), "audio/mpeg");
        assert_eq!(audio_mime_type("mpga"), "audio/mpeg");
        assert_eq!(audio_mime_type("mp4"), "audio/mp4");
        assert_eq!(audio_mime_type("m4a"), "audio/mp4");
        assert_eq!(audio_mime_type("wav"), "audio/wav");
        assert_eq!(audio_mime_type("webm"), "audio/webm");
    }

    #[test]
    fn test_to_strict_schema_marks_all_required_and_closes_objects() {
        // Mirrors the schemars-0.8 shape: an optional field omitted from
        // `required`, nested object behind `definitions`/`$ref`, and no
        // `additionalProperties` set anywhere.
        let schema = json!({
            "type": "object",
            "properties": {
                "nodes": { "type": "array", "items": { "$ref": "#/definitions/Node" } }
            },
            "required": ["nodes"],
            "definitions": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "type": { "type": "string" },
                        "description": { "type": ["string", "null"] }
                    },
                    "required": ["name", "type"]
                }
            }
        });

        let strict = to_strict_schema(&schema);

        // Root object closed + all props required.
        assert_eq!(strict["additionalProperties"], json!(false));
        assert_eq!(strict["required"], json!(["nodes"]));

        // Nested object inside definitions: every property now required
        // (including the previously-optional `description`) and closed.
        let node = &strict["definitions"]["Node"];
        assert_eq!(node["additionalProperties"], json!(false));
        let mut req: Vec<String> = node["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        req.sort();
        assert_eq!(req, vec!["description", "name", "type"]);
    }
}
