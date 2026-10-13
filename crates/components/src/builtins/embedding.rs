//! Built-in embedding engine factories, one per provider id.
//!
//! Each factory registers the config section its provider reads (`embedding.onnx`,
//! `embedding.openai_compatible`, `embedding.ollama`, `aws`), so a provider that
//! is not compiled in brings no settings. The settings every provider shares are
//! the `embedding` kind section ([`crate::sections::EmbeddingConfig`]).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::{EmbeddingConfig, EmbeddingEngine, EmbeddingProvider};

use crate::context::{BackendBuildContext, EmbeddingInputs};
use crate::error::ComponentError;
use crate::traits::EmbeddingFactory;

/// ONNX asset defaults for callers populating [`EmbeddingInputs`].
pub struct OnnxAssetDefaults {
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
    pub max_sequence_length: usize,
    pub batch_size: usize,
}

/// The embedding crate's own ONNX asset defaults — or inert values when this
/// crate's `onnx` feature is off.
///
/// Callers fill `EmbeddingInputs::onnx_*` but cannot see `OnnxEmbeddingConfig`
/// unless *they* enable `onnx`, and cfg'ing on their own feature is wrong:
/// features unify per build graph, so a caller compiled without `onnx` can still
/// end up in a graph where this crate has it on and [`build_embedding_config`]
/// reads those fields. Resolving the defaults here keeps the decision in the same
/// crate — and behind the same cfg — as the code that consumes them, so the inert
/// branch is provably unread rather than merely intended to be.
pub fn onnx_asset_defaults() -> OnnxAssetDefaults {
    #[cfg(feature = "onnx")]
    {
        let d = cognee_embedding::OnnxEmbeddingConfig::default();
        OnnxAssetDefaults {
            model_path: d.model_path,
            tokenizer_path: d.tokenizer_path,
            max_sequence_length: d.max_sequence_length,
            batch_size: d.batch_size,
        }
    }
    #[cfg(not(feature = "onnx"))]
    {
        OnnxAssetDefaults {
            model_path: PathBuf::new(),
            tokenizer_path: PathBuf::new(),
            max_sequence_length: 0,
            batch_size: 0,
        }
    }
}

/// Reject a non-empty but unrecognized provider id.
///
/// This is the guard that keeps [`build_embedding_config`]'s
/// `unwrap_or(EmbeddingProvider::Onnx)` from ever being reached with a typo:
/// without it, `EMBEDDING_PROVIDER=gemini` would silently select ONNX and
/// produce vectors from a completely different model than the caller asked
/// for — a misconfiguration that looks like a working setup until retrieval
/// quality is inexplicably wrong.
///
/// Called by [`crate::ComponentRegistry::build_embedding`]; kept separate so it is reachable from a
/// unit test: exercising it through the factory needs a fully-populated
/// `BackendBuildContext`, which is why this behaviour previously had no direct
/// coverage at all.
///
/// An empty id means "use the default" and is allowed. `mock` short-circuits
/// provider selection entirely, so the id is not consulted when mocking.
pub(crate) fn validate_provider_id(provider: &str, mock: bool) -> Result<(), ComponentError> {
    let provider = provider.trim();
    if !mock && !provider.is_empty() && parse_embedding_provider(provider).is_none() {
        return Err(ComponentError::EmbeddingEngine(format!(
            "unknown embedding provider '{provider}'. Supported: onnx, fastembed, \
             openai, openai_compatible, ollama, bedrock, mock."
        )));
    }
    Ok(())
}

/// Parse a provider id (case-insensitive) into an [`EmbeddingProvider`], or
/// `None` if it is not a recognized backend. This is the *single* source of
/// truth for the provider-id set: validity is `is_some()`, and config mapping
/// falls back to `onnx` via `unwrap_or`. The recognized ids mirror
/// `EmbeddingProvider`'s serde `snake_case` names.
fn parse_embedding_provider(provider: &str) -> Option<EmbeddingProvider> {
    match provider.trim().to_lowercase().as_str() {
        "onnx" => Some(EmbeddingProvider::Onnx),
        "fastembed" => Some(EmbeddingProvider::Fastembed),
        "openai" => Some(EmbeddingProvider::OpenAi),
        "openai_compatible" => Some(EmbeddingProvider::OpenAiCompatible),
        "ollama" => Some(EmbeddingProvider::Ollama),
        // Ungated on purpose: the id is a recognised backend regardless of the
        // build's features, so `EMBEDDING_PROVIDER=bedrock` is never rejected
        // here as a typo. A build without the `bedrock` feature registers no
        // `bedrock` factory, so the registry reports it as not available in
        // this build instead.
        "bedrock" => Some(EmbeddingProvider::Bedrock),
        "mock" => Some(EmbeddingProvider::Mock),
        _ => None,
    }
}

/// Map resolved [`EmbeddingInputs`] plus the provider sections in `sections`
/// to a `cognee_embedding::EmbeddingConfig`.
///
/// Empty and unrecognized provider strings map to `onnx`; the registry rejects a
/// non-empty unrecognized id before a factory is chosen, so a typo surfaces as an
/// error rather than a silent ONNX fallback. `MOCK_EMBEDDING` handling is
/// expressed through [`EmbeddingInputs::mock`] / [`EmbeddingInputs::mock_mode`].
#[allow(
    clippy::field_reassign_with_default,
    reason = "the `onnx` field is cfg-gated on the embedding crate's own feature \
              (which can be enabled independently of this crate's), so the config \
              must be default-constructed first and the fields assigned after"
)]
pub fn build_embedding_config(
    inputs: &EmbeddingInputs,
    sections: &cognee_config_core::ConfigValues,
) -> EmbeddingConfig {
    use cognee_config_core::ConfigSection as _;

    let provider = if inputs.mock {
        EmbeddingProvider::Mock
    } else {
        parse_embedding_provider(&inputs.provider).unwrap_or(EmbeddingProvider::Onnx)
    };

    // Start from the embedding crate's own defaults and override the fields we
    // resolve. Crucially, the `onnx` field's *existence* is gated on
    // `cognee-embedding`'s `onnx` feature, which can be enabled independently of
    // *this* crate's `onnx` feature under Cargo feature unification. Default-
    // constructing first fills that field when present (regardless of our own
    // feature), avoiding an `E0063 missing field 'onnx'` in mixed builds; we
    // then override it from the context only when our `onnx` feature is on.
    let mut config = EmbeddingConfig::default();
    config.provider = provider;
    config.model = inputs.model.clone();
    config.dimensions = inputs.dimensions;
    config.endpoint = inputs.endpoint.clone();
    config.api_key = inputs.api_key.clone();
    config.max_completion_tokens = inputs.max_completion_tokens;
    config.batch_size = inputs.batch_size;
    config.mock = inputs.mock;
    config.mock_mode = inputs.mock_mode;
    config.api_version = crate::sections::non_empty(
        &crate::sections::OpenAiCompatibleEmbeddingConfig::from_values(sections).api_version,
    );
    config.huggingface_tokenizer = crate::sections::non_empty(
        &crate::sections::OllamaEmbeddingConfig::from_values(sections).huggingface_tokenizer,
    );
    // Same feature-unification caveat as `onnx` below, with a benign
    // degradation: when `cognee-embedding/bedrock` is unified on while *this*
    // crate's `bedrock` feature is off, the field exists but stays `Default` —
    // which resolves region, credentials and endpoint entirely from the ambient
    // environment. That is exactly Python's embedding-path behaviour (plan
    // §1.5), so the only thing lost is an explicitly caller-supplied override.
    #[cfg(feature = "bedrock")]
    {
        config.aws = (&crate::sections::AwsConfig::from_values(sections).to_inputs()).into();
    }
    #[cfg(feature = "onnx")]
    {
        let onnx = crate::sections::OnnxEmbeddingConfig::from_values(sections);
        // Pooling and the query instruction are not knobs a caller picks: they
        // are what the named model was trained with, so they are derived from
        // the name rather than configured.
        let (pooling, query_instruction) =
            cognee_embedding::OnnxEmbeddingConfig::recipe_for(&inputs.model);
        config.onnx = cognee_embedding::OnnxEmbeddingConfig {
            model_path: PathBuf::from(onnx.model_path),
            tokenizer_path: PathBuf::from(onnx.tokenizer_path),
            model_name: inputs.model.clone(),
            dimensions: inputs.dimensions,
            max_sequence_length: inputs.max_sequence_length,
            batch_size: onnx.batch_size as usize,
            pooling,
            query_instruction,
        };
    }
    config
}

/// Install the process-wide embedding pacer before any engine is built, so the
/// adapters' `embedding_pacer()` lookups resolve. Without this the admission
/// points are dead and `EMBEDDING_RATE_LIMIT_ENABLED` does nothing. Idempotent
/// (first call wins), like the LLM pacer.
///
/// Called by [`crate::ComponentRegistry::build_embedding`] before it picks a
/// factory, so it covers externally registered factories too. The call in
/// [`DefaultEmbeddingFactory::build`] keeps a factory used on its own (outside
/// a registry) paced as well.
pub(crate) fn install_embedding_pacer(ctx: &BackendBuildContext) {
    cognee_utils::pacing::init_embedding_pacer(
        ctx.embedding.rate_limit_requests,
        std::time::Duration::from_secs(u64::from(ctx.embedding.rate_limit_interval)),
        ctx.embedding.rate_limit_enabled,
    );
}

/// Build the mock embedding engine (`MOCK_EMBEDDING` / provider `mock`).
pub(crate) async fn build_mock_embedding(
    ctx: &BackendBuildContext,
) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
    let mut inputs = ctx.embedding.clone();
    inputs.mock = true;
    build_embedding_config(&inputs, &ctx.sections)
        .create_engine()
        .await
        .map_err(|e| ComponentError::EmbeddingEngine(format!("embedding engine init failed: {e}")))
}

/// Built-in factory for one embedding provider id. Maps the context's
/// [`EmbeddingInputs`] and the provider's config section to a config and calls
/// `create_engine`.
pub struct DefaultEmbeddingFactory {
    provider: &'static str,
}

impl DefaultEmbeddingFactory {
    /// A factory serving `provider` (`onnx`, `fastembed`, `openai`,
    /// `openai_compatible`, `ollama` or `bedrock`).
    pub fn new(provider: &'static str) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl EmbeddingFactory for DefaultEmbeddingFactory {
    fn provider(&self) -> &str {
        self.provider
    }

    fn config_sections(&self) -> Vec<&'static cognee_config_core::SectionSchema> {
        use cognee_config_core::ConfigSection as _;
        match self.provider {
            "onnx" | "fastembed" => vec![crate::sections::OnnxEmbeddingConfig::schema()],
            "openai" | "openai_compatible" => {
                vec![crate::sections::OpenAiCompatibleEmbeddingConfig::schema()]
            }
            "ollama" => vec![crate::sections::OllamaEmbeddingConfig::schema()],
            "bedrock" => vec![crate::sections::AwsConfig::schema()],
            _ => Vec::new(),
        }
    }

    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        install_embedding_pacer(ctx);
        let mut inputs = ctx.embedding.clone();
        inputs.provider = self.provider.to_string();
        build_embedding_config(&inputs, &ctx.sections)
            .create_engine()
            .await
            .map_err(|e| {
                ComponentError::EmbeddingEngine(format!("embedding engine init failed: {e}"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_recognizes_all_ids_and_rejects_unknown() {
        for id in [
            "onnx",
            "fastembed",
            "openai",
            "openai_compatible",
            "ollama",
            "bedrock",
            "mock",
        ] {
            assert!(parse_embedding_provider(id).is_some(), "'{id}' must parse");
        }
        assert_eq!(
            parse_embedding_provider("  BEDROCK "),
            Some(EmbeddingProvider::Bedrock),
            "trimmed and case-insensitive, like every other id",
        );
        assert!(
            parse_embedding_provider("OpenAI").is_some(),
            "case-insensitive"
        );
        assert!(
            parse_embedding_provider("azure").is_none(),
            "unknown must not parse"
        );
        assert!(
            parse_embedding_provider("").is_none(),
            "empty is not a known id"
        );
    }

    fn inputs(provider: &str, mock: bool) -> EmbeddingInputs {
        EmbeddingInputs {
            provider: provider.to_string(),
            model: String::new(),
            dimensions: 384,
            endpoint: None,
            api_key: None,
            batch_size: 36,
            rate_limit_enabled: false,
            rate_limit_requests: 60,
            rate_limit_interval: 60,
            mock,
            mock_mode: Default::default(),
            max_completion_tokens: 8191,
            max_sequence_length: 512,
        }
    }

    /// The guard that makes `build_embedding_config`'s ONNX fallback
    /// unreachable for a typo. Deleting it would turn
    /// `EMBEDDING_PROVIDER=gemini` into a silent ONNX selection, and until this
    /// test existed nothing would have gone red — the sibling test below
    /// asserts the *fallback*, which is the behaviour this guard prevents from
    /// mattering.
    #[test]
    fn unknown_provider_id_is_rejected_not_silently_defaulted() {
        let msg = match validate_provider_id("gemini", false) {
            Err(e) => e.to_string(),
            Ok(()) => panic!("a non-empty unrecognized provider must be rejected"),
        };
        assert!(
            msg.contains("unknown embedding provider 'gemini'"),
            "the error must name the offending id; got: {msg}"
        );
        assert!(
            msg.contains("openai_compatible"),
            "the error must list the supported ids so the fix is obvious; got: {msg}"
        );

        // Surrounding whitespace must not smuggle a bad id past the check.
        assert!(validate_provider_id("  gemini  ", false).is_err());

        // Empty means "use the default", and mock short-circuits the id.
        assert!(validate_provider_id("", false).is_ok());
        assert!(validate_provider_id("gemini", true).is_ok());

        // Every id the parser accepts must pass the guard, or the two would
        // disagree and a supported backend would be rejected.
        for id in [
            "onnx",
            "fastembed",
            "openai",
            "openai_compatible",
            "ollama",
            "bedrock",
            "mock",
        ] {
            assert!(
                validate_provider_id(id, false).is_ok(),
                "'{id}' parses, so the guard must accept it"
            );
        }
    }

    #[test]
    fn config_maps_unknown_to_onnx_but_mock_wins() {
        // build_embedding_config maps an unknown provider to onnx (the factory
        // is what rejects it); mock overrides the provider regardless.
        assert_eq!(
            build_embedding_config(&inputs("azure", false), &Default::default()).provider,
            EmbeddingProvider::Onnx
        );
        assert_eq!(
            build_embedding_config(&inputs("openai", true), &Default::default()).provider,
            EmbeddingProvider::Mock
        );
        assert_eq!(
            build_embedding_config(&inputs("ollama", false), &Default::default()).provider,
            EmbeddingProvider::Ollama
        );
        assert_eq!(
            build_embedding_config(&inputs("bedrock", false), &Default::default()).provider,
            EmbeddingProvider::Bedrock
        );
    }

    /// The carry-across in `build_embedding_config`: without it the Bedrock
    /// engine silently resolves everything from the ambient environment and an
    /// explicitly supplied credential is dropped.
    #[test]
    #[cfg(feature = "bedrock")]
    fn aws_inputs_are_carried_into_the_embedding_config() {
        let inputs = inputs("bedrock", false);
        let sections: cognee_config_core::ConfigValues = [
            ("aws.region", "eu-central-1"),
            ("aws.access_key_id", "AKIDEXAMPLE"),
            ("aws.secret_access_key", "secret"),
            // An alias works too: a raw map may not be normalized yet.
            ("aws_profile_name", "cognee"),
            ("aws.bedrock_runtime_endpoint", "https://vpce.example"),
            ("aws.bearer_token", "bedrock-api-key"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), serde_json::json!(v)))
        .collect();

        let config = build_embedding_config(&inputs, &sections);

        let aws = <crate::sections::AwsConfig as cognee_config_core::ConfigSection>::from_values(
            &sections,
        )
        .to_inputs();
        let expected: cognee_llm::adapters::bedrock::aws::env::AwsInputs = (&aws).into();
        assert_eq!(config.aws, expected);
        assert_eq!(config.aws.region.as_deref(), Some("eu-central-1"));
        assert_eq!(config.aws.access_key_id.as_deref(), Some("AKIDEXAMPLE"));
        assert_eq!(config.aws.profile_name.as_deref(), Some("cognee"));
        assert_eq!(
            config.aws.bedrock_runtime_endpoint.as_deref(),
            Some("https://vpce.example")
        );
        assert_eq!(config.aws.bearer_token.as_deref(), Some("bedrock-api-key"));
    }
}
