//! HTTP server configuration.
//!
//! `HttpServerConfig` holds all tuneable parameters.  `from_env()` reads the
//! documented environment variables and overlays them on the struct defaults.
//! Only the standalone binary calls `from_env()`; library embedders construct
//! `HttpServerConfig` directly.

use std::{path::PathBuf, str::FromStr, time::Duration};

use secrecy::{ExposeSecret, SecretString};

use crate::error::ServerError;

// re-export for use in state.rs
pub use cognee_core::pipeline_run_registry::RegistryConfig;

// ─── Environment enum ─────────────────────────────────────────────────────────

/// Deployment environment.  Controls log format (pretty vs JSON) and other
/// dev-vs-prod defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Environment {
    Dev,
    #[default]
    Prod,
    Test,
}

impl FromStr for Environment {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "dev" | "development" => Ok(Environment::Dev),
            "test" | "testing" => Ok(Environment::Test),
            _ => Ok(Environment::Prod),
        }
    }
}

// ─── HttpServerConfig ─────────────────────────────────────────────────────────

/// All tuneable server parameters.
///
/// Defaults mirror the Python FastAPI server defaults.
#[derive(Debug, Clone)]
pub struct HttpServerConfig {
    /// Bind address. Env: `HTTP_API_HOST`. Default: `"0.0.0.0"`.
    pub host: String,
    /// Bind port. Env: `HTTP_API_PORT`. Default: `8000`.
    pub port: u16,
    /// Explicit CORS allowed origins. Env: `CORS_ALLOWED_ORIGINS` (comma-sep).
    /// Falls back to `[ui_app_url]` when empty.
    pub cors_allowed_origins: Vec<String>,
    /// Frontend URL used as the CORS fallback. Env: `UI_APP_URL`.
    /// Default: `"http://localhost:3000"`.
    pub ui_app_url: String,
    /// Deployment environment. Env: `ENV`. Default: `Prod`.
    pub env: Environment,
    /// Enforce authentication on every request.
    ///
    /// OSS default: `false` — the slim auth/extractor.rs falls back
    /// to `default_user_from_state` (the `uuid5(NAMESPACE_OID, email)`
    /// default user) when no `AuthResolver` is wired and the request
    /// carries no credential. Closed cloud builds inject an
    /// `AuthResolver` via `RouterBuilder::with_auth_resolver(...)` and
    /// set this to `true` to require credentials.
    ///
    /// Override at runtime with `REQUIRE_AUTHENTICATION=true`.
    pub require_authentication: bool,
    /// JWT signing secret. Env: `AUTH_JWT_SECRET`.
    /// Randomly generated at boot when unset (tokens are invalidated on restart).
    pub jwt_secret: SecretString,
    /// JWT validity window. Env: `AUTH_JWT_LIFETIME_SECONDS`. Default: 3600 s.
    pub jwt_lifetime: Duration,
    /// Maximum request body size in bytes. Env: `HTTP_BODY_LIMIT_BYTES`.
    /// Default: 100 MiB.
    pub body_limit: usize,

    // ── Pipeline registry knobs ──────────────────────────────────────────────
    //
    // These map to `cognee_core::pipeline_run_registry::RegistryConfig` fields.
    // Env vars are prefixed `PIPELINE_REGISTRY_` per pipelines.md §6.2.
    /// Max in-memory runs. Env: `PIPELINE_REGISTRY_MAX_RUNS`. Default: 4096.
    pub pipeline_registry_max_runs: usize,
    /// Finished-run retention in seconds. Env: `PIPELINE_REGISTRY_FINISHED_RETENTION_SECS`.
    /// Default: 3600.
    pub pipeline_registry_finished_retention_secs: u64,
    /// Per-run broadcast channel capacity. Env: `PIPELINE_REGISTRY_CHANNEL_CAPACITY`.
    /// Default: 64.
    pub pipeline_registry_channel_capacity: usize,
    /// Whether to write ERRORED rows on abort/shutdown.
    /// Env: `PIPELINE_REGISTRY_ABORT_WRITES_ERRORED`. Default: true.
    /// Set to false for strict Python parity (Python leaves rows as STARTED on
    /// unclean shutdown). See pipelines.md §12.
    pub pipeline_registry_abort_writes_errored: bool,

    /// Wall-clock timeout for `POST /api/v1/notebooks/{id}/{cell}/run`.
    /// Env: `NOTEBOOK_RUN_TIMEOUT_SECS`. Default: 30 s.
    pub notebook_run_timeout: Duration,

    // ── Health checker knobs ─────────────────────────────────────────────────
    /// Whether the `/health/detailed` probe should test the LLM provider and
    /// the embedding engine.
    /// Env: `COGNEE_HEALTH_PROBE_LLM`. Default: `false`.
    ///
    /// LLM probes consume tokens; embedding probes can hit a remote provider,
    /// so both are opt-in. When `false`, the corresponding entries are omitted
    /// from the report (mirrors Python's opt-in behavior).
    pub health_probe_llm: bool,

    /// Per-probe timeout in milliseconds. Each component probe is wrapped in
    /// `tokio::time::timeout(..)` with this value; expiry yields an
    /// `Unhealthy` (critical) or `Degraded` (non-critical) entry.
    /// Env: `COGNEE_HEALTH_PROBE_TIMEOUT_MS`. Default: 2000.
    pub health_probe_timeout_ms: u64,

    /// In-process cache TTL for the aggregated `HealthCheckReport`.
    /// Back-to-back `/health` requests within this window are served from
    /// cache to avoid hammering all backends from k8s liveness probes.
    /// Env: `COGNEE_HEALTH_CACHE_TTL_MS`. Default: 5000. Set to `0` to
    /// disable caching.
    pub health_cache_ttl_ms: u64,

    // ── Standalone backend wiring knobs ─────────────────────────────────────
    /// Root directory for ingested data files (LocalStorage).
    /// Env: `DATA_ROOT_DIRECTORY`.
    pub data_root_directory: PathBuf,

    /// Root directory for system state (graph/vector/sqlite files).
    /// Env: `SYSTEM_ROOT_DIRECTORY`.
    pub system_root_directory: PathBuf,

    /// Relational DB URL.
    /// Env: `RELATIONAL_DB_URL` (fallback `DATABASE_URL`).
    pub relational_db_url: String,

    /// Graph provider name.
    /// Env: `GRAPH_DATABASE_PROVIDER`. Default: `ladybug`.
    pub graph_provider: String,

    /// Graph file path (for embedded ladybug graph DB).
    /// Env: `GRAPH_FILE_PATH`.
    pub graph_file_path: PathBuf,

    /// Vector provider name.
    /// Env: `VECTOR_DB_PROVIDER`. Default: `pgvector`.
    /// Note: the qdrant adapter has been extracted to the closed
    /// `cognee-vector-qdrant` crate as part of the OSS/closed split. The OSS
    /// http-server now defaults to pgvector and supports `mock` only when
    /// built with the `dev-mock` cargo feature.
    pub vector_provider: String,

    /// Vector DB URL/path.
    /// Env: `VECTOR_DB_URL`. For pgvector this is a Postgres connection string.
    pub vector_db_url: String,

    /// Embedding provider name.
    /// Env: `EMBEDDING_PROVIDER`.
    pub embedding_provider: String,

    /// Embedding vector dimensions.
    /// Env: `EMBEDDING_DIMENSIONS`.
    pub embedding_dimensions: u32,

    /// Embedding model identifier.
    /// Env: `EMBEDDING_MODEL_NAME` (fallback `EMBEDDING_MODEL`).
    pub embedding_model_name: String,

    /// Embedding model file path (ONNX).
    /// Env: `EMBEDDING_MODEL_PATH`.
    pub embedding_model_path: Option<PathBuf>,

    /// Embedding tokenizer file path (ONNX).
    /// Env: `EMBEDDING_TOKENIZER_PATH`.
    pub embedding_tokenizer_path: Option<PathBuf>,

    /// Embedding endpoint for remote providers.
    /// Env: `EMBEDDING_ENDPOINT`.
    pub embedding_endpoint: String,

    /// Embedding API key.
    /// Env: `EMBEDDING_API_KEY` (fallbacks: `LLM_API_KEY`, `OPENAI_TOKEN`).
    pub embedding_api_key: SecretString,

    /// LLM provider name.
    /// Env: `LLM_PROVIDER`.
    pub llm_provider: String,

    /// LLM model name.
    /// Env: `LLM_MODEL` (fallback `OPENAI_MODEL`).
    pub llm_model: String,

    /// LLM API key.
    /// Env: `LLM_API_KEY` (fallback `OPENAI_TOKEN`).
    pub llm_api_key: SecretString,

    /// LLM endpoint.
    /// Env: `LLM_ENDPOINT` (fallback `OPENAI_URL`).
    pub llm_endpoint: String,

    /// LLM retry count for both structured-output and network retries.
    /// Env: `LLM_MAX_RETRIES`.
    pub llm_max_retries: u32,

    /// Session store backend selector.
    /// Env: `COGNEE_SESSION_STORE`.
    pub session_store_backend: String,

    /// Session root directory (for fs-based stores).
    /// Env: `COGNEE_SESSION_DIR`.
    pub session_root_directory: PathBuf,

    /// Whether notebook code execution backend is enabled.
    /// Env: `COGNEE_NOTEBOOK_RUNNER_ENABLED`.
    pub notebook_runner_enabled: bool,

    /// Whether Responses API client should be wired.
    /// Env: `COGNEE_RESPONSES_CLIENT_ENABLED`.
    pub responses_client_enabled: bool,

    /// Disable standalone default backend wiring in `main`.
    /// Env: `COGNEE_DISABLE_DEFAULT_BACKENDS`.
    pub disable_default_backends: bool,

    /// Email address used to derive the synthetic default user when
    /// `require_authentication=false` and no `AuthResolver` is wired.
    ///
    /// The resulting owner id is
    /// `Uuid::new_v5(&Uuid::NAMESPACE_OID, default_user_email.as_bytes())`
    /// — the same derivation used by
    /// [`cognee_lib::api::user::get_or_create_default_user`] and the
    /// Python reference SDK (`uuid5(NAMESPACE_OID, email)`).
    ///
    /// Mirrors `Settings::default_user_email` in `cognee-lib` so the HTTP
    /// server and the bindings/CLI agree on owner ids for the same
    /// configured email. Env: `DEFAULT_USER_EMAIL`. Default:
    /// `"default_user@example.com"`.
    pub default_user_email: String,
}

fn default_cache_root() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        PathBuf::from(xdg).join("cognee")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache").join("cognee")
    } else {
        PathBuf::from("./.cognee")
    }
}

fn parse_env_bool_with_default(v: &str, default: bool) -> bool {
    if cognee_utils::parse_env_bool(v) {
        true
    } else {
        let trimmed = v.trim().to_ascii_lowercase();
        if matches!(trimmed.as_str(), "false" | "0" | "no" | "off") {
            false
        } else {
            default
        }
    }
}

fn first_non_empty_env(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Append `?mode=rwc` to a file-backed SQLite URL that has no query string, so
/// the sea-orm/sqlx driver creates the database file when it does not yet
/// exist. Leaves in-memory URLs, URLs that already carry a query, and non-SQLite
/// URLs untouched.
fn ensure_sqlite_rwc(url: &str) -> String {
    if url.starts_with("sqlite:") && !url.contains(":memory:") && !url.contains('?') {
        format!("{url}?mode=rwc")
    } else {
        url.to_string()
    }
}

fn default_relational_db_url(system_root_directory: &std::path::Path) -> String {
    format!(
        "sqlite://{}",
        system_root_directory.join("cognee.db").display()
    )
}

fn default_graph_file_path(system_root_directory: &std::path::Path) -> PathBuf {
    system_root_directory.join("graph")
}

fn default_vector_db_url(system_root_directory: &std::path::Path) -> String {
    system_root_directory.join("vectors").display().to_string()
}

fn default_session_root_directory(system_root_directory: &std::path::Path) -> PathBuf {
    system_root_directory.join("sessions")
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        let cache_root = default_cache_root();
        let data_root = cache_root.join("data");
        let system_root = cache_root.join("system");
        Self {
            host: "0.0.0.0".into(),
            port: 8000,
            cors_allowed_origins: Vec::new(),
            ui_app_url: "http://localhost:3000".into(),
            env: Environment::Prod,
            require_authentication: false,
            jwt_secret: SecretString::new(uuid::Uuid::new_v4().to_string().into()),
            jwt_lifetime: Duration::from_secs(3600),
            body_limit: 100 * 1024 * 1024,
            pipeline_registry_max_runs: 4096,
            pipeline_registry_finished_retention_secs: 3600,
            pipeline_registry_channel_capacity: 64,
            pipeline_registry_abort_writes_errored: true,
            notebook_run_timeout: Duration::from_secs(30),
            health_probe_llm: false,
            health_probe_timeout_ms: 2000,
            health_cache_ttl_ms: 5000,
            data_root_directory: data_root,
            system_root_directory: system_root.clone(),
            relational_db_url: default_relational_db_url(&system_root),
            graph_provider: "ladybug".to_string(),
            graph_file_path: default_graph_file_path(&system_root),
            vector_provider: "pgvector".to_string(),
            vector_db_url: default_vector_db_url(&system_root),
            embedding_provider: "onnx".to_string(),
            embedding_dimensions: 384,
            embedding_model_name: "bge-small-en-v1.5".to_string(),
            embedding_model_path: None,
            embedding_tokenizer_path: None,
            embedding_endpoint: String::new(),
            embedding_api_key: SecretString::new(String::new().into()),
            llm_provider: "openai".to_string(),
            llm_model: "gpt-4o-mini".to_string(),
            llm_api_key: SecretString::new(String::new().into()),
            llm_endpoint: String::new(),
            llm_max_retries: 3,
            session_store_backend: "seaorm".to_string(),
            session_root_directory: default_session_root_directory(&system_root),
            notebook_runner_enabled: false,
            responses_client_enabled: false,
            disable_default_backends: false,
            default_user_email: "default_user@example.com".to_string(),
        }
    }
}

impl HttpServerConfig {
    /// Build config by overlaying environment variables on top of the defaults.
    ///
    /// Called only by the standalone binary entry point; library embedders
    /// construct `HttpServerConfig` directly.
    pub fn from_env() -> Result<Self, ServerError> {
        let mut cfg = Self::default();
        let default_system_root_directory = cfg.system_root_directory.clone();

        if let Ok(v) = std::env::var("HTTP_API_HOST") {
            cfg.host = v;
        }
        if let Ok(v) = std::env::var("HTTP_API_PORT") {
            cfg.port = v
                .parse::<u16>()
                .map_err(|e| ServerError::Other(anyhow::anyhow!("HTTP_API_PORT: {e}")))?;
        }
        if let Ok(v) = std::env::var("CORS_ALLOWED_ORIGINS") {
            cfg.cors_allowed_origins = v
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(v) = std::env::var("UI_APP_URL") {
            cfg.ui_app_url = v;
        }
        if let Ok(v) = std::env::var("ENV") {
            cfg.env = v.parse().unwrap_or(Environment::Prod);
        }
        if let Ok(v) = std::env::var("REQUIRE_AUTHENTICATION") {
            cfg.require_authentication =
                !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no");
        }
        if let Ok(v) = std::env::var("AUTH_JWT_SECRET") {
            cfg.jwt_secret = SecretString::new(v.into());
        }
        if let Ok(v) = std::env::var("AUTH_JWT_LIFETIME_SECONDS") {
            let secs = v.parse::<u64>().map_err(|e| {
                ServerError::Other(anyhow::anyhow!("AUTH_JWT_LIFETIME_SECONDS: {e}"))
            })?;
            cfg.jwt_lifetime = Duration::from_secs(secs);
        }
        if let Ok(v) = std::env::var("HTTP_BODY_LIMIT_BYTES") {
            cfg.body_limit = v
                .parse::<usize>()
                .map_err(|e| ServerError::Other(anyhow::anyhow!("HTTP_BODY_LIMIT_BYTES: {e}")))?;
        }

        // Pipeline registry knobs
        if let Ok(v) = std::env::var("PIPELINE_REGISTRY_MAX_RUNS") {
            cfg.pipeline_registry_max_runs = v.parse::<usize>().map_err(|e| {
                ServerError::Other(anyhow::anyhow!("PIPELINE_REGISTRY_MAX_RUNS: {e}"))
            })?;
        }
        if let Ok(v) = std::env::var("PIPELINE_REGISTRY_FINISHED_RETENTION_SECS") {
            cfg.pipeline_registry_finished_retention_secs = v.parse::<u64>().map_err(|e| {
                ServerError::Other(anyhow::anyhow!(
                    "PIPELINE_REGISTRY_FINISHED_RETENTION_SECS: {e}"
                ))
            })?;
        }
        if let Ok(v) = std::env::var("PIPELINE_REGISTRY_CHANNEL_CAPACITY") {
            cfg.pipeline_registry_channel_capacity = v.parse::<usize>().map_err(|e| {
                ServerError::Other(anyhow::anyhow!("PIPELINE_REGISTRY_CHANNEL_CAPACITY: {e}"))
            })?;
        }
        if let Ok(v) = std::env::var("PIPELINE_REGISTRY_ABORT_WRITES_ERRORED") {
            cfg.pipeline_registry_abort_writes_errored =
                !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no");
        }

        if let Ok(v) = std::env::var("NOTEBOOK_RUN_TIMEOUT_SECS") {
            let secs = v.parse::<u64>().map_err(|e| {
                ServerError::Other(anyhow::anyhow!("NOTEBOOK_RUN_TIMEOUT_SECS: {e}"))
            })?;
            cfg.notebook_run_timeout = Duration::from_secs(secs);
        }

        // Health checker knobs
        if let Ok(v) = std::env::var("COGNEE_HEALTH_PROBE_LLM") {
            cfg.health_probe_llm =
                matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on");
        }
        if let Ok(v) = std::env::var("COGNEE_HEALTH_PROBE_TIMEOUT_MS") {
            cfg.health_probe_timeout_ms = v.parse::<u64>().map_err(|e| {
                ServerError::Other(anyhow::anyhow!("COGNEE_HEALTH_PROBE_TIMEOUT_MS: {e}"))
            })?;
        }
        if let Ok(v) = std::env::var("COGNEE_HEALTH_CACHE_TTL_MS") {
            cfg.health_cache_ttl_ms = v.parse::<u64>().map_err(|e| {
                ServerError::Other(anyhow::anyhow!("COGNEE_HEALTH_CACHE_TTL_MS: {e}"))
            })?;
        }

        // Standalone backend wiring knobs
        if let Ok(v) = std::env::var("DATA_ROOT_DIRECTORY") {
            cfg.data_root_directory = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("SYSTEM_ROOT_DIRECTORY") {
            cfg.system_root_directory = PathBuf::from(v);

            // Keep standalone wiring coherent: if dependent paths still match
            // their old defaults, rebase them to the new system root.
            if cfg.relational_db_url == default_relational_db_url(&default_system_root_directory) {
                cfg.relational_db_url = default_relational_db_url(&cfg.system_root_directory);
            }
            if cfg.graph_file_path == default_graph_file_path(&default_system_root_directory) {
                cfg.graph_file_path = default_graph_file_path(&cfg.system_root_directory);
            }
            if cfg.vector_db_url == default_vector_db_url(&default_system_root_directory) {
                cfg.vector_db_url = default_vector_db_url(&cfg.system_root_directory);
            }
            if cfg.session_root_directory
                == default_session_root_directory(&default_system_root_directory)
            {
                cfg.session_root_directory =
                    default_session_root_directory(&cfg.system_root_directory);
            }
        }

        if let Some(v) = first_non_empty_env(&["RELATIONAL_DB_URL", "DATABASE_URL"]) {
            cfg.relational_db_url = v;
        }

        if let Ok(v) = std::env::var("GRAPH_DATABASE_PROVIDER") {
            cfg.graph_provider = v;
        }
        if let Ok(v) = std::env::var("GRAPH_FILE_PATH") {
            cfg.graph_file_path = PathBuf::from(v);
        }

        if let Ok(v) = std::env::var("VECTOR_DB_PROVIDER") {
            cfg.vector_provider = v;
        }
        if let Ok(v) = std::env::var("VECTOR_DB_URL") {
            cfg.vector_db_url = v;
        }

        if let Ok(v) = std::env::var("EMBEDDING_PROVIDER") {
            cfg.embedding_provider = v;
        }
        if let Ok(v) = std::env::var("EMBEDDING_DIMENSIONS") {
            cfg.embedding_dimensions = v
                .parse::<u32>()
                .map_err(|e| ServerError::Other(anyhow::anyhow!("EMBEDDING_DIMENSIONS: {e}")))?;
        }
        if let Some(v) = first_non_empty_env(&["EMBEDDING_MODEL_NAME", "EMBEDDING_MODEL"]) {
            cfg.embedding_model_name = v;
        }
        if let Ok(v) = std::env::var("EMBEDDING_MODEL_PATH") {
            cfg.embedding_model_path = Some(PathBuf::from(v));
        }
        if let Ok(v) = std::env::var("EMBEDDING_TOKENIZER_PATH") {
            cfg.embedding_tokenizer_path = Some(PathBuf::from(v));
        }
        if let Ok(v) = std::env::var("EMBEDDING_ENDPOINT") {
            cfg.embedding_endpoint = v;
        }
        if let Some(v) = first_non_empty_env(&["EMBEDDING_API_KEY", "LLM_API_KEY", "OPENAI_TOKEN"])
        {
            cfg.embedding_api_key = SecretString::new(v.into());
        }

        if let Ok(v) = std::env::var("LLM_PROVIDER") {
            cfg.llm_provider = v;
        }
        if let Some(v) = first_non_empty_env(&["LLM_MODEL", "OPENAI_MODEL"]) {
            cfg.llm_model = v;
        }
        if let Some(v) = first_non_empty_env(&["LLM_API_KEY", "OPENAI_TOKEN"]) {
            cfg.llm_api_key = SecretString::new(v.into());
        }
        if let Some(v) = first_non_empty_env(&["LLM_ENDPOINT", "OPENAI_URL"]) {
            cfg.llm_endpoint = v;
        }
        if let Ok(v) = std::env::var("LLM_MAX_RETRIES") {
            cfg.llm_max_retries = v
                .parse::<u32>()
                .map_err(|e| ServerError::Other(anyhow::anyhow!("LLM_MAX_RETRIES: {e}")))?;
        }

        if let Ok(v) = std::env::var("COGNEE_SESSION_STORE") {
            cfg.session_store_backend = v;
        }
        if let Ok(v) = std::env::var("COGNEE_SESSION_DIR") {
            cfg.session_root_directory = PathBuf::from(v);
        }

        if let Ok(v) = std::env::var("COGNEE_NOTEBOOK_RUNNER_ENABLED") {
            cfg.notebook_runner_enabled = cognee_utils::parse_env_bool(&v);
        }

        if let Ok(v) = std::env::var("COGNEE_RESPONSES_CLIENT_ENABLED") {
            cfg.responses_client_enabled = parse_env_bool_with_default(&v, false);
        } else {
            cfg.responses_client_enabled = !cfg.llm_api_key.expose_secret().is_empty();
        }

        if let Ok(v) = std::env::var("COGNEE_DISABLE_DEFAULT_BACKENDS") {
            cfg.disable_default_backends = cognee_utils::parse_env_bool(&v);
        }

        if let Ok(v) = std::env::var("DEFAULT_USER_EMAIL") {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                cfg.default_user_email = trimmed.to_string();
            }
        }

        Ok(cfg)
    }
}

impl HttpServerConfig {
    /// Lower these settings into a [`cognee_components::BackendBuildContext`].
    ///
    /// Unlike `cognee-lib`'s `Settings::backend_context`, the standalone server
    /// deliberately does **not** read `MOCK_LLM` / `MOCK_EMBEDDING` or wire the
    /// recording path: a production server must never silently honor those.
    /// Mock backends are opt-in through the `dev-mock` feature + an explicit
    /// `vector_provider="mock"`.
    pub fn backend_context(&self) -> cognee_components::BackendBuildContext {
        let vector_provider = self.vector_provider.to_ascii_lowercase();
        // The pgvector coherence guard runs in `wire_vector_db` before build;
        // by the time the factory runs, `vector_db_url` is a validated
        // `postgres://…` string. Trim it here so a copy-pasted value with
        // surrounding whitespace reaches PgVectorAdapter::new cleanly (the
        // validator checks the trimmed form).
        let vector_postgres_url = if vector_provider == "pgvector" {
            // Already validated as a postgres URL by wire_vector_db; trim so a
            // copy-pasted value with surrounding whitespace reaches the adapter
            // cleanly. Wrapped in `Ok` — the standalone server assembles this URL
            // directly, so resolution never fails here.
            Some(Ok(self.vector_db_url.trim().to_string()))
        } else {
            None
        };

        let endpoint = if self.embedding_endpoint.trim().is_empty() {
            None
        } else {
            Some(self.embedding_endpoint.clone())
        };
        let api_key = if self.embedding_api_key.expose_secret().is_empty() {
            None
        } else {
            Some(self.embedding_api_key.expose_secret().to_string())
        };

        // Source embedding scalar + ONNX-asset defaults from the embedding
        // crate's own constructors rather than duplicating magic literals here
        // (this crate always enables `cognee-embedding/onnx`, so both are
        // available). Keeps these in lockstep with the embedding crate.
        let emb_defaults = cognee_embedding::EmbeddingConfig::default();
        let onnx_defaults = cognee_embedding::OnnxEmbeddingConfig::default();

        cognee_components::BackendBuildContext {
            data_root_directory: self.data_root_directory.clone(),
            system_root_directory: self.system_root_directory.clone(),
            // Ensure a file-backed SQLite URL carries `?mode=rwc` so the driver
            // creates the DB file when missing. The standalone server's default
            // URL (and operator-provided ones) have no query, and the shared
            // `build_database` no longer creates the file itself — this restores
            // the old wire_database "create on boot" behavior via the driver.
            relational_db_url: ensure_sqlite_rwc(&self.relational_db_url),
            graph_provider: self.graph_provider.to_ascii_lowercase(),
            graph_file_path: self.graph_file_path.to_string_lossy().into_owned(),
            // The standalone server supports only the embedded ladybug graph;
            // Postgres graph is not wired here.
            graph_postgres_url: None,
            vector_provider,
            vector_db_url: self.vector_db_url.clone(),
            vector_postgres_url,
            embedding_dimensions: self.embedding_dimensions as usize,
            embedding: cognee_components::EmbeddingInputs {
                provider: self.embedding_provider.trim().to_ascii_lowercase(),
                model: self.embedding_model_name.clone(),
                dimensions: self.embedding_dimensions as usize,
                endpoint,
                api_key,
                batch_size: emb_defaults.batch_size,
                mock: false,
                mock_deterministic: false,
                api_version: None,
                huggingface_tokenizer: None,
                max_completion_tokens: emb_defaults.max_completion_tokens,
                // When no explicit ONNX asset path is configured, fall back to
                // the embedding crate's own BGE-Small defaults.
                onnx_model_path: self
                    .embedding_model_path
                    .clone()
                    .unwrap_or(onnx_defaults.model_path),
                onnx_tokenizer_path: self
                    .embedding_tokenizer_path
                    .clone()
                    .unwrap_or(onnx_defaults.tokenizer_path),
                onnx_model_name: self.embedding_model_name.clone(),
                onnx_dimensions: self.embedding_dimensions as usize,
                onnx_max_sequence_length: onnx_defaults.max_sequence_length,
                onnx_batch_size: onnx_defaults.batch_size,
            },
            llm: cognee_components::LlmInputs {
                provider: self.llm_provider.to_ascii_lowercase(),
                model: self.llm_model.clone(),
                api_key: self.llm_api_key.expose_secret().to_string(),
                endpoint: self.llm_endpoint.clone(),
                max_retries: self.llm_max_retries,
                // The HTTP server config does not yet expose an `LLM_ARGS`
                // equivalent; default to no extra request params (a no-op).
                // The CLI/ComponentManager path wires `LLM_ARGS` via
                // `cognee_lib::Settings`.
                llm_args: serde_json::Map::new(),
                mock: false,
                cassette: String::new(),
                record_path: String::new(),
            },
        }
    }

    /// Build a `RegistryConfig` from the matching `HttpServerConfig` fields.
    pub fn to_registry_config(&self) -> RegistryConfig {
        RegistryConfig {
            max_in_memory_runs: self.pipeline_registry_max_runs,
            finished_retention: Duration::from_secs(self.pipeline_registry_finished_retention_secs),
            channel_capacity: self.pipeline_registry_channel_capacity,
            yield_throttle: None, // not exposed via env in Phase 3
            abort_writes_errored_row: self.pipeline_registry_abort_writes_errored,
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn test_defaults() {
        let cfg = HttpServerConfig::default();
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 8000);
        assert_eq!(cfg.ui_app_url, "http://localhost:3000");
        assert_eq!(cfg.body_limit, 100 * 1024 * 1024);
        assert_eq!(cfg.jwt_lifetime, Duration::from_secs(3600));
        // OSS default is `false`: without an `AuthResolver`, requests fall
        // back to the synthetic default user (see auth/extractor.rs). Closed
        // cloud builds inject an `AuthResolver` and flip this to `true`.
        assert!(!cfg.require_authentication);
        assert!(cfg.cors_allowed_origins.is_empty());
        assert_eq!(cfg.env, Environment::Prod);
    }

    #[test]
    fn test_env_override_port() {
        // SAFETY: test-only; no concurrent threads modify this env var.
        unsafe {
            std::env::set_var("HTTP_API_PORT", "9999");
        }
        let cfg = HttpServerConfig::from_env().expect("from_env");
        // SAFETY: test-only.
        unsafe {
            std::env::remove_var("HTTP_API_PORT");
        }
        assert_eq!(cfg.port, 9999);
    }

    #[test]
    fn test_env_cors_origins() {
        // SAFETY: test-only; no concurrent threads modify this env var.
        unsafe {
            std::env::set_var("CORS_ALLOWED_ORIGINS", "http://a.test, http://b.test");
        }
        let cfg = HttpServerConfig::from_env().expect("from_env");
        // SAFETY: test-only.
        unsafe {
            std::env::remove_var("CORS_ALLOWED_ORIGINS");
        }
        assert_eq!(
            cfg.cors_allowed_origins,
            vec!["http://a.test", "http://b.test"]
        );
    }

    #[test]
    fn test_environment_from_str() {
        assert_eq!("dev".parse::<Environment>().unwrap(), Environment::Dev);
        assert_eq!("test".parse::<Environment>().unwrap(), Environment::Test);
        assert_eq!("prod".parse::<Environment>().unwrap(), Environment::Prod);
        assert_eq!(
            "anything".parse::<Environment>().unwrap(),
            Environment::Prod
        );
    }

    #[test]
    fn test_bool_backend_flags_from_env() {
        // SAFETY: test-only; no concurrent threads modify these env vars.
        unsafe {
            std::env::set_var("COGNEE_NOTEBOOK_RUNNER_ENABLED", "yes");
            std::env::set_var("COGNEE_RESPONSES_CLIENT_ENABLED", "1");
            std::env::set_var("COGNEE_DISABLE_DEFAULT_BACKENDS", "true");
        }
        let cfg = HttpServerConfig::from_env().expect("from_env");
        // SAFETY: test-only.
        unsafe {
            std::env::remove_var("COGNEE_NOTEBOOK_RUNNER_ENABLED");
            std::env::remove_var("COGNEE_RESPONSES_CLIENT_ENABLED");
            std::env::remove_var("COGNEE_DISABLE_DEFAULT_BACKENDS");
        }

        assert!(cfg.notebook_runner_enabled);
        assert!(cfg.responses_client_enabled);
        assert!(cfg.disable_default_backends);
    }

    #[test]
    fn test_llm_fallback_env_aliases() {
        // SAFETY: test-only; no concurrent threads modify these env vars.
        unsafe {
            std::env::set_var("OPENAI_TOKEN", "test-key");
            std::env::set_var("OPENAI_MODEL", "gpt-test");
            std::env::set_var("OPENAI_URL", "https://example.test/v1");
            std::env::remove_var("LLM_API_KEY");
            std::env::remove_var("LLM_MODEL");
            std::env::remove_var("LLM_ENDPOINT");
        }
        let cfg = HttpServerConfig::from_env().expect("from_env");
        // SAFETY: test-only.
        unsafe {
            std::env::remove_var("OPENAI_TOKEN");
            std::env::remove_var("OPENAI_MODEL");
            std::env::remove_var("OPENAI_URL");
        }

        assert_eq!(cfg.llm_api_key.expose_secret(), "test-key");
        assert_eq!(cfg.llm_model, "gpt-test");
        assert_eq!(cfg.llm_endpoint, "https://example.test/v1");
    }

    #[test]
    fn test_system_root_directory_rebases_dependent_defaults() {
        let temp = tempfile::tempdir().expect("tempdir");
        let new_root = temp.path().join("custom-system-root");

        // SAFETY: test-only; no concurrent threads modify these env vars.
        unsafe {
            std::env::set_var("SYSTEM_ROOT_DIRECTORY", &new_root);
            std::env::remove_var("RELATIONAL_DB_URL");
            std::env::remove_var("DATABASE_URL");
            std::env::remove_var("GRAPH_FILE_PATH");
            std::env::remove_var("VECTOR_DB_URL");
            std::env::remove_var("COGNEE_SESSION_DIR");
        }

        let cfg = HttpServerConfig::from_env().expect("from_env");

        // SAFETY: test-only.
        unsafe {
            std::env::remove_var("SYSTEM_ROOT_DIRECTORY");
        }

        assert_eq!(cfg.system_root_directory, new_root);
        assert_eq!(cfg.relational_db_url, default_relational_db_url(&new_root));
        assert_eq!(cfg.graph_file_path, default_graph_file_path(&new_root));
        assert_eq!(cfg.vector_db_url, default_vector_db_url(&new_root));
        assert_eq!(
            cfg.session_root_directory,
            default_session_root_directory(&new_root)
        );
    }
}
