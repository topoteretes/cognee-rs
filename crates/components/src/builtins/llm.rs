//! Built-in LLM factory (OpenAI + OpenAI-compatible providers) plus the
//! cross-cutting mock-override and record-wrap helpers consumed by
//! [`crate::ComponentRegistry::build_llm`].

use std::sync::Arc;

use async_trait::async_trait;
use cognee_llm::{AnthropicAdapter, Llm, Transcriber, build_openai_compatible_adapter};

/// Install the process-wide LLM pacer from config.
///
/// Idempotent (first call wins), so every factory can call it without caring
/// which provider is built first — mirroring Python's lazily-built module-level
/// limiter in `shared/rate_limiting.py`. Adapters then discover it through
/// `cognee_utils::pacing::llm_pacer()`, so no adapter needs it passed in.
fn install_pacer(ctx: &BackendBuildContext) {
    cognee_utils::pacing::init_llm_pacer(
        ctx.llm.rate_limit_requests,
        std::time::Duration::from_secs(u64::from(ctx.llm.rate_limit_interval)),
        ctx.llm.rate_limit_enabled,
        ctx.llm.auto_rate_limit,
    );
    // The concurrency half of the same contract, installed alongside so no
    // factory can wire one without the other. The pacer bounds how *often*
    // requests start; this bounds how many are in flight at once — the ceiling
    // Python gets for free from its HTTP client's connection pool and `reqwest`
    // does not provide. Applying it here rather than per-adapter is what makes
    // `LLM_MAX_PARALLEL_REQUESTS` bind on paths that never thread config into a
    // stage-level semaphore, such as the HTTP cognify routers.
    cognee_llm::in_flight::init_llm_in_flight(ctx.llm.max_parallel_requests as usize);
}

/// The configured minimum retry window, as a duration.
fn min_retry_elapsed(ctx: &BackendBuildContext) -> std::time::Duration {
    std::time::Duration::from_secs(u64::from(ctx.llm.min_retry_seconds))
}

/// The configured per-request and TCP-connect timeouts.
fn http_timeouts(ctx: &BackendBuildContext) -> (std::time::Duration, std::time::Duration) {
    (
        std::time::Duration::from_secs(u64::from(ctx.llm.request_timeout_seconds)),
        std::time::Duration::from_secs(u64::from(ctx.llm.connect_timeout_seconds)),
    )
}

/// The configured aggregate ceiling for one logical structured-output call, or
/// `None` when disabled with `0`.
///
/// Warns when the budget cannot accommodate the retry ladder it has to contain.
/// A budget that cuts calls the retry design deliberately intends to keep
/// waiting turns the rate-limit-window survival of that design back off by the
/// side door. Warn rather than clamp: an operator who deliberately wants a tight
/// ceiling is entitled to one, but should not get one by accident.
///
/// The ladder is `CASCADE_MODES x max_retries x min_retry_seconds`. All three
/// factors matter: the cascade tries three request shapes, each is retried
/// `max_retries` times (`LLM_MAX_RETRIES` feeds *both* the structured-output and
/// network retry counts), and every attempt honours the time floor before it is
/// allowed to give up. Counting the modes but not the attempts under-reports the
/// ladder by the retry multiplier and lets the misconfiguration this warning
/// exists to catch pass silently.
fn request_deadline(ctx: &BackendBuildContext) -> Option<std::time::Duration> {
    /// Request shapes `structured_output_impl` falls through: tool calls, legacy
    /// functions, JSON mode.
    const CASCADE_MODES: u64 = 3;

    if ctx.llm.request_deadline_seconds == 0 {
        return None;
    }
    let deadline = u64::from(ctx.llm.request_deadline_seconds);
    let ladder = u64::from(ctx.llm.min_retry_seconds)
        .saturating_mul(u64::from(ctx.llm.max_retries).max(1))
        .saturating_mul(CASCADE_MODES);
    if deadline < ladder {
        tracing::warn!(
            deadline_seconds = deadline,
            ladder_seconds = ladder,
            min_retry_seconds = ctx.llm.min_retry_seconds,
            max_retries = ctx.llm.max_retries,
            "LLM_REQUEST_DEADLINE_SECONDS is below the retry ladder it has to \
             contain (3 cascade modes x LLM_MAX_RETRIES x LLM_MIN_RETRY_SECONDS), \
             so structured extraction will be cut mid-retry; raise the deadline, \
             or lower LLM_MAX_RETRIES / LLM_MIN_RETRY_SECONDS",
        );
    }
    Some(std::time::Duration::from_secs(deadline))
}

use crate::context::BackendBuildContext;
use crate::error::ComponentError;
use crate::traits::LlmFactory;

/// Provider ids served by [`OpenAiCompatibleLlmFactory`].
pub const OPENAI_COMPATIBLE_PROVIDERS: &[&str] = &[
    "openai",
    "ollama",
    "mistral",
    "gemini",
    "custom",
    "openai_compatible",
];

/// Built-in factory covering OpenAI and every OpenAI-compatible provider,
/// routed through the shared `build_openai_compatible_adapter` factory.
pub struct OpenAiCompatibleLlmFactory {
    provider: &'static str,
}

impl OpenAiCompatibleLlmFactory {
    /// Construct a factory registered under `provider`.
    pub fn new(provider: &'static str) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl LlmFactory for OpenAiCompatibleLlmFactory {
    fn provider(&self) -> &str {
        self.provider
    }

    async fn build(&self, ctx: &BackendBuildContext) -> Result<Arc<dyn Llm>, ComponentError> {
        install_pacer(ctx);
        let adapter = build_openai_compatible_adapter(
            &ctx.llm.provider,
            &ctx.llm.model,
            &ctx.llm.api_key,
            &ctx.llm.endpoint,
            ctx.llm.max_retries,
        )
        .map_err(|e| ComponentError::Llm(e.to_string()))?
        .with_min_retry_elapsed(min_retry_elapsed(ctx))
        .with_extra_args(ctx.llm.llm_args.clone())
        .with_default_max_tokens(Some(ctx.llm.max_completion_tokens))
        .with_reasoning_override(ctx.llm.reasoning_override)
        .with_request_deadline(request_deadline(ctx));
        let (request_timeout, connect_timeout) = http_timeouts(ctx);
        let adapter = adapter.with_http_timeouts(request_timeout, connect_timeout);
        Ok(Arc::new(adapter))
    }

    async fn build_transcriber(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Option<Arc<dyn Transcriber>>, ComponentError> {
        // Whisper-style transcription works against OpenAI and any user-pointed
        // OpenAI-compatible server exposing /audio/transcriptions (Groq, vLLM, a
        // LiteLLM proxy). Ollama/Mistral/Gemini do not expose that route via the
        // chat path, so they return None (graceful no-audio) rather than an
        // adapter that 404s at runtime.
        if !matches!(
            ctx.llm.provider.as_str(),
            "openai" | "custom" | "openai_compatible"
        ) {
            return Ok(None);
        }
        // Transcription is an LLM HTTP call like any other, so it must be paced
        // and in-flight-bounded too. Previously only the chat factories installed
        // the gates, leaving a transcriber-only process (audio ingestion with no
        // cognify) entirely unpaced. `install_pacer` is first-call-wins, so this
        // is a no-op when a chat adapter was built first.
        install_pacer(ctx);
        let (request_timeout, connect_timeout) = http_timeouts(ctx);
        let adapter = build_openai_compatible_adapter(
            &ctx.llm.provider,
            &ctx.llm.model,
            &ctx.llm.api_key,
            &ctx.llm.endpoint,
            ctx.llm.max_retries,
        )
        .map_err(|e| ComponentError::Llm(e.to_string()))?
        .with_http_timeouts(request_timeout, connect_timeout);
        Ok(Some(Arc::new(adapter) as Arc<dyn Transcriber>))
    }
}

/// Provider id served by [`AnthropicLlmFactory`].
pub const ANTHROPIC_PROVIDER: &str = "anthropic";

/// Built-in factory for the native Anthropic Messages API adapter. Anthropic is
/// not OpenAI-compatible, so it cannot route through the shared factory (issue
/// #17, Tier 2).
pub struct AnthropicLlmFactory;

#[async_trait]
impl LlmFactory for AnthropicLlmFactory {
    fn provider(&self) -> &str {
        ANTHROPIC_PROVIDER
    }

    async fn build(&self, ctx: &BackendBuildContext) -> Result<Arc<dyn Llm>, ComponentError> {
        if ctx.llm.api_key.trim().is_empty() {
            return Err(ComponentError::Config(
                "anthropic provider requires an API key (set LLM_API_KEY)".to_string(),
            ));
        }
        // Use the dedicated `ctx.llm.anthropic_base_url` (env `ANTHROPIC_BASE_URL`
        // / `ANTHROPIC_API_BASE`), NOT `ctx.llm.endpoint`: the latter aliases
        // OPENAI_URL (a documented-required var in this repo), so flipping
        // LLM_PROVIDER=anthropic while OPENAI_URL is still set would POST every
        // request to the OpenAI host with an x-api-key header (404/401 on all
        // traffic). `None` (the default) uses the public Anthropic API, matching
        // Python; the override exists for proxy / gateway / Bedrock-compatible
        // endpoints.
        install_pacer(ctx);
        let adapter = AnthropicAdapter::new(
            ctx.llm.model.clone(),
            ctx.llm.api_key.clone(),
            ctx.llm.anthropic_base_url.clone(),
        )
        .map_err(|e| ComponentError::Llm(e.to_string()))?
        .with_structured_output_retries(ctx.llm.max_retries)
        .with_network_retries(ctx.llm.max_retries)
        .with_min_retry_elapsed(min_retry_elapsed(ctx))
        .with_max_completion_tokens(ctx.llm.max_completion_tokens)
        .with_extra_args(ctx.llm.llm_args.clone());
        Ok(Arc::new(adapter))
    }

    async fn build_transcriber(
        &self,
        _ctx: &BackendBuildContext,
    ) -> Result<Option<Arc<dyn Transcriber>>, ComponentError> {
        // The Anthropic Messages API has no Whisper-style transcription route, so
        // audio degrades gracefully to None (same as ollama/mistral/gemini).
        Ok(None)
    }
}

/// Provider id served by [`AzureLlmFactory`].
pub const AZURE_PROVIDER: &str = "azure";

/// Built-in factory for Azure OpenAI. Azure is wire-compatible with the OpenAI
/// chat API but authenticates with an `api-key` header and appends an
/// `?api-version=<v>` query, and the deployment is encoded in the endpoint URL
/// (issue #17, Tier 3). It builds the shared `OpenAIAdapter` against the explicit
/// deployment endpoint, then switches to the Azure auth/URL conventions with
/// `with_api_version`.
pub struct AzureLlmFactory;

#[async_trait]
impl LlmFactory for AzureLlmFactory {
    fn provider(&self) -> &str {
        AZURE_PROVIDER
    }

    async fn build(&self, ctx: &BackendBuildContext) -> Result<Arc<dyn Llm>, ComponentError> {
        if ctx.llm.endpoint.trim().is_empty() {
            return Err(ComponentError::Config(
                "azure provider requires LLM_ENDPOINT (the deployment URL: \
                 https://<resource>.openai.azure.com/openai/deployments/<deployment>)"
                    .to_string(),
            ));
        }
        let api_version = ctx.llm.api_version.trim();
        if api_version.is_empty() {
            return Err(ComponentError::Config(
                "azure provider requires LLM_API_VERSION (e.g. 2024-12-01-preview)".to_string(),
            ));
        }
        // Azure's endpoint is the deployment URL, so build it as a "custom"
        // explicit-endpoint OpenAI adapter (which enforces endpoint + key), then
        // switch to api-key auth and the api-version query.
        install_pacer(ctx);
        let adapter = build_openai_compatible_adapter(
            "custom",
            &ctx.llm.model,
            &ctx.llm.api_key,
            &ctx.llm.endpoint,
            ctx.llm.max_retries,
        )
        .map_err(|e| ComponentError::Llm(e.to_string()))?
        .with_min_retry_elapsed(min_retry_elapsed(ctx))
        .with_api_version(api_version)
        .with_extra_args(ctx.llm.llm_args.clone())
        // Honour the operator's completion ceiling on Azure too, matching the
        // OpenAI-compatible factory: without this, option-less generate() calls
        // (search/recall/feedback) fall back to the adapter's hardcoded 16384 and
        // Azure deployments with a smaller output cap 400 on every such call.
        .with_default_max_tokens(Some(ctx.llm.max_completion_tokens))
        .with_reasoning_override(ctx.llm.reasoning_override)
        .with_request_deadline(request_deadline(ctx));
        let (request_timeout, connect_timeout) = http_timeouts(ctx);
        let adapter = adapter.with_http_timeouts(request_timeout, connect_timeout);
        Ok(Arc::new(adapter))
    }

    async fn build_transcriber(
        &self,
        _ctx: &BackendBuildContext,
    ) -> Result<Option<Arc<dyn Transcriber>>, ComponentError> {
        // Azure Whisper deployments exist but need their own deployment URL and
        // api-version; not wired here, so audio degrades gracefully to None.
        Ok(None)
    }
}

/// Provider id served by [`BedrockLlmFactory`].
#[cfg(feature = "bedrock")]
pub const BEDROCK_PROVIDER: &str = "bedrock";

/// Built-in factory for the native AWS Bedrock **Converse** adapter (plan §4 R5).
///
/// Bedrock is neither OpenAI-compatible nor Anthropic-Messages-compatible: it
/// carries its own request shape, its own auth (SigV4 or a Bedrock API key) and
/// its own region/endpoint resolution, all of which live in
/// [`cognee_llm::adapters::BedrockAdapter`] behind the `bedrock` feature.
#[cfg(feature = "bedrock")]
pub struct BedrockLlmFactory;

#[cfg(feature = "bedrock")]
#[async_trait]
impl LlmFactory for BedrockLlmFactory {
    fn provider(&self) -> &str {
        BEDROCK_PROVIDER
    }

    async fn build(&self, ctx: &BackendBuildContext) -> Result<Arc<dyn Llm>, ComponentError> {
        // Deliberately NO API-key requirement, unlike the Anthropic factory
        // above. Bedrock is absent from Python's `_API_KEY_REQUIRED_PROVIDERS`
        // (`get_llm_client.py:98`) and listed in `_NO_API_KEY_PROVIDERS`
        // (`get_native_client.py:20`) — the exemption holds on both framework
        // paths (plan §1.1). An empty `LLM_API_KEY` is the *normal* IAM
        // configuration: it must fall through to the SigV4 credential ladder
        // rather than error.
        let api_key = Some(ctx.llm.api_key.trim()).filter(|key| !key.is_empty());

        // The single crossing point to the adapter-side struct; see
        // `crate::context::AwsInputs`.
        let aws: cognee_llm::adapters::bedrock::aws::env::AwsInputs = (&ctx.llm.aws).into();

        // `api_base = None` on purpose: never pass `ctx.llm.endpoint`, which
        // aliases OPENAI_URL (the same trap `anthropic_base_url` exists to
        // avoid — see the Anthropic factory above). The Bedrock runtime
        // endpoint travels via `AWS_BEDROCK_RUNTIME_ENDPOINT` on
        // `ctx.llm.aws.bedrock_runtime_endpoint` and the plan's §1.3 chain
        // inside the adapter.
        let adapter =
            cognee_llm::adapters::BedrockAdapter::new(ctx.llm.model.clone(), api_key, None, &aws)
                .await
                .map_err(|e| ComponentError::Llm(e.to_string()))?
                .with_structured_output_retries(ctx.llm.max_retries)
                .with_network_retries(ctx.llm.max_retries)
                .with_max_completion_tokens(ctx.llm.max_completion_tokens)
                .with_extra_args(ctx.llm.llm_args.clone());
        Ok(Arc::new(adapter))
    }

    async fn build_transcriber(
        &self,
        _ctx: &BackendBuildContext,
    ) -> Result<Option<Arc<dyn Transcriber>>, ComponentError> {
        // Bedrock has no Whisper equivalent (plan §6.4): Python's
        // `BedrockAdapter.create_transcript` raises NotImplementedError, so
        // audio degrades gracefully to None here, matching the
        // anthropic/ollama/gemini/mistral factories.
        Ok(None)
    }
}

// ── Cross-cutting mock / record helpers ───────────────────────────────────
//
// These are applied uniformly by `ComponentRegistry::build_llm` regardless of
// provider: a mock request replaces the adapter entirely (before provider
// lookup), and a record path wraps whatever real adapter was built. Only the
// real adapter is worth recording — replaying a recording of a mock is
// pointless — so wrapping happens after the factory produces the adapter.

/// Build the cassette-replay mock LLM (`MOCK_LLM` / `llm_provider=mock`).
pub(crate) fn build_mock_llm(ctx: &BackendBuildContext) -> Result<Arc<dyn Llm>, ComponentError> {
    #[cfg(feature = "mock-llm")]
    {
        let cassette = ctx.llm.cassette.trim();
        if cassette.is_empty() {
            return Err(ComponentError::Config(
                "MOCK_LLM is set but MOCK_LLM_CASSETTE is empty; set it to a cassette path"
                    .to_string(),
            ));
        }
        let replay = cognee_llm::mock::ReplayLlm::from_path(cassette)
            .map_err(|e| ComponentError::Llm(format!("mock cassette load failed: {e}")))?;
        Ok(Arc::new(replay))
    }
    #[cfg(not(feature = "mock-llm"))]
    {
        let _ = ctx;
        Err(ComponentError::Config(
            "MOCK_LLM was requested but the mock LLM is unavailable; \
             rebuild with the `mock-llm` feature"
                .to_string(),
        ))
    }
}

/// Wrap a real adapter in a recorder (`COGNEE_RECORD_LLM`).
pub(crate) fn wrap_recording(
    adapter: Arc<dyn Llm>,
    record_path: &str,
) -> Result<Arc<dyn Llm>, ComponentError> {
    #[cfg(feature = "mock-llm")]
    {
        let recorder = cognee_llm::mock::RecordingLlm::new(adapter, record_path.trim().to_string());
        Ok(Arc::new(recorder))
    }
    #[cfg(not(feature = "mock-llm"))]
    {
        let _ = (adapter, record_path);
        Err(ComponentError::Config(
            "COGNEE_RECORD_LLM was set but LLM recording is unavailable; \
             rebuild with the `mock-llm` feature"
                .to_string(),
        ))
    }
}
