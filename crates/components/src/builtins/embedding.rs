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

/// Whether `provider` is a recognized embedding backend id (case-insensitive).
/// An empty string is *not* recognized here; callers treat empty as "use the
/// onnx default", while a non-empty unrecognized value is a misconfiguration.
fn is_known_embedding_provider(provider: &str) -> bool {
    matches!(
        provider.trim().to_lowercase().as_str(),
        "onnx" | "fastembed" | "openai" | "openai_compatible" | "ollama" | "mock"
    )
}

/// Map resolved [`EmbeddingInputs`] to a `cognee_embedding::EmbeddingConfig`.
///
/// Empty and unrecognized provider strings map to `onnx`; the factory validates
/// non-empty unrecognized values separately (see [`DefaultEmbeddingFactory`]) so
/// a typo surfaces as an error rather than a silent ONNX fallback.
/// `MOCK_EMBEDDING` handling is expressed through [`EmbeddingInputs::mock`] /
/// [`EmbeddingInputs::mock_deterministic`], which the caller populates.
pub fn build_embedding_config(inputs: &EmbeddingInputs) -> EmbeddingConfig {
    let provider = match inputs.provider.trim().to_lowercase().as_str() {
        "onnx" => EmbeddingProvider::Onnx,
        "fastembed" => EmbeddingProvider::Fastembed,
        "openai" => EmbeddingProvider::OpenAi,
        "openai_compatible" => EmbeddingProvider::OpenAiCompatible,
        "ollama" => EmbeddingProvider::Ollama,
        "mock" => EmbeddingProvider::Mock,
        _ => EmbeddingProvider::Onnx,
    };

    let mock_mode = if inputs.mock_deterministic {
        MockVectorMode::Deterministic
    } else {
        MockVectorMode::Zero
    };

    EmbeddingConfig {
        provider: if inputs.mock {
            EmbeddingProvider::Mock
        } else {
            provider
        },
        model: inputs.model.clone(),
        dimensions: inputs.dimensions,
        endpoint: inputs.endpoint.clone(),
        api_key: inputs.api_key.clone(),
        api_version: inputs.api_version.clone(),
        max_completion_tokens: inputs.max_completion_tokens,
        batch_size: inputs.batch_size,
        mock: inputs.mock,
        mock_mode,
        #[cfg(feature = "onnx")]
        onnx: cognee_embedding::OnnxEmbeddingConfig {
            model_path: inputs.onnx_model_path.clone(),
            tokenizer_path: inputs.onnx_tokenizer_path.clone(),
            model_name: inputs.onnx_model_name.clone(),
            dimensions: inputs.onnx_dimensions,
            max_sequence_length: inputs.onnx_max_sequence_length,
            batch_size: inputs.onnx_batch_size,
        },
        huggingface_tokenizer: inputs.huggingface_tokenizer.clone(),
    }
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
        if !ctx.embedding.mock && !provider.is_empty() && !is_known_embedding_provider(provider) {
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
