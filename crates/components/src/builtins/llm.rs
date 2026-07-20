//! Built-in LLM factory (OpenAI + OpenAI-compatible providers) plus the
//! cross-cutting mock-override and record-wrap helpers consumed by
//! [`crate::ComponentRegistry::build_llm`].

use std::sync::Arc;

use async_trait::async_trait;
use cognee_llm::{AnthropicAdapter, Llm, Transcriber, build_openai_compatible_adapter};

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
        let adapter = build_openai_compatible_adapter(
            &ctx.llm.provider,
            &ctx.llm.model,
            &ctx.llm.api_key,
            &ctx.llm.endpoint,
            ctx.llm.max_retries,
        )
        .map_err(|e| ComponentError::Llm(e.to_string()))?
        .with_extra_args(ctx.llm.llm_args.clone());
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
        let adapter = build_openai_compatible_adapter(
            &ctx.llm.provider,
            &ctx.llm.model,
            &ctx.llm.api_key,
            &ctx.llm.endpoint,
            ctx.llm.max_retries,
        )
        .map_err(|e| ComponentError::Llm(e.to_string()))?;
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
        // Do NOT inherit `ctx.llm.endpoint`: it aliases OPENAI_URL (a
        // documented-required var in this repo), so flipping LLM_PROVIDER=anthropic
        // while OPENAI_URL is still set would POST every request to the OpenAI host
        // with an x-api-key header (404/401 on all traffic). Python's Anthropic
        // path passes no base_url either, so use the Anthropic default.
        let adapter = AnthropicAdapter::new(ctx.llm.model.clone(), ctx.llm.api_key.clone(), None)
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
        // The Anthropic Messages API has no Whisper-style transcription route, so
        // audio degrades gracefully to None (same as ollama/mistral/gemini).
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
