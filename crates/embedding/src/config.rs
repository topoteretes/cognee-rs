use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::engine::EmbeddingEngine;
use crate::error::EmbeddingResult;
use crate::mock::{MockEmbeddingEngine, MockVectorMode};
use crate::ollama::OllamaEmbeddingEngine;
use crate::openai_compatible::OpenAICompatibleEmbeddingEngine;
use crate::provider::EmbeddingProvider;

#[cfg(feature = "onnx")]
use crate::onnx::OnnxEmbeddingEngine;
#[cfg(feature = "onnx")]
use std::path::PathBuf;

/// Fallback dimension when the model is unknown AND `EMBEDDING_DIMENSIONS` is unset.
///
/// 384 matches the ONNX BGE-Small edge model (Android default). On non-Android,
/// the default model (`text-embedding-3-small` → 1536) resolves via the
/// [`known_model_dimensions`] table, so this fallback is only hit for truly
/// unknown models.
///
/// **Note:** Python uses 3072 as its fallback (matching the OpenAI default).
/// Rust deliberately uses 384 because the Rust edge default is BGE-Small, not
/// `text-embedding-3-large`.
const FALLBACK_DIMENSIONS: usize = 384;

/// Best-effort lookup of the output vector dimension for a known embedding model.
///
/// Mirrors the dimension table that Python resolves dynamically via the
/// `litellm` and `fastembed` registries (`_resolve_embedding_dimensions` in
/// `cognee/infrastructure/databases/vector/embeddings/config.py`). Rust
/// hardcodes a small, curated table instead of depending on those Python
/// packages.
///
/// Resolution rules (matches Python semantics):
/// - Strips a leading provider segment: `"openai/text-embedding-3-large"` →
///   `"text-embedding-3-large"` (uses the last `/`-separated component).
/// - Matching is **case-insensitive**.
/// - Returns `None` for unknown models so the caller can fall back with a
///   warning rather than silently using a wrong dimension.
///
/// The `provider` argument is accepted for forward-compatibility (future
/// provider-scoped overrides) but is not used in the current table.
pub fn known_model_dimensions(provider: EmbeddingProvider, model: &str) -> Option<usize> {
    // Strip a provider prefix: "openai/text-embedding-3-large" -> "text-embedding-3-large"
    // rsplit('/').next() is infallible for any &str (always yields ≥ 1 element).
    let bare = model.rsplit('/').next().unwrap_or(model);
    let key = bare.to_ascii_lowercase();
    let dim = match key.as_str() {
        // --- OpenAI models (verified via litellm registry) ---
        "text-embedding-3-large" => 3072,
        "text-embedding-3-small" => 1536,
        "text-embedding-ada-002" => 1536,
        // --- BGE family (fastembed registry + ONNX defaults) ---
        "bge-small-v1.5" | "bge-small-en-v1.5" => 384,
        "bge-base-en-v1.5" => 768,
        "bge-large-en-v1.5" => 1024,
        // --- MiniLM ---
        "all-minilm-l6-v2" => 384,
        // --- Common Ollama models ---
        "nomic-embed-text" => 768,
        "mxbai-embed-large" => 1024,
        _ => return None,
    };
    let _ = provider; // provider currently unused; kept for future provider-scoped dims
    Some(dim)
}

/// ONNX-specific configuration.
///
/// Only used when `EmbeddingConfig::provider` is `Onnx` or `Fastembed`.
/// All other providers use the top-level `EmbeddingConfig` fields only.
#[cfg(feature = "onnx")]
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

#[cfg(feature = "onnx")]
impl Default for OnnxEmbeddingConfig {
    fn default() -> Self {
        Self::bge_small("./target/models")
    }
}

#[cfg(feature = "onnx")]
impl OnnxEmbeddingConfig {
    /// Create config for BGE-Small-v1.5 model
    pub fn bge_small(model_dir: impl Into<PathBuf>) -> Self {
        let base = model_dir.into();
        let model_path = base.join("BGE-Small-v1.5-model_quantized.onnx");
        let tokenizer_path = base.join("bge-small-tokenizer.json");
        Self {
            model_path,
            tokenizer_path,
            model_name: "bge-small-en-v1.5".to_string(),
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
/// - `EMBEDDING_PROVIDER` — backend selection (default: `openai`; `onnx` on Android)
/// - `MOCK_EMBEDDING` — set to `true`/`1`/`yes` to force mock mode
/// - `EMBEDDING_MODEL` — model identifier
/// - `EMBEDDING_DIMENSIONS` — vector size
/// - `EMBEDDING_ENDPOINT` — API endpoint URL
/// - `EMBEDDING_API_KEY` — API key (fallback: `LLM_API_KEY`)
/// - `EMBEDDING_API_VERSION` — API version string
/// - `EMBEDDING_MAX_COMPLETION_TOKENS` — maximum tokens (default: 8191)
/// - `EMBEDDING_BATCH_SIZE` — texts per embedding request (default: 36)
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
    ///
    /// Matches the Python SDK and stays within the small client-batch limits of
    /// the self-hosted servers this adapter targets (e.g. TEI defaults to 32).
    /// Raise it via `EMBEDDING_BATCH_SIZE` for providers that accept larger
    /// batches. For the OpenAI-compatible engine, up to `MAX_CONCURRENT_BATCHES`
    /// sub-batches are also dispatched concurrently.
    pub batch_size: usize,

    /// If true, use mock embeddings regardless of `provider`.
    /// Overrides `provider` to `Mock`. Set via `MOCK_EMBEDDING=true`.
    pub mock: bool,

    /// How the mock engine generates vectors when `provider` is `Mock`.
    /// Defaults to [`MockVectorMode::Zero`]. Set via `MOCK_EMBEDDING=deterministic`
    /// to derive content-stable vectors from `sha256(text)`.
    #[serde(default)]
    pub mock_mode: MockVectorMode,

    /// ONNX-specific configuration. Only consulted when provider is `Onnx` or `Fastembed`.
    #[cfg(feature = "onnx")]
    pub onnx: OnnxEmbeddingConfig,

    /// HuggingFace tokenizer identifier for chunking token counting.
    /// When set, used by `HuggingFaceTokenCounter` in the chunking crate.
    pub huggingface_tokenizer: Option<String>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        // On Android, local ONNX inference is the right default (edge deployment).
        // Everywhere else, match the Python SDK default: OpenAI text-embedding-3-small.
        #[cfg(all(feature = "onnx", target_os = "android"))]
        let (provider, model, dimensions, endpoint) = {
            let onnx_cfg = OnnxEmbeddingConfig::default();
            (
                EmbeddingProvider::Onnx,
                onnx_cfg.model_name.clone(),
                onnx_cfg.dimensions,
                None,
            )
        };
        #[cfg(all(feature = "onnx", not(target_os = "android")))]
        let (provider, model, dimensions, endpoint) = {
            let m = "text-embedding-3-small".to_string();
            // Resolve via the known-model table so there is one source of truth.
            let d = known_model_dimensions(EmbeddingProvider::OpenAi, &m)
                .unwrap_or(FALLBACK_DIMENSIONS);
            (
                EmbeddingProvider::OpenAi,
                m,
                d,
                Some("https://api.openai.com/v1".to_string()),
            )
        };
        #[cfg(not(feature = "onnx"))]
        let (provider, model, dimensions, endpoint) = {
            let m = "text-embedding-3-small".to_string();
            // Resolve via the known-model table so there is one source of truth.
            let d = known_model_dimensions(EmbeddingProvider::OpenAi, &m)
                .unwrap_or(FALLBACK_DIMENSIONS);
            (
                EmbeddingProvider::OpenAi,
                m,
                d,
                Some("https://api.openai.com/v1".to_string()),
            )
        };

        Self {
            provider,
            model,
            dimensions,
            endpoint,
            api_key: None,
            api_version: None,
            max_completion_tokens: 8191,
            batch_size: 36,
            mock: false,
            mock_mode: MockVectorMode::Zero,
            #[cfg(feature = "onnx")]
            onnx: OnnxEmbeddingConfig::default(),
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

        // Parse MOCK_EMBEDDING first — it overrides everything else if set.
        // `deterministic` (or `hash`) selects the SHA-256-derived deterministic
        // mode; other truthy values keep the legacy zero-vector mode.
        if let Ok(val) = std::env::var("MOCK_EMBEDDING") {
            let val = val.trim().to_lowercase();
            if val == "deterministic" || val == "hash" {
                config.mock = true;
                config.provider = EmbeddingProvider::Mock;
                config.mock_mode = MockVectorMode::Deterministic;
                return config;
            }
            if val == "true" || val == "1" || val == "yes" {
                config.mock = true;
                config.provider = EmbeddingProvider::Mock;
                config.mock_mode = MockVectorMode::Zero;
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
                    // Unknown provider — leave the platform default (OpenAI, or
                    // ONNX on Android) and log nothing; the caller will get a
                    // clear error from create_engine() if needed.
                }
            }
        }

        // Apply provider-specific model defaults before checking env var overrides.
        // This ensures that when a user switches to EMBEDDING_PROVIDER=ollama
        // without setting EMBEDDING_MODEL explicitly, they get a sensible Ollama
        // default model name rather than the ONNX model name.
        // (Dimension is resolved below via the known-model table, not hardcoded here.)
        if config.provider == EmbeddingProvider::Ollama {
            config.model = "avr/sfr-embedding-mistral:latest".to_string();
        }

        // EMBEDDING_MODEL
        if let Ok(val) = std::env::var("EMBEDDING_MODEL") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                config.model = val;
            }
        }

        // EMBEDDING_DIMENSIONS — resolution order (mirrors Python model_post_init):
        //   1. Explicit EMBEDDING_DIMENSIONS env var — always wins.
        //   2. known_model_dimensions(provider, model) — table lookup.
        //   3. Fallback FALLBACK_DIMENSIONS (384) with a tracing::warn! so the user
        //      knows to set EMBEDDING_DIMENSIONS explicitly for unknown models.
        // For ONNX the model file dictates the true dimension, so we prefer the
        // onnx_cfg.dimensions unless the user set EMBEDDING_DIMENSIONS explicitly.
        let explicit_dims = std::env::var("EMBEDDING_DIMENSIONS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok());

        // Resolve via the known-model table, falling back to FALLBACK_DIMENSIONS
        // with a warning when the model is unknown (parity with Python
        // model_post_init). Used for every provider except ONNX/Fastembed, whose
        // dimension is dictated by the model file (handled below).
        let resolve_from_table = |config: &EmbeddingConfig| match known_model_dimensions(
            config.provider.clone(),
            &config.model,
        ) {
            // 2. Known model — derived dimension.
            Some(d) => d,
            // 3. Unknown model — fallback with warning.
            None => {
                tracing::warn!(
                    provider = ?config.provider,
                    model = %config.model,
                    fallback = FALLBACK_DIMENSIONS,
                    "Could not auto-derive embedding dimensions; set \
                     EMBEDDING_DIMENSIONS explicitly if your embedder produces \
                     a different vector size, otherwise the first vector write \
                     will fail with a shape mismatch."
                );
                FALLBACK_DIMENSIONS
            }
        };

        config.dimensions = match explicit_dims {
            // 1. Explicit override always wins.
            Some(d) => d,
            None => {
                // For ONNX/Fastembed the model file carries the authoritative
                // dimension, so use onnx.dimensions rather than the text table —
                // this keeps custom ONNX models working.
                #[cfg(feature = "onnx")]
                {
                    if matches!(
                        config.provider,
                        EmbeddingProvider::Onnx | EmbeddingProvider::Fastembed
                    ) {
                        config.onnx.dimensions
                    } else {
                        resolve_from_table(&config)
                    }
                }
                #[cfg(not(feature = "onnx"))]
                {
                    resolve_from_table(&config)
                }
            }
        };

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
        if let Ok(val) = std::env::var("EMBEDDING_MAX_COMPLETION_TOKENS")
            && let Ok(n) = val.trim().parse::<usize>()
        {
            config.max_completion_tokens = n;
        }

        // EMBEDDING_BATCH_SIZE
        if let Ok(val) = std::env::var("EMBEDDING_BATCH_SIZE")
            && let Ok(n) = val.trim().parse::<usize>()
        {
            config.batch_size = n;
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
            #[cfg(feature = "onnx")]
            EmbeddingProvider::Onnx | EmbeddingProvider::Fastembed => {
                let engine = OnnxEmbeddingEngine::with_auto_download(self.onnx.clone()).await?;
                Ok(Arc::new(engine))
            }
            #[cfg(not(feature = "onnx"))]
            EmbeddingProvider::Onnx | EmbeddingProvider::Fastembed => {
                Err(crate::error::EmbeddingError::NotImplemented(
                    "ONNX embedding engine requires the `onnx` crate feature".to_string(),
                ))
            }
            EmbeddingProvider::OpenAi | EmbeddingProvider::OpenAiCompatible => {
                let engine = OpenAICompatibleEmbeddingEngine::new(self)?;
                Ok(Arc::new(engine))
            }
            EmbeddingProvider::Ollama => {
                let engine = OllamaEmbeddingEngine::new(self)?;
                Ok(Arc::new(engine))
            }
            EmbeddingProvider::Mock => Ok(Arc::new(
                MockEmbeddingEngine::new(self.dimensions).with_mode(self.mock_mode),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[cfg(all(feature = "onnx", target_os = "android"))]
    fn test_default_is_onnx_on_android() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.provider, EmbeddingProvider::Onnx);
        assert_eq!(config.dimensions, 384);
        assert_eq!(config.batch_size, 36);
        assert_eq!(config.max_completion_tokens, 8191);
        assert!(!config.mock);
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn test_default_is_openai_off_android() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.provider, EmbeddingProvider::OpenAi);
        assert_eq!(config.model, "text-embedding-3-small");
        assert_eq!(config.dimensions, 1536);
        assert_eq!(
            config.endpoint.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert!(!config.mock);
    }

    #[test]
    fn test_effective_provider_mock_override() {
        let config = EmbeddingConfig {
            mock: true,
            ..Default::default()
        };
        assert_eq!(config.effective_provider(), EmbeddingProvider::Mock);
    }

    #[test]
    #[cfg(all(feature = "onnx", target_os = "android"))]
    fn test_effective_provider_passthrough_onnx() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.effective_provider(), EmbeddingProvider::Onnx);
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn test_effective_provider_passthrough_openai() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.effective_provider(), EmbeddingProvider::OpenAi);
    }

    // env-var tests mutate global process state, so they are serialized with
    // #[serial] to prevent races with each other. All env-mutating tests in this
    // crate live in this single test binary, so serial_test (which serializes
    // within a process) is sufficient; each test also cleans up its own vars.

    #[test]
    #[serial]
    fn test_from_env_mock_embedding_true() {
        // SAFETY: env var mutation is safe because #[serial] guarantees no other
        // env-mutating test in this binary runs concurrently.
        unsafe { std::env::set_var("MOCK_EMBEDDING", "true") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("MOCK_EMBEDDING") };
        assert!(config.mock);
        assert_eq!(config.effective_provider(), EmbeddingProvider::Mock);
    }

    #[test]
    #[serial]
    fn test_from_env_mock_embedding_numeric() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("MOCK_EMBEDDING", "1") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("MOCK_EMBEDDING") };
        assert!(config.mock);
        // Legacy truthy values keep the zero-vector mode.
        assert_eq!(config.mock_mode, MockVectorMode::Zero);
    }

    #[test]
    #[ignore = "mutates global env vars; run with --test-threads=1 --ignored"]
    fn test_from_env_mock_embedding_deterministic() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("MOCK_EMBEDDING", "deterministic") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("MOCK_EMBEDDING") };
        assert!(config.mock);
        assert_eq!(config.effective_provider(), EmbeddingProvider::Mock);
        assert_eq!(config.mock_mode, MockVectorMode::Deterministic);
    }

    #[test]
    #[serial]
    fn test_from_env_provider() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("EMBEDDING_PROVIDER", "openai") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("EMBEDDING_PROVIDER") };
        assert_eq!(config.provider, EmbeddingProvider::OpenAi);
    }

    #[test]
    #[serial]
    fn test_from_env_fastembed_alias() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("EMBEDDING_PROVIDER", "fastembed") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("EMBEDDING_PROVIDER") };
        assert_eq!(config.provider, EmbeddingProvider::Fastembed);
    }

    #[test]
    #[serial]
    fn test_from_env_dimensions() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::set_var("EMBEDDING_DIMENSIONS", "1536") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("EMBEDDING_DIMENSIONS") };
        assert_eq!(config.dimensions, 1536);
    }

    #[test]
    #[serial]
    fn test_from_env_api_key_fallback() {
        // SAFETY: see test_from_env_mock_embedding_true
        unsafe { std::env::remove_var("EMBEDDING_API_KEY") };
        unsafe { std::env::set_var("LLM_API_KEY", "my-llm-key") };
        let config = EmbeddingConfig::from_env();
        unsafe { std::env::remove_var("LLM_API_KEY") };
        assert_eq!(config.api_key, Some("my-llm-key".to_string()));
    }

    #[test]
    #[serial]
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
    #[cfg(feature = "onnx")]
    fn test_onnx_config_bge_small() {
        let cfg = OnnxEmbeddingConfig::bge_small("/models");
        assert_eq!(cfg.dimensions, 384);
        assert_eq!(cfg.max_sequence_length, 512);
        assert_eq!(cfg.model_name, "bge-small-en-v1.5");
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_onnx_config_minilm_l6() {
        let cfg = OnnxEmbeddingConfig::minilm_l6("/models");
        assert_eq!(cfg.dimensions, 384);
        assert_eq!(cfg.max_sequence_length, 256);
        assert_eq!(cfg.model_name, "all-MiniLM-L6-v2");
    }

    // ── known_model_dimensions unit tests ──────────────────────────────────
    // These are pure lookup tests — no env vars, no network, no model files.

    #[test]
    fn known_dims_openai_large() {
        assert_eq!(
            known_model_dimensions(EmbeddingProvider::OpenAi, "text-embedding-3-large"),
            Some(3072),
        );
    }

    #[test]
    fn known_dims_openai_small() {
        assert_eq!(
            known_model_dimensions(EmbeddingProvider::OpenAi, "text-embedding-3-small"),
            Some(1536),
        );
    }

    #[test]
    fn known_dims_ada_002() {
        assert_eq!(
            known_model_dimensions(EmbeddingProvider::OpenAi, "text-embedding-ada-002"),
            Some(1536),
        );
    }

    /// Verify that a provider-prefixed model name is normalized before lookup.
    /// Python uses `model.split("/")[-1]`; Rust uses `rsplit('/').next()`.
    #[test]
    fn known_dims_prefix_stripped() {
        assert_eq!(
            known_model_dimensions(EmbeddingProvider::OpenAi, "openai/text-embedding-3-small"),
            Some(1536),
        );
        // Azure-prefixed variant
        assert_eq!(
            known_model_dimensions(
                EmbeddingProvider::OpenAiCompatible,
                "azure/text-embedding-3-large"
            ),
            Some(3072),
        );
    }

    /// BGE-Small variants: bare name (both v1.5 spellings) and BAAI-prefixed.
    #[test]
    fn known_dims_bge_small() {
        assert_eq!(
            known_model_dimensions(EmbeddingProvider::Onnx, "bge-small-en-v1.5"),
            Some(384),
        );
        assert_eq!(
            known_model_dimensions(EmbeddingProvider::Onnx, "BGE-Small-v1.5"),
            Some(384),
        );
        // fastembed-style prefix stripped correctly
        assert_eq!(
            known_model_dimensions(EmbeddingProvider::Fastembed, "BAAI/bge-small-en-v1.5"),
            Some(384),
        );
    }

    #[test]
    fn known_dims_bge_large() {
        assert_eq!(
            known_model_dimensions(EmbeddingProvider::Fastembed, "bge-large-en-v1.5"),
            Some(1024),
        );
    }

    #[test]
    fn known_dims_unknown_returns_none() {
        assert_eq!(
            known_model_dimensions(EmbeddingProvider::OpenAi, "some-unknown-model"),
            None,
        );
    }

    // ── from_env dimension-resolution tests ────────────────────────────────
    // These mutate process env vars and must not run in parallel.

    /// Explicit EMBEDDING_DIMENSIONS always overrides the table lookup.
    #[test]
    #[serial]
    fn from_env_explicit_override_wins() {
        // SAFETY: #[serial] guarantees no concurrent env readers in this binary.
        unsafe {
            std::env::set_var("EMBEDDING_PROVIDER", "openai");
            std::env::set_var("EMBEDDING_MODEL", "text-embedding-3-large");
            std::env::set_var("EMBEDDING_DIMENSIONS", "999");
        }
        let config = EmbeddingConfig::from_env();
        unsafe {
            std::env::remove_var("EMBEDDING_PROVIDER");
            std::env::remove_var("EMBEDDING_MODEL");
            std::env::remove_var("EMBEDDING_DIMENSIONS");
        }
        // Explicit env var must win over the table value (3072).
        assert_eq!(config.dimensions, 999);
    }

    /// Changing EMBEDDING_MODEL to a known model (without EMBEDDING_DIMENSIONS) must
    /// resolve the correct dimension — not silently keep the default 384.
    /// This is the regression this task fixes (audit B7.2).
    #[test]
    #[serial]
    fn from_env_model_change_resolves() {
        // SAFETY: see from_env_explicit_override_wins
        unsafe {
            std::env::set_var("EMBEDDING_PROVIDER", "openai");
            std::env::set_var("EMBEDDING_MODEL", "text-embedding-3-large");
            std::env::remove_var("EMBEDDING_DIMENSIONS");
        }
        let config = EmbeddingConfig::from_env();
        unsafe {
            std::env::remove_var("EMBEDDING_PROVIDER");
            std::env::remove_var("EMBEDDING_MODEL");
        }
        // Previously returned 384 (the ONNX default); now must return 3072.
        assert_eq!(config.dimensions, 3072);
    }

    /// An unknown model with no explicit EMBEDDING_DIMENSIONS must fall back to
    /// FALLBACK_DIMENSIONS (384) and log a warning (we only assert the dimension here).
    #[test]
    #[serial]
    fn from_env_unknown_falls_back() {
        // SAFETY: see from_env_explicit_override_wins
        unsafe {
            std::env::set_var("EMBEDDING_PROVIDER", "openai");
            std::env::set_var("EMBEDDING_MODEL", "some-unknown-model-xyz");
            std::env::remove_var("EMBEDDING_DIMENSIONS");
        }
        let config = EmbeddingConfig::from_env();
        unsafe {
            std::env::remove_var("EMBEDDING_PROVIDER");
            std::env::remove_var("EMBEDDING_MODEL");
        }
        assert_eq!(config.dimensions, FALLBACK_DIMENSIONS);
    }
}
