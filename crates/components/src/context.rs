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
    /// Pace embedding dispatch (`EMBEDDING_RATE_LIMIT_ENABLED`). Flag-gated
    /// only: unlike the LLM path, provider overload never switches this on by
    /// itself, matching Python's embedding limiter.
    pub rate_limit_enabled: bool,
    /// Requests admitted per `rate_limit_interval` once pacing is active.
    pub rate_limit_requests: u32,
    /// Pacing window in seconds.
    pub rate_limit_interval: u32,
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
    /// Optional Anthropic Messages API base URL override (env `ANTHROPIC_BASE_URL`,
    /// alias `ANTHROPIC_API_BASE`). `None` uses the public Anthropic API. Kept
    /// separate from `endpoint` on purpose: `endpoint` aliases `OPENAI_URL`, so
    /// inheriting it would point Anthropic traffic at the OpenAI host. Only the
    /// Anthropic factory consumes it; other providers ignore it.
    pub anthropic_base_url: Option<String>,
    pub max_retries: u32,
    /// Minimum seconds a transient failure is retried for. Together with
    /// `max_retries` this is Python's dual-floor stop condition; `0` reduces it
    /// to a plain attempt cap. See `Settings::llm_min_retry_seconds`.
    pub min_retry_seconds: u32,
    /// Pace dispatch unconditionally (`LLM_RATE_LIMIT_ENABLED`).
    pub rate_limit_enabled: bool,
    /// Requests admitted per `rate_limit_interval` once pacing is active.
    pub rate_limit_requests: u32,
    /// Pacing window in seconds.
    pub rate_limit_interval: u32,
    /// Let provider overload switch pacing on by itself (`AUTO_RATE_LIMIT`).
    pub auto_rate_limit: bool,
    /// Output-token ceiling (Python's `llm_max_completion_tokens`), lowered from
    /// `Settings.llm_max_completion_tokens` (`setLlmMaxCompletionTokens`). Applied
    /// to LLM calls that pass no per-call generation options (the search/completion
    /// retrievers) so the setter actually caps `recall`/`search`. Wired into the
    /// OpenAI-compatible adapter via `OpenAIAdapter::with_default_max_tokens`;
    /// adapters that must send an explicit `max_tokens` (Anthropic) clamp it
    /// against the model's documented output limit.
    pub max_completion_tokens: u32,
    /// Extra request parameters merged into every chat-completion request body,
    /// lowered from `LLM_ARGS` (Python `llm_config.llm_args`). Empty = no-op.
    /// Applied by the OpenAI-compatible factory via
    /// `OpenAIAdapter::with_extra_args`. See that field's docs for semantics.
    pub llm_args: serde_json::Map<String, serde_json::Value>,
    /// Azure `api-version` (empty = unset). Only consumed by the azure provider,
    /// which requires it alongside a deployment `endpoint`.
    pub api_version: String,
    /// Explicit override for OpenAI reasoning-model detection, from `LLM_REASONING`
    /// (`auto` | `always` | `never`). `None` keeps the adapter's name/host
    /// auto-detection; `Some(true)`/`Some(false)` force the reasoning / legacy
    /// parameter shape. Lets an operator correct an endpoint whose reasoning
    /// nature the model name mis-signals. Applied via
    /// `OpenAIAdapter::with_reasoning_override`.
    pub reasoning_override: Option<bool>,
    /// Replaces the provider adapter with a cassette replay mock.
    pub mock: bool,
    /// Cassette path for the replay mock (consumed only under `mock-llm`).
    pub cassette: String,
    /// When non-empty, wraps the real adapter in a recorder (`mock-llm`).
    pub record_path: String,
}

/// Read the optional Anthropic base-URL override from the environment
/// (`ANTHROPIC_BASE_URL`, alias `ANTHROPIC_API_BASE`). Trims surrounding
/// whitespace and treats an empty value as unset (`None`).
///
/// Lives here — the env-read boundary for [`LlmInputs`] — so both the SDK
/// `Settings` and the standalone HTTP server resolve the same knob identically
/// when lowering their config into a [`BackendBuildContext`].
pub fn anthropic_base_url_from_env() -> Option<String> {
    std::env::var("ANTHROPIC_BASE_URL")
        .or_else(|_| std::env::var("ANTHROPIC_API_BASE"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse the `LLM_REASONING` knob into a reasoning-detection override for
/// [`LlmInputs::reasoning_override`]. `always`/`true`/`on`/`1` force reasoning
/// mode on, `never`/`false`/`off`/`0` force it off, and everything else —
/// including the default `auto`, an empty value, or an unrecognised token —
/// leaves auto-detection in place (`None`). Case- and whitespace-insensitive.
///
/// Lives here — the shared config→[`LlmInputs`] boundary — so the SDK `Settings`
/// and the standalone HTTP server resolve the knob identically.
pub fn parse_reasoning_override(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "always" | "true" | "on" | "1" => Some(true),
        "never" | "false" | "off" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_reasoning_override;

    #[test]
    fn parse_reasoning_override_maps_known_tokens() {
        for on in ["always", "true", "on", "1", "  ALWAYS  ", "True"] {
            assert_eq!(parse_reasoning_override(on), Some(true), "{on:?}");
        }
        for off in ["never", "false", "off", "0", "Never"] {
            assert_eq!(parse_reasoning_override(off), Some(false), "{off:?}");
        }
        // Default, empty, and unrecognised tokens fall through to auto (None).
        for auto in ["auto", "", "   ", "maybe", "yes"] {
            assert_eq!(parse_reasoning_override(auto), None, "{auto:?}");
        }
    }
}
