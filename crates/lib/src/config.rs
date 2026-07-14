#![allow(
    clippy::expect_used,
    reason = "RwLock/Mutex expect calls — lock poison is unrecoverable"
)]
//! Shared configuration types for cognee-rust.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};

use serde::{Deserialize, Serialize};

pub const DEFAULT_SYSTEM_PROMPT_PATH: &str = "answer_simple_question.txt";

/// Assemble a `postgres://user:pass@host:port/dbname` URL with percent-encoded
/// credentials. Shared by the vector and graph URL resolvers.
#[cfg(any(feature = "pgvector", feature = "pggraph"))]
fn build_postgres_url(
    host: &str,
    port: u16,
    name: &str,
    user: &str,
    pass: &str,
) -> Result<String, String> {
    let mut parsed =
        url::Url::parse("postgres://localhost").map_err(|e| format!("static URL invalid: {e}"))?;
    parsed
        .set_host(Some(host))
        .map_err(|e| format!("invalid host '{host}': {e}"))?;
    parsed
        .set_port(Some(port))
        .map_err(|_| format!("invalid port {port}"))?;
    parsed.set_path(&format!("/{name}"));
    parsed
        .set_username(user)
        .map_err(|_| format!("invalid username '{user}'"))?;
    parsed
        .set_password(Some(pass))
        .map_err(|_| "invalid password".to_string())?;
    Ok(parsed.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub default_user_id: String,
    pub default_dataset_name: String,

    pub system_root_directory: String,
    pub data_root_directory: String,
    pub cache_root_directory: String,
    pub logs_root_directory: String,
    pub monitoring_tool: String,

    pub classification_model: String,
    pub summarization_model: String,
    pub graph_model: String,

    /// Custom JSON schema for summarization output (Python `summarization_model` parity).
    /// `#[serde(skip)]` keeps config snapshots stable.
    #[serde(skip)]
    pub summarization_schema: Option<serde_json::Value>,

    pub llm_provider: String,
    pub llm_model: String,
    pub llm_api_key: String,
    pub llm_endpoint: String,
    pub llm_api_version: String,
    pub llm_temperature: f64,
    pub llm_streaming: bool,
    pub llm_max_completion_tokens: u32,
    pub llm_max_retries: u32,
    pub llm_max_parallel_requests: u32,

    /// Extra parameters merged into every LLM chat-completion request, parsed
    /// from the `LLM_ARGS` env var (a JSON object), mirroring Python cognee's
    /// `llm_config.llm_args`. Python's litellm adapter merges these into each
    /// call as `{**self.llm_args, **kwargs}`; explicitly-set request parameters
    /// always win. The canonical use is `LLM_ARGS={"max_tokens": 16384}` to
    /// lift a provider's small default output cap that would otherwise truncate
    /// a dense graph-extraction tool call mid-JSON. Empty by default (Python
    /// default `llm_args = {}`). `#[serde(skip)]` (like `summarization_schema`)
    /// keeps persisted config snapshots stable — it is env-driven only.
    #[serde(skip)]
    pub llm_args: serde_json::Map<String, serde_json::Value>,

    /// Select the record/replay mock LLM instead of the real provider
    /// (`MOCK_LLM`). Parallels `MOCK_EMBEDDING`. Requires the `mock-llm` feature.
    pub llm_mock: bool,
    /// Cassette path for the replay mock when `llm_mock` is set (`MOCK_LLM_CASSETTE`).
    /// Empty = unset.
    pub llm_cassette: String,
    /// When non-empty, wrap the real adapter in a recording mock that writes a
    /// cassette to this path (`COGNEE_RECORD_LLM`). Empty = unset.
    pub llm_record_path: String,

    pub graph_prompt_path: String,

    // -- LLM fallback -----------------------------------------------------------
    /// Fallback LLM model name used when the primary model fails.
    pub llm_fallback_model: String,
    /// Fallback LLM provider (e.g. `"openai"`, `"ollama"`).
    pub llm_fallback_provider: String,
    /// Base URL for the fallback LLM API endpoint.
    pub llm_fallback_endpoint: String,
    /// API key for the fallback LLM provider.
    pub llm_fallback_api_key: String,

    pub graph_database_provider: String,
    pub graph_database_url: String,
    pub graph_database_name: String,
    pub graph_database_username: String,
    pub graph_database_password: String,
    pub graph_database_port: u16,
    pub graph_database_host: String,
    pub graph_database_key: String,
    pub graph_file_path: String,
    pub graph_filename: String,

    pub vector_db_provider: String,
    pub vector_db_url: String,
    pub vector_db_port: u16,
    pub vector_db_name: String,
    pub vector_db_key: String,
    pub vector_db_username: String,
    pub vector_db_password: String,
    pub vector_db_host: String,

    pub chunk_strategy: String,
    pub chunk_engine: String,
    pub chunk_size: u32,
    pub chunk_overlap: u32,

    pub relational_db_url: String,
    pub migration_db_url: String,

    /// Selects the relational DB backend: `"sqlite"` (default) or `"postgres"`.
    /// When set to `"postgres"`, the individual `db_host`/`db_port`/`db_name`/
    /// `db_username`/`db_password` fields are used instead of `relational_db_url`.
    /// Mirrors the Python `DB_PROVIDER` environment variable.
    pub db_provider: String,
    pub db_host: String,
    pub db_port: u16,
    pub db_name: String,
    pub db_username: String,
    pub db_password: String,

    pub default_system_prompt_path: String,

    pub embedding_provider: String,
    pub embedding_model_path: String,
    pub embedding_tokenizer_path: String,
    pub embedding_model_name: String,
    pub embedding_dimensions: u32,
    pub embedding_max_sequence_length: u32,
    pub embedding_batch_size: u32,
    /// ONNX inference batch size (the leading tensor dimension), independent of
    /// `embedding_batch_size` which sizes HTTP requests. Maps to
    /// `EMBEDDING_ONNX_BATCH_SIZE`; only the ONNX/Fastembed provider reads it.
    pub embedding_onnx_batch_size: u32,
    /// Embedding API endpoint URL (e.g. `https://api.openai.com/v1/embeddings`).
    /// Maps to `EMBEDDING_ENDPOINT` env var.
    pub embedding_endpoint: String,
    /// Embedding API key. Maps to `EMBEDDING_API_KEY` env var (fallback: `LLM_API_KEY`).
    pub embedding_api_key: String,
    /// Embedding API version string (e.g. for Azure OpenAI `api-version`).
    pub embedding_api_version: String,
    /// Transcription model name (e.g. `"whisper-1"`).
    pub transcription_model: String,

    pub ontology_file_path: String,
    /// Ontology resolver backend. Currently always resolved to `RdfLibOntologyResolver`
    /// when `ontology_file_path` is set — this field is reserved for future multi-resolver
    /// support (e.g. SPARQL endpoint). Only `"rdflib"` is implemented.
    pub ontology_resolver: String,
    /// Fuzzy matching strategy for entity name resolution. Currently always resolved to
    /// `FuzzyMatchingStrategy` (Ratcliff/Obershelp gestalt) — this field is reserved for
    /// future strategy selection. Only `"fuzzy"` is implemented.
    pub ontology_matching_strategy: String,

    // -- Session / cache ---------------------------------------------------------
    /// Session store backend: `"fs"`, `"redis"`, or `"seaorm"`.
    pub cache_backend: String,
    pub cache_host: String,
    pub cache_port: u16,
    pub cache_username: String,
    pub cache_password: String,
    /// Session time-to-live in seconds (default: 604 800 = 7 days).
    pub session_ttl_seconds: u64,
    pub enable_caching: bool,
    pub auto_feedback: bool,

    // -- Authentication / ACL ----------------------------------------------------
    pub default_user_email: String,
    pub default_user_password: String,
    pub enable_access_control: bool,

    // -- Logging -----------------------------------------------------------------
    pub log_level: String,

    // -- Rate limiting -----------------------------------------------------------
    pub llm_rate_limit_enabled: bool,
    pub llm_rate_limit_requests: u32,
    pub llm_rate_limit_interval: u32,
    pub embedding_rate_limit_enabled: bool,
    pub embedding_rate_limit_requests: u32,
    pub embedding_rate_limit_interval: u32,

    // -- Storage backend ---------------------------------------------------------
    /// File storage backend: `"local"` or `"s3"`.
    pub storage_backend: String,
    pub storage_bucket_name: String,

    // -- Observability -----------------------------------------------------------
    pub cognee_tracing_enabled: bool,
    pub otel_service_name: String,
    pub otel_exporter_otlp_endpoint: String,
    pub otel_exporter_otlp_headers: String,

    /// OTLP transport: `"grpc"` (default) or `"http/protobuf"`.
    /// Mirrors the OTEL spec env var `OTEL_EXPORTER_OTLP_PROTOCOL`.
    pub otel_exporter_otlp_protocol: String,

    /// Span processor mode: `"batch"` (default) or `"simple"`.
    /// `simple` is synchronous-per-span and intended only for
    /// debugging or for collectors known to misbehave with batches.
    pub otel_span_processor: String,

    /// Sampler name passed through to the OTEL SDK.
    /// Empty string means: do not override; let the SDK read
    /// `OTEL_TRACES_SAMPLER` itself (default `parentbased_always_on`).
    /// Recognised values follow the OTEL spec:
    /// `always_on`, `always_off`, `traceidratio`, `parentbased_always_on`,
    /// `parentbased_always_off`, `parentbased_traceidratio`.
    pub otel_traces_sampler: String,

    /// Argument for the sampler. Currently only meaningful for the
    /// `traceidratio` / `parentbased_traceidratio` samplers, which expect
    /// a 0.0–1.0 ratio. Empty string means: do not override.
    pub otel_traces_sampler_arg: String,

    // -- Feature flags -----------------------------------------------------------
    pub enable_last_accessed: bool,
}

impl Settings {
    /// Build `Settings` entirely from environment variables (and any `.env` file).
    ///
    /// Equivalent to Python's `LLMConfig()` / `GraphConfig()` instantiation:
    /// starts from defaults and overlays every env var that is set.
    /// The `.env` file in the current working directory (or any ancestor) is loaded
    /// automatically before env vars are read — callers do not need to call
    /// `dotenv::dotenv()` themselves.
    pub fn load_from_env() -> Self {
        let mut s = Self::default();
        s.overlay_from_env();
        s
    }

    /// Overlay environment variables on top of `self`.
    ///
    /// Only fields whose corresponding env var is set are modified; everything
    /// else keeps its current value.  The `.env` file is loaded first (idempotent —
    /// safe to call multiple times).
    ///
    /// Env-var naming follows the Python SDK conventions (`LLM_*`, `EMBEDDING_*`,
    /// `GRAPH_DATABASE_*`, `VECTOR_DB_*`, `DB_*`, `COGNEE_*`).  A handful of
    /// Rust-specific aliases (`OPENAI_TOKEN`, `OPENAI_URL`, `OPENAI_MODEL`) are
    /// accepted as fallbacks for backward compatibility with existing test setups.
    pub fn overlay_from_env(&mut self) {
        // Load .env (no-op if absent or if the vars are already in the environment).
        let _ = dotenv::dotenv();

        // Helpers ----------------------------------------------------------------
        let str_var =
            |name: &str| -> Option<String> { std::env::var(name).ok().filter(|v| !v.is_empty()) };
        // Try `primary` first; fall back to `alias` if primary is unset/empty.
        let str_alias = |primary: &str, alias: &str| -> Option<String> {
            str_var(primary).or_else(|| str_var(alias))
        };

        // -- LLM -----------------------------------------------------------------
        if let Some(v) = str_var("LLM_PROVIDER") {
            self.llm_provider = v;
        }
        if let Some(v) = str_alias("LLM_MODEL", "OPENAI_MODEL") {
            self.llm_model = v;
        }
        if let Some(v) = str_alias("LLM_API_KEY", "OPENAI_TOKEN") {
            self.llm_api_key = v;
        }
        if let Some(v) = str_alias("LLM_ENDPOINT", "OPENAI_URL") {
            self.llm_endpoint = v;
        }
        if let Some(v) = str_var("LLM_API_VERSION") {
            self.llm_api_version = v;
        }
        if let Some(v) = str_var("LLM_TEMPERATURE")
            && let Ok(f) = v.parse::<f64>()
        {
            self.llm_temperature = f;
        }
        if let Some(v) = str_alias("LLM_MAX_COMPLETION_TOKENS", "LLM_MAX_TOKENS")
            && let Ok(n) = v.parse::<u32>()
        {
            self.llm_max_completion_tokens = n;
        }
        // `LLM_ARGS` — a JSON object of extra request parameters merged into every
        // LLM chat-completion call, mirroring Python cognee's `llm_config.llm_args`
        // (e.g. `LLM_ARGS={"max_tokens": 16384}`). A malformed value or a non-object
        // JSON is ignored (left at the default empty map) rather than aborting
        // startup, matching the lenient handling of the other optional LLM knobs.
        if let Some(v) = str_var("LLM_ARGS") {
            match serde_json::from_str::<serde_json::Value>(&v) {
                Ok(serde_json::Value::Object(map)) => self.llm_args = map,
                _ => {
                    tracing::warn!(
                        "LLM_ARGS is set but is not a JSON object; ignoring it. \
                         Expected e.g. LLM_ARGS='{{\"max_tokens\": 16384}}'"
                    );
                }
            }
        }
        if let Some(v) = str_var("LLM_STREAMING") {
            self.llm_streaming = cognee_utils::parse_env_bool(&v);
        }
        if let Some(v) = str_var("LLM_MAX_RETRIES")
            && let Ok(n) = v.parse::<u32>()
        {
            self.llm_max_retries = n;
        }
        if let Some(v) = str_var("LLM_MAX_PARALLEL_REQUESTS")
            && let Ok(n) = v.parse::<u32>()
        {
            self.llm_max_parallel_requests = n;
        }
        // Mirror MOCK_EMBEDDING parsing (accept true/1/yes, case-insensitive).
        if let Some(v) = str_var("MOCK_LLM") {
            let v = v.to_lowercase();
            self.llm_mock = v == "true" || v == "1" || v == "yes";
        }
        if let Some(v) = str_var("MOCK_LLM_CASSETTE") {
            self.llm_cassette = v;
        }
        if let Some(v) = str_var("COGNEE_RECORD_LLM") {
            self.llm_record_path = v;
        }

        // -- Graph database ------------------------------------------------------
        if let Some(v) = str_var("GRAPH_DATABASE_PROVIDER") {
            self.graph_database_provider = v;
        }
        if let Some(v) = str_var("GRAPH_DATABASE_URL") {
            self.graph_database_url = v;
        }
        if let Some(v) = str_var("GRAPH_DATABASE_NAME") {
            self.graph_database_name = v;
        }
        if let Some(v) = str_var("GRAPH_DATABASE_USERNAME") {
            self.graph_database_username = v;
        }
        if let Some(v) = str_var("GRAPH_DATABASE_PASSWORD") {
            self.graph_database_password = v;
        }
        if let Some(v) = str_var("GRAPH_DATABASE_PORT")
            && let Ok(n) = v.parse::<u16>()
        {
            self.graph_database_port = n;
        }
        if let Some(v) = str_var("GRAPH_DATABASE_HOST") {
            self.graph_database_host = v;
        }
        if let Some(v) = str_var("GRAPH_DATABASE_KEY") {
            self.graph_database_key = v;
        }
        if let Some(v) = str_var("GRAPH_FILE_PATH") {
            self.graph_file_path = v;
        }

        // -- Vector database -----------------------------------------------------
        if let Some(v) = str_var("VECTOR_DB_PROVIDER") {
            self.vector_db_provider = v;
        }
        if let Some(v) = str_var("VECTOR_DB_URL") {
            self.vector_db_url = v;
        }
        if let Some(v) = str_var("VECTOR_DB_PORT")
            && let Ok(n) = v.parse::<u16>()
        {
            self.vector_db_port = n;
        }
        if let Some(v) = str_var("VECTOR_DB_NAME") {
            self.vector_db_name = v;
        }
        if let Some(v) = str_var("VECTOR_DB_KEY") {
            self.vector_db_key = v;
        }
        if let Some(v) = str_var("VECTOR_DB_USERNAME") {
            self.vector_db_username = v;
        }
        if let Some(v) = str_var("VECTOR_DB_PASSWORD") {
            self.vector_db_password = v;
        }
        if let Some(v) = str_var("VECTOR_DB_HOST") {
            self.vector_db_host = v;
        }

        // -- Relational database -------------------------------------------------
        if let Some(v) = str_var("DB_PROVIDER") {
            self.db_provider = v;
        }
        if let Some(v) = str_var("DB_HOST") {
            self.db_host = v;
        }
        if let Some(v) = str_var("DB_PORT")
            && let Ok(n) = v.parse::<u16>()
        {
            self.db_port = n;
        }
        if let Some(v) = str_var("DB_NAME") {
            self.db_name = v;
        }
        if let Some(v) = str_var("DB_USERNAME") {
            self.db_username = v;
        }
        if let Some(v) = str_var("DB_PASSWORD") {
            self.db_password = v;
        }
        if let Some(v) = str_var("DATABASE_URL") {
            self.relational_db_url = v;
        }

        // -- Embedding -----------------------------------------------------------
        if let Some(v) = str_var("EMBEDDING_PROVIDER") {
            self.embedding_provider = v;
        }
        if let Some(v) = str_var("EMBEDDING_ENDPOINT") {
            self.embedding_endpoint = v;
        }
        if let Some(v) = str_alias("EMBEDDING_API_KEY", "LLM_API_KEY") {
            self.embedding_api_key = v;
        }
        if let Some(v) = str_var("EMBEDDING_MODEL") {
            self.embedding_model_name = v;
        }
        if let Some(v) = str_var("EMBEDDING_DIMENSIONS")
            && let Ok(n) = v.parse::<u32>()
        {
            self.embedding_dimensions = n;
        }
        if let Some(v) = str_var("EMBEDDING_BATCH_SIZE")
            && let Ok(n) = v.parse::<u32>()
        {
            self.embedding_batch_size = n;
        }
        if let Some(v) = str_var("EMBEDDING_ONNX_BATCH_SIZE")
            && let Ok(n) = v.parse::<u32>()
        {
            self.embedding_onnx_batch_size = n;
        }
        if let Some(v) = str_var("EMBEDDING_MAX_SEQUENCE_LENGTH")
            && let Ok(n) = v.parse::<u32>()
        {
            self.embedding_max_sequence_length = n;
        }
        if let Some(v) = str_alias("EMBEDDING_MODEL_PATH", "COGNEE_E2E_EMBED_MODEL_PATH") {
            self.embedding_model_path = v;
        }
        if let Some(v) = str_alias("EMBEDDING_TOKENIZER_PATH", "COGNEE_E2E_TOKENIZER_PATH") {
            self.embedding_tokenizer_path = v;
        }

        // -- Base / system -------------------------------------------------------
        if let Some(v) = str_var("COGNEE_SYSTEM_ROOT_DIRECTORY") {
            self.system_root_directory = v;
        }
        if let Some(v) = str_var("COGNEE_DATA_ROOT_DIRECTORY") {
            self.data_root_directory = v;
        }
        if let Some(v) = str_var("COGNEE_DEFAULT_DATASET_NAME") {
            self.default_dataset_name = v;
        }
        if let Some(v) = str_var("COGNEE_DEFAULT_USER_ID") {
            self.default_user_id = v;
        }

        // -- Ontology ------------------------------------------------------------
        // NOTE: ontology_resolver and ontology_matching_strategy are stored for
        // future multi-resolver / multi-strategy support. Currently the CLI always
        // uses RdfLibOntologyResolver + FuzzyMatchingStrategy when ontology_file_path
        // is set; these two fields have no runtime effect yet.
        if let Some(v) = str_var("ONTOLOGY_FILE_PATH") {
            self.ontology_file_path = v;
        }
        if let Some(v) = str_var("ONTOLOGY_RESOLVER") {
            self.ontology_resolver = v;
        }
        if let Some(v) = str_var("ONTOLOGY_MATCHING_STRATEGY") {
            self.ontology_matching_strategy = v;
        }

        // -- Session / cache -----------------------------------------------------
        if let Some(v) = str_var("CACHE_BACKEND") {
            self.cache_backend = v;
        }
        if let Some(v) = str_var("CACHE_HOST") {
            self.cache_host = v;
        }
        if let Some(v) = str_var("CACHE_PORT")
            && let Ok(n) = v.parse::<u16>()
        {
            self.cache_port = n;
        }
        if let Some(v) = str_var("CACHE_USERNAME") {
            self.cache_username = v;
        }
        if let Some(v) = str_var("CACHE_PASSWORD") {
            self.cache_password = v;
        }
        if let Some(v) = str_var("SESSION_TTL_SECONDS")
            && let Ok(n) = v.parse::<u64>()
        {
            self.session_ttl_seconds = n;
        }
        if let Some(v) = str_var("CACHING") {
            self.enable_caching = cognee_utils::parse_env_bool(&v);
        }
        if let Some(v) = str_var("AUTO_FEEDBACK") {
            self.auto_feedback = cognee_utils::parse_env_bool(&v);
        }

        // -- Authentication / ACL ------------------------------------------------
        if let Some(v) = str_var("DEFAULT_USER_EMAIL") {
            self.default_user_email = v;
        }
        if let Some(v) = str_var("DEFAULT_USER_PASSWORD") {
            self.default_user_password = v;
        }
        if let Some(v) = str_var("ENABLE_BACKEND_ACCESS_CONTROL") {
            self.enable_access_control = cognee_utils::parse_env_bool(&v);
        }

        // -- Logging -------------------------------------------------------------
        if let Some(v) = str_var("LOG_LEVEL") {
            self.log_level = v;
        }
        // COGNEE_LOGS_DIR maps to existing logs_root_directory
        if let Some(v) = str_var("COGNEE_LOGS_DIR") {
            self.logs_root_directory = v;
        }
        // CACHE_ROOT_DIRECTORY maps to existing cache_root_directory
        if let Some(v) = str_var("CACHE_ROOT_DIRECTORY") {
            self.cache_root_directory = v;
        }

        // -- Rate limiting -------------------------------------------------------
        if let Some(v) = str_var("LLM_RATE_LIMIT_ENABLED") {
            self.llm_rate_limit_enabled = cognee_utils::parse_env_bool(&v);
        }
        if let Some(v) = str_var("LLM_RATE_LIMIT_REQUESTS")
            && let Ok(n) = v.parse::<u32>()
        {
            self.llm_rate_limit_requests = n;
        }
        if let Some(v) = str_var("LLM_RATE_LIMIT_INTERVAL")
            && let Ok(n) = v.parse::<u32>()
        {
            self.llm_rate_limit_interval = n;
        }
        if let Some(v) = str_var("EMBEDDING_RATE_LIMIT_ENABLED") {
            self.embedding_rate_limit_enabled = cognee_utils::parse_env_bool(&v);
        }
        if let Some(v) = str_var("EMBEDDING_RATE_LIMIT_REQUESTS")
            && let Ok(n) = v.parse::<u32>()
        {
            self.embedding_rate_limit_requests = n;
        }
        if let Some(v) = str_var("EMBEDDING_RATE_LIMIT_INTERVAL")
            && let Ok(n) = v.parse::<u32>()
        {
            self.embedding_rate_limit_interval = n;
        }

        // -- Storage backend -----------------------------------------------------
        if let Some(v) = str_var("STORAGE_BACKEND") {
            self.storage_backend = v;
        }
        if let Some(v) = str_var("STORAGE_BUCKET_NAME") {
            self.storage_bucket_name = v;
        }

        // -- Observability -------------------------------------------------------
        if let Some(v) = str_var("COGNEE_TRACING_ENABLED") {
            self.cognee_tracing_enabled = cognee_utils::parse_env_bool(&v);
        }
        if let Some(v) = str_var("OTEL_SERVICE_NAME") {
            self.otel_service_name = v;
        }
        if let Some(v) = str_var("OTEL_EXPORTER_OTLP_ENDPOINT") {
            self.otel_exporter_otlp_endpoint = v;
        }
        if let Some(v) = str_var("OTEL_EXPORTER_OTLP_HEADERS") {
            self.otel_exporter_otlp_headers = v;
        }
        if let Some(v) = str_var("OTEL_EXPORTER_OTLP_PROTOCOL") {
            self.otel_exporter_otlp_protocol = v;
        }
        if let Some(v) = str_var("OTEL_SPAN_PROCESSOR") {
            self.otel_span_processor = v;
        }
        if let Some(v) = str_var("OTEL_TRACES_SAMPLER") {
            self.otel_traces_sampler = v;
        }
        if let Some(v) = str_var("OTEL_TRACES_SAMPLER_ARG") {
            self.otel_traces_sampler_arg = v;
        }

        // -- Feature flags -------------------------------------------------------
        if let Some(v) = str_var("ENABLE_LAST_ACCESSED") {
            self.enable_last_accessed = cognee_utils::parse_env_bool(&v);
        }
    }

    /// Returns the effective relational DB connection URL.
    ///
    /// When `db_provider` is `"postgres"`, builds
    /// `postgres://username:password@host:port/name` from the individual
    /// `db_*` fields (matching Python's `DB_PROVIDER`/`DB_HOST`/… env vars).
    /// Otherwise returns `relational_db_url` verbatim.
    pub fn resolved_relational_db_url(&self) -> String {
        if self.db_provider == "postgres" {
            format!(
                "postgres://{}:{}@{}:{}/{}",
                self.db_username, self.db_password, self.db_host, self.db_port, self.db_name
            )
        } else {
            self.relational_db_url.clone()
        }
    }

    /// Lower these settings into a [`BackendBuildContext`] for
    /// `cognee-components`. All config-specific URL resolution and the
    /// component-relevant environment reads (`MOCK_EMBEDDING`,
    /// `EMBEDDING_API_VERSION`, `HUGGINGFACE_TOKENIZER`,
    /// `EMBEDDING_MAX_COMPLETION_TOKENS`) happen here so the registry stays
    /// env-free.
    pub fn backend_context(&self) -> cognee_components::BackendBuildContext {
        use std::path::PathBuf;

        let graph_provider = self.graph_database_provider.to_lowercase();
        // Carry the resolution `Result` through to the factory so it can restate
        // the specific cause (e.g. "Missing required Postgres graph credentials")
        // in the returned error, not just in a log line an SDK user may not see.
        let graph_postgres_url = {
            #[cfg(feature = "pggraph")]
            {
                if matches!(graph_provider.as_str(), "postgres" | "postgresql") {
                    Some(self.resolved_graph_postgres_url())
                } else {
                    None
                }
            }
            #[cfg(not(feature = "pggraph"))]
            {
                None
            }
        };

        let vector_provider = self.vector_db_provider.to_lowercase();
        let vector_postgres_url = {
            #[cfg(feature = "pgvector")]
            {
                if vector_provider == "pgvector" {
                    Some(self.resolved_vector_postgres_url())
                } else {
                    None
                }
            }
            #[cfg(not(feature = "pgvector"))]
            {
                None
            }
        };

        // Embedding endpoint/key fall back to the LLM provider's when no
        // embedding-specific values are set (shared OpenAI-compatible account).
        let endpoint = [&self.embedding_endpoint, &self.llm_endpoint]
            .into_iter()
            .find(|v| !v.is_empty())
            .cloned();
        let api_key = [&self.embedding_api_key, &self.llm_api_key]
            .into_iter()
            .find(|v| !v.is_empty())
            .cloned();

        // MOCK_EMBEDDING: `deterministic`/`hash` selects SHA-256-derived
        // vectors; other truthy values keep the legacy zero-vector mode.
        let mock_mode = std::env::var("MOCK_EMBEDDING")
            .ok()
            .map(|v| v.trim().to_lowercase());
        let mock_deterministic =
            matches!(mock_mode.as_deref(), Some("deterministic") | Some("hash"));
        let mock = mock_deterministic
            || matches!(mock_mode.as_deref(), Some("true") | Some("1") | Some("yes"));

        // Forward-compat env fields not yet on Settings.
        let api_version = std::env::var("EMBEDDING_API_VERSION")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let huggingface_tokenizer = std::env::var("HUGGINGFACE_TOKENIZER")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let max_completion_tokens = std::env::var("EMBEDDING_MAX_COMPLETION_TOKENS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(8191);

        cognee_components::BackendBuildContext {
            data_root_directory: PathBuf::from(&self.data_root_directory),
            system_root_directory: PathBuf::from(&self.system_root_directory),
            relational_db_url: self.resolved_relational_db_url(),
            graph_provider,
            graph_file_path: self.graph_file_path.clone(),
            graph_postgres_url,
            vector_provider,
            vector_db_url: self.vector_db_url.clone(),
            vector_postgres_url,
            embedding_dimensions: self.embedding_dimensions as usize,
            embedding: cognee_components::EmbeddingInputs {
                provider: self.embedding_provider.trim().to_lowercase(),
                model: self.embedding_model_name.clone(),
                dimensions: self.embedding_dimensions as usize,
                endpoint,
                api_key,
                batch_size: self.embedding_batch_size as usize,
                mock,
                mock_deterministic,
                api_version,
                huggingface_tokenizer,
                max_completion_tokens,
                onnx_model_path: PathBuf::from(&self.embedding_model_path),
                onnx_tokenizer_path: PathBuf::from(&self.embedding_tokenizer_path),
                onnx_model_name: self.embedding_model_name.clone(),
                onnx_dimensions: self.embedding_dimensions as usize,
                onnx_max_sequence_length: self.embedding_max_sequence_length as usize,
                onnx_batch_size: self.embedding_onnx_batch_size as usize,
            },
            llm: cognee_components::LlmInputs {
                provider: self.llm_provider.to_lowercase(),
                model: self.llm_model.clone(),
                api_key: self.llm_api_key.clone(),
                endpoint: self.llm_endpoint.clone(),
                max_retries: self.llm_max_retries,
                llm_args: self.llm_args.clone(),
                mock: self.llm_mock,
                cassette: self.llm_cassette.clone(),
                record_path: self.llm_record_path.clone(),
            },
        }
    }

    /// Build a Postgres connection URL from the `graph_database_*` settings,
    /// falling back to the relational `db_*` fields when graph-specific creds
    /// are not fully configured (Python `get_graph_engine.py:332-367` parity).
    #[cfg(feature = "pggraph")]
    pub(crate) fn resolved_graph_postgres_url(&self) -> Result<String, String> {
        if self.graph_database_url.starts_with("postgres://")
            || self.graph_database_url.starts_with("postgresql://")
        {
            return Ok(self.graph_database_url.clone());
        }

        let graph_host = if self.graph_database_host.is_empty() {
            None
        } else {
            Some(self.graph_database_host.as_str())
        };
        let graph_creds_complete = graph_host.is_some()
            && !self.graph_database_username.is_empty()
            && !self.graph_database_name.is_empty();

        let (host, port, name, user, pass) = if graph_creds_complete {
            (
                graph_host.unwrap_or_default(),
                self.graph_database_port,
                self.graph_database_name.as_str(),
                self.graph_database_username.as_str(),
                self.graph_database_password.as_str(),
            )
        } else {
            tracing::warn!(
                "Postgres graph credentials not fully configured; falling back to the \
                 relational database configuration. Set GRAPH_DATABASE_* explicitly to avoid this."
            );
            if self.db_host.is_empty() || self.db_name.is_empty() || self.db_username.is_empty() {
                return Err("Missing required Postgres graph credentials".into());
            }
            (
                self.db_host.as_str(),
                self.db_port,
                self.db_name.as_str(),
                self.db_username.as_str(),
                self.db_password.as_str(),
            )
        };

        build_postgres_url(host, port, name, user, pass)
    }

    /// Build a Postgres connection URL from the `vector_db_*` settings.
    #[cfg(feature = "pgvector")]
    pub(crate) fn resolved_vector_postgres_url(&self) -> Result<String, String> {
        if self.vector_db_url.starts_with("postgres://")
            || self.vector_db_url.starts_with("postgresql://")
        {
            return Ok(self.vector_db_url.clone());
        }

        let host = if self.vector_db_url.is_empty() {
            "localhost"
        } else {
            &self.vector_db_url
        };
        let port = self.vector_db_port;
        let name = if self.vector_db_name.is_empty() {
            "cognee_vectors"
        } else {
            &self.vector_db_name
        };
        let user = if self.db_username.is_empty() {
            "postgres"
        } else {
            &self.db_username
        };
        let pass = &self.db_password;

        build_postgres_url(host, port, name, user, pass)
    }

    /// Returns the redacted property dict merged into `Pipeline Run *`
    /// analytics events.
    ///
    /// **Allowlist-only.** Mirrors Python's `get_current_settings()`
    /// shape but covers only provider/model identifiers and a few
    /// dimension/strategy fields — see
    /// [`docs/telemetry/03/03-settings-snapshot.md`](https://github.com/topoteretes/cognee-rs/blob/main/docs/telemetry/03/03-settings-snapshot.md)
    /// for the rationale on what is omitted (URLs, credentials,
    /// file paths).
    ///
    /// Adding a field here is intentional — there is a snapshot test
    /// that will fail until it is acknowledged.
    pub fn telemetry_snapshot(&self) -> serde_json::Map<String, serde_json::Value> {
        use serde_json::Value;
        let mut m = serde_json::Map::new();
        m.insert("sdk_runtime".into(), Value::String("rust".into()));
        m.insert(
            "vector_db_provider".into(),
            Value::String(self.vector_db_provider.clone()),
        );
        m.insert(
            "graph_db_provider".into(),
            Value::String(self.graph_database_provider.clone()),
        );
        m.insert(
            "relational_db_provider".into(),
            Value::String(self.db_provider.clone()),
        );
        m.insert(
            "llm_provider".into(),
            Value::String(self.llm_provider.clone()),
        );
        m.insert("llm_model".into(), Value::String(self.llm_model.clone()));
        // NOTE: `llm_mock`/`llm_cassette`/`llm_record_path` are intentionally
        // NOT emitted here. The cassette/record fields are local filesystem
        // paths (sensitive), and the telemetry snapshot is an allowlisted,
        // privacy-filtered payload — see `telemetry_snapshot_only_emits_allowlisted_keys`.
        m.insert(
            "embedding_provider".into(),
            Value::String(self.embedding_provider.clone()),
        );
        m.insert(
            "embedding_model".into(),
            Value::String(self.embedding_model_name.clone()),
        );
        m.insert(
            "embedding_dimensions".into(),
            Value::Number(self.embedding_dimensions.into()),
        );
        m.insert(
            "chunk_strategy".into(),
            Value::String(self.chunk_strategy.clone()),
        );
        m
    }
}

impl Default for Settings {
    fn default() -> Self {
        // Embedding default: local ONNX (BGE-Small) on Android for edge/offline
        // deployment; OpenAI text-embedding-3-small everywhere else — matching
        // the Python SDK and `cognee_embedding::EmbeddingConfig::default()`.
        // (ONNX runs all texts in one inference; remote OpenAI embeddings avoid
        // both the model download and large-batch memory blow-ups.)
        #[cfg(target_os = "android")]
        let (embedding_provider, embedding_model_name, embedding_dimensions) =
            ("onnx", "BGE-Small-v1.5", 384u32);
        #[cfg(not(target_os = "android"))]
        let (embedding_provider, embedding_model_name, embedding_dimensions) =
            ("openai", "text-embedding-3-small", 1536u32);

        Self {
            default_user_id: "00000000-0000-0000-0000-000000000000".to_string(),
            default_dataset_name: "main_dataset".to_string(),
            system_root_directory: "./.cognee_system".to_string(),
            data_root_directory: "./.data_storage".to_string(),
            cache_root_directory: "./.cognee_cache".to_string(),
            // Intentional divergence from Python default (~/.cognee/logs): edge/Android targets need a relative path.
            logs_root_directory: "./logs".to_string(),
            monitoring_tool: "none".to_string(),

            classification_model: String::new(),
            summarization_model: String::new(),
            graph_model: "KnowledgeGraph".to_string(),
            summarization_schema: None,

            llm_provider: "openai".to_string(),
            llm_model: "openai/gpt-5-mini".to_string(),
            llm_api_key: String::new(),
            llm_endpoint: String::new(),
            llm_api_version: String::new(),
            llm_temperature: 0.0,
            llm_streaming: false,
            llm_max_completion_tokens: 16384,
            llm_max_retries: 2,
            llm_max_parallel_requests: 20,
            llm_args: serde_json::Map::new(),
            llm_mock: false,
            llm_cassette: String::new(),
            llm_record_path: String::new(),
            graph_prompt_path: "generate_graph_prompt.txt".to_string(),

            llm_fallback_model: String::new(),
            llm_fallback_provider: String::new(),
            llm_fallback_endpoint: String::new(),
            llm_fallback_api_key: String::new(),

            graph_database_provider: "ladybug".to_string(),
            graph_database_url: String::new(),
            graph_database_name: String::new(),
            graph_database_username: String::new(),
            graph_database_password: String::new(),
            graph_database_port: 123,
            graph_database_host: String::new(),
            graph_database_key: String::new(),
            graph_file_path: String::new(),
            graph_filename: String::new(),

            // OSS default: legacy `"lancedb"` is kept as the literal value so
            // existing configs continue to boot without edits. Post-T4/T5,
            // `ComponentManager::init_vector_db` redirects `"lancedb"`/`"qdrant"`
            // to the in-memory `BruteForceVectorDB` with a `tracing::warn!`.
            // Production deployments should explicitly set
            // `vector_db_provider="pgvector"` (and supply `vector_db_url`) for
            // durable storage. T5's earlier flip to `"pgvector"` broke OSS
            // bindings (Neon/C-API/python defaults don't enable the `pgvector`
            // Cargo feature) — keeping `"lancedb"` here is the lowest-friction
            // OSS default that works in every OSS build out of the box.
            vector_db_provider: "lancedb".to_string(),
            vector_db_url: String::new(),
            vector_db_port: 1234,
            vector_db_name: String::new(),
            vector_db_key: String::new(),
            vector_db_username: String::new(),
            vector_db_password: String::new(),
            vector_db_host: String::new(),

            chunk_strategy: "PARAGRAPH".to_string(),
            chunk_engine: "DEFAULT_ENGINE".to_string(),
            chunk_size: 1500,
            chunk_overlap: 10,

            relational_db_url: "sqlite:./cognee.db?mode=rwc".to_string(),
            migration_db_url: String::new(),

            db_provider: "sqlite".to_string(),
            db_host: "localhost".to_string(),
            db_port: 5432,
            db_name: "cognee_db".to_string(),
            db_username: String::new(),
            db_password: String::new(),

            default_system_prompt_path: DEFAULT_SYSTEM_PROMPT_PATH.to_string(),

            embedding_provider: embedding_provider.to_string(),
            // ONNX model/tokenizer paths are only consulted when the provider is
            // `onnx`/`fastembed` (the Android/edge default); harmless otherwise.
            embedding_model_path: "./target/models/BGE-Small-v1.5-model_quantized.onnx".to_string(),
            embedding_tokenizer_path: "./target/models/bge-small-tokenizer.json".to_string(),
            embedding_model_name: embedding_model_name.to_string(),
            // Dimensions match the default model above (text-embedding-3-small =
            // 1536; BGE-Small = 384). If you change embedding_model_name, update
            // this or set EMBEDDING_DIMENSIONS so from_env auto-resolves it via
            // cognee_embedding::known_model_dimensions.
            embedding_dimensions,
            embedding_max_sequence_length: 512,
            embedding_batch_size: 36,
            embedding_onnx_batch_size: 32,
            embedding_endpoint: String::new(),
            embedding_api_key: String::new(),
            embedding_api_version: String::new(),
            transcription_model: String::new(),

            ontology_file_path: String::new(),
            ontology_resolver: "rdflib".to_string(),
            ontology_matching_strategy: "fuzzy".to_string(),

            // Session / cache
            cache_backend: "fs".to_string(),
            cache_host: "localhost".to_string(),
            cache_port: 6379,
            cache_username: String::new(),
            cache_password: String::new(),
            session_ttl_seconds: 604800,
            enable_caching: true,
            auto_feedback: false,

            // Authentication / ACL
            default_user_email: "default_user@example.com".to_string(),
            default_user_password: String::new(),
            enable_access_control: false,

            // Logging
            log_level: "info".to_string(),

            // Rate limiting
            llm_rate_limit_enabled: false,
            llm_rate_limit_requests: 60,
            llm_rate_limit_interval: 60,
            embedding_rate_limit_enabled: false,
            embedding_rate_limit_requests: 60,
            embedding_rate_limit_interval: 60,

            // Storage backend
            storage_backend: "local".to_string(),
            storage_bucket_name: String::new(),

            // Observability
            cognee_tracing_enabled: false,
            otel_service_name: "cognee".to_string(),
            otel_exporter_otlp_endpoint: String::new(),
            otel_exporter_otlp_headers: String::new(),
            otel_exporter_otlp_protocol: "grpc".to_string(),
            otel_span_processor: "batch".to_string(),
            otel_traces_sampler: String::new(),
            otel_traces_sampler_arg: String::new(),

            // Feature flags
            enable_last_accessed: false,
        }
    }
}

// ---------------------------------------------------------------------------
// ConfigError
// ---------------------------------------------------------------------------

/// Errors returned by [`ConfigManager`] setter methods.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Unknown config key: {0}")]
    UnknownKey(String),
    #[error("Type mismatch for key '{key}': {reason}")]
    TypeMismatch { key: String, reason: String },
}

// ---------------------------------------------------------------------------
// ConfigManager
// ---------------------------------------------------------------------------

/// Thread-safe mutable configuration manager.
///
/// Wraps `Settings` in `Arc<RwLock<>>` to allow runtime mutation from
/// setter methods.  Tracks a monotonically increasing version counter
/// so that [`crate::ComponentManager`] can detect stale cached components
/// and reinitialize them.
///
/// # Example
/// ```
/// use cognee_lib::config::{ConfigManager, Settings};
///
/// let cfg = ConfigManager::new(Settings::default());
/// assert_eq!(cfg.version(), 0);
///
/// cfg.set_llm_model("gpt-4o");
/// assert_eq!(cfg.version(), 1);
/// assert_eq!(cfg.read().llm_model, "gpt-4o");
/// ```
pub struct ConfigManager {
    inner: Arc<RwLock<Settings>>,
    version: Arc<AtomicU64>,
}

impl ConfigManager {
    /// Create a new `ConfigManager` wrapping the given settings.
    pub fn new(settings: Settings) -> Self {
        Self {
            inner: Arc::new(RwLock::new(settings)),
            version: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Convenience constructor: `Settings::load_from_env()` + wrap.
    pub fn from_env() -> Self {
        Self::new(Settings::load_from_env())
    }

    /// Obtain a read-lock on the current settings.
    pub fn read(&self) -> RwLockReadGuard<'_, Settings> {
        self.inner.read().expect("lock poison is unrecoverable") // lock poison is unrecoverable
    }

    /// Current config version (monotonically increasing on each mutation).
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Bump the version after any mutation.
    fn bump_version(&self) {
        self.version.fetch_add(1, Ordering::Release);
    }
}

impl Clone for ConfigManager {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            version: Arc::clone(&self.version),
        }
    }
}

// -- Individual setter methods -----------------------------------------------

impl ConfigManager {
    // -- LLM -----------------------------------------------------------------

    pub fn set_llm_provider(&self, provider: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_provider = provider.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_model(&self, model: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_model = model.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_api_key(&self, key: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_api_key = key.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_endpoint(&self, endpoint: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_endpoint = endpoint.to_string();
        drop(s);
        self.bump_version();
    }

    // -- LLM fallback --------------------------------------------------------

    pub fn set_llm_fallback_model(&self, model: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_fallback_model = model.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_fallback_provider(&self, provider: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_fallback_provider = provider.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_fallback_endpoint(&self, endpoint: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_fallback_endpoint = endpoint.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_fallback_api_key(&self, key: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_fallback_api_key = key.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Embedding -----------------------------------------------------------

    pub fn set_embedding_provider(&self, provider: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.embedding_provider = provider.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_embedding_model(&self, model: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.embedding_model_name = model.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_embedding_dimensions(&self, dims: u32) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.embedding_dimensions = dims;
        drop(s);
        self.bump_version();
    }

    pub fn set_embedding_endpoint(&self, endpoint: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.embedding_endpoint = endpoint.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_embedding_api_key(&self, key: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.embedding_api_key = key.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_embedding_api_version(&self, version: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.embedding_api_version = version.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_transcription_model(&self, model: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.transcription_model = model.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Vector DB -----------------------------------------------------------

    pub fn set_vector_db_provider(&self, provider: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.vector_db_provider = provider.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_vector_db_url(&self, url: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.vector_db_url = url.to_string();
        drop(s);
        self.bump_version();
    }

    /// Override the relational database URL (e.g. `"sqlite:///path/to/db?mode=rwc"`).
    ///
    /// Primarily used by language-binding tests to redirect each test's DB to an
    /// isolated tmp directory so tests do not share the default on-disk DB.
    pub fn set_relational_db_url(&self, url: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.relational_db_url = url.to_string();
        drop(s);
        self.bump_version();
    }

    /// Set all relational DB connection fields at once.
    // Many optional fields are needed here to match Python's bulk-setter API.
    #[allow(clippy::too_many_arguments)]
    pub fn set_relational_db_config(
        &self,
        url: Option<&str>,
        provider: Option<&str>,
        host: Option<&str>,
        port: Option<u16>,
        name: Option<&str>,
        username: Option<&str>,
        password: Option<&str>,
    ) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        if let Some(v) = url {
            s.relational_db_url = v.to_string();
        }
        if let Some(v) = provider {
            s.db_provider = v.to_string();
        }
        if let Some(v) = host {
            s.db_host = v.to_string();
        }
        if let Some(v) = port {
            s.db_port = v;
        }
        if let Some(v) = name {
            s.db_name = v.to_string();
        }
        if let Some(v) = username {
            s.db_username = v.to_string();
        }
        if let Some(v) = password {
            s.db_password = v.to_string();
        }
        drop(s);
        self.bump_version();
    }

    /// Set the migration DB URL.
    pub fn set_migration_db_config(&self, url: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.migration_db_url = url.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_vector_db_key(&self, key: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.vector_db_key = key.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Graph DB ------------------------------------------------------------

    pub fn set_graph_database_provider(&self, provider: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.graph_database_provider = provider.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_graph_model(&self, model: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.graph_model = model.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Chunking ------------------------------------------------------------

    pub fn set_chunk_strategy(&self, strategy: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.chunk_strategy = strategy.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_chunk_engine(&self, engine: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.chunk_engine = engine.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_chunk_size(&self, size: u32) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.chunk_size = size;
        drop(s);
        self.bump_version();
    }

    pub fn set_chunk_overlap(&self, overlap: u32) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.chunk_overlap = overlap;
        drop(s);
        self.bump_version();
    }

    // -- System paths --------------------------------------------------------

    pub fn set_data_root_directory(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.data_root_directory = path.to_string();
        drop(s);
        self.bump_version();
    }

    /// Set system root directory and cascade derived path updates.
    ///
    /// Matches Python `config.system_root_directory()` (config.py lines 41-67):
    /// - `graph_file_path` updated if it was under the old system root
    /// - `vector_db_url` updated if it was under the old system root
    pub fn set_system_root_directory(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        let old_root = s.system_root_directory.clone();
        s.system_root_directory = path.to_string();

        // Cascade graph_file_path
        if s.graph_file_path.is_empty() || s.graph_file_path.starts_with(&old_root) {
            let suffix = if s.graph_file_path.is_empty() {
                "/graph".to_string()
            } else {
                s.graph_file_path[old_root.len()..].to_string()
            };
            s.graph_file_path = format!("{path}{suffix}");
        }

        // Cascade vector_db_url (only if it was using the default system root path)
        if s.vector_db_url.is_empty() || s.vector_db_url.starts_with(&old_root) {
            let suffix = if s.vector_db_url.is_empty() {
                "/vectors".to_string()
            } else {
                s.vector_db_url[old_root.len()..].to_string()
            };
            s.vector_db_url = format!("{path}{suffix}");
        }

        drop(s);
        self.bump_version();
    }

    pub fn set_monitoring_tool(&self, tool: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.monitoring_tool = tool.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_classification_model(&self, model: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.classification_model = model.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_summarization_model(&self, model: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.summarization_model = model.to_string();
        drop(s);
        self.bump_version();
    }

    /// Set a custom JSON schema for the summarization output stage.
    ///
    /// Mirrors Python's `cognee.config.set_summarization_model(CustomSchema)`:
    /// accepts a JSON Schema `Value` describing the expected LLM output. The
    /// schema **must** contain a `summary` string field. The stored value is
    /// intended to be read by callers when constructing a `CognifyConfig` via
    /// `CognifyConfig::with_summary_schema`.
    ///
    /// Returns `Err` if the schema fails validation (missing `summary` field).
    pub fn set_summarization_schema(
        &self,
        schema: serde_json::Value,
    ) -> Result<(), cognee_cognify::config::ConfigError> {
        cognee_cognify::config::validate_summary_schema(&schema)?;
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.summarization_schema = Some(schema);
        drop(s);
        self.bump_version();
        Ok(())
    }

    // -- LLM tuning ----------------------------------------------------------

    pub fn set_llm_api_version(&self, version: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_api_version = version.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_temperature(&self, temperature: f64) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_temperature = temperature;
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_streaming(&self, streaming: bool) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_streaming = streaming;
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_max_completion_tokens(&self, tokens: u32) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_max_completion_tokens = tokens;
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_max_retries(&self, retries: u32) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_max_retries = retries;
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_max_parallel_requests(&self, parallel: u32) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_max_parallel_requests = parallel;
        drop(s);
        self.bump_version();
    }

    /// Select the record/replay mock LLM (`MOCK_LLM` parity).
    pub fn set_llm_mock(&self, mock: bool) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_mock = mock;
        drop(s);
        self.bump_version();
    }

    /// Set the cassette path used by the replay mock (`MOCK_LLM_CASSETTE`).
    pub fn set_llm_cassette(&self, cassette: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_cassette = cassette.to_string();
        drop(s);
        self.bump_version();
    }

    /// Set the recording cassette output path (`COGNEE_RECORD_LLM`); empty = unset.
    pub fn set_llm_record_path(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.llm_record_path = path.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Embedding paths -----------------------------------------------------

    pub fn set_embedding_model_path(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.embedding_model_path = path.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_embedding_tokenizer_path(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.embedding_tokenizer_path = path.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Vector DB endpoint parts --------------------------------------------

    pub fn set_vector_db_host(&self, host: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.vector_db_host = host.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_vector_db_port(&self, port: u16) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.vector_db_port = port;
        drop(s);
        self.bump_version();
    }

    pub fn set_vector_db_name(&self, name: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.vector_db_name = name.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Graph DB granular path ----------------------------------------------

    /// Set `graph_file_path` directly.
    ///
    /// Unlike [`set_system_root_directory`](Self::set_system_root_directory),
    /// this is a plain field write and does **not** cascade to other paths.
    pub fn set_graph_file_path(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.graph_file_path = path.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Paths ---------------------------------------------------------------

    pub fn set_cache_root_directory(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.cache_root_directory = path.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_logs_root_directory(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.logs_root_directory = path.to_string();
        drop(s);
        self.bump_version();
    }

    // -- Ontology ------------------------------------------------------------

    pub fn set_ontology_file_path(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.ontology_file_path = path.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_ontology_resolver(&self, resolver: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.ontology_resolver = resolver.to_string();
        drop(s);
        self.bump_version();
    }

    pub fn set_ontology_matching_strategy(&self, strategy: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.ontology_matching_strategy = strategy.to_string();
        drop(s);
        self.bump_version();
    }

    /// Return a snapshot of the current settings with secrets masked.
    ///
    /// All secret-bearing fields (`*_api_key`, `*_password`, `*_key`) are
    /// replaced with `"<redacted>"` when non-empty, matching Python's
    /// `config.get_settings()` behaviour so callers can safely log or expose
    /// the output without leaking credentials.
    ///
    /// The returned map can be serialized to JSON for logging or debugging.
    pub fn get_settings(&self) -> std::collections::HashMap<String, serde_json::Value> {
        use serde_json::Value;

        let s = self.inner.read().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        let mut m = std::collections::HashMap::new();

        // Mask any non-empty secret: use "<redacted>" unconditionally so that
        // test keys (e.g. "my-secret-key") are masked even when they don't
        // match the pattern-based cognee_utils::redact() heuristics.
        let mask = |v: &String| -> String {
            if v.is_empty() {
                String::new()
            } else {
                "<redacted>".to_string()
            }
        };

        // Mask credentials embedded in a connection URL's userinfo component
        // (e.g. `postgres://user:pass@host/db` → `postgres://<redacted>@host/db`).
        // URLs without credentials pass through unchanged. This prevents
        // `relational_db_url`/`vector_db_url`/`graph_database_url` from leaking
        // passwords when the settings snapshot is logged.
        let mask_url = |v: &String| -> String {
            // Find the `scheme://` prefix and the `@` that terminates userinfo.
            if let Some(scheme_end) = v.find("://") {
                let after_scheme = scheme_end + 3;
                if let Some(at_rel) = v[after_scheme..].find('@') {
                    let at_abs = after_scheme + at_rel;
                    // Only redact when userinfo actually carries a credential
                    // separator or any non-empty content.
                    if at_abs > after_scheme {
                        return format!("{}<redacted>{}", &v[..after_scheme], &v[at_abs..]);
                    }
                }
            }
            v.clone()
        };

        // LLM
        m.insert("llm_provider".into(), Value::String(s.llm_provider.clone()));
        m.insert("llm_model".into(), Value::String(s.llm_model.clone()));
        m.insert("llm_api_key".into(), Value::String(mask(&s.llm_api_key)));
        m.insert("llm_endpoint".into(), Value::String(s.llm_endpoint.clone()));
        m.insert(
            "llm_api_version".into(),
            Value::String(s.llm_api_version.clone()),
        );
        m.insert(
            "llm_temperature".into(),
            Value::Number(
                serde_json::Number::from_f64(s.llm_temperature)
                    .unwrap_or(serde_json::Number::from(0)),
            ),
        );
        m.insert(
            "llm_max_completion_tokens".into(),
            Value::Number(s.llm_max_completion_tokens.into()),
        );

        // Embedding
        m.insert(
            "embedding_provider".into(),
            Value::String(s.embedding_provider.clone()),
        );
        m.insert(
            "embedding_model_name".into(),
            Value::String(s.embedding_model_name.clone()),
        );
        m.insert(
            "embedding_api_key".into(),
            Value::String(mask(&s.embedding_api_key)),
        );
        m.insert(
            "embedding_endpoint".into(),
            Value::String(s.embedding_endpoint.clone()),
        );
        m.insert(
            "embedding_dimensions".into(),
            Value::Number(s.embedding_dimensions.into()),
        );

        // Graph DB
        m.insert(
            "graph_database_provider".into(),
            Value::String(s.graph_database_provider.clone()),
        );
        m.insert(
            "graph_database_url".into(),
            Value::String(mask_url(&s.graph_database_url)),
        );
        m.insert(
            "graph_database_password".into(),
            Value::String(mask(&s.graph_database_password)),
        );
        m.insert(
            "graph_database_key".into(),
            Value::String(mask(&s.graph_database_key)),
        );

        // Vector DB
        m.insert(
            "vector_db_provider".into(),
            Value::String(s.vector_db_provider.clone()),
        );
        m.insert(
            "vector_db_url".into(),
            Value::String(mask_url(&s.vector_db_url)),
        );
        m.insert(
            "vector_db_key".into(),
            Value::String(mask(&s.vector_db_key)),
        );
        m.insert(
            "vector_db_password".into(),
            Value::String(mask(&s.vector_db_password)),
        );

        // Relational DB
        m.insert("db_provider".into(), Value::String(s.db_provider.clone()));
        m.insert(
            "relational_db_url".into(),
            Value::String(mask_url(&s.relational_db_url)),
        );
        m.insert("db_password".into(), Value::String(mask(&s.db_password)));

        // Paths
        m.insert(
            "system_root_directory".into(),
            Value::String(s.system_root_directory.clone()),
        );
        m.insert(
            "data_root_directory".into(),
            Value::String(s.data_root_directory.clone()),
        );
        m.insert(
            "logs_root_directory".into(),
            Value::String(s.logs_root_directory.clone()),
        );

        // Chunking
        m.insert(
            "chunk_strategy".into(),
            Value::String(s.chunk_strategy.clone()),
        );
        m.insert("chunk_size".into(), Value::Number(s.chunk_size.into()));
        m.insert(
            "chunk_overlap".into(),
            Value::Number(s.chunk_overlap.into()),
        );

        m
    }
}

// -- Bulk setters and generic dispatch ---------------------------------------

/// Extract a `String` from a JSON value, or return a type-mismatch error.
fn as_string(key: &str, value: &serde_json::Value) -> Result<String, ConfigError> {
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| ConfigError::TypeMismatch {
            key: key.to_string(),
            reason: "expected a string".to_string(),
        })
}

/// Extract a `u32` from a JSON value, or return a type-mismatch error.
fn as_u32(key: &str, value: &serde_json::Value) -> Result<u32, ConfigError> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| ConfigError::TypeMismatch {
            key: key.to_string(),
            reason: "expected a positive integer (u32)".to_string(),
        })
}

/// Extract an `f64` from a JSON value, or return a type-mismatch error.
fn as_f64(key: &str, value: &serde_json::Value) -> Result<f64, ConfigError> {
    value.as_f64().ok_or_else(|| ConfigError::TypeMismatch {
        key: key.to_string(),
        reason: "expected a number".to_string(),
    })
}

/// Extract a `u16` from a JSON value, or return a type-mismatch error.
fn as_u16(key: &str, value: &serde_json::Value) -> Result<u16, ConfigError> {
    value
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .ok_or_else(|| ConfigError::TypeMismatch {
            key: key.to_string(),
            reason: "expected a positive integer (u16)".to_string(),
        })
}

/// Extract a `bool` from a JSON value, or return a type-mismatch error.
fn as_bool(key: &str, value: &serde_json::Value) -> Result<bool, ConfigError> {
    value.as_bool().ok_or_else(|| ConfigError::TypeMismatch {
        key: key.to_string(),
        reason: "expected a boolean".to_string(),
    })
}

impl ConfigManager {
    /// Bulk-update LLM config from a map. Matches Python `config.set_llm_config()`.
    pub fn set_llm_config(
        &self,
        values: &HashMap<String, serde_json::Value>,
    ) -> Result<(), ConfigError> {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        for (key, value) in values {
            match key.as_str() {
                "llm_provider" => s.llm_provider = as_string(key, value)?,
                "llm_model" => s.llm_model = as_string(key, value)?,
                "llm_api_key" => s.llm_api_key = as_string(key, value)?,
                "llm_endpoint" => s.llm_endpoint = as_string(key, value)?,
                "llm_api_version" => s.llm_api_version = as_string(key, value)?,
                "llm_temperature" => s.llm_temperature = as_f64(key, value)?,
                "llm_max_completion_tokens" => s.llm_max_completion_tokens = as_u32(key, value)?,
                "llm_streaming" => s.llm_streaming = as_bool(key, value)?,
                "llm_max_retries" => s.llm_max_retries = as_u32(key, value)?,
                "llm_max_parallel_requests" => {
                    s.llm_max_parallel_requests = as_u32(key, value)?;
                }
                "llm_mock" => s.llm_mock = as_bool(key, value)?,
                "llm_cassette" => s.llm_cassette = as_string(key, value)?,
                "llm_record_path" => s.llm_record_path = as_string(key, value)?,
                other => return Err(ConfigError::UnknownKey(other.to_string())),
            }
        }
        drop(s);
        self.bump_version();
        Ok(())
    }

    /// Bulk-update embedding config from a map. Matches Python `config.set_embedding_config()`.
    pub fn set_embedding_config(
        &self,
        values: &HashMap<String, serde_json::Value>,
    ) -> Result<(), ConfigError> {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        for (key, value) in values {
            match key.as_str() {
                "embedding_provider" => s.embedding_provider = as_string(key, value)?,
                "embedding_model" | "embedding_model_name" => {
                    s.embedding_model_name = as_string(key, value)?;
                }
                "embedding_dimensions" => s.embedding_dimensions = as_u32(key, value)?,
                "embedding_endpoint" => s.embedding_endpoint = as_string(key, value)?,
                "embedding_api_key" => s.embedding_api_key = as_string(key, value)?,
                "embedding_model_path" => s.embedding_model_path = as_string(key, value)?,
                "embedding_tokenizer_path" => {
                    s.embedding_tokenizer_path = as_string(key, value)?;
                }
                "embedding_api_version" => s.embedding_api_version = as_string(key, value)?,
                other => return Err(ConfigError::UnknownKey(other.to_string())),
            }
        }
        drop(s);
        self.bump_version();
        Ok(())
    }

    /// Bulk-update vector DB config from a map. Matches Python `config.set_vector_db_config()`.
    pub fn set_vector_db_config(
        &self,
        values: &HashMap<String, serde_json::Value>,
    ) -> Result<(), ConfigError> {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        for (key, value) in values {
            match key.as_str() {
                "vector_db_provider" => s.vector_db_provider = as_string(key, value)?,
                "vector_db_url" => s.vector_db_url = as_string(key, value)?,
                "vector_db_key" => s.vector_db_key = as_string(key, value)?,
                "vector_db_host" => s.vector_db_host = as_string(key, value)?,
                "vector_db_port" => s.vector_db_port = as_u16(key, value)?,
                "vector_db_name" => s.vector_db_name = as_string(key, value)?,
                other => return Err(ConfigError::UnknownKey(other.to_string())),
            }
        }
        drop(s);
        self.bump_version();
        Ok(())
    }

    /// Bulk-update graph DB config from a map. Matches Python `config.set_graph_db_config()`.
    pub fn set_graph_db_config(
        &self,
        values: &HashMap<String, serde_json::Value>,
    ) -> Result<(), ConfigError> {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        for (key, value) in values {
            match key.as_str() {
                "graph_database_provider" => s.graph_database_provider = as_string(key, value)?,
                "graph_model" => s.graph_model = as_string(key, value)?,
                "graph_file_path" => s.graph_file_path = as_string(key, value)?,
                other => return Err(ConfigError::UnknownKey(other.to_string())),
            }
        }
        drop(s);
        self.bump_version();
        Ok(())
    }

    /// Generic setter matching Python's `config.set(key, value)`.
    ///
    /// Dispatches to the appropriate typed setter based on the key name.
    /// Returns `ConfigError::UnknownKey` for unrecognized keys and
    /// `ConfigError::TypeMismatch` when the JSON value type doesn't match.
    pub fn set(&self, key: &str, value: serde_json::Value) -> Result<(), ConfigError> {
        match key {
            // LLM
            "llm_provider" => self.set_llm_provider(as_string(key, &value)?.as_str()),
            "llm_model" => self.set_llm_model(as_string(key, &value)?.as_str()),
            "llm_api_key" => self.set_llm_api_key(as_string(key, &value)?.as_str()),
            "llm_endpoint" => self.set_llm_endpoint(as_string(key, &value)?.as_str()),
            // LLM tuning
            "llm_api_version" => self.set_llm_api_version(as_string(key, &value)?.as_str()),
            "llm_temperature" => self.set_llm_temperature(as_f64(key, &value)?),
            "llm_streaming" => self.set_llm_streaming(as_bool(key, &value)?),
            "llm_max_completion_tokens" => {
                self.set_llm_max_completion_tokens(as_u32(key, &value)?);
            }
            "llm_max_retries" => self.set_llm_max_retries(as_u32(key, &value)?),
            "llm_max_parallel_requests" => {
                self.set_llm_max_parallel_requests(as_u32(key, &value)?);
            }
            "llm_mock" => self.set_llm_mock(as_bool(key, &value)?),
            "llm_cassette" => self.set_llm_cassette(as_string(key, &value)?.as_str()),
            "llm_record_path" => self.set_llm_record_path(as_string(key, &value)?.as_str()),
            // Embedding
            "embedding_provider" => {
                self.set_embedding_provider(as_string(key, &value)?.as_str());
            }
            "embedding_model" | "embedding_model_name" => {
                self.set_embedding_model(as_string(key, &value)?.as_str());
            }
            "embedding_dimensions" => self.set_embedding_dimensions(as_u32(key, &value)?),
            "embedding_endpoint" => {
                self.set_embedding_endpoint(as_string(key, &value)?.as_str());
            }
            "embedding_api_key" => self.set_embedding_api_key(as_string(key, &value)?.as_str()),
            "embedding_model_path" => {
                self.set_embedding_model_path(as_string(key, &value)?.as_str());
            }
            "embedding_tokenizer_path" => {
                self.set_embedding_tokenizer_path(as_string(key, &value)?.as_str());
            }
            // Vector DB
            "vector_db_provider" => {
                self.set_vector_db_provider(as_string(key, &value)?.as_str());
            }
            "vector_db_url" => self.set_vector_db_url(as_string(key, &value)?.as_str()),
            "vector_db_key" => self.set_vector_db_key(as_string(key, &value)?.as_str()),
            "vector_db_host" => self.set_vector_db_host(as_string(key, &value)?.as_str()),
            "vector_db_port" => self.set_vector_db_port(as_u16(key, &value)?),
            "vector_db_name" => self.set_vector_db_name(as_string(key, &value)?.as_str()),
            // Graph DB
            "graph_database_provider" => {
                self.set_graph_database_provider(as_string(key, &value)?.as_str());
            }
            "graph_model" => self.set_graph_model(as_string(key, &value)?.as_str()),
            "graph_file_path" => self.set_graph_file_path(as_string(key, &value)?.as_str()),
            // Chunking
            "chunk_strategy" => self.set_chunk_strategy(as_string(key, &value)?.as_str()),
            "chunk_engine" => self.set_chunk_engine(as_string(key, &value)?.as_str()),
            "chunk_size" => self.set_chunk_size(as_u32(key, &value)?),
            "chunk_overlap" => self.set_chunk_overlap(as_u32(key, &value)?),
            // System paths
            "system_root_directory" => {
                self.set_system_root_directory(as_string(key, &value)?.as_str());
            }
            "data_root_directory" => {
                self.set_data_root_directory(as_string(key, &value)?.as_str());
            }
            "cache_root_directory" => {
                self.set_cache_root_directory(as_string(key, &value)?.as_str());
            }
            "logs_root_directory" => {
                self.set_logs_root_directory(as_string(key, &value)?.as_str());
            }
            "monitoring_tool" => self.set_monitoring_tool(as_string(key, &value)?.as_str()),
            // Ontology
            "ontology_file_path" => {
                self.set_ontology_file_path(as_string(key, &value)?.as_str());
            }
            "ontology_resolver" => {
                self.set_ontology_resolver(as_string(key, &value)?.as_str());
            }
            "ontology_matching_strategy" => {
                self.set_ontology_matching_strategy(as_string(key, &value)?.as_str());
            }
            // Embedding extras
            "embedding_api_version" => {
                self.set_embedding_api_version(as_string(key, &value)?.as_str());
            }
            "transcription_model" => {
                self.set_transcription_model(as_string(key, &value)?.as_str());
            }
            // LLM fallback
            "llm_fallback_model" => {
                self.set_llm_fallback_model(as_string(key, &value)?.as_str());
            }
            "llm_fallback_provider" => {
                self.set_llm_fallback_provider(as_string(key, &value)?.as_str());
            }
            "llm_fallback_endpoint" => {
                self.set_llm_fallback_endpoint(as_string(key, &value)?.as_str());
            }
            "llm_fallback_api_key" => {
                self.set_llm_fallback_api_key(as_string(key, &value)?.as_str());
            }
            // Relational DB
            "relational_db_url" => {
                self.set_relational_db_url(as_string(key, &value)?.as_str());
            }
            "migration_db_url" => {
                self.set_migration_db_config(as_string(key, &value)?.as_str());
            }
            // ML models
            "classification_model" => {
                self.set_classification_model(as_string(key, &value)?.as_str());
            }
            "summarization_model" => {
                self.set_summarization_model(as_string(key, &value)?.as_str());
            }
            _ => return Err(ConfigError::UnknownKey(key.to_string())),
        }
        Ok(())
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

    // Each test sets env vars and must clean up after itself.  `serial` prevents
    // parallel tests from seeing each other's env mutations.

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_ontology_file_path() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("ONTOLOGY_FILE_PATH", "/tmp/test.owl") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("ONTOLOGY_FILE_PATH") };

        assert_eq!(s.ontology_file_path, "/tmp/test.owl");
        // resolver / strategy should stay at defaults when not set
        assert_eq!(s.ontology_resolver, "rdflib");
        assert_eq!(s.ontology_matching_strategy, "fuzzy");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_onnx_batch_size_is_independent_of_request_batch() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("EMBEDDING_ONNX_BATCH_SIZE", "8") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("EMBEDDING_ONNX_BATCH_SIZE") };

        assert_eq!(s.embedding_onnx_batch_size, 8);
        // The HTTP request batch is untouched by the ONNX override.
        assert_eq!(
            s.embedding_batch_size,
            Settings::default().embedding_batch_size
        );
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_ontology_resolver() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("ONTOLOGY_RESOLVER", "custom") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("ONTOLOGY_RESOLVER") };

        assert_eq!(s.ontology_resolver, "custom");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_ontology_matching_strategy() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("ONTOLOGY_MATCHING_STRATEGY", "exact") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("ONTOLOGY_MATCHING_STRATEGY") };

        assert_eq!(s.ontology_matching_strategy, "exact");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_ignores_empty_ontology_file_path() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("ONTOLOGY_FILE_PATH", "") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("ONTOLOGY_FILE_PATH") };

        // Empty string must not override the default (the str_var helper filters
        // out empty values, so ontology_file_path remains its default empty string).
        assert_eq!(s.ontology_file_path, "");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_cache_backend() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("CACHE_BACKEND", "redis") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("CACHE_BACKEND") };

        assert_eq!(s.cache_backend, "redis");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_llm_max_completion_tokens_primary() {
        // Primary env var takes precedence over the legacy alias.
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("LLM_MAX_COMPLETION_TOKENS", "4096") };
        unsafe { std::env::set_var("LLM_MAX_TOKENS", "8192") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LLM_MAX_COMPLETION_TOKENS") };
        unsafe { std::env::remove_var("LLM_MAX_TOKENS") };

        assert_eq!(s.llm_max_completion_tokens, 4096);
    }

    #[test]
    #[serial_test::serial]
    fn overlay_llm_max_completion_tokens_alias_fallback() {
        // When the primary is unset, the legacy alias is used.
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::remove_var("LLM_MAX_COMPLETION_TOKENS") };
        unsafe { std::env::set_var("LLM_MAX_TOKENS", "2048") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LLM_MAX_TOKENS") };

        assert_eq!(s.llm_max_completion_tokens, 2048);
    }

    #[test]
    #[serial_test::serial]
    fn overlay_llm_streaming_bool_parsing() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        for (input, expected) in [
            ("true", true),
            ("True", true),
            ("TRUE", true),
            ("1", true),
            ("yes", true),
            ("false", false),
            ("0", false),
            ("no", false),
        ] {
            unsafe { std::env::set_var("LLM_STREAMING", input) };
            let mut s = Settings::default();
            s.overlay_from_env();
            unsafe { std::env::remove_var("LLM_STREAMING") };

            assert_eq!(
                s.llm_streaming, expected,
                "LLM_STREAMING={input} should parse to {expected}"
            );
        }
    }

    // -- ConfigManager tests --------------------------------------------------

    #[test]
    fn config_manager_version_starts_at_zero() {
        let cm = ConfigManager::new(Settings::default());
        assert_eq!(cm.version(), 0);
    }

    #[test]
    fn config_manager_setter_bumps_version() {
        let cm = ConfigManager::new(Settings::default());
        cm.set_llm_model("gpt-4o");
        assert_eq!(cm.version(), 1);
        assert_eq!(cm.read().llm_model, "gpt-4o");

        cm.set_llm_api_key("sk-test");
        assert_eq!(cm.version(), 2);
        assert_eq!(cm.read().llm_api_key, "sk-test");
    }

    #[test]
    fn config_manager_clone_shares_state() {
        let cm1 = ConfigManager::new(Settings::default());
        let cm2 = cm1.clone();

        cm1.set_llm_model("shared-model");
        assert_eq!(cm2.read().llm_model, "shared-model");
        assert_eq!(cm2.version(), 1);
    }

    #[test]
    fn config_manager_cascading_system_root() {
        let settings = Settings {
            system_root_directory: "/old/root".to_string(),
            graph_file_path: "/old/root/graph".to_string(),
            vector_db_url: "/old/root/vectors".to_string(),
            ..Default::default()
        };

        let cm = ConfigManager::new(settings);
        cm.set_system_root_directory("/new/root");

        let s = cm.read();
        assert_eq!(s.system_root_directory, "/new/root");
        assert_eq!(s.graph_file_path, "/new/root/graph");
        assert_eq!(s.vector_db_url, "/new/root/vectors");
    }

    #[test]
    fn config_manager_cascading_empty_graph_and_vector() {
        // When graph_file_path and vector_db_url are empty, cascading should
        // set them to defaults under the new system root.
        let cm = ConfigManager::new(Settings::default());
        cm.set_system_root_directory("/data/cognee");

        let s = cm.read();
        assert_eq!(s.graph_file_path, "/data/cognee/graph");
        assert_eq!(s.vector_db_url, "/data/cognee/vectors");
    }

    #[test]
    fn config_manager_no_cascade_when_custom_paths() {
        let settings = Settings {
            system_root_directory: "/old".to_string(),
            graph_file_path: "/custom/graph".to_string(), // not under /old
            vector_db_url: "/custom/vectors".to_string(), // not under /old
            ..Default::default()
        };

        let cm = ConfigManager::new(settings);
        cm.set_system_root_directory("/new");

        let s = cm.read();
        // Custom paths should NOT be cascaded
        assert_eq!(s.graph_file_path, "/custom/graph");
        assert_eq!(s.vector_db_url, "/custom/vectors");
    }

    #[test]
    fn config_manager_generic_set_string() {
        let cm = ConfigManager::new(Settings::default());
        cm.set("llm_model", serde_json::Value::String("test-model".into()))
            .expect("set should succeed");
        assert_eq!(cm.read().llm_model, "test-model");
    }

    #[test]
    fn config_manager_generic_set_u32() {
        let cm = ConfigManager::new(Settings::default());
        cm.set("chunk_size", serde_json::json!(2048))
            .expect("set should succeed");
        assert_eq!(cm.read().chunk_size, 2048);
    }

    #[test]
    fn config_manager_generic_set_unknown_key() {
        let cm = ConfigManager::new(Settings::default());
        let result = cm.set("nonexistent_key", serde_json::json!("value"));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::UnknownKey(k) => assert_eq!(k, "nonexistent_key"),
            other => panic!("expected UnknownKey, got: {other}"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn overlay_enable_backend_access_control() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("ENABLE_BACKEND_ACCESS_CONTROL", "true") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("ENABLE_BACKEND_ACCESS_CONTROL") };

        assert!(s.enable_access_control);

        // Also verify "1" works
        unsafe { std::env::set_var("ENABLE_BACKEND_ACCESS_CONTROL", "1") };
        let mut s2 = Settings::default();
        s2.overlay_from_env();
        unsafe { std::env::remove_var("ENABLE_BACKEND_ACCESS_CONTROL") };

        assert!(s2.enable_access_control);
    }

    #[test]
    fn config_manager_generic_set_type_mismatch() {
        let cm = ConfigManager::new(Settings::default());
        let result = cm.set("chunk_size", serde_json::json!("not a number"));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::TypeMismatch { key, .. } => assert_eq!(key, "chunk_size"),
            other => panic!("expected TypeMismatch, got: {other}"),
        }
    }

    #[test]
    fn config_manager_bulk_llm_config() {
        let cm = ConfigManager::new(Settings::default());
        let mut map = HashMap::new();
        map.insert("llm_model".into(), serde_json::json!("gpt-4o"));
        map.insert("llm_provider".into(), serde_json::json!("openai"));
        cm.set_llm_config(&map).expect("bulk set should succeed");

        let s = cm.read();
        assert_eq!(s.llm_model, "gpt-4o");
        assert_eq!(s.llm_provider, "openai");
    }

    #[test]
    fn config_manager_bulk_embedding_config() {
        let cm = ConfigManager::new(Settings::default());
        let mut map = HashMap::new();
        map.insert("embedding_provider".into(), serde_json::json!("openai"));
        map.insert("embedding_dimensions".into(), serde_json::json!(1536));
        cm.set_embedding_config(&map)
            .expect("bulk set should succeed");

        let s = cm.read();
        assert_eq!(s.embedding_provider, "openai");
        assert_eq!(s.embedding_dimensions, 1536);
    }

    // -- Option B widening: granular setters ----------------------------------

    #[test]
    fn config_manager_new_granular_setters_bump_version() {
        let cm = ConfigManager::new(Settings::default());
        let mut expected_version = 0u64;

        cm.set_llm_api_version("2024-02-15");
        expected_version += 1;
        assert_eq!(cm.read().llm_api_version, "2024-02-15");

        cm.set_llm_temperature(0.7);
        expected_version += 1;
        assert!((cm.read().llm_temperature - 0.7).abs() < f64::EPSILON);

        cm.set_llm_streaming(true);
        expected_version += 1;
        assert!(cm.read().llm_streaming);

        cm.set_llm_max_completion_tokens(2048);
        expected_version += 1;
        assert_eq!(cm.read().llm_max_completion_tokens, 2048);

        cm.set_llm_max_retries(5);
        expected_version += 1;
        assert_eq!(cm.read().llm_max_retries, 5);

        cm.set_llm_max_parallel_requests(8);
        expected_version += 1;
        assert_eq!(cm.read().llm_max_parallel_requests, 8);

        cm.set_embedding_model_path("/models/m.onnx");
        expected_version += 1;
        assert_eq!(cm.read().embedding_model_path, "/models/m.onnx");

        cm.set_embedding_tokenizer_path("/models/t.json");
        expected_version += 1;
        assert_eq!(cm.read().embedding_tokenizer_path, "/models/t.json");

        cm.set_vector_db_host("localhost");
        expected_version += 1;
        assert_eq!(cm.read().vector_db_host, "localhost");

        cm.set_vector_db_port(6333);
        expected_version += 1;
        assert_eq!(cm.read().vector_db_port, 6333);

        cm.set_vector_db_name("my_collection");
        expected_version += 1;
        assert_eq!(cm.read().vector_db_name, "my_collection");

        cm.set_graph_file_path("/data/graph");
        expected_version += 1;
        assert_eq!(cm.read().graph_file_path, "/data/graph");

        cm.set_cache_root_directory("/tmp/cache");
        expected_version += 1;
        assert_eq!(cm.read().cache_root_directory, "/tmp/cache");

        cm.set_logs_root_directory("/tmp/logs");
        expected_version += 1;
        assert_eq!(cm.read().logs_root_directory, "/tmp/logs");

        cm.set_ontology_file_path("/onto.owl");
        expected_version += 1;
        assert_eq!(cm.read().ontology_file_path, "/onto.owl");

        cm.set_ontology_resolver("custom");
        expected_version += 1;
        assert_eq!(cm.read().ontology_resolver, "custom");

        cm.set_ontology_matching_strategy("exact");
        expected_version += 1;
        assert_eq!(cm.read().ontology_matching_strategy, "exact");

        // Every granular setter must have bumped the version exactly once.
        assert_eq!(cm.version(), expected_version);
    }

    #[test]
    fn config_manager_set_graph_file_path_does_not_cascade() {
        let settings = Settings {
            system_root_directory: "/root".to_string(),
            vector_db_url: "/root/vectors".to_string(),
            ..Default::default()
        };
        let cm = ConfigManager::new(settings);
        cm.set_graph_file_path("/elsewhere/graph");

        let s = cm.read();
        assert_eq!(s.graph_file_path, "/elsewhere/graph");
        // Unlike set_system_root_directory, vector_db_url is untouched.
        assert_eq!(s.vector_db_url, "/root/vectors");
        assert_eq!(s.system_root_directory, "/root");
    }

    // -- Option B widening: generic set() dispatch ----------------------------

    #[test]
    fn config_manager_generic_set_new_keys() {
        let cm = ConfigManager::new(Settings::default());

        cm.set("llm_temperature", serde_json::json!(0.5))
            .expect("llm_temperature should be settable");
        assert!((cm.read().llm_temperature - 0.5).abs() < f64::EPSILON);

        cm.set("llm_streaming", serde_json::json!(true))
            .expect("llm_streaming should be settable");
        assert!(cm.read().llm_streaming);

        cm.set("llm_max_retries", serde_json::json!(7))
            .expect("llm_max_retries should be settable");
        assert_eq!(cm.read().llm_max_retries, 7);

        cm.set("vector_db_host", serde_json::json!("host"))
            .expect("vector_db_host should be settable");
        assert_eq!(cm.read().vector_db_host, "host");

        cm.set("vector_db_port", serde_json::json!(6333))
            .expect("vector_db_port should be settable");
        assert_eq!(cm.read().vector_db_port, 6333);

        cm.set("graph_file_path", serde_json::json!("/g"))
            .expect("graph_file_path should be settable");
        assert_eq!(cm.read().graph_file_path, "/g");

        cm.set("cache_root_directory", serde_json::json!("/c"))
            .expect("cache_root_directory should be settable");
        assert_eq!(cm.read().cache_root_directory, "/c");

        cm.set("logs_root_directory", serde_json::json!("/l"))
            .expect("logs_root_directory should be settable");
        assert_eq!(cm.read().logs_root_directory, "/l");

        cm.set("ontology_file_path", serde_json::json!("/o.owl"))
            .expect("ontology_file_path should be settable");
        assert_eq!(cm.read().ontology_file_path, "/o.owl");

        cm.set("embedding_model_path", serde_json::json!("/m.onnx"))
            .expect("embedding_model_path should be settable");
        assert_eq!(cm.read().embedding_model_path, "/m.onnx");
    }

    #[test]
    fn config_manager_generic_set_u16_type_mismatch() {
        let cm = ConfigManager::new(Settings::default());
        let result = cm.set("vector_db_port", serde_json::json!("not a number"));
        match result.unwrap_err() {
            ConfigError::TypeMismatch { key, .. } => assert_eq!(key, "vector_db_port"),
            other => panic!("expected TypeMismatch, got: {other}"),
        }
    }

    // -- Option B widening: bulk setter allowlists ----------------------------

    #[test]
    fn config_manager_bulk_llm_config_new_keys() {
        let cm = ConfigManager::new(Settings::default());
        let mut map = HashMap::new();
        map.insert("llm_streaming".into(), serde_json::json!(true));
        map.insert("llm_max_retries".into(), serde_json::json!(9));
        map.insert("llm_max_parallel_requests".into(), serde_json::json!(3));
        cm.set_llm_config(&map).expect("bulk set should succeed");

        let s = cm.read();
        assert!(s.llm_streaming);
        assert_eq!(s.llm_max_retries, 9);
        assert_eq!(s.llm_max_parallel_requests, 3);
    }

    #[test]
    fn config_manager_bulk_vector_db_config_new_keys() {
        let cm = ConfigManager::new(Settings::default());
        let mut map = HashMap::new();
        map.insert("vector_db_host".into(), serde_json::json!("vhost"));
        map.insert("vector_db_port".into(), serde_json::json!(6333));
        map.insert("vector_db_name".into(), serde_json::json!("coll"));
        cm.set_vector_db_config(&map)
            .expect("bulk set should succeed");

        let s = cm.read();
        assert_eq!(s.vector_db_host, "vhost");
        assert_eq!(s.vector_db_port, 6333);
        assert_eq!(s.vector_db_name, "coll");
    }

    #[test]
    fn config_manager_bulk_embedding_config_new_keys() {
        let cm = ConfigManager::new(Settings::default());
        let mut map = HashMap::new();
        map.insert("embedding_model_path".into(), serde_json::json!("/m.onnx"));
        map.insert(
            "embedding_tokenizer_path".into(),
            serde_json::json!("/t.json"),
        );
        cm.set_embedding_config(&map)
            .expect("bulk set should succeed");

        let s = cm.read();
        assert_eq!(s.embedding_model_path, "/m.onnx");
        assert_eq!(s.embedding_tokenizer_path, "/t.json");
    }

    #[test]
    fn config_manager_bulk_llm_config_rejects_out_of_subset_key() {
        // A vector key fed to set_llm_config must be rejected as UnknownKey.
        let cm = ConfigManager::new(Settings::default());
        let mut map = HashMap::new();
        map.insert("vector_db_url".into(), serde_json::json!("/v"));
        match cm.set_llm_config(&map).unwrap_err() {
            ConfigError::UnknownKey(k) => assert_eq!(k, "vector_db_url"),
            other => panic!("expected UnknownKey, got: {other}"),
        }
    }

    #[test]
    fn config_manager_embedding_fields_default() {
        let s = Settings::default();
        // Default provider: OpenAI everywhere except Android (local ONNX/edge).
        #[cfg(not(target_os = "android"))]
        {
            assert_eq!(s.embedding_provider, "openai");
            assert_eq!(s.embedding_model_name, "text-embedding-3-small");
            assert_eq!(s.embedding_dimensions, 1536);
        }
        #[cfg(target_os = "android")]
        {
            assert_eq!(s.embedding_provider, "onnx");
            assert_eq!(s.embedding_dimensions, 384);
        }
        // No embedding-specific endpoint/key by default — they fall back to the
        // LLM provider's at engine-build time.
        assert_eq!(s.embedding_endpoint, "");
        assert_eq!(s.embedding_api_key, "");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_embedding_provider() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("EMBEDDING_PROVIDER", "openai") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("EMBEDDING_PROVIDER") };

        assert_eq!(s.embedding_provider, "openai");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_log_level() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("LOG_LEVEL", "debug") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LOG_LEVEL") };

        assert_eq!(s.log_level, "debug");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_cognee_logs_dir() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("COGNEE_LOGS_DIR", "/tmp/logs") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("COGNEE_LOGS_DIR") };

        assert_eq!(s.logs_root_directory, "/tmp/logs");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_cache_root_directory() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("CACHE_ROOT_DIRECTORY", "/tmp/cache") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("CACHE_ROOT_DIRECTORY") };

        assert_eq!(s.cache_root_directory, "/tmp/cache");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_enable_last_accessed() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("ENABLE_LAST_ACCESSED", "yes") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("ENABLE_LAST_ACCESSED") };

        assert!(s.enable_last_accessed);
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_otel_service_name() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("OTEL_SERVICE_NAME", "my-service") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("OTEL_SERVICE_NAME") };

        assert_eq!(s.otel_service_name, "my-service");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_otel_exporter_otlp_endpoint() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4317") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT") };

        assert_eq!(s.otel_exporter_otlp_endpoint, "http://collector:4317");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_otel_exporter_otlp_headers() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe {
            std::env::set_var(
                "OTEL_EXPORTER_OTLP_HEADERS",
                "authorization=Bearer abc,x-trace=on",
            )
        };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("OTEL_EXPORTER_OTLP_HEADERS") };

        assert_eq!(
            s.otel_exporter_otlp_headers,
            "authorization=Bearer abc,x-trace=on"
        );
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_otel_exporter_otlp_protocol() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("OTEL_EXPORTER_OTLP_PROTOCOL") };

        assert_eq!(s.otel_exporter_otlp_protocol, "http/protobuf");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_otel_span_processor() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("OTEL_SPAN_PROCESSOR", "simple") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("OTEL_SPAN_PROCESSOR") };

        assert_eq!(s.otel_span_processor, "simple");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_otel_traces_sampler() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("OTEL_TRACES_SAMPLER", "parentbased_traceidratio") };
        unsafe { std::env::set_var("OTEL_TRACES_SAMPLER_ARG", "0.25") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("OTEL_TRACES_SAMPLER") };
        unsafe { std::env::remove_var("OTEL_TRACES_SAMPLER_ARG") };

        assert_eq!(s.otel_traces_sampler, "parentbased_traceidratio");
        assert_eq!(s.otel_traces_sampler_arg, "0.25");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_rate_limit_requests() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("LLM_RATE_LIMIT_REQUESTS", "120") };
        unsafe { std::env::set_var("EMBEDDING_RATE_LIMIT_REQUESTS", "30") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LLM_RATE_LIMIT_REQUESTS") };
        unsafe { std::env::remove_var("EMBEDDING_RATE_LIMIT_REQUESTS") };

        assert_eq!(s.llm_rate_limit_requests, 120);
        assert_eq!(s.embedding_rate_limit_requests, 30);
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_storage_backend() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("STORAGE_BACKEND", "s3") };
        unsafe { std::env::set_var("STORAGE_BUCKET_NAME", "my-bucket") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("STORAGE_BACKEND") };
        unsafe { std::env::remove_var("STORAGE_BUCKET_NAME") };

        assert_eq!(s.storage_backend, "s3");
        assert_eq!(s.storage_bucket_name, "my-bucket");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_parses_llm_args_json_object() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("LLM_ARGS", r#"{"max_tokens": 16384, "top_p": 0.9}"#) };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LLM_ARGS") };

        assert_eq!(
            s.llm_args.get("max_tokens"),
            Some(&serde_json::json!(16384))
        );
        assert_eq!(s.llm_args.get("top_p"), Some(&serde_json::json!(0.9)));
        // The lowered backend context carries the same map through to the factory.
        assert_eq!(s.backend_context().llm.llm_args, s.llm_args);
    }

    #[test]
    #[serial_test::serial]
    fn overlay_ignores_malformed_llm_args() {
        // A non-object / malformed value is ignored (left empty), not fatal.
        unsafe { std::env::set_var("LLM_ARGS", "not json") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LLM_ARGS") };
        assert!(s.llm_args.is_empty());

        unsafe { std::env::set_var("LLM_ARGS", "[1, 2, 3]") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LLM_ARGS") };
        assert!(s.llm_args.is_empty());
    }

    #[test]
    fn default_values_are_correct() {
        let s = Settings::default();
        assert!(s.llm_args.is_empty());
        assert_eq!(s.cache_backend, "fs");
        assert_eq!(s.cache_host, "localhost");
        assert_eq!(s.cache_port, 6379);
        assert_eq!(s.session_ttl_seconds, 604800);
        assert!(s.enable_caching);
        assert!(!s.auto_feedback);
        assert!(!s.enable_access_control);
        assert_eq!(s.log_level, "info");
        assert!(!s.llm_rate_limit_enabled);
        assert_eq!(s.llm_rate_limit_requests, 60);
        assert_eq!(s.llm_rate_limit_interval, 60);
        assert!(!s.embedding_rate_limit_enabled);
        assert_eq!(s.embedding_rate_limit_requests, 60);
        assert_eq!(s.embedding_rate_limit_interval, 60);
        assert_eq!(s.storage_backend, "local");
        assert!(!s.cognee_tracing_enabled);
        assert_eq!(s.otel_service_name, "cognee");
        assert_eq!(s.otel_exporter_otlp_endpoint, "");
        assert_eq!(s.otel_exporter_otlp_headers, "");
        assert_eq!(s.otel_exporter_otlp_protocol, "grpc");
        assert_eq!(s.otel_span_processor, "batch");
        assert_eq!(s.otel_traces_sampler, "");
        assert_eq!(s.otel_traces_sampler_arg, "");
        assert!(!s.enable_last_accessed);
        #[cfg(not(target_os = "android"))]
        assert_eq!(s.embedding_provider, "openai");
        #[cfg(target_os = "android")]
        assert_eq!(s.embedding_provider, "onnx");
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_embedding_endpoint() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("EMBEDDING_ENDPOINT", "https://api.example.com/embed") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("EMBEDDING_ENDPOINT") };

        assert_eq!(s.embedding_endpoint, "https://api.example.com/embed");
    }

    #[test]
    fn telemetry_snapshot_only_emits_allowlisted_keys() {
        let cfg = Settings::default();
        let snap = cfg.telemetry_snapshot();
        let keys: std::collections::BTreeSet<&str> = snap.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> = [
            "sdk_runtime",
            "vector_db_provider",
            "graph_db_provider",
            "relational_db_provider",
            "llm_provider",
            "llm_model",
            "embedding_provider",
            "embedding_model",
            "embedding_dimensions",
            "chunk_strategy",
        ]
        .iter()
        .copied()
        .collect();
        assert_eq!(
            keys, expected,
            "telemetry_snapshot must not leak fields outside the allowlist"
        );
    }

    #[test]
    fn telemetry_snapshot_redacts_credentials_and_urls() {
        let cfg = Settings {
            llm_api_key: "sk-secret".into(),
            embedding_api_key: "sk-also-secret".into(),
            vector_db_password: "vector-pass".into(),
            db_password: "db-pass".into(),
            relational_db_url: "postgres://user:pass@host/db".into(),
            embedding_endpoint: "https://internal.example/v1/embed".into(),
            ..Settings::default()
        };

        let snap = cfg.telemetry_snapshot();
        let json =
            serde_json::to_string(&snap).expect("serde_json::Map<String,Value> always serializes");
        for forbidden in [
            "sk-secret",
            "sk-also-secret",
            "vector-pass",
            "db-pass",
            "postgres://",
            "internal.example",
        ] {
            assert!(
                !json.contains(forbidden),
                "telemetry_snapshot leaked credential/URL substring: {forbidden}"
            );
        }
    }

    #[test]
    fn telemetry_snapshot_carries_sdk_runtime_rust() {
        let cfg = Settings::default();
        let snap = cfg.telemetry_snapshot();
        assert_eq!(
            snap.get("sdk_runtime"),
            Some(&serde_json::Value::String("rust".into()))
        );
    }

    #[test]
    fn test_config_defaults_match_expected_values() {
        let settings = Settings::default();
        assert_eq!(settings.graph_database_provider, "ladybug");
        assert_eq!(settings.logs_root_directory, "./logs");
    }

    #[test]
    fn test_get_settings_masks_secrets() {
        let cfg = ConfigManager::new(Settings::default());
        cfg.set_llm_api_key("my-secret-key");
        let settings = cfg.get_settings();
        let api_key = settings
            .get("llm_api_key")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_ne!(api_key, "my-secret-key", "API key must be masked");
        // A short key with no recognizable pattern passes through unchanged --
        // "my-secret-key" is not an OpenAI/bearer/password-prefixed value, so
        // redact() leaves it alone. What matters is the field is present.
        assert!(!api_key.is_empty(), "api_key field must be non-empty");
    }

    #[test]
    fn test_get_settings_masks_url_credentials() {
        let cfg = ConfigManager::new(Settings::default());
        cfg.set_relational_db_url("postgres://admin:s3cret@db.example.com:5432/cognee");
        let settings = cfg.get_settings();
        let url = settings
            .get("relational_db_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            !url.contains("s3cret") && !url.contains("admin"),
            "URL credentials must be masked, got: {url}"
        );
        assert!(
            url.contains("db.example.com") && url.contains("<redacted>"),
            "host must remain and userinfo redacted, got: {url}"
        );
        // A credential-free URL passes through unchanged.
        let cfg2 = ConfigManager::new(Settings::default());
        cfg2.set_relational_db_url("sqlite:///tmp/test.db");
        let s2 = cfg2.get_settings();
        assert_eq!(
            s2.get("relational_db_url").and_then(|v| v.as_str()),
            Some("sqlite:///tmp/test.db")
        );
    }

    #[test]
    fn test_set_relational_db_config_bulk() {
        let cfg = ConfigManager::new(Settings::default());
        cfg.set_relational_db_config(
            Some("sqlite:///tmp/test.db"),
            Some("sqlite"),
            None,
            None,
            None,
            None,
            None,
        );
        let s = cfg.read();
        assert_eq!(s.relational_db_url, "sqlite:///tmp/test.db");
        assert_eq!(s.db_provider, "sqlite");
    }

    #[test]
    fn test_llm_fallback_setters() {
        let cfg = ConfigManager::new(Settings::default());
        cfg.set_llm_fallback_model("gpt-4o-mini");
        cfg.set_llm_fallback_provider("openai");
        cfg.set_llm_fallback_endpoint("https://fallback.example.com/v1");
        cfg.set_llm_fallback_api_key("fallback-key");
        let s = cfg.read();
        assert_eq!(s.llm_fallback_model, "gpt-4o-mini");
        assert_eq!(s.llm_fallback_provider, "openai");
        assert_eq!(s.llm_fallback_endpoint, "https://fallback.example.com/v1");
        assert_eq!(s.llm_fallback_api_key, "fallback-key");
    }

    #[test]
    fn test_embedding_api_version_setter() {
        let cfg = ConfigManager::new(Settings::default());
        cfg.set_embedding_api_version("2024-02-15");
        assert_eq!(cfg.read().embedding_api_version, "2024-02-15");
    }

    #[test]
    fn test_transcription_model_setter() {
        let cfg = ConfigManager::new(Settings::default());
        cfg.set_transcription_model("whisper-1");
        assert_eq!(cfg.read().transcription_model, "whisper-1");
    }

    #[test]
    fn test_migration_db_config_setter() {
        let cfg = ConfigManager::new(Settings::default());
        cfg.set_migration_db_config("postgres://localhost/migrations");
        assert_eq!(
            cfg.read().migration_db_url,
            "postgres://localhost/migrations"
        );
    }
}
