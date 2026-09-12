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
use tracing::{debug, info, instrument, warn};

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
    /// The resolved auth, kept so [`with_http_timeouts`](Self::with_http_timeouts)
    /// can rebuild the transport around a new HTTP client without re-running the
    /// §1.2 credential ladder. The transport holds its own clone; this one never
    /// reaches a log line (see the hand-written `Debug`).
    auth: Arc<aws::credentials::BedrockAuthProvider>,
    structured_output_retries: usize,
    /// Transport attempts, as `0..=network_retries` — so this is `n + 1`
    /// attempts, and `LLM_NETWORK_RETRIES=2` buys three.
    ///
    /// ⚠️ Deliberately noted because it diverges from the other adapters, which
    /// run the same knob through [`crate::retry::RetryBudget`] and stop at `n`
    /// attempts once the `retry_min_elapsed` floor is also met. This adapter
    /// carries no such floor, so `LLM_MIN_RETRY_SECONDS` does not reach it and
    /// its ladder is a plain attempt count. Both differences predate
    /// `LLM_NETWORK_RETRIES`; unifying them belongs with the Bedrock pacing work
    /// (SDK-612), which rewrites this loop.
    network_retries: usize,
    /// Wall-clock ceiling on **one logical structured-output call** — spanning
    /// every corrective re-ask and every transport retry inside them. `None`
    /// leaves the call unbounded.
    ///
    /// The same bound [`OpenAIAdapter`](crate::OpenAIAdapter) carries, and for
    /// the same reason: the HTTP client timeout is per-request and composes into
    /// no aggregate, so `structured_output_retries` re-asks each running a
    /// `network_retries + 1` transport ladder multiply out to
    /// `retries x (network_retries + 1) x request_timeout` — hours at the
    /// adapter defaults. It was unreachable from config here until SDK-624:
    /// there was no setter to call, so `LLM_REQUEST_DEADLINE_SECONDS` silently
    /// did nothing on Bedrock.
    ///
    /// Bounds *starting* work rather than cancelling it, so a request already on
    /// the wire when the budget expires still runs to its own timeout: the
    /// effective ceiling is `request_deadline + request_timeout`.
    request_deadline: Option<std::time::Duration>,
    /// Output-token ceiling (Python's `llm_max_completion_tokens`). Applied only
    /// when the CALLER supplies a budget: the per-request
    /// `inferenceConfig.maxTokens` is then `min(caller, this, the model cap)`.
    /// A caller passing `None` gets no `maxTokens` at all and this bounds
    /// nothing — see `effective_max_tokens`.
    max_completion_tokens: u32,
    /// Operator-configured sampling temperature, or `None` when unset.
    ///
    /// Applied only when the caller passes no temperature of its own, which is
    /// Python's rule: its validator folds `llm_temperature` into `llm_args`
    /// (which every adapter merges) if and only if the field was explicitly
    /// set, and per-request kwargs override it. `None` sends nothing, so the
    /// provider default applies — not the same as `Some(0.0)`.
    default_temperature: Option<f32>,
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
    /// Default per-HTTP-request timeout, aliasing the OpenAI adapter's so the
    /// two cannot drift. Overridable with
    /// [`with_http_timeouts`](Self::with_http_timeouts)
    /// (`LLM_REQUEST_TIMEOUT_SECONDS`); it was a bare `600` constant with no
    /// setter until SDK-624.
    pub const DEFAULT_REQUEST_TIMEOUT: std::time::Duration =
        crate::OpenAIAdapter::DEFAULT_REQUEST_TIMEOUT;
    /// Default TCP connect timeout, aliasing the OpenAI adapter's. `reqwest`
    /// applies none by default.
    pub const DEFAULT_CONNECT_TIMEOUT: std::time::Duration =
        crate::OpenAIAdapter::DEFAULT_CONNECT_TIMEOUT;

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
        let auth = aws::credentials::resolve_auth_provider(api_key, &settings, &region).await?;

        let client =
            Self::build_http_client(Self::DEFAULT_REQUEST_TIMEOUT, Self::DEFAULT_CONNECT_TIMEOUT)?;
        let auth = Arc::new(auth);
        let transport = Arc::new(ReqwestBedrockTransport::with_auth_provider(
            client,
            Arc::clone(&auth),
            region.clone(),
        ));

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
            auth,
            structured_output_retries: Self::DEFAULT_STRUCTURED_OUTPUT_RETRIES,
            network_retries: Self::DEFAULT_NETWORK_RETRIES,
            request_deadline: None,
            max_completion_tokens: Self::DEFAULT_MAX_COMPLETION_TOKENS,
            default_temperature: None,
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

    /// Build the HTTP client used for every request.
    ///
    /// Kept as one place so `new` and
    /// [`with_http_timeouts`](Self::with_http_timeouts) cannot drift in which
    /// timeouts they set. `0` means "no limit" for both, matching the OpenAI
    /// adapter: handing `reqwest` a `Duration::ZERO` timeout times every request
    /// out instantly, so an operator generalising the `0` escape hatch from
    /// `LLM_REQUEST_DEADLINE_SECONDS` would stop all traffic rather than lift a
    /// bound.
    fn build_http_client(
        request_timeout: std::time::Duration,
        connect_timeout: std::time::Duration,
    ) -> LlmResult<reqwest::Client> {
        let mut builder = reqwest::Client::builder();
        if !request_timeout.is_zero() {
            builder = builder.timeout(request_timeout);
        }
        // See `OpenAIAdapter::DEFAULT_CONNECT_TIMEOUT`: without this a
        // black-holed connect consumes the whole request timeout.
        if !connect_timeout.is_zero() {
            builder = builder.connect_timeout(connect_timeout);
        }
        builder
            .build()
            .map_err(|e| LlmError::ConfigError(format!("Failed to create HTTP client: {e}")))
    }

    /// Override the per-request and TCP-connect timeouts
    /// (`LLM_REQUEST_TIMEOUT_SECONDS` / `LLM_CONNECT_TIMEOUT_SECONDS`).
    ///
    /// Rebuilds the client *and* the transport around it, reusing the auth
    /// resolved in [`new`](Self::new) rather than re-running the credential
    /// ladder — which is why this is a builder rather than a setter: it is only
    /// sound before any request is in flight. On the (TLS-init-only) failure
    /// path the existing transport is kept and a warning logged, so a
    /// misconfigured timeout degrades to the defaults rather than failing
    /// component construction.
    #[must_use]
    pub fn with_http_timeouts(
        mut self,
        request_timeout: std::time::Duration,
        connect_timeout: std::time::Duration,
    ) -> Self {
        match Self::build_http_client(request_timeout, connect_timeout) {
            Ok(client) => {
                self.transport = Arc::new(ReqwestBedrockTransport::with_auth_provider(
                    client,
                    Arc::clone(&self.auth),
                    self.region.clone(),
                ));
            }
            Err(e) => warn!(
                error = %e,
                "failed to rebuild the Bedrock HTTP client with configured timeouts; \
                 keeping defaults",
            ),
        }
        self
    }

    /// Set the aggregate ceiling for one logical structured-output call
    /// (`LLM_REQUEST_DEADLINE_SECONDS`). `None` disables it.
    ///
    /// See the [`request_deadline`](Self::request_deadline) field for what it
    /// does and does not bound.
    #[must_use]
    pub fn with_request_deadline(mut self, deadline: Option<std::time::Duration>) -> Self {
        self.request_deadline = deadline;
        self
    }

    /// The deadline error for a call that started at `started`, if the budget is
    /// set and already spent.
    ///
    /// Returns the error rather than a bool so the message can name the budget,
    /// the elapsed time and the stage that was about to be entered — without
    /// that, an aggregate cut is indistinguishable from a provider timeout in a
    /// log.
    fn deadline_exceeded(&self, started: std::time::Instant, next_stage: &str) -> Option<LlmError> {
        let deadline = self.request_deadline?;
        let elapsed = started.elapsed();
        if elapsed < deadline {
            return None;
        }
        Some(LlmError::Timeout(format!(
            "Bedrock structured output exceeded its {}s aggregate budget \
             (LLM_REQUEST_DEADLINE_SECONDS) after {:.0}s, before {next_stage}; raise the \
             budget, or lower LLM_MAX_RETRIES / LLM_NETWORK_RETRIES so the retry ladder \
             fits inside it",
            deadline.as_secs(),
            elapsed.as_secs_f64(),
        )))
    }

    /// Set the output-token ceiling (`llm_max_completion_tokens`). The
    /// per-request `maxTokens` is still clamped to the model's documented cap.
    pub fn with_max_completion_tokens(mut self, ceiling: u32) -> Self {
        self.max_completion_tokens = ceiling;
        self
    }

    /// Set the operator-configured temperature (`llm_temperature`).
    ///
    /// `None` means the operator set none, so no `temperature` reaches the wire
    /// and the model's own default applies. A per-call
    /// `GenerationOptions::temperature` still wins over this.
    #[must_use]
    pub fn with_default_temperature(mut self, temperature: Option<f32>) -> Self {
        self.default_temperature = temperature;
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
    /// `max_tokens: None` one, so a caller cannot silently bypass a lower
    /// operator-configured ceiling. The model cap is applied last so Bedrock
    /// never 400s on `maxTokens > model limit`.
    ///
    /// A caller who passes NO budget of their own — `None`, which is also what
    /// `GenerationOptions::default()` leaves in `max_tokens` since SDK-581 —
    /// gets `None` back, and `maxTokens` is omitted from the request entirely.
    /// Bedrock then applies the model maximum, matching the Python engine, whose
    /// Bedrock adapter never serialises a budget at all.
    ///
    /// CONSEQUENCE, deliberate and worth knowing before you rely on it:
    /// `llm_max_completion_tokens` does NOT bound such a call. On the cognify
    /// path — which passes `max_tokens: None` by design — a runaway generation
    /// can bill up to the model maximum per chunk, and there is no setting that
    /// caps it. That is Python's behaviour too: there the setting only sizes
    /// input chunks and never reaches the request. A caller that needs a bound
    /// passes one explicitly, and it is clamped by the ceiling and then the
    /// model cap.
    fn effective_max_tokens(&self, opts: &GenerationOptions) -> Option<u32> {
        // `None` is sent as an ABSENT `maxTokens`, not as the configured
        // ceiling. Bedrock documents the omitted case as "the maximum allowed
        // value for the model that you are using", which is what the Python
        // engine gets: its Bedrock adapter never serialises a budget, so its
        // Converse body carries a literal `"inferenceConfig": {}`.
        //
        // Substituting the ceiling here reversed the caller's intent. Cognify's
        // `extraction_options()` sets `max_tokens: None` with a comment stating
        // it means "use the model's full default output budget, for Python
        // parity" — and this function turned that into 16384, a quarter of
        // Sonnet 4.5's 64000. Three chunks of a 55-chunk document then truncated
        // at a limit Python never applies, and `RollbackScope::WholeRun`
        // discarded the other 52.
        //
        // POLICY NOTE: `llm_max_completion_tokens` therefore no longer bounds a
        // caller that passes no budget of its own. That matches Python, where
        // the setting only ever sizes input chunks
        // (`cognee/infrastructure/llm/utils.py`) and never reaches the Bedrock
        // request. A caller that needs a bound passes one explicitly, and it is
        // still clamped by the ceiling and the model cap below.
        let requested = opts.max_tokens?;
        Some(
            requested
                .min(self.max_completion_tokens)
                .min(self.caps.max_output_tokens)
                .max(1),
        )
    }

    /// The output budget a truncation retry may raise to.
    ///
    /// When the caller supplied a budget, the ceiling still bounds the raise.
    /// When it did not, the request went out with no `maxTokens` at all and the
    /// model's own maximum was already in force — so there is nothing above it
    /// to climb to, and a truncation there is genuinely terminal.
    fn effective_output_cap(&self, opts: &GenerationOptions) -> u32 {
        if opts.max_tokens.is_none() {
            return self.caps.max_output_tokens.max(1);
        }
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
        // Caller's own temperature wins; otherwise the operator-configured one;
        // otherwise nothing at all. Mirrors Python, where per-request kwargs
        // override the folded `llm_args` value and an unset field contributes
        // no key.
        body["inferenceConfig"] = converse::inference_config(
            opts,
            self.effective_max_tokens(opts),
            self.default_temperature,
        );
        converse::merge_additional_model_request_fields(
            &mut body,
            &self.extra_args,
            &converse::penalty_model_fields(opts),
        );
        body
    }

    /// POST `request_body` to the Converse endpoint, unbounded by any aggregate
    /// budget.
    ///
    /// The plain-completion entry point; the structured-output loop goes through
    /// [`call_converse_before`](Self::call_converse_before) so its re-asks share
    /// one deadline.
    async fn call_converse(&self, request_body: &Value) -> LlmResult<ConverseResponse> {
        self.call_converse_before(request_body, None).await
    }

    /// POST `request_body` to the Converse endpoint with a transient-retry
    /// ladder and exponential backoff.
    ///
    /// `deadline` is the caller's aggregate budget as an absolute instant, so it
    /// already counts every earlier attempt in the same logical call. `None`
    /// leaves the ladder unbounded.
    #[instrument(
        name = "llm.api_call",
        level = "info",
        skip(self, request_body, deadline),
        fields(
            url = tracing::field::Empty,
            cognee.llm.model = self.model.as_str(),
            cognee.llm.provider = "bedrock",
        ),
    )]
    async fn call_converse_before(
        &self,
        request_body: &Value,
        deadline: Option<std::time::Instant>,
    ) -> LlmResult<ConverseResponse> {
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
        // Only for the deadline messages below; the ladder itself is a plain
        // attempt count, with no time floor to measure.
        let started = std::time::Instant::now();

        for attempt in 0..=self.network_retries {
            if attempt > 0 {
                // Shared jittered backoff (issue #19): a batch of concurrent
                // requests that all throttle at once must not retry in lockstep.
                let mut delay = crate::retry::retry_backoff(attempt as u32);
                // The caller's aggregate budget outranks the retry ladder. Give
                // up rather than start an attempt that cannot finish inside it,
                // and never sleep past it — a 128s backoff against 5s of
                // remaining budget would otherwise blow the ceiling on its own.
                if let Some(deadline) = deadline {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        return Err(LlmError::Timeout(format!(
                            "Bedrock request abandoned after {:.0}s with {attempt} attempt(s): \
                             the call's aggregate budget (LLM_REQUEST_DEADLINE_SECONDS) was spent \
                             mid-retry; last error: {last_error}",
                            started.elapsed().as_secs_f64(),
                        )));
                    }
                    delay = delay.min(remaining);
                }
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
        // Set only by the truncation branch, so the retry log can distinguish an
        // expensive truncation loop from a routine corrective re-ask.
        let mut truncation_retry = false;

        // Start of the aggregate budget. Every re-ask and transport retry below
        // is measured against this one instant, because the thing that needs
        // bounding is the *logical* call: no individual HTTP request in a
        // 45-minute extraction was itself slow.
        let call_started = std::time::Instant::now();
        // Absolute form of the budget, threaded into every transport call below
        // so the retry ladder inside an attempt is bounded by it too, not just
        // the gaps between attempts.
        let call_deadline = self.request_deadline.map(|d| call_started + d);

        for attempt in 0..self.structured_output_retries {
            if attempt > 0 {
                // `call_converse`'s ladder only covers transport retries inside
                // a single attempt, so without this the outer loop would re-ask
                // immediately (Python waits between structured retries via
                // `wait_exponential_jitter`).
                let mut delay = crate::retry::retry_backoff(attempt as u32);
                // Never sleep past the aggregate budget: a 128s backoff against
                // 5s of remaining budget would blow the ceiling on its own. The
                // check below then abandons rather than paying for a re-ask the
                // budget can no longer cover.
                if let Some(deadline) = call_deadline {
                    delay =
                        delay.min(deadline.saturating_duration_since(std::time::Instant::now()));
                }
                // Never render `last_error` here. `DeserializationError` embeds
                // the raw Converse body verbatim (see `call_converse`), which on
                // the cognify path is model output derived from the user's
                // ingested documents — logging it would put user content in
                // operator logs. Only the variant is safe unconditionally; the
                // message is included solely for the errors this loop synthesises
                // itself, which quote no provider payload.
                // Only the variant, never the message. `DeserializationError`
                // embeds the raw Converse body, and `converse::map_error` puts
                // the raw HTTP body into `InvalidResponse` for a 400 /
                // ValidationException — which can echo the offending input. On
                // the cognify path both are model output derived from the user's
                // ingested documents, so neither may reach a log sink. The
                // truncation case gets its own message on the branch below.
                let reason = last_error.log_kind();
                // `warn` only for a truncation re-ask: that one costs a full
                // generation plus this backoff and is what makes a cognify stall
                // for minutes with nothing at INFO to explain it. A validator- or
                // parse-driven repair is routine and expected — cognify runs this
                // loop once per chunk, so shouting about those would emit a WARN
                // per repaired chunk on an ordinary ingest.
                if truncation_retry {
                    warn!(
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        max_tokens = ?body["inferenceConfig"].get("maxTokens"),
                        reason,
                        "retrying Bedrock structured output after a truncated answer",
                    );
                } else {
                    debug!(
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        max_tokens = ?body["inferenceConfig"].get("maxTokens"),
                        reason,
                        "retrying Bedrock structured output",
                    );
                }
                tokio::time::sleep(delay).await;
            }
            // Aggregate budget check at the head of the attempt — after the
            // backoff above, so a sleep that consumed the rest of the budget
            // aborts here rather than buying one more full generation, and
            // inside the loop rather than only around it so a long transport
            // ladder inside one attempt is bounded too.
            if let Some(e) = self.deadline_exceeded(call_started, "another re-ask") {
                return Err(e);
            }
            // Cleared per attempt: the flag describes the attempt that just
            // failed, so leaving it latched would label a later validator-driven
            // re-ask as a truncation and reintroduce the WARN-per-chunk noise.
            truncation_retry = false;

            match self.call_converse_before(&body, call_deadline).await {
                Ok(response) => {
                    // Emitted at INFO so a single production run can tell the two
                    // parity fixes apart. If output_tokens now sit near the
                    // previously-expected ~5-6k, the runaway was driven by the
                    // 0.1 temperature and dropping it is what mattered. If some
                    // calls legitimately exceed 16384, the runaway is real and
                    // omitting `maxTokens` is what saved them. Without this the
                    // two are indistinguishable from a green run.
                    if let Some(usage) = response.usage.as_ref() {
                        info!(
                            attempt,
                            input_tokens = usage.input_tokens,
                            output_tokens = usage.output_tokens,
                            truncated = response.is_truncated(),
                            budget = ?body["inferenceConfig"].get("maxTokens"),
                            "bedrock structured output usage",
                        );
                    }
                    // Truncation is classified *before* the payload is inspected,
                    // because a cut-off answer arrives in one of two disguises
                    // and neither is self-describing. On the native branch it is
                    // always the harder one: `structured_payload` parses the
                    // response *text* as JSON, and text cut off at maxTokens
                    // never parses — so testing truncation only on a *present*
                    // payload left the stall recorded as "did not contain
                    // parseable JSON", re-asked at the SAME budget, and
                    // re-truncated at the same point every attempt until
                    // MaxRetriesExceeded, burning a full generation plus a
                    // backoff each time. On the fallback branch the partial
                    // `toolUse.input` usually does survive parsing, and must not
                    // be validated and returned either.
                    if response.is_truncated() {
                        // Matching the Python reference (instructor), a
                        // length-truncated structured response is rejected
                        // outright rather than returned as a partial object — a
                        // shallow top-level check cannot tell a complete object
                        // that happened to finish at the budget from one whose
                        // nested list/string was cut off. Re-asking with the SAME
                        // budget would truncate at the same point, so raise it
                        // toward the effective output budget. That budget is the
                        // model cap bounded by the configured
                        // `llm_max_completion_tokens` ceiling, which is an upper
                        // bound on every path — so when we are already at it,
                        // fail terminally rather than loop until
                        // MaxRetriesExceeded.
                        let cap = self.effective_output_cap(&opts);
                        // An ABSENT maxTokens means the model's own maximum was
                        // already in force, so read `current` as that maximum
                        // rather than 0 — otherwise a truncation at the model
                        // ceiling would look like headroom and drive a pointless
                        // re-ask at the very budget that just truncated.
                        let current = body["inferenceConfig"]["maxTokens"]
                            .as_u64()
                            .map_or(self.caps.max_output_tokens, |v| v as u32);
                        truncation_retry = true;
                        // At the cap there is no larger budget to move to, but a
                        // re-ask is still worth an attempt, because this failure
                        // is STOCHASTIC rather than a property of the input.
                        //
                        // Measured on Bedrock/Sonnet 4.5 with native structured
                        // output: ~4.5% of extraction calls run away and emit
                        // until they hit the model maximum, on ordinary ~2.7k-token
                        // chunks whose siblings answer in under 2k. It is a
                        // per-call dice roll, not a poisoned chunk — the runaway
                        // chunk ids differ between runs of the same document.
                        //
                        // Direct evidence that a re-ask clears it: in one run a
                        // call ran past the 600s transport timeout, the network
                        // ladder re-POSTed the IDENTICAL body, and the retry
                        // returned a normal 686-token answer 11 seconds later.
                        // Same chunk, same request, same budget.
                        //
                        // Returning terminally here therefore threw away a run for
                        // a failure that a second attempt usually survives — and
                        // because `RollbackScope::WholeRun` then sweeps, 2 bad
                        // chunks out of 55 discarded all 53 good extractions.
                        //
                        // Bounded by `structured_output_retries`, so a chunk that
                        // truly cannot be answered still terminates, now with the
                        // exhaustion error rather than this one.
                        let at_cap = current >= cap;
                        if !at_cap {
                            body["inferenceConfig"]["maxTokens"] = json!(cap);
                        }
                        // The cap is named in the message so the exhaustion error a
                        // caller finally sees still says which budget was hit, not
                        // merely that something truncated.
                        let at_cap_reason;
                        let reason: &str = if at_cap {
                            at_cap_reason = format!(
                                "the previous answer ran to the effective {cap}-token output \
                                 budget without completing the object; produce a smaller, \
                                 complete result rather than continuing the previous one"
                            );
                            &at_cap_reason
                        } else {
                            "the previous answer was cut off at maxTokens before the \
                             object was complete"
                        };
                        last_error = LlmError::InvalidResponse(format!(
                            "Bedrock structured output truncated: {reason}"
                        ));
                        converse::append_corrective_instruction(
                            &mut body,
                            Some(reason),
                            self.caps.supports_native_structured_output,
                        );
                        // Explicit, because the payload match below must stay
                        // unreachable for a truncated response however parseable
                        // the fragment happens to look.
                        continue;
                    }

                    match response.structured_payload(&self.caps) {
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
            .field("default_temperature", &self.default_temperature)
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

    /// The configured ceiling clamped by the model's documented output cap —
    /// the same `min` [`effective_max_tokens`](Self::effective_max_tokens)
    /// applies per request.
    fn max_completion_tokens(&self) -> u32 {
        self.max_completion_tokens.min(self.caps.max_output_tokens)
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

        // Normalised the same way `image_format_for_mime` normalises, so a
        // valid-but-oddly-cased `IMAGE/PNG` is not rejected here before the
        // helper below ever gets to map it.
        let normalised_mime = mime_type.trim().to_ascii_lowercase();
        if !normalised_mime.starts_with("image/") {
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
        // `llm_max_completion_tokens` ceiling. A caller passing a large
        // max_tokens against a model that caps lower would otherwise 400, and
        // would slip past an operator ceiling that bounds every other path.
        // Floored at 1 so a zero never 400s either.
        //
        // A caller who sets nothing keeps the 300-token vision default. Note the
        // asymmetry with the chat path above, which substitutes the *ceiling* for
        // an unset budget: 300 is a deliberate vision-specific floor, not the
        // configured ceiling. Since SDK-581 that also covers
        // `GenerationOptions::default()`, which no longer carries 16384 — so a
        // published-crate caller passing default options gets 300 here where it
        // used to get 16384. The only in-tree caller
        // (`cognee-ingestion`'s image loader) passes `None` and always took this
        // path.
        let max_tokens = options
            .as_ref()
            .and_then(|o| o.max_tokens)
            .unwrap_or(300)
            .min(self.caps.max_output_tokens.min(self.max_completion_tokens))
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
    async fn an_absent_caller_budget_sends_no_max_tokens() {
        // The whole point of the fix: no caller budget => no `maxTokens` on the
        // wire => Bedrock applies the model maximum, exactly as the Python
        // engine gets (its body carries a literal `"inferenceConfig": {}`).
        // Previously this substituted the 16384 ceiling and truncated real
        // extractions a quarter of the way into Sonnet 4.5's 64000.
        for model in [
            "eu.amazon.nova-lite-v1:0",
            "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
        ] {
            let a = adapter(model).await;
            assert_eq!(
                a.effective_max_tokens(&GenerationOptions::default()),
                None,
                "{model}: an unset budget must be omitted, not substituted",
            );
        }
        // A configured ceiling does NOT resurrect a budget the caller declined
        // to set — matching Python, where llm_max_completion_tokens only ever
        // sizes input chunks and never reaches the Converse request.
        let capped = adapter("eu.anthropic.claude-sonnet-4-5-20250929-v1:0")
            .await
            .with_max_completion_tokens(2_000);
        assert_eq!(
            capped.effective_max_tokens(&GenerationOptions::default()),
            None
        );
    }

    #[tokio::test]
    async fn an_explicit_caller_budget_is_still_clamped() {
        // An explicit budget keeps every previous guarantee: bounded by the
        // configured ceiling, then by the model cap, and never zero.
        let nova = adapter("eu.amazon.nova-lite-v1:0").await;
        assert_eq!(
            nova.effective_max_tokens(&GenerationOptions {
                max_tokens: Some(50_000),
                ..Default::default()
            }),
            Some(10_000),
            "clamped to the nova-lite model cap",
        );
        let capped = adapter("eu.anthropic.claude-sonnet-4-5-20250929-v1:0")
            .await
            .with_max_completion_tokens(2_000);
        assert_eq!(
            capped.effective_max_tokens(&GenerationOptions {
                max_tokens: Some(9_000),
                ..Default::default()
            }),
            Some(2_000),
            "clamped to the configured ceiling",
        );
        let zeroed = adapter("eu.amazon.nova-lite-v1:0")
            .await
            .with_max_completion_tokens(0);
        assert_eq!(
            zeroed.effective_max_tokens(&GenerationOptions {
                max_tokens: Some(500),
                ..Default::default()
            }),
            Some(1),
            "a zero ceiling must not 400 every request",
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
