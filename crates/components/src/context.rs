//! [`BackendBuildContext`] — the resolved, env-free input to every factory.
//!
//! The construction contract: **all config-specific resolution (assembling
//! Postgres URLs from parts) and all environment-variable reads happen when a
//! caller lowers its config into a `BackendBuildContext`** (see
//! `Settings::backend_context` / `HttpServerConfig::backend_context`). The
//! registry and its factories are pure — given the same context they build the
//! same components, regardless of the process environment. This keeps
//! provider-specific URL assembly where the field differences live and lets
//! each caller opt into mock / recording behavior explicitly.

use std::path::PathBuf;

/// Resolved inputs consumed by [`crate::ComponentRegistry`] and the free
/// `build_storage` / `build_database` constructors.
#[derive(Clone)]
pub struct BackendBuildContext {
    // ── storage / relational database ─────────────────────────────────────
    /// Root directory for ingested data files (LocalStorage).
    pub data_root_directory: PathBuf,
    /// Root for system state; graph / vector backends derive default paths from
    /// it when their explicit path is unset.
    pub system_root_directory: PathBuf,
    /// Fully-resolved relational DB URL (sqlite:… or postgres://…).
    pub relational_db_url: String,

    // ── graph ─────────────────────────────────────────────────────────────
    /// Lowercase graph provider id (`ladybug` | `kuzu` | `postgres`).
    pub graph_provider: String,
    /// Explicit ladybug/kuzu graph file path. Empty → the factory derives
    /// `{system_root_directory}/graph`.
    pub graph_file_path: String,
    /// Resolution outcome for the Postgres graph backend: `None` when the
    /// provider is not Postgres, `Some(Ok(url))` on success, `Some(Err(msg))`
    /// when the provider *is* Postgres but URL resolution failed — carrying the
    /// specific cause (e.g. missing credentials) so the factory can restate it
    /// in the returned error rather than only logging it.
    pub graph_postgres_url: Option<Result<String, String>>,

    // ── vector ────────────────────────────────────────────────────────────
    /// Lowercase vector provider id (`pgvector` | `lancedb` | `brute-force` |
    /// `mock` | …).
    pub vector_provider: String,
    /// Raw vector DB URL/path as configured. Consulted for the `:memory:`
    /// escape hatch and for the LanceDB on-disk path.
    pub vector_db_url: String,
    /// Resolution outcome for the pgvector backend: `None` when the provider is
    /// not pgvector, `Some(Ok(url))` on success, `Some(Err(msg))` when the
    /// provider *is* pgvector but URL resolution failed (carries the cause).
    pub vector_postgres_url: Option<Result<String, String>>,
    /// Embedding vector dimensionality (needed by pgvector table creation).
    pub embedding_dimensions: usize,

    // ── embedding / llm ───────────────────────────────────────────────────
    /// Resolved embedding-engine inputs.
    pub embedding: EmbeddingInputs,
    /// Resolved LLM / transcriber inputs.
    pub llm: LlmInputs,
}

/// Resolved embedding-engine inputs. Mapped to a `cognee_embedding::EmbeddingConfig`
/// by [`crate::build_embedding_config`].
#[derive(Clone)]
pub struct EmbeddingInputs {
    /// Lowercase provider string. An empty value defaults to `onnx`; a
    /// non-empty *unrecognized* value is rejected as a misconfiguration by the
    /// default embedding factory (rather than silently falling back to `onnx`).
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
    /// Resolved endpoint (embedding-specific, falling back to the LLM endpoint).
    pub endpoint: Option<String>,
    /// Resolved API key (embedding-specific, falling back to the LLM key).
    pub api_key: Option<String>,
    pub batch_size: usize,
    /// `MOCK_EMBEDDING` opt-in — overrides `provider` to the mock engine.
    pub mock: bool,
    /// When `mock` is set, selects SHA-256-derived vectors instead of zeros.
    pub mock_deterministic: bool,
    /// Forward-compat fields historically read from the environment.
    pub api_version: Option<String>,
    pub huggingface_tokenizer: Option<String>,
    pub max_completion_tokens: usize,

    // ONNX asset paths — carried unconditionally; only consumed under the
    // `onnx` feature.
    pub onnx_model_path: PathBuf,
    pub onnx_tokenizer_path: PathBuf,
    pub onnx_model_name: String,
    pub onnx_dimensions: usize,
    pub onnx_max_sequence_length: usize,
    pub onnx_batch_size: usize,
}

/// Resolved LLM / transcriber inputs.
#[derive(Clone)]
pub struct LlmInputs {
    /// Lowercase provider string (`openai` | `ollama` | `mistral` | `gemini` |
    /// `custom` | `openai_compatible` | `mock` | closed providers).
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub endpoint: String,
    pub max_retries: u32,
    /// Extra request parameters merged into every chat-completion request body,
    /// lowered from `LLM_ARGS` (Python `llm_config.llm_args`). Empty = no-op.
    /// Applied by the OpenAI-compatible factory via
    /// `OpenAIAdapter::with_extra_args`. See that field's docs for semantics.
    pub llm_args: serde_json::Map<String, serde_json::Value>,
    /// Replaces the provider adapter with a cassette replay mock.
    pub mock: bool,
    /// Cassette path for the replay mock (consumed only under `mock-llm`).
    pub cassette: String,
    /// When non-empty, wraps the real adapter in a recorder (`mock-llm`).
    pub record_path: String,
}
