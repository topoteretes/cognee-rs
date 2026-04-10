use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::engine::EmbeddingEngine;
use crate::error::{EmbeddingError, EmbeddingResult};
use crate::onnx::OnnxEmbeddingEngine;
use crate::openai_compatible::OpenAICompatibleEmbeddingEngine;
use crate::provider::EmbeddingProvider;

/// ONNX-specific configuration.
///
/// Only used when `EmbeddingConfig::provider` is `Onnx` or `Fastembed`.
/// All other providers use the top-level `EmbeddingConfig` fields only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnnxEmbeddingConfig {
    /// Path to ONNX model file (.onnx)
    pub model_path: PathBuf,

    /// Path to tokenizer.json file
    pub tokenizer_path: PathBuf,

    /// Model name for logging/identification and auto-download selection
    pub model_name: String,

    /// Embedding dimensions (must match model output)
    pub dimensions: usize,

    /// Maximum sequence length in tokens (truncate if longer)
    pub max_sequence_length: usize,

    /// Batch size for ONNX inference (max texts per inference call)
    pub batch_size: usize,
}

impl Default for OnnxEmbeddingConfig {
    fn default() -> Self {
        Self::bge_small("./target/models")
    }
}

impl OnnxEmbeddingConfig {
    /// Create config for BGE-Small-v1.5 model
    pub fn bge_small(model_dir: impl Into<PathBuf>) -> Self {
        let base = model_dir.into();
        let model_path = base.join("BGE-Small-v1.5-model_quantized.onnx");
        let tokenizer_path = base.join("bge-small-tokenizer.json");
        Self {
            model_path,
            tokenizer_path,
            model_name: "BGE-Small-v1.5".to_string(),
            dimensions: 384,
            max_sequence_length: 512,
            batch_size: 32,
        }
    }

    /// Create config for all-MiniLM-L6-v2 model
    pub fn minilm_l6(model_dir: impl Into<PathBuf>) -> Self {
        let base = model_dir.into();
        let model_path = base.join("all-MiniLM-L6-v2.onnx");
        let tokenizer_path = base.join("minilm-l6-tokenizer.json");
        Self {
            model_path,
            tokenizer_path,
            model_name: "all-MiniLM-L6-v2".to_string(),
            dimensions: 384,
            max_sequence_length: 256,
            batch_size: 32,
        }
    }
}

/// Unified embedding configuration.
///
/// Provider-agnostic; holds fields for all supported backends.
/// Load from environment variables via [`EmbeddingConfig::from_env`], or construct
/// programmatically and pass to [`EmbeddingConfig::create_engine`].
///
/// Environment variables (match Python SDK names):
/// - `EMBEDDING_PROVIDER` — backend selection (default: `onnx`)
/// - `MOCK_EMBEDDING` — set to `true`/`1`/`yes` to force mock mode
/// - `EMBEDDING_MODEL` — model identifier
/// - `EMBEDDING_DIMENSIONS` — vector size
/// - `EMBEDDING_ENDPOINT` — API endpoint URL
/// - `EMBEDDING_API_KEY` — API key (fallback: `LLM_API_KEY`)
/// - `EMBEDDING_API_VERSION` — API version string
/// - `EMBEDDING_MAX_COMPLETION_TOKENS` — maximum tokens (default: 8191)
/// - `EMBEDDING_BATCH_SIZE` — texts per batch (default: 36)
/// - `HUGGINGFACE_TOKENIZER` — HuggingFace tokenizer identifier
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Which backend to use for embedding generation.
    pub provider: EmbeddingProvider,

    /// Model identifier. For ONNX this is informational; for API providers this is sent in
    /// the request body. Default depends on provider (BGE-Small-v1.5 for ONNX, empty for others).
    pub model: String,

    /// Embedding vector dimensionality. Must match the model output.
    pub dimensions: usize,

    /// API endpoint URL (used by OpenAI-compatible and Ollama providers).
    pub endpoint: Option<String>,

    /// API key. Reads `EMBEDDING_API_KEY` first, falls back to `LLM_API_KEY`.
    pub api_key: Option<String>,

    /// API version string (e.g. "2023-05-15" for Azure OpenAI).
    pub api_version: Option<String>,

    /// Maximum tokens for completion requests (default: 8191).
    pub max_completion_tokens: usize,

    /// Number of texts to send in a single embedding request (default: 36).
    pub batch_size: usize,

    /// If true, use mock zero-vector embeddings regardless of `provider`.
    /// Overrides `provider` to `Mock`. Set via `MOCK_EMBEDDING=true`.
    pub mock: bool,

    /// ONNX-specific configuration. Only consulted when provider is `Onnx` or `Fastembed`.
    pub onnx: OnnxEmbeddingConfig,

    /// HuggingFace tokenizer identifier for chunking token counting.
    /// When set, used by `HuggingFaceTokenCounter` in the chunking crate.
    pub huggingface_tokenizer: Option<String>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        let onnx = OnnxEmbeddingConfig::default();
        let dimensions = onnx.dimensions;
        let model = onnx.model_name.clone();
        Self {
            provider: EmbeddingProvider::Onnx,
            model,
            dimensions,
            endpoint: None,
            api_key: None,
            api_version: None,
            max_completion_tokens: 8191,
            batch_size: 36,
            mock: false,
            onnx,
            huggingface_tokenizer: None,
        }
    }
}

impl EmbeddingConfig {
    /// Load configuration from environment variables.
    ///
    /// Reads the same env var names as the Python SDK so that a shared `.env` file
    /// works across both implementations without modification.
    pub fn from_env() -> Self {
        let mut config = Self::default();

        // Parse MOCK_EMBEDDING first — it overrides everything else if set
        if let Ok(val) = std::env::var("MOCK_EMBEDDING") {
            let val = val.trim().to_lowercase();
            if val == "true" || val == "1" || val == "yes" {
                config.mock = true;
                config.provider = EmbeddingProvider::Mock;
                return config;
            }
        }

        // Parse EMBEDDING_PROVIDER
        if let Ok(val) = std::env::var("EMBEDDING_PROVIDER") {
            let val = val.trim().to_lowercase();
            match val.as_str() {
                "onnx" => config.provider = EmbeddingProvider::Onnx,
                "fastembed" => config.provider = EmbeddingProvider::Fastembed,
                "openai" => config.provider = EmbeddingProvider::OpenAi,
                "openai_compatible" => config.provider = EmbeddingProvider::OpenAiCompatible,
                "ollama" => config.provider = EmbeddingProvider::Ollama,
                "mock" => {
                    config.mock = true;
                    config.provider = EmbeddingProvider::Mock;
                }
                _ => {
                    // Unknown provider — leave default (Onnx) and log nothing;
                    // the caller will get a clear error from create_engine() if needed.
                }
            }
        }

        // EMBEDDING_MODEL
        if let Ok(val) = std::env::var("EMBEDDING_MODEL") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                config.model = val;
            }
        }

        // EMBEDDING_DIMENSIONS
        if let Ok(val) = std::env::var("EMBEDDING_DIMENSIONS") {
            if let Ok(n) = val.trim().parse::<usize>() {
                config.dimensions = n;
            }
        }

        // EMBEDDING_ENDPOINT
        if let Ok(val) = std::env::var("EMBEDDING_ENDPOINT") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                config.endpoint = Some(val);
            }
        }

        // EMBEDDING_API_KEY, fallback to LLM_API_KEY
        if let Ok(val) = std::env::var("EMBEDDING_API_KEY") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                config.api_key = Some(val);
            }
        } else if let Ok(val) = std::env::var("LLM_API_KEY") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                config.api_key = Some(val);
            }
        }

        // EMBEDDING_API_VERSION
        if let Ok(val) = std::env::var("EMBEDDING_API_VERSION") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                config.api_version = Some(val);
            }
        }

        // EMBEDDING_MAX_COMPLETION_TOKENS
        if let Ok(val) = std::env::var("EMBEDDING_MAX_COMPLETION_TOKENS") {
            if let Ok(n) = val.trim().parse::<usize>() {
                config.max_completion_tokens = n;
            }
        }

        // EMBEDDING_BATCH_SIZE
        if let Ok(val) = std::env::var("EMBEDDING_BATCH_SIZE") {
            if let Ok(n) = val.trim().parse::<usize>() {
                config.batch_size = n;
            }
        }

        // HUGGINGFACE_TOKENIZER
        if let Ok(val) = std::env::var("HUGGINGFACE_TOKENIZER") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                config.huggingface_tokenizer = Some(val);
            }
        }

        config
    }

    /// Returns the effective provider, substituting Mock when `self.mock` is true.
    pub fn effective_provider(&self) -> EmbeddingProvider {
        if self.mock {
            EmbeddingProvider::Mock
        } else {
            self.provider.clone()
        }
    }

    /// Create an embedding engine based on this configuration.
    ///
    /// Dispatches to the appropriate engine implementation based on
    /// [`EmbeddingConfig::effective_provider`]. Providers not yet implemented
    /// return [`EmbeddingError::NotImplemented`].
    pub async fn create_engine(&self) -> EmbeddingResult<Arc<dyn EmbeddingEngine>> {
        match self.effective_provider() {
            EmbeddingProvider::Onnx | EmbeddingProvider::Fastembed => {
                let engine = OnnxEmbeddingEngine::with_auto_download(self.onnx.clone()).await?;
                Ok(Arc::new(engine))
            }
            EmbeddingProvider::OpenAi | EmbeddingProvider::OpenAiCompatible => {
                let engine = OpenAICompatibleEmbeddingEngine::new(self)?;
                Ok(Arc::new(engine))
            }
            other => Err(EmbeddingError::NotImplemented(format!(
                "Provider {:?} is not yet implemented",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_onnx() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.provider, EmbeddingProvider::Onnx);
        assert_eq!(config.dimensions, 384);
        assert_eq!(config.batch_size, 36);
        assert_eq!(config.max_completion_tokens, 8191);
        assert!(!config.mock);
    }

    #[test]
    fn test_effective_provider_mock_override() {
        let mut config = EmbeddingConfig::default();
        config.mock = true;
        assert_eq!(config.effective_provider(), EmbeddingProvider::Mock);
    }

    #[test]
    fn test_effective_provider_passthrough() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.effective_provider(), EmbeddingProvider::Onnx);
    }

    // env-var tests mutate global process state and must not run in parallel.
    // Run with: cargo test -p cognee-embedding -- --test-threads=1 --ignored
    // or simply: cargo test -p cognee-embedding -- --include-ignored --test-threads=1

    #[test]
    #[ignore = "mutates global env vars; run with --test-threads=1 --ignored"]
    fn test_from_env_mock_embedding_true() {
        // SAFETY: env var mutation is safe when no other threads read env vars concurrently.
        // Gated behind #[ignore] to prevent races in the default parallel test runner.
        unsafe { std::env::set_var("MOCK_EMBEDDING", "true") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("MOCK_EMBEDDING") };
        assert!(config.mock);
        assert_eq!(config.effective_provider(), EmbeddingProvider::Mock);
    }

    #[test]
    #[ignore = "mutates global env vars; run with --test-threads=1 --ignored"]
    fn test_from_env_mock_embedding_numeric() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("MOCK_EMBEDDING", "1") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("MOCK_EMBEDDING") };
        assert!(config.mock);
    }

    #[test]
    #[ignore = "mutates global env vars; run with --test-threads=1 --ignored"]
    fn test_from_env_provider() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("EMBEDDING_PROVIDER", "openai") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("EMBEDDING_PROVIDER") };
        assert_eq!(config.provider, EmbeddingProvider::OpenAi);
    }

    #[test]
    #[ignore = "mutates global env vars; run with --test-threads=1 --ignored"]
    fn test_from_env_fastembed_alias() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("EMBEDDING_PROVIDER", "fastembed") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("EMBEDDING_PROVIDER") };
        assert_eq!(config.provider, EmbeddingProvider::Fastembed);
    }

    #[test]
    #[ignore = "mutates global env vars; run with --test-threads=1 --ignored"]
    fn test_from_env_dimensions() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("EMBEDDING_DIMENSIONS", "1536") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("EMBEDDING_DIMENSIONS") };
        assert_eq!(config.dimensions, 1536);
    }

    #[test]
    #[ignore = "mutates global env vars; run with --test-threads=1 --ignored"]
    fn test_from_env_api_key_fallback() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::remove_var("EMBEDDING_API_KEY") };
        unsafe { std::env::set_var("LLM_API_KEY", "my-llm-key") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("LLM_API_KEY") };
        assert_eq!(config.api_key, Some("my-llm-key".to_string()));
    }

    #[test]
    #[ignore = "mutates global env vars; run with --test-threads=1 --ignored"]
    fn test_from_env_api_key_prefers_embedding() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("EMBEDDING_API_KEY", "embed-key") };
        unsafe { std::env::set_var("LLM_API_KEY", "llm-key") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("EMBEDDING_API_KEY") };
        unsafe { std::env::remove_var("LLM_API_KEY") };
        assert_eq!(config.api_key, Some("embed-key".to_string()));
    }

    #[test]
    fn test_onnx_config_bge_small() {
        let cfg = OnnxEmbeddingConfig::bge_small("/models");
        assert_eq!(cfg.dimensions, 384);
        assert_eq!(cfg.max_sequence_length, 512);
        assert_eq!(cfg.model_name, "BGE-Small-v1.5");
    }

    #[test]
    fn test_onnx_config_minilm_l6() {
        let cfg = OnnxEmbeddingConfig::minilm_l6("/models");
        assert_eq!(cfg.dimensions, 384);
        assert_eq!(cfg.max_sequence_length, 256);
        assert_eq!(cfg.model_name, "all-MiniLM-L6-v2");
    }
}
