//! AWS Bedrock provider (feature `bedrock`).
//!
//! [`BedrockAdapter`] implements [`Llm`] against the Bedrock **Converse** API,
//! `POST {endpoint}/model/{modelId}/converse`. The shared AWS plumbing — env
//! resolution, the region and endpoint chains, the credential ladder, SigV4
//! signing and the transport seam — lives in [`aws`]; the four modules beside
//! it carry the chat-side wire spec:
//!
//! | Module | Plan section | What it decides |
//! |---|---|---|
//! | [`model_id`] | §1.4.1 | normalisation, which runs **before** routing |
//! | [`route`] | §1.4.2 | converse vs invoke (and the explicit route prefixes) |
//! | [`caps`] | §1.4.3 / §1.0 | per-model capabilities and the output-token cap |
//! | [`converse`] | §1.4.2 / §1.4.3 | the request/response transforms |
//!
//! # Two rules that fail silently if forgotten
//!
//! 1. **Normalisation feeds routing and the capability lookup only — the
//!    request URL keeps the original id.** Every model cognee ships is
//!    `eu.`-prefixed while litellm's converse table stores bare ids, so
//!    skipping normalisation routes all three defaults to `invoke`
//!    (`model_id`).
//! 2. **Structured output is capability-gated, not hard-coded.** The synthetic
//!    `json_tool_call` tool is the *fallback*; both Anthropic ids cognee ships
//!    take Converse's native `outputConfig.textFormat` branch, and
//!    `amazon.nova-lite-v1:0` takes the tool branch **without** a forced
//!    `toolChoice` (`caps` + `converse::apply_structured_output`).
//!
//! # Scope
//!
//! * **No streaming** (plan §1.6): [`Llm::supports_streaming`] returns `false`.
//! * **No `/invoke` chat** (plan §6.7): an invoke-routed chat model is rejected
//!   at construction with [`LlmError::FeatureNotSupported`]. No model cognee
//!   ships routes there, and the legacy per-family transforms were the largest
//!   lump of code in the original plan.
//! * **Vision exceeds Python parity, on purpose** (plan §6.5):
//!   [`Llm::transcribe_image`] is implemented with Converse image blocks, while
//!   Python's `BedrockAdapter.transcribe_image` raises `NotImplementedError`.
//!   Plan P6 is the optional path to closing that gap from the Python side.

pub mod aws;
pub mod caps;
pub mod converse;
pub mod model_id;
pub mod route;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use tracing::{debug, instrument, warn};

use self::aws::env::AwsInputs;
use self::aws::transport::{BedrockTransport, ReqwestBedrockTransport};
use self::caps::ModelCaps;
use self::converse::{ConverseResponse, RESPONSE_FORMAT_TOOL_NAME};
use self::route::BedrockRoute;
use crate::error::{LlmError, LlmResult};
use crate::llm_trait::{Llm, StructuredOutputValidator};
use crate::types::{GenerationOptions, GenerationResponse, Message, TokenUsage};

/// Native adapter for the AWS Bedrock Converse API.
///
/// Built by `cognee-components`' Bedrock factory (plan §4 R5); see the module
/// docs for the wire spec it implements.
pub struct BedrockAdapter {
    /// The model id **exactly as configured**. This is what goes in the request
    /// URL — cross-region prefix, ARN wrapper and suffixes included.
    model: String,
    /// The §1.4.1-normalised id. Routing and the capability lookup key on this;
    /// nothing on the wire does.
    base_model: String,
    /// Capabilities resolved once from [`base_model`](Self::base_model).
    caps: ModelCaps,
    /// Resolved runtime endpoint, without a trailing slash.
    endpoint: String,
    /// Resolved AWS region — kept for diagnostics; the transport holds its own
    /// copy for signing.
    region: String,
    /// The §3 transport seam. `pub(crate)` by design, so it never appears in a
    /// public signature of this adapter.
    transport: Arc<dyn BedrockTransport>,
    structured_output_retries: usize,
    network_retries: usize,
    /// Output-token ceiling (Python's `llm_max_completion_tokens`). The
    /// per-request `inferenceConfig.maxTokens` is `min(this, the model cap)`.
    max_completion_tokens: u32,
    /// `LLM_ARGS`, merged into `additionalModelRequestFields`.
    extra_args: Map<String, Value>,
}

impl BedrockAdapter {
    /// Default structured-output repair retries (Python instructor parity: 5).
    pub const DEFAULT_STRUCTURED_OUTPUT_RETRIES: usize = 5;
    /// Default transient-network retries.
    pub const DEFAULT_NETWORK_RETRIES: usize = 3;
    /// Default output-token ceiling, aliasing the crate-wide
    /// [`crate::DEFAULT_MAX_COMPLETION_TOKENS`] so it moves in lockstep with the
    /// config and `GenerationOptions` defaults.
    pub const DEFAULT_MAX_COMPLETION_TOKENS: u32 = crate::DEFAULT_MAX_COMPLETION_TOKENS;
    /// Request timeout, matching the other adapters.
    const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

    /// Build an adapter for `model`.
    ///
    /// * `api_key` is the Bedrock API key (`LLM_API_KEY`). When set it
    ///   short-circuits to `Authorization: Bearer …` with **no** SigV4 and no
    ///   credential lookup at all (plan §1.2); when unset, the credential ladder
    ///   runs. Bedrock is exempt from cognee's API-key requirement (§1.1), so
    ///   `None` is a supported configuration, not an error.
    /// * `api_base` is the highest rung of the §1.3 endpoint chain. Pass `None`
    ///   to let `AWS_BEDROCK_RUNTIME_ENDPOINT` / the regional default decide —
    ///   in particular do **not** pass `LlmInputs::endpoint` through, which
    ///   aliases `OPENAI_URL` (the same trap `anthropic_base_url` exists to
    ///   avoid).
    /// * `aws` carries the §2.1 env-resolved AWS inputs.
    ///
    /// Fails when the model does not route to Converse (plan §6.7), or when the
    /// region / credential chains cannot resolve.
    pub async fn new(
        model: impl Into<String>,
        api_key: Option<&str>,
        api_base: Option<&str>,
        aws: &AwsInputs,
    ) -> LlmResult<Self> {
        let model: String = model.into();

        // Route first: an invoke-routed chat model must fail loudly here rather
        // than POST a Converse body to an endpoint that cannot serve it.
        let route = route::select_route(&model);
        if route != BedrockRoute::Converse {
            return Err(LlmError::FeatureNotSupported(format!(
                "Bedrock model {model:?} routes to `{}`, and this adapter only implements the \
                 Converse API. No model cognee ships routes elsewhere; see \
                 docs/roadmap/bedrock-provider-plan.md §6.7.",
                route.as_str()
            )));
        }

        let settings = aws.resolve();
        let region = aws::region::resolve_region(&settings, Some(&model)).await?;
        let endpoint = aws::endpoint::resolve_endpoint(api_base, &settings, &region);
        let auth = aws::credentials::resolve_auth(api_key, &settings, &region).await?;

        let client = reqwest::Client::builder()
            .timeout(Self::REQUEST_TIMEOUT)
            .build()
            .map_err(|e| LlmError::ConfigError(format!("Failed to create HTTP client: {e}")))?;
        let transport = Arc::new(ReqwestBedrockTransport::new(client, auth, region.clone()));

        let base_model = model_id::base_model(&model);
        let caps = caps::caps_for_base_model(&base_model);
        debug!(
            model = model.as_str(),
            base_model = base_model.as_str(),
            region = region.as_str(),
            endpoint = endpoint.as_str(),
            native_structured_output = caps.supports_native_structured_output,
            supports_tool_choice = caps.supports_tool_choice,
            "built Bedrock Converse adapter",
        );

        Ok(Self {
            model,
            base_model,
            caps,
            endpoint,
            region,
            transport,
            structured_output_retries: Self::DEFAULT_STRUCTURED_OUTPUT_RETRIES,
            network_retries: Self::DEFAULT_NETWORK_RETRIES,
            max_completion_tokens: Self::DEFAULT_MAX_COMPLETION_TOKENS,
            extra_args: Map::new(),
        })
    }

    /// Configure structured-output repair retries (floored at 1).
    pub fn with_structured_output_retries(mut self, retries: u32) -> Self {
        self.structured_output_retries = usize::try_from(retries).unwrap_or(usize::MAX).max(1);
        self
    }

    /// Configure transient network/server retry attempts.
    pub fn with_network_retries(mut self, retries: u32) -> Self {
        self.network_retries = usize::try_from(retries).unwrap_or(usize::MAX);
        self
    }

    /// Set the output-token ceiling (`llm_max_completion_tokens`). The
    /// per-request `maxTokens` is still clamped to the model's documented cap.
    pub fn with_max_completion_tokens(mut self, ceiling: u32) -> Self {
        self.max_completion_tokens = ceiling;
        self
    }

    /// Set `LLM_ARGS`, merged into `additionalModelRequestFields`. Explicit
    /// keys the adapter sets always win (litellm's `{**llm_args, **kwargs}`).
    pub fn with_extra_args(mut self, args: Map<String, Value>) -> Self {
        self.extra_args = args;
        self
    }

    /// The §1.4.1-normalised model id used for routing and the capability
    /// lookup. Exposed for diagnostics and tests; never sent on the wire.
    pub fn base_model(&self) -> &str {
        &self.base_model
    }

    /// The resolved AWS region.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// The resolved runtime endpoint.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Capabilities resolved for this model.
    pub fn caps(&self) -> ModelCaps {
        self.caps
    }

    /// The `inferenceConfig.maxTokens` to send: `min(caller value, configured
    /// ceiling, model cap)`, floored at 1 (plan §1.0).
    ///
    /// The configured ceiling bounds **every** path, not only the
    /// `max_tokens: None` one: `GenerationOptions::default()` carries
    /// `Some(16384)`, so a default-options caller would otherwise silently
    /// bypass a lower operator-configured ceiling. The model cap is applied last
    /// so Bedrock never 400s on `maxTokens > model limit`.
    fn effective_max_tokens(&self, opts: &GenerationOptions) -> u32 {
        let requested = opts.max_tokens.map_or(self.max_completion_tokens, |value| {
            value.min(self.max_completion_tokens)
        });
        requested.min(self.caps.max_output_tokens).max(1)
    }

    /// The output budget a truncation retry may raise to: the lesser of the
    /// model cap and the configured ceiling.
    fn effective_output_cap(&self) -> u32 {
        self.caps
            .max_output_tokens
            .min(self.max_completion_tokens)
            .max(1)
    }

    /// Build the Converse body shared by completion, structured output and the
    /// repair loop.
    fn base_request(&self, messages: &[Message], opts: &GenerationOptions) -> Value {
        let (system, turns) = converse::split_messages(messages);
        // Converse requires at least one user/assistant turn. When the caller
        // passes only system messages, hoisting leaves `turns` empty and the API
        // 400s on an empty `messages` array — so fold the system blocks into a
        // single user turn instead, the way the Anthropic adapter does.
        let (system, turns) = if turns.is_empty() && !system.is_empty() {
            (
                Vec::new(),
                vec![json!({ "role": "user", "content": system })],
            )
        } else {
            (system, turns)
        };

        let mut body = json!({ "messages": turns });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        body["inferenceConfig"] = converse::inference_config(opts, self.effective_max_tokens(opts));
        converse::merge_additional_model_request_fields(
            &mut body,
            &self.extra_args,
            &converse::penalty_model_fields(opts),
        );
        body
    }

    /// POST `request_body` to the Converse endpoint with a transient-retry
    /// ladder and exponential backoff.
    #[instrument(
        name = "llm.api_call",
        level = "info",
        skip(self, request_body),
        fields(
            url = tracing::field::Empty,
            cognee.llm.model = self.model.as_str(),
            cognee.llm.provider = "bedrock",
        ),
    )]
    async fn call_converse(&self, request_body: &Value) -> LlmResult<ConverseResponse> {
        let url = converse::converse_url(&self.endpoint, &self.model);
        tracing::Span::current().record("url", url.as_str());

        let debug_enabled = std::env::var("COGNEE_DEBUG_LLM_REQUEST")
            .map(|value| cognee_utils::parse_env_bool(&value))
            .unwrap_or(false);
        if debug_enabled {
            let pretty = serde_json::to_string_pretty(request_body)
                .unwrap_or_else(|_| request_body.to_string());
            eprintln!("\n[COGNEE_DEBUG_LLM_REQUEST] POST {url}\n{pretty}\n");
        }

        let payload = serde_json::to_vec(request_body).map_err(|e| {
            LlmError::SerializationError(format!("Failed to serialize Converse request: {e}"))
        })?;

        let mut last_error = LlmError::NetworkError("No attempt made".to_string());

        for attempt in 0..=self.network_retries {
            if attempt > 0 {
                // Shared jittered backoff (issue #19): a batch of concurrent
                // requests that all throttle at once must not retry in lockstep.
                let delay = crate::retry::retry_backoff(attempt as u32);
                warn!(
                    attempt,
                    network_retries = self.network_retries,
                    delay_ms = delay.as_millis() as u64,
                    error = %last_error,
                    "Bedrock request failed, retrying",
                );
                tokio::time::sleep(delay).await;
            }

            let response = match self.transport.post_json(&url, payload.clone()).await {
                Ok(response) => response,
                Err(error) => {
                    if !converse::is_retryable(&error) {
                        return Err(error);
                    }
                    last_error = error;
                    continue;
                }
            };

            if !response.status.is_success() {
                let error = converse::map_error(response.status.as_u16(), &response.body_lossy());
                // Terminal: auth, unknown model, and a ValidationException —
                // re-POSTing the identical body cannot start working.
                if !converse::is_retryable(&error) {
                    return Err(error);
                }
                last_error = error;
                continue;
            }

            let body = response.body_lossy().into_owned();
            if debug_enabled {
                eprintln!("\n[COGNEE_DEBUG_LLM_RESPONSE] POST {url}\n{body}\n");
            }
            return serde_json::from_str::<ConverseResponse>(&body).map_err(|e| {
                LlmError::DeserializationError(format!(
                    "Failed to parse Converse response: {e}. Raw body: {body}"
                ))
            });
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Bedrock request failed after {} attempt(s): {}",
            self.network_retries + 1,
            last_error
        )))
    }

    /// Shared structured-output loop with instructor-style corrective retries.
    ///
    /// A re-implementation of the Anthropic repair loop's *behaviour* over
    /// Converse's JSON (`stopReason`, `toolUse.input`, native text format) —
    /// plan §6.7 makes clear that loop is a pattern, not shared code. Invalid,
    /// empty or validator-rejected output triggers a corrective re-ask inside
    /// the same retry budget, with a backoff between re-asks; terminal provider
    /// errors short-circuit instead of burning it.
    async fn structured_output_impl(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
        validator: Option<StructuredOutputValidator<'_>>,
    ) -> LlmResult<Value> {
        let opts = options.unwrap_or_default();
        let mut body = self.base_request(&messages, &opts);
        // §1.4.3: the branch is read from the capability table, never hard-coded.
        converse::apply_structured_output(&mut body, json_schema, &self.caps);

        let mut last_error =
            LlmError::InvalidResponse("No structured-output attempt made".to_string());

        for attempt in 0..self.structured_output_retries {
            if attempt > 0 {
                // `call_converse`'s ladder only covers transport retries inside
                // a single attempt, so without this the outer loop would re-ask
                // immediately (Python waits between structured retries via
                // `wait_exponential_jitter`).
                let delay = crate::retry::retry_backoff(attempt as u32);
                debug!(
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    "retrying Bedrock structured output",
                );
                tokio::time::sleep(delay).await;
            }

            match self.call_converse(&body).await {
                Ok(response) => {
                    let truncated = response.is_truncated();
                    match response.structured_payload(&self.caps) {
                        Some(_) if truncated => {
                            // Cut off at maxTokens: present and JSON-parseable,
                            // but incomplete. Matching the Python reference
                            // (instructor), a length-truncated structured
                            // response is rejected outright rather than returned
                            // as a partial object — a shallow top-level check
                            // cannot tell a complete object that happened to
                            // finish at the budget from one whose nested
                            // list/string was cut off. Re-asking with the SAME
                            // budget would truncate at the same point, so raise
                            // it toward the effective output budget. That budget
                            // is the model cap bounded by the configured
                            // `llm_max_completion_tokens` ceiling, which is an
                            // upper bound on every path — so when we are already
                            // at it, fail terminally rather than loop until
                            // MaxRetriesExceeded.
                            let cap = self.effective_output_cap();
                            let current =
                                body["inferenceConfig"]["maxTokens"].as_u64().unwrap_or(0) as u32;
                            if current >= cap {
                                return Err(LlmError::InvalidResponse(format!(
                                    "Bedrock structured output was truncated at the effective \
                                     {cap}-token output budget (the lesser of the \
                                     llm_max_completion_tokens ceiling and the model cap) and \
                                     cannot be completed within that budget"
                                )));
                            }
                            body["inferenceConfig"]["maxTokens"] = json!(cap);
                            let reason = "the previous answer was cut off at maxTokens before the \
                                          object was complete";
                            last_error = LlmError::InvalidResponse(format!(
                                "Bedrock structured output truncated: {reason}"
                            ));
                            converse::append_corrective_instruction(
                                &mut body,
                                Some(reason),
                                self.caps.supports_native_structured_output,
                            );
                        }
                        Some(payload) => match validator.map(|validate| validate(&payload)) {
                            None | Some(Ok(())) => return Ok(payload),
                            Some(Err(reason)) => {
                                last_error = LlmError::InvalidResponse(format!(
                                    "Bedrock structured output failed validation: {reason}"
                                ));
                                converse::append_corrective_instruction(
                                    &mut body,
                                    Some(&reason),
                                    self.caps.supports_native_structured_output,
                                );
                            }
                        },
                        None => {
                            last_error = LlmError::InvalidResponse(
                                if self.caps.supports_native_structured_output {
                                    "Bedrock response did not contain parseable JSON in its text \
                                 output"
                                        .to_string()
                                } else {
                                    format!(
                                        "Bedrock response did not contain a `{RESPONSE_FORMAT_TOOL_NAME}` toolUse block"
                                    )
                                },
                            );
                            converse::append_corrective_instruction(
                                &mut body,
                                None,
                                self.caps.supports_native_structured_output,
                            );
                        }
                    }
                }
                // Terminal: retrying cannot fix auth or an unknown model, and
                // `call_converse` has already exhausted its own transport ladder
                // when it returns MaxRetriesExceeded — re-entering it would
                // restart backoff at attempt 0 and hammer a failing endpoint.
                Err(
                    e @ (LlmError::AuthenticationError(_)
                    | LlmError::ModelNotFound(_)
                    | LlmError::ConfigError(_)
                    | LlmError::MaxRetriesExceeded(_)),
                ) => return Err(e),
                // What reaches here is a ValidationException (InvalidResponse):
                // 429/5xx/network all exhaust inside `call_converse`. Retrying it
                // is Python-faithful, but re-POSTing an identical body just fails
                // the same way — so append the reason so the next attempt
                // differs, the way instructor always reasks with changed content.
                Err(e) => {
                    converse::append_corrective_instruction(
                        &mut body,
                        Some(&e.to_string()),
                        self.caps.supports_native_structured_output,
                    );
                    last_error = e;
                }
            }
        }

        Err(LlmError::MaxRetriesExceeded(format!(
            "Bedrock structured output failed after {} attempt(s): {}",
            self.structured_output_retries, last_error
        )))
    }
}

/// Hand-written so the resolved auth (held by the transport) can never reach a
/// log line through `{:?}`; only the routing/limit decisions are shown.
impl std::fmt::Debug for BedrockAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BedrockAdapter")
            .field("model", &self.model)
            .field("base_model", &self.base_model)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("caps", &self.caps)
            .field("max_completion_tokens", &self.max_completion_tokens)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Llm for BedrockAdapter {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let opts = options.unwrap_or_default();
        let body = self.base_request(&messages, &opts);
        let response = self.call_converse(&body).await?;
        Ok(GenerationResponse {
            content: response.text(),
            // Converse echoes no model id, so report the one we addressed.
            model: self.model.clone(),
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
        // schema-aware validator (shared with the OpenAI/Anthropic adapters): a
        // payload omitting a required field drives a corrective retry instead of
        // returning `Ok` and aborting the caller at deserialization.
        let validator = crate::schema::schema_required_validator(json_schema);
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

    /// `false` — Converse streaming uses the binary `vnd.amazon.eventstream`
    /// framing, which is out of scope (plan §1.6).
    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_function_calling(&self) -> bool {
        true
    }

    fn max_context_length(&self) -> u32 {
        self.caps.max_input_tokens
    }

    fn supports_vision(&self) -> bool {
        self.caps.supports_vision
    }

    /// Describe an image via Converse image content blocks.
    ///
    /// Plan §6.5: this **exceeds** Python parity, whose
    /// `BedrockAdapter.transcribe_image` raises `NotImplementedError`. Without
    /// it, a dataset containing an image would abort a whole cognify run under
    /// `LLM_PROVIDER=bedrock`.
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
        let Some(format) = converse::image_format_for_mime(mime_type) else {
            return Err(LlmError::FeatureNotSupported(format!(
                "Bedrock Converse accepts png, jpeg, gif and webp images; got: {mime_type}"
            )));
        };
        if !self.caps.supports_vision {
            return Err(LlmError::FeatureNotSupported(format!(
                "Vision is not supported by Bedrock model: {}",
                self.model
            )));
        }

        let encoded = base64::engine::general_purpose::STANDARD.encode(image_bytes);
        // Clamp to the same effective budget as the chat path — the lesser of
        // the model's documented output cap and the configured
        // `llm_max_completion_tokens` ceiling. A caller passing
        // GenerationOptions with the default max_tokens (16384) against a model
        // that caps lower would otherwise 400, and would slip past an operator
        // ceiling that bounds every other path. Floored at 1 so a zero never
        // 400s either.
        let max_tokens = options
            .as_ref()
            .and_then(|o| o.max_tokens)
            .unwrap_or(300)
            .min(self.effective_output_cap())
            .max(1);

        // Built directly rather than via `base_request`, so `LLM_ARGS` do not
        // bleed into the description request (matching the other adapters).
        let body = json!({
            "messages": [converse::image_message(format, &encoded)],
            "inferenceConfig": { "maxTokens": max_tokens },
        });

        let response = self.call_converse(&body).await?;
        Ok(response.text())
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

    /// Offline construction: a bearer key short-circuits the credential ladder
    /// and an explicit region short-circuits the region chain, so nothing here
    /// touches the network or `~/.aws`.
    async fn adapter(model: &str) -> BedrockAdapter {
        let aws = AwsInputs {
            region: Some("eu-central-1".to_string()),
            ..AwsInputs::default()
        };
        BedrockAdapter::new(model, Some("bedrock-api-key"), None, &aws)
            .await
            .expect("adapter should build")
    }

    #[tokio::test]
    async fn invoke_routed_chat_models_are_rejected_at_construction() {
        let aws = AwsInputs {
            region: Some("us-east-1".to_string()),
            ..AwsInputs::default()
        };
        let error = BedrockAdapter::new("cohere.command-text-v14", Some("k"), None, &aws)
            .await
            .expect_err("an invoke-routed chat model is out of scope");
        assert!(
            matches!(error, LlmError::FeatureNotSupported(_)),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn max_tokens_is_clamped_to_the_model_cap() {
        // nova-lite caps output at 10_000, below the 16_384 default ceiling.
        let nova = adapter("eu.amazon.nova-lite-v1:0").await;
        assert_eq!(
            nova.effective_max_tokens(&GenerationOptions::default()),
            10_000
        );
        // Sonnet 4.5 caps at 64_000, so the default ceiling passes under it.
        let sonnet = adapter("eu.anthropic.claude-sonnet-4-5-20250929-v1:0").await;
        assert_eq!(
            sonnet.effective_max_tokens(&GenerationOptions::default()),
            16_384
        );
        // A configured ceiling below the cap wins, on the default-options path
        // too (GenerationOptions::default() carries Some(16384)).
        let capped = adapter("eu.anthropic.claude-sonnet-4-5-20250929-v1:0")
            .await
            .with_max_completion_tokens(2_000);
        assert_eq!(
            capped.effective_max_tokens(&GenerationOptions::default()),
            2_000
        );
        // A zero ceiling must not 400 every request.
        let zeroed = adapter("eu.amazon.nova-lite-v1:0")
            .await
            .with_max_completion_tokens(0);
        assert_eq!(
            zeroed.effective_max_tokens(&GenerationOptions {
                max_tokens: None,
                ..Default::default()
            }),
            1
        );
    }

    #[tokio::test]
    async fn base_request_folds_system_only_input_into_a_user_turn() {
        let adapter = adapter("eu.amazon.nova-lite-v1:0").await;
        let body = adapter.base_request(
            &[Message::system("be terse")],
            &GenerationOptions::default(),
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "be terse");
        assert!(body.get("system").is_none());
    }

    #[tokio::test]
    async fn transcribe_image_rejects_non_image_and_unsupported_formats() {
        let adapter = adapter("eu.amazon.nova-lite-v1:0").await;
        let error = adapter
            .transcribe_image(b"not an image", "text/plain", None)
            .await
            .unwrap_err();
        assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");

        let error = adapter
            .transcribe_image(b"x", "image/tiff", None)
            .await
            .unwrap_err();
        assert!(
            matches!(error, LlmError::FeatureNotSupported(_)),
            "{error:?}"
        );
    }
}
