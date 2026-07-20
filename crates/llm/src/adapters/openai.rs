//! OpenAI API adapter with structured-output support.
//!
//! This adapter uses OpenAI's tool calling (`tools` + `tool_choice`) — the same
//! shape Python cognee sends via instructor/litellm — to generate structured
//! outputs based on JSON schemas derived from Rust types, falling back to legacy
//! function calling and JSON mode for older OpenAI-compatible servers.

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, instrument, warn};

#[allow(unused_imports)]
use cognee_utils::tracing_keys::{COGNEE_LLM_MODEL, COGNEE_LLM_PROVIDER};

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::{Llm, StructuredOutputValidator};
use crate::transcriber::{Transcriber, TranscriptionOutput, validate_audio_format};
use crate::types::{GenerationOptions, GenerationResponse, Message, MessageRole, TokenUsage};

/// OpenAI API adapter.
///
/// Supports structured output generation via (in fallback order):
/// - Tool calling (`tools` + forced `tool_choice`) — the primary path, matching
///   Python cognee's instructor/litellm `Mode.TOOLS`
/// - Legacy function calling (`functions` + `function_call`)
/// - JSON mode (response_format with type: "json_object")
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
    /// When `Some`, the adapter targets Azure OpenAI: requests authenticate with
    /// the `api-key` header (not `Authorization: Bearer`) and carry an
    /// `?api-version=<v>` query parameter. `None` is the standard OpenAI path.
    api_version: Option<String>,
    client: Client,
    structured_output_retries: usize,
    /// Number of times to retry the HTTP request on transient network/server errors.
    network_retries: usize,
    /// Model name for audio transcription (e.g. `"whisper-1"`).
    transcription_model: String,
    /// Extra request parameters merged into every chat-completion request body,
    /// mirroring Python cognee's `LLM_ARGS` / `llm_config.llm_args`, which the
    /// litellm adapter merges into each call as `{**self.llm_args, **kwargs}`
    /// (see `openai/adapter.py`). Keys already present on the built request body
    /// (the explicit "kwargs", e.g. `model`, `messages`, an options-supplied
    /// `max_tokens`) win, so these only ever *fill gaps*. The canonical use is
    /// `{"max_tokens": 16384}` to lift a provider's small default output cap that
    /// would otherwise truncate a dense graph-extraction tool call mid-JSON.
    /// Empty by default (Python default `llm_args = {}`) — a no-op.
    extra_args: serde_json::Map<String, Value>,
    /// Whether this adapter's `(model, base_url)` pair is an OpenAI reasoning
    /// model that needs the reasoning parameter shape. Computed once at
    /// construction (both inputs are fixed there) so the per-request build path
    /// does not re-parse `base_url` on every `is_reasoning_model()` call. See
    /// [`compute_reasoning_model`].
    reasoning_model: bool,
}

/// Whether `model` is an OpenAI reasoning family (`gpt-5*`, `o1*`, `o3*`, `o4*`)
/// served from a host that requires the reasoning parameter shape.
///
/// Detection is name-based and host-agnostic for remote hosts, so it fires on
/// both official OpenAI and Azure deployments (`*.openai.azure.com`) of these
/// models, which both require the reasoning parameter shape. (A host gate on
/// `api.openai.com` used to leave Azure o-series/gpt-5 deployments sending
/// `max_tokens`+`temperature`, which Azure 400s on every call.) It is suppressed
/// only for a local host (see [`is_local_base_url`]) — loopback, or Ollama's
/// default port on a private-network address — which keeps accepting the legacy
/// parameters even when a served model name collides with a reasoning prefix.
///
/// A self-hosted gateway on a private network but a non-Ollama port (e.g. a LAN
/// vLLM at `http://192.168.1.5:8000`) is treated as remote and gets the
/// reasoning shape; such a host is often a proxy to real OpenAI, where that
/// shape is correct. A deployment that needs the opposite can point the model at
/// a loopback bind or use Ollama's port.
fn compute_reasoning_model(model: &str, base_url: &str) -> bool {
    let m = model.to_lowercase();
    let is_reasoning_name =
        m.starts_with("gpt-5") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4");
    is_reasoning_name && !is_local_base_url(base_url)
}

/// Heuristic for a local / non-OpenAI-compatible host that does not accept the
/// reasoning-model parameter shape.
///
/// Matches on the parsed host authority, not a substring scan of the whole URL,
/// so a genuinely remote endpoint whose URL merely contains `localhost` as a
/// subdomain (`o3.localhost.example.com`) or `127.0.0.1` in a path/query is not
/// misclassified as local. Local means either:
/// - a loopback host (`localhost`, `127.0.0.0/8`, `::1`), any port; or
/// - Ollama's default port `11434` on a private-network host (loopback or an
///   RFC-1918 / link-local address). The port shortcut is gated on a private
///   host so a genuinely remote endpoint that merely listens on `11434`
///   (`https://gateway.example.com:11434`) is not classified local and still
///   gets the reasoning shape for a real o-series/gpt-5 model.
fn is_local_base_url(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        // An unparseable base_url can't be classified as local; treat it as
        // remote so a reasoning model still gets the correct parameter shape.
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    // url keeps brackets on IPv6 literals; strip them so `[::1]` parses as `::1`.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        // A named remote host (not the `localhost` label) is not local.
        return false;
    };
    if ip.is_loopback() {
        return true;
    }
    let is_private = match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        // Unique-local (`fc00::/7`) detection is not stable on IpAddr; loopback
        // (handled above) covers the realistic local-IPv6 case.
        std::net::IpAddr::V6(_) => false,
    };
    is_private && url.port() == Some(11434)
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

        // The model is used verbatim on the wire. litellm-style provider prefix
        // stripping (`openai/`, `baseten/`, …) is owned by
        // `build_openai_compatible_adapter`, which has the provider/endpoint
        // context needed to strip correctly (and to leave `custom` slugs
        // untouched). Stripping here as well would wrongly mangle real slugs
        // that legitimately contain a slash (e.g. Baseten's `openai/gpt-oss-120b`).
        let model: String = model.into();

        // Normalise a trailing slash so request URLs built as
        // `{base_url}/chat/completions` never produce a double slash. The
        // Gemini OpenAI-compat base ends in `/v1beta/openai/`, and a
        // user-supplied endpoint may too; both would otherwise 404.
        let base_url = base_url
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| Self::DEFAULT_BASE_URL.to_string());

        // Both `model` and `base_url` are fixed for the adapter's lifetime, so
        // classify once here instead of re-parsing `base_url` on every request.
        let reasoning_model = compute_reasoning_model(&model, &base_url);

        Ok(Self {
            model,
            api_key: api_key.into(),
            base_url,
            api_version: None,
            client,
            structured_output_retries: Self::DEFAULT_STRUCTURED_OUTPUT_RETRIES,
            network_retries: Self::DEFAULT_NETWORK_RETRIES,
            transcription_model,
            extra_args: serde_json::Map::new(),
            reasoning_model,
        })
    }

    /// Set extra request parameters merged into every chat-completion request,
    /// mirroring Python cognee's `LLM_ARGS` / `llm_config.llm_args`.
    ///
    /// Merge semantics match Python's `{**self.llm_args, **kwargs}`: an entry is
    /// only applied when the request body does not already carry that key, so
    /// explicitly-set parameters (model, messages, an options-supplied
    /// `max_tokens`, …) always win. See the [`extra_args`](Self::extra_args)
    /// field docs for the primary use case (lifting a provider output cap).
    pub fn with_extra_args(mut self, args: serde_json::Map<String, Value>) -> Self {
        self.extra_args = args;
        self
    }

    /// Merge [`extra_args`](Self::extra_args) into a request body, filling only
    /// keys that are not already present (explicit params win — Python parity).
    fn apply_extra_args(&self, body: &mut Value) {
        if self.extra_args.is_empty() {
            return;
        }
        // Reasoning models (`gpt-5*`/`o1*`/`o3*`/`o4*` on api.openai.com) reject
        // `max_tokens` and require `max_completion_tokens`. The request body's
        // output cap is written by `write_max_tokens` as `max_completion_tokens`,
        // so a bare `max_tokens` coming from `LLM_ARGS` here would land alongside
        // it and OpenAI rejects a request carrying *both* keys with a 400. Fold a
        // `LLM_ARGS` `max_tokens` into `max_completion_tokens` (only filling the
        // gap) so exactly one of the two keys is ever emitted.
        let reasoning = self.is_reasoning_model();
        if let Some(obj) = body.as_object_mut() {
            for (key, value) in &self.extra_args {
                if reasoning && key == "max_tokens" {
                    obj.entry("max_completion_tokens")
                        .or_insert_with(|| value.clone());
                    continue;
                }
                obj.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
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

    /// Enable Azure OpenAI mode by setting the API version. An empty/whitespace
    /// value is treated as unset (stays on the standard OpenAI path). In Azure
    /// mode the `base_url` is expected to be the deployment endpoint
    /// (`https://<resource>.openai.azure.com/openai/deployments/<deployment>`),
    /// so `{base_url}/chat/completions?api-version=<v>` is the Azure request URL.
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Self {
        let v = api_version.into();
        let trimmed = v.trim();
        // Store trimmed: `endpoint_url` interpolates this raw, so trailing
        // whitespace would percent-encode to `?api-version=...%20` and Azure
        // 400s every request. (Matches the embedding path's api-version handling.)
        self.api_version = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        self
    }

    /// Build a request URL for `path`, appending `api-version=<v>` in Azure mode.
    ///
    /// The api-version is added through `Url::query_pairs_mut`, so the query
    /// separator is chosen correctly even if `base_url` already carries a query
    /// (no malformed double-`?`) and the value is percent-encoded rather than
    /// interpolated raw. If `base_url` does not parse as a URL we fall back to a
    /// manual append that still picks `?` vs `&` from the existing query, so the
    /// api-version is never silently dropped (Azure 400s without it).
    fn endpoint_url(&self, path: &str) -> String {
        let base = format!("{}/{path}", self.base_url);
        match &self.api_version {
            Some(v) => match reqwest::Url::parse(&base) {
                Ok(mut url) => {
                    url.query_pairs_mut().append_pair("api-version", v);
                    url.into()
                }
                Err(_) => {
                    let sep = if base.contains('?') { '&' } else { '?' };
                    format!("{base}{sep}api-version={v}")
                }
            },
            None => base,
        }
    }

    /// Apply the provider's auth header: `api-key` for Azure, `Authorization:
    /// Bearer` for standard OpenAI / OpenAI-compatible endpoints.
    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_version {
            Some(_) => req.header("api-key", &self.api_key),
            None => req.header("Authorization", self.auth_header()),
        }
    }

    /// Whether to request non-thinking mode for local Qwen OpenAI-compatible endpoints.
    fn should_disable_thinking(&self) -> bool {
        self.model.to_lowercase().starts_with("qwen") && !self.base_url.contains("api.openai.com")
    }

    /// True for OpenAI reasoning-model families (`gpt-5*`, `o1*`, `o3*`, `o4*`)
    /// that reject `temperature`/`top_p`/`frequency_penalty`/`presence_penalty`
    /// overrides and require `max_completion_tokens` in place of `max_tokens`.
    ///
    /// Returns the value classified once at construction (see
    /// [`compute_reasoning_model`]); no per-call URL parsing.
    fn is_reasoning_model(&self) -> bool {
        self.reasoning_model
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
    async fn call_api(&self, mut request_body: Value) -> LlmResult<OpenAIResponse> {
        // Merge configured `LLM_ARGS` (Python `llm_config.llm_args`) into every
        // chat-completion / structured-output request. Only fills keys the request
        // does not already set, so explicit parameters win — Python's
        // `{**self.llm_args, **kwargs}`. Scoped to the chat/structured paths: the
        // transcription (vision) path calls `send_chat_request` directly so a
        // graph-extraction `LLM_ARGS` (e.g. a large `max_tokens`) never leaks into
        // an image-description request.
        self.apply_extra_args(&mut request_body);
        self.send_chat_request(request_body).await
    }

    /// Perform the actual chat-completions HTTP POST, retrying on transient
    /// network/server errors. Does *not* merge [`extra_args`](Self::extra_args) —
    /// callers that want the `LLM_ARGS` merge go through [`call_api`](Self::call_api).
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
    async fn send_chat_request(&self, request_body: Value) -> LlmResult<OpenAIResponse> {
        let url = self.endpoint_url("chat/completions");
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
                let delay = crate::retry::retry_backoff(attempt as u32);
                warn!(
                    attempt,
                    network_retries = self.network_retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %last_error,
                    "LLM request failed, retrying",
                );
                tokio::time::sleep(delay).await;
            }

            let response = match self
                .apply_auth(self.client.post(&url))
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

    /// Append a corrective instruction to the last user message of `request`,
    /// nudging the model to return a single valid object on a retry attempt.
    /// Mirrors instructor's repair prompt on a validation/parse failure.
    ///
    /// When `reason` is `Some`, it is surfaced verbatim (e.g. a serde
    /// `missing field \`type\`` message) so the model knows precisely which
    /// required field or structural constraint the previous response violated —
    /// this threads `T`'s typed validation into the repair prompt, matching how
    /// instructor feeds the validation error back to the model.
    /// Build a schema-aware validator for the type-erased raw path (which has no
    /// Rust type to deserialize into).
    ///
    /// Enforces that every property named in the schema's top-level `required`
    /// array is present and non-null. This gives the raw path the same
    /// required-field guarantee instructor provides for a typed model, *without*
    /// strict/grammar-constrained decoding (`response_format: json_schema`, which
    /// 501s on Baseten): a response omitting a required field is fed back into the
    /// existing corrective-retry loop instead of being accepted or hard-failing.
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

    /// Recompute the schema's top-level `required` array to list every property
    /// whose subschema carries no literal `"default"` key — a port of
    /// instructor's `generate_openai_schema` (`instructor/processing/schema.py`),
    /// which cognee's Python side relies on for its default `Mode.TOOLS` path.
    ///
    /// Why this matters: Python's pydantic emits no schema `"default"` for
    /// `default_factory` fields, so instructor's rewrite re-adds them to
    /// `required`, which is what makes the model reliably populate fields like
    /// `KnowledgeGraph.edges` on the *non-strict* tool-calling path. Rust/schemars
    /// already derives an equivalent `required` from the absence of
    /// `#[serde(default)]`, so for a correctly-derived schema this is a no-op; it
    /// exists to guarantee that parity holds for every response model and to keep
    /// the request byte-aligned with what litellm/instructor sends.
    ///
    /// Deliberately shallow: only the top-level `required` is recomputed. It does
    /// NOT recurse into `$defs`, set `additionalProperties:false`, or add
    /// `strict:true` — those drive grammar-constrained decoding and make Baseten's
    /// gpt-oss-120b return HTTP 501 (see the note in `structured_output_impl`).
    /// The mild top-level rewrite is verified to be accepted by Baseten.
    fn recompute_top_level_required(schema: &Value) -> Value {
        let mut schema = schema.clone();
        let Some(props) = schema.get("properties").and_then(Value::as_object) else {
            return schema;
        };
        let mut required: Vec<String> = props
            .iter()
            .filter(|(_, subschema)| subschema.get("default").is_none())
            .map(|(name, _)| name.clone())
            .collect();
        required.sort();
        if let Some(obj) = schema.as_object_mut() {
            if required.is_empty() {
                obj.remove("required");
            } else {
                obj.insert("required".to_string(), json!(required));
            }
        }
        schema
    }

    fn append_corrective_instruction(request: &mut Value, reason: Option<&str>) {
        if let Some(messages) = request["messages"].as_array_mut()
            && let Some(last_msg) = messages.last_mut()
            && last_msg["role"] == "user"
        {
            let original = last_msg["content"].as_str().unwrap_or("");
            let detail = match reason {
                Some(r) => format!("Your previous response failed validation: {r}. "),
                None => "Your previous response could not be parsed into the required structure. "
                    .to_string(),
            };
            last_msg["content"] = json!(format!(
                "{original}\n\n{detail}Call the `extract_structured_data` function again and \
                 return ONE valid object that fills in every required field, strictly matching \
                 the schema. No extra text."
            ));
        }
    }
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
        // The type-erased raw path has no Rust type to deserialize into, but it
        // must still enforce the schema's required fields (summarization's
        // custom-schema path and the HTTP structured endpoints rely on this — e.g.
        // `summarize_one` needs the `summary` field present). Synthesise a
        // schema-aware validator so an omitted required field drives the same
        // corrective retry a typed caller gets, matching instructor.
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

        // Deliberately use `send_chat_request` (not `call_api`): `LLM_ARGS`
        // (`extra_args`) are scoped to chat/structured extraction and must not
        // bleed into the image-description request.
        let response = self.send_chat_request(request_body).await?;

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

impl OpenAIAdapter {
    /// Shared implementation backing both the plain and the validated
    /// structured-output trait methods.
    ///
    /// When `validator` is `Some`, a response that parses as JSON but fails it
    /// (e.g. the model returned a well-formed object that omits a required
    /// field) is treated as a *retryable miss* — exactly like a malformed or
    /// empty payload — and re-asked with a corrective instruction carrying the
    /// validation error. This threads the caller's typed validation into the
    /// existing repair loop (the mechanism instructor uses for parity) without
    /// introducing a second, multiplying retry loop: total attempts stay bounded
    /// by `structured_output_retries` because validation reuses the same loop.
    async fn structured_output_impl(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
        validator: Option<StructuredOutputValidator<'_>>,
    ) -> LlmResult<Value> {
        // Blank = empty or whitespace-only. Kept separate from JSON *validity*
        // so a non-empty-but-invalid payload can surface a clear error instead
        // of being lumped together with "no output" (which should retry / fall
        // back to a different mode).
        let is_blank = |raw: &str| raw.trim().is_empty();

        let parse_json =
            |raw: &str| -> Result<Value, serde_json::Error> { serde_json::from_str(raw) };

        // Returns `Some(reason)` when a parsed value fails the caller's typed
        // validation (missing required field, wrong type, …), `None` otherwise
        // (including when no validator was supplied).
        let validation_error =
            |parsed: &Value| -> Option<String> { validator.and_then(|v| v(parsed).err()) };

        let opts = options.unwrap_or_default();
        // Align the advertised `required` array with what instructor sends on its
        // default TOOLS path (every non-default property is required). See
        // `recompute_top_level_required`. This is the shallow, Baseten-safe
        // rewrite — NOT the all-required/strict transform warned about below.
        let schema = Self::recompute_top_level_required(json_schema);

        // Primary path: OpenAI tool calling (`tools` + forced `tool_choice`).
        //
        // This mirrors Python cognee's request: instructor's default `Mode.TOOLS`
        // (used by `LLMGateway.acreate_structured_output`) sends the response
        // model as a single function tool and forces the model to call it,
        // passing the schema *as-is*.
        //
        // We deliberately do NOT use `response_format: {type: json_schema,
        // strict: true}` here, and we do NOT do the *heavy* strict rewrite
        // (recursively forcing every nested field required +
        // `additionalProperties:false`). Both drive grammar-constrained decoding
        // on OpenAI-compatible backends; Baseten's gpt-oss-120b returns HTTP 501
        // "Error making prediction" on such requests (verified: even the
        // recursive all-required rewrite *without* `strict:true` reproduces the
        // 501). We DO apply the shallow top-level `required` recompute
        // (`recompute_top_level_required`) — the exact shape instructor's default
        // TOOLS mode sends, verified accepted by Baseten — so the model reliably
        // returns every non-default field (e.g. `KnowledgeGraph.edges`). The
        // required-field guarantee is further backed by retrying on a
        // malformed/incomplete/validation-failing response with a corrective
        // instruction below.
        let mut tools_request = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
            "tools": [{
                "type": "function",
                "function": {
                    "name": "extract_structured_data",
                    "description": "Extract structured data from the input",
                    "parameters": schema.clone()
                }
            }],
            "tool_choice": {
                "type": "function",
                "function": {"name": "extract_structured_data"}
            }
        });

        if !self.is_reasoning_model()
            && let Some(temp) = opts.temperature
        {
            tools_request["temperature"] = json!(temp);
        }
        self.write_max_tokens(&mut tools_request, opts.max_tokens);
        if self.should_disable_thinking() {
            tools_request["think"] = json!(false);
            tools_request["reasoning"] = json!({"effort": "none"});
        }

        // Retry loop. A parseable object that also satisfies the validator
        // returns immediately. A non-empty but invalid *or* validation-failing
        // payload retries with a corrective instruction carrying the failure
        // reason (instructor parity) and, once retries are exhausted, surfaces a
        // `DeserializationError` carrying the raw payload. An empty / missing
        // tool call retries and, once exhausted, falls through to the legacy
        // function-calling / JSON-mode paths below (so servers that do not
        // support tool calling still work).
        // Last outcome of the tool-calling loop, used to decide how to proceed
        // once retries are exhausted. We distinguish a *validation miss* (the
        // server clearly speaks tool calling and returns JSON, it merely omits a
        // required field) from a *parse failure* / empty output / API error,
        // because the two want different post-loop handling (see below).
        enum ToolOutcome {
            /// No usable output yet, or the request itself errored — fall through.
            NoUsableOutput,
            /// Valid JSON that failed the caller's typed/schema validation.
            ValidationMiss { reason: String, raw: String },
            /// A non-empty payload that did not parse as JSON.
            ParseFailure,
        }
        let mut outcome = ToolOutcome::NoUsableOutput;
        // Most recent failure reason, threaded into the next corrective retry.
        let mut last_reason: Option<String> = None;
        for attempt in 0..self.structured_output_retries {
            let mut request_for_attempt = tools_request.clone();
            if attempt > 0 {
                Self::append_corrective_instruction(
                    &mut request_for_attempt,
                    last_reason.as_deref(),
                );
                if !self.is_reasoning_model() {
                    request_for_attempt["temperature"] = json!(0.0);
                }
            }

            match self.call_api(request_for_attempt).await {
                Ok(tools_response) => {
                    let choice = tools_response.choices.first().ok_or_else(|| {
                        LlmError::InvalidResponse("No choices in tool-call response".to_string())
                    })?;

                    // Prefer a modern `tool_calls[0]`, then a legacy
                    // `function_call`, then raw `content` (some servers echo the
                    // JSON directly).
                    // An empty/whitespace `arguments` string must be treated as
                    // *absent* so the `.or(content)` fallback engages — some
                    // servers emit a `tool_calls[0]` with empty arguments but put
                    // the JSON in `message.content`. Without the `filter`, the
                    // `Some("")` would shadow the real payload.
                    let non_blank = |s: &str| !s.trim().is_empty();
                    let arguments = choice
                        .message
                        .tool_calls
                        .as_ref()
                        .and_then(|calls| calls.first())
                        .map(|c| c.function.arguments.as_str())
                        .filter(|s| non_blank(s))
                        .or_else(|| {
                            choice
                                .message
                                .function_call
                                .as_ref()
                                .map(|f| f.arguments.as_str())
                                .filter(|s| non_blank(s))
                        })
                        .or(choice.message.content.as_deref())
                        .unwrap_or("");

                    if is_blank(arguments) {
                        // No usable output this attempt: retry until exhausted,
                        // then fall through to the legacy paths.
                        outcome = ToolOutcome::NoUsableOutput;
                        last_reason = None;
                        continue;
                    }

                    match parse_json(arguments) {
                        Ok(parsed) => {
                            // Valid JSON — but does it satisfy the caller's type?
                            // A missing required field is caught here and fed
                            // into the next corrective retry (instructor parity),
                            // rather than surfacing as an un-retried failure.
                            if let Some(reason) = validation_error(&parsed) {
                                debug!(
                                    attempt,
                                    structured_output_retries = self.structured_output_retries,
                                    %reason,
                                    "tool-call response parsed but failed typed validation; \
                                     retrying with corrective instruction",
                                );
                                last_reason = Some(reason.clone());
                                outcome = ToolOutcome::ValidationMiss {
                                    reason,
                                    raw: arguments.to_string(),
                                };
                                continue;
                            }
                            return Ok(parsed);
                        }
                        Err(e) => {
                            // Non-empty but invalid JSON: retry, and remember that
                            // the failure was a *parse* failure so we fall through
                            // to the legacy/JSON-mode fallbacks once exhausted.
                            last_reason = Some(e.to_string());
                            outcome = ToolOutcome::ParseFailure;
                            continue;
                        }
                    }
                }
                Err(e) => {
                    // The tool-calling request itself errored (tool calling
                    // unsupported, schema rejected, transient API/network error).
                    // Fall through to the legacy/JSON-mode fallbacks — a server
                    // may reject tool calling yet answer one of those, and those
                    // loops re-issue the request and surface any real API error
                    // via `?`. Crucially we do NOT return a stale validation/parse
                    // error here [#5]; we discard the prior miss and fall through.
                    warn!(error = %e, "tool-call request failed; falling back to legacy function/JSON mode");
                    outcome = ToolOutcome::NoUsableOutput;
                    break;
                }
            }
        }

        // Every tool-calling attempt returned valid JSON that failed the caller's
        // typed/schema validation (e.g. persistently omits a required field). The
        // server clearly speaks tool calling and returns well-formed JSON, so the
        // legacy/JSON-mode fallbacks would only re-ask the same model; surface the
        // validation error instead (instructor parity), naming the field. This is
        // deliberately NOT done for a *parse* failure or empty output [#4], which
        // fall through below in case a different request mode succeeds.
        if let ToolOutcome::ValidationMiss { reason, raw } = outcome {
            return Err(LlmError::DeserializationError(format!(
                "Tool-call arguments failed schema validation after {} attempt(s): {reason}. Raw: {raw}",
                self.structured_output_retries
            )));
        }

        // Try legacy function calling next (older OpenAI-compatible servers)
        let mut request_body = json!({
            "model": self.model,
            "messages": Self::convert_messages(&messages),
            "functions": [{
                "name": "extract_structured_data",
                "description": "Extract structured data from the input",
                "parameters": schema.clone()
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

        // Reason carried into the next attempt's corrective instruction so a
        // legacy retry is not a byte-identical re-send (which just reproduces the
        // same bad output) — it appends the failure detail and drops temperature
        // to 0, exactly like the tool-calling and JSON-mode loops.
        let mut legacy_last_reason: Option<String> = None;
        for attempt in 0..self.structured_output_retries {
            let mut request_for_attempt = request_body.clone();
            if attempt > 0 {
                Self::append_corrective_instruction(
                    &mut request_for_attempt,
                    legacy_last_reason.as_deref(),
                );
                if !self.is_reasoning_model() {
                    request_for_attempt["temperature"] = json!(0.0);
                }
            }

            let response = self.call_api(request_for_attempt).await?;

            let choice = response
                .choices
                .first()
                .ok_or_else(|| LlmError::InvalidResponse("No choices in response".to_string()))?;

            if let Some(function_call) = &choice.message.function_call {
                let last_attempt = attempt + 1 >= self.structured_output_retries;
                match parse_json(&function_call.arguments) {
                    Ok(parsed) => {
                        if let Some(reason) = validation_error(&parsed) {
                            // Valid JSON but fails the caller's type: retry, and
                            // surface the validation error once exhausted.
                            if last_attempt {
                                return Err(LlmError::DeserializationError(format!(
                                    "Function call arguments failed schema validation: {reason}. \
                                     Raw: {}",
                                    function_call.arguments
                                )));
                            }
                            legacy_last_reason = Some(reason);
                            continue;
                        }
                        return Ok(parsed);
                    }
                    Err(e) => {
                        if is_blank(&function_call.arguments) {
                            // Empty output: retry until exhausted, then fall
                            // through to JSON mode.
                            if last_attempt {
                                break;
                            }
                            legacy_last_reason = None;
                            continue;
                        }
                        // Non-empty but invalid: surface it on the last attempt,
                        // otherwise retry.
                        if last_attempt {
                            return Err(LlmError::DeserializationError(format!(
                                "Failed to deserialize function call arguments: {}. Raw: {}",
                                e, function_call.arguments
                            )));
                        }
                        legacy_last_reason = Some(e.to_string());
                        continue;
                    }
                }
            }

            break;
        }

        // Fallback to JSON mode (works with Ollama and other providers)
        let mut json_messages = Self::convert_messages(&messages);

        let example = Self::schema_to_example(&schema);

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

            let last_attempt = attempt + 1 >= self.structured_output_retries;
            match parse_json(content) {
                Ok(parsed) => {
                    if let Some(reason) = validation_error(&parsed) {
                        // Valid JSON but fails the caller's type: retry, and
                        // surface the validation error once exhausted.
                        if last_attempt {
                            return Err(LlmError::DeserializationError(format!(
                                "JSON content failed schema validation: {reason}. Raw: {content}"
                            )));
                        }
                        continue;
                    }
                    return Ok(parsed);
                }
                Err(e) => {
                    // Retry on *any* parse failure while attempts remain — an
                    // empty response OR a non-empty-but-invalid one (e.g. JSON
                    // wrapped in prose/markdown). The retry above appends a
                    // "return ONLY one valid JSON object" instruction and drops
                    // temperature to 0, so a re-ask can recover; narrowing this to
                    // blank-only [#8] would give up on a malformed-but-present
                    // payload after a single attempt.
                    if !last_attempt {
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
        let url = self.endpoint_url("audio/transcriptions");
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
            .apply_auth(self.client.post(&url))
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
                let delay = crate::retry::retry_backoff(attempt as u32);
                warn!(
                    attempt,
                    network_retries = self.network_retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %last_error,
                    "Transcription request failed, retrying",
                );
                tokio::time::sleep(delay).await;
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
    /// Modern tool-calling response (`tool_choice`/`tools`); the structured
    /// output is the first call's `function.arguments` JSON string.
    tool_calls: Option<Vec<OpenAIToolCall>>,
    /// Legacy `function_call` response (older OpenAI-compatible servers).
    function_call: Option<OpenAIFunctionCall>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct OpenAIToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "type")]
    call_type: Option<String>,
    /// Defaulted so a `tool_calls` entry missing its `function` object (seen on
    /// some OpenAI-compatible servers) does not fail deserialization of the whole
    /// response — the fallback chain then engages instead of erroring out.
    #[serde(default)]
    function: OpenAIFunctionCall,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct OpenAIFunctionCall {
    #[serde(default)]
    name: String,
    /// Defaulted to `""` so a `function` object without `arguments` deserializes
    /// (treated as empty → drives a retry / fallback) rather than erroring the
    /// entire `ApiResponse`.
    #[serde(default)]
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
    fn test_model_is_used_verbatim() {
        // The adapter no longer strips provider prefixes — that is owned by
        // `build_openai_compatible_adapter`. The model must reach the wire
        // exactly as constructed so real slugs containing a slash (e.g.
        // Baseten's `openai/gpt-oss-120b`) are preserved.
        let adapter = OpenAIAdapter::new("openai/gpt-oss-120b", "test-key", None).unwrap();
        assert_eq!(adapter.model(), "openai/gpt-oss-120b");
        let adapter = OpenAIAdapter::new("gpt-5-mini", "test-key", None).unwrap();
        assert_eq!(adapter.model(), "gpt-5-mini");
    }

    #[test]
    fn test_tool_call_missing_arguments_deserializes_to_empty() {
        // #7: a `tool_calls` entry whose `function` lacks `arguments` must not
        // fail deserialization of the whole response — it defaults to "" so the
        // fallback chain engages.
        let raw = r#"{
            "id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{"index":0,"message":{"role":"assistant","tool_calls":[
                {"id":"c1","type":"function","function":{"name":"extract_structured_data"}}
            ]},"finish_reason":"tool_calls"}]
        }"#;
        let resp: OpenAIResponse =
            serde_json::from_str(raw).expect("missing arguments should default, not error");
        let call = &resp.choices[0].message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.function.arguments, "");
    }

    #[test]
    fn test_tool_call_missing_function_deserializes() {
        // #7: a `tool_calls` entry with no `function` object at all must also
        // deserialize (defaulted) rather than erroring the whole `ApiResponse`.
        let raw = r#"{
            "id":"x","object":"chat.completion","created":1,"model":"m",
            "choices":[{"index":0,"message":{"role":"assistant","tool_calls":[
                {"id":"c1","type":"function"}
            ]},"finish_reason":"tool_calls"}]
        }"#;
        let resp: OpenAIResponse =
            serde_json::from_str(raw).expect("missing function should default, not error");
        let call = &resp.choices[0].message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.function.name, "");
        assert_eq!(call.function.arguments, "");
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
    fn openai_mode_has_no_api_version_and_bearer_url() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "sk-test", None).unwrap();
        assert!(adapter.api_version.is_none());
        // No api-version query on the standard OpenAI path.
        assert_eq!(
            adapter.endpoint_url("chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn with_api_version_enables_azure_mode_and_query() {
        let adapter = OpenAIAdapter::new(
            "gpt-4o-mini",
            "sk-test",
            Some("https://res.openai.azure.com/openai/deployments/gpt-4o-mini".to_string()),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview");
        assert_eq!(adapter.api_version.as_deref(), Some("2024-12-01-preview"));
        assert_eq!(
            adapter.endpoint_url("chat/completions"),
            "https://res.openai.azure.com/openai/deployments/gpt-4o-mini/chat/completions?api-version=2024-12-01-preview"
        );
    }

    #[test]
    fn endpoint_url_hardens_against_double_query_and_encodes_api_version() {
        // base_url already carrying a query must not yield a malformed double-`?`;
        // the api-version is appended with `&`.
        let with_query = OpenAIAdapter::new(
            "gpt-4o-mini",
            "sk-test",
            Some("https://res.openai.azure.com/openai/deployments/gpt-4o-mini?foo=bar".to_string()),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview");
        let url = with_query.endpoint_url("chat/completions");
        assert_eq!(url.matches('?').count(), 1, "exactly one '?': {url}");
        assert!(url.contains("foo=bar"), "existing query preserved: {url}");
        assert!(
            url.contains("api-version=2024-12-01-preview"),
            "api-version appended: {url}"
        );

        // A value with a reserved character is percent-encoded, not interpolated raw.
        let odd = OpenAIAdapter::new(
            "gpt-4o-mini",
            "sk-test",
            Some("https://res.openai.azure.com/openai/deployments/gpt-4o-mini".to_string()),
        )
        .unwrap()
        .with_api_version("2024 preview&x=1");
        let url = odd.endpoint_url("chat/completions");
        assert!(
            !url.contains("api-version=2024 preview&x=1"),
            "raw value must not appear unencoded: {url}"
        );
        assert!(url.contains("api-version="), "api-version present: {url}");
    }

    #[test]
    fn with_api_version_empty_stays_openai_mode() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "sk-test", None)
            .unwrap()
            .with_api_version("   ");
        assert!(adapter.api_version.is_none());
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
    fn is_reasoning_model_detected_on_azure_and_remote_gateways() {
        // The bug this fixes: a host gate on api.openai.com left Azure o-series /
        // gpt-5 deployments sending max_tokens + temperature, which Azure 400s on
        // every call. Detection is now name-based, so the Azure deployment matches.
        let azure = OpenAIAdapter::new(
            "o3-mini",
            "sk-test",
            Some("https://my-resource.openai.azure.com/openai/deployments/o3".to_string()),
        )
        .unwrap()
        .with_api_version("2024-12-01-preview");
        assert!(azure.is_reasoning_model());

        // A remote (non-local) OpenAI-compatible gateway serving a reasoning
        // model is detected too, since detection is host-agnostic.
        let gateway = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("https://gateway.example.com/v1".to_string()),
        )
        .unwrap();
        assert!(gateway.is_reasoning_model());

        // Regression: the old substring scan misclassified a genuinely remote
        // host as local when the URL merely contained a loopback token. A
        // `localhost` subdomain label must NOT suppress detection.
        let subdomain = OpenAIAdapter::new(
            "o3-mini",
            "sk-test",
            Some("https://o3.localhost.example.com/v1".to_string()),
        )
        .unwrap();
        assert!(subdomain.is_reasoning_model());
        // `127.0.0.1` appearing only in a path must NOT suppress detection.
        let path_ip = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("https://gateway.example.com/proxy/127.0.0.1/v1".to_string()),
        )
        .unwrap();
        assert!(path_ip.is_reasoning_model());
        // An actual loopback host is still suppressed (local runtimes reject the shape).
        let loopback = OpenAIAdapter::new(
            "o3-mini",
            "sk-test",
            Some("http://127.0.0.1:8080/v1".to_string()),
        )
        .unwrap();
        assert!(!loopback.is_reasoning_model());

        // A genuinely remote endpoint that merely listens on Ollama's default
        // port 11434 is NOT local: a real reasoning model there still gets the
        // reasoning shape. (The port shortcut is gated on a private host.)
        let remote_11434 = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("https://gateway.example.com:11434/v1".to_string()),
        )
        .unwrap();
        assert!(remote_11434.is_reasoning_model());

        // Ollama on a private-network host (RFC-1918 + port 11434) is local, so
        // a name-colliding model is suppressed.
        let lan_ollama = OpenAIAdapter::new(
            "o3-mini",
            "sk-test",
            Some("http://192.168.1.5:11434/v1".to_string()),
        )
        .unwrap();
        assert!(!lan_ollama.is_reasoning_model());

        // A private-network host on a non-Ollama port is treated as remote (often
        // a proxy to real OpenAI), so the reasoning shape applies.
        let lan_gateway = OpenAIAdapter::new(
            "gpt-5",
            "sk-test",
            Some("http://192.168.1.5:8000/v1".to_string()),
        )
        .unwrap();
        assert!(lan_gateway.is_reasoning_model());
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
    fn test_apply_extra_args_fills_missing_keys_only() {
        // Mirrors Python's `{**self.llm_args, **kwargs}`: llm_args fill gaps,
        // explicitly-set request params win.
        let args = json!({"max_tokens": 16384, "top_p": 0.9})
            .as_object()
            .unwrap()
            .clone();
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", None)
            .unwrap()
            .with_extra_args(args);

        // `max_tokens` absent → filled from extra_args; existing `temperature`
        // untouched (not in extra_args); `top_p` filled.
        let mut body = json!({"model": "gpt-4o-mini", "temperature": 0.0});
        adapter.apply_extra_args(&mut body);
        assert_eq!(body["max_tokens"], 16384);
        assert_eq!(body["top_p"], 0.9);
        assert_eq!(body["temperature"], 0.0);

        // An explicitly-set key is NOT overwritten by extra_args.
        let mut body = json!({"model": "gpt-4o-mini", "max_tokens": 512});
        adapter.apply_extra_args(&mut body);
        assert_eq!(body["max_tokens"], 512);
    }

    #[test]
    fn test_apply_extra_args_translates_max_tokens_for_reasoning_models() {
        // #1: `write_max_tokens` emits `max_completion_tokens` for a reasoning
        // model; a bare `LLM_ARGS` `max_tokens` must be folded into
        // `max_completion_tokens` (never sent alongside it), or OpenAI 400s on
        // both keys.
        let args = json!({"max_tokens": 16384}).as_object().unwrap().clone();
        let reasoning = OpenAIAdapter::new("gpt-5-mini", "test-key", None)
            .unwrap()
            .with_extra_args(args.clone());

        // Body already carries `max_completion_tokens` (from write_max_tokens):
        // the extra `max_tokens` must NOT be added, and no bare `max_tokens` key.
        let mut body = json!({"model": "gpt-5-mini", "max_completion_tokens": 2048});
        reasoning.apply_extra_args(&mut body);
        assert!(
            body.get("max_tokens").is_none(),
            "reasoning model must never carry a bare max_tokens"
        );
        assert_eq!(
            body["max_completion_tokens"], 2048,
            "explicit max_completion_tokens must win over LLM_ARGS"
        );

        // Body has no output cap yet: the LLM_ARGS max_tokens fills
        // max_completion_tokens (translated), still no bare max_tokens.
        let mut body = json!({"model": "gpt-5-mini"});
        reasoning.apply_extra_args(&mut body);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 16384);

        // A classic (non-reasoning) model keeps the bare max_tokens.
        let classic = OpenAIAdapter::new("gpt-4o-mini", "test-key", None)
            .unwrap()
            .with_extra_args(args);
        let mut body = json!({"model": "gpt-4o-mini"});
        classic.apply_extra_args(&mut body);
        assert_eq!(body["max_tokens"], 16384);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn test_schema_required_validator_enforces_required_fields() {
        // #3: the raw path synthesises a schema-aware validator so an omitted
        // required field is a retryable miss (not silently accepted).
        let schema = json!({
            "type": "object",
            "required": ["summary"],
            "properties": {"summary": {"type": "string"}}
        });
        let validate = OpenAIAdapter::schema_required_validator(&schema);
        assert!(validate(&json!({"summary": "hello"})).is_ok());
        assert!(validate(&json!({"other": "x"})).is_err());
        assert!(validate(&json!({"summary": null})).is_err());

        // No `required` array → nothing to enforce.
        let loose = json!({"type": "object"});
        let validate = OpenAIAdapter::schema_required_validator(&loose);
        assert!(validate(&json!({})).is_ok());
    }

    #[test]
    fn test_apply_extra_args_empty_is_noop() {
        let adapter = OpenAIAdapter::new("gpt-4o-mini", "test-key", None).unwrap();
        let mut body = json!({"model": "gpt-4o-mini"});
        let before = body.clone();
        adapter.apply_extra_args(&mut body);
        assert_eq!(body, before);
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
}
