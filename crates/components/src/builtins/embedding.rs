//! Built-in embedding engine factory.
//!
//! Provider selection happens inside `EmbeddingConfig::create_engine`, so there
//! is a single default embedding factory (replaceable via
//! [`crate::ComponentRegistry::set_embedding`]) rather than a per-provider map.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::{EmbeddingConfig, EmbeddingEngine, EmbeddingProvider, MockVectorMode};

use crate::context::{BackendBuildContext, EmbeddingInputs};
use crate::error::ComponentError;
use crate::traits::EmbeddingFactory;

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
        "mock" => Some(EmbeddingProvider::Mock),
        _ => None,
    }
}

/// Map resolved [`EmbeddingInputs`] to a `cognee_embedding::EmbeddingConfig`.
///
/// Empty and unrecognized provider strings map to `onnx`; the factory validates
/// non-empty unrecognized values separately (see [`DefaultEmbeddingFactory`]) so
/// a typo surfaces as an error rather than a silent ONNX fallback.
/// `MOCK_EMBEDDING` handling is expressed through [`EmbeddingInputs::mock`] /
/// [`EmbeddingInputs::mock_deterministic`], which the caller populates.
#[allow(
    clippy::field_reassign_with_default,
    reason = "the `onnx` field is cfg-gated on the embedding crate's own feature \
              (which can be enabled independently of this crate's), so the config \
              must be default-constructed first and the fields assigned after"
)]
pub fn build_embedding_config(inputs: &EmbeddingInputs) -> EmbeddingConfig {
    let provider = if inputs.mock {
        EmbeddingProvider::Mock
    } else {
        parse_embedding_provider(&inputs.provider).unwrap_or(EmbeddingProvider::Onnx)
    };

    let mock_mode = if inputs.mock_deterministic {
        MockVectorMode::Deterministic
    } else {
        MockVectorMode::Zero
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
    config.api_version = inputs.api_version.clone();
    config.max_completion_tokens = inputs.max_completion_tokens;
    config.batch_size = inputs.batch_size;
    config.mock = inputs.mock;
    config.mock_mode = mock_mode;
    config.huggingface_tokenizer = inputs.huggingface_tokenizer.clone();
    #[cfg(feature = "onnx")]
    {
        config.onnx = cognee_embedding::OnnxEmbeddingConfig {
            model_path: inputs.onnx_model_path.clone(),
            tokenizer_path: inputs.onnx_tokenizer_path.clone(),
            model_name: inputs.onnx_model_name.clone(),
            dimensions: inputs.onnx_dimensions,
            max_sequence_length: inputs.onnx_max_sequence_length,
            batch_size: inputs.onnx_batch_size,
        };
    }
    config
}

/// Default embedding factory — maps the context's [`EmbeddingInputs`] to a
/// config and calls `create_engine`.
pub struct DefaultEmbeddingFactory;

#[async_trait]
impl EmbeddingFactory for DefaultEmbeddingFactory {
    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        // A non-empty but unrecognized provider is a misconfiguration (typo /
        // unsupported backend): surface it instead of silently falling back to
        // ONNX. Empty means "use the default" and is allowed. `mock` short-
        // circuits provider selection, so skip the check when mocking.
        let provider = ctx.embedding.provider.trim();
        if !ctx.embedding.mock
            && !provider.is_empty()
            && parse_embedding_provider(provider).is_none()
        {
            return Err(ComponentError::EmbeddingEngine(format!(
                "unknown embedding provider '{provider}'. Supported: onnx, fastembed, \
                 openai, openai_compatible, ollama, mock."
            )));
        }
        let config = build_embedding_config(&ctx.embedding);
        config.create_engine().await.map_err(|e| {
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
            "mock",
        ] {
            assert!(parse_embedding_provider(id).is_some(), "'{id}' must parse");
        }
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
            mock,
            mock_deterministic: false,
            api_version: None,
            huggingface_tokenizer: None,
            max_completion_tokens: 8191,
            onnx_model_path: std::path::PathBuf::new(),
            onnx_tokenizer_path: std::path::PathBuf::new(),
            onnx_model_name: String::new(),
            onnx_dimensions: 384,
            onnx_max_sequence_length: 512,
            onnx_batch_size: 32,
        }
    }

    #[test]
    fn config_maps_unknown_to_onnx_but_mock_wins() {
        // build_embedding_config maps an unknown provider to onnx (the factory
        // is what rejects it); mock overrides the provider regardless.
        assert_eq!(
            build_embedding_config(&inputs("azure", false)).provider,
            EmbeddingProvider::Onnx
        );
        assert_eq!(
            build_embedding_config(&inputs("openai", true)).provider,
            EmbeddingProvider::Mock
        );
        assert_eq!(
            build_embedding_config(&inputs("ollama", false)).provider,
            EmbeddingProvider::Ollama
        );
    }
}
