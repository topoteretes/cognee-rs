#![allow(
    clippy::expect_used,
    reason = "RwLock/Mutex expect calls — lock poison is unrecoverable"
)]
//! Shared configuration types for cognee-rust.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};

use serde::{Deserialize, Serialize};

/// The configuration-boundary environment abstraction, re-exported so callers of
/// [`Settings::overlay_from`] need no direct `cognee-config-core` dependency.
pub use cognee_config_core::{EnvSource, MapEnv};

mod keys;
pub use cognee_config_core::{ConfigSchema, ConfigSection, ConfigValues, KeyDef, SectionSchema};
pub use keys::{KeySpec, builtin_schema, key_spec, keys, schema_with};

pub const DEFAULT_SYSTEM_PROMPT_PATH: &str = "answer_simple_question.txt";

/// Assemble a `postgres://user:pass@host:port/dbname` URL with percent-encoded
/// credentials. Shared by the vector and graph URL resolvers.
///
/// Not gated on the built-in Postgres features: an externally registered
/// provider may be Postgres-backed on a build where `pgvector` / `pggraph` are
/// off, and nothing here touches an adapter. Its one dependency, `url`, is
/// therefore a direct non-optional dep of this crate — it is already in the
/// graph of a `--no-default-features` build, so that costs no compilation.
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

/// Append `?sslmode=<value>` to a Postgres URL cognee has *built* from parts.
///
/// No-op when `sslmode` is empty (local no-TLS Postgres, or a sqlite URL). Only
/// ever applied to URLs assembled from the `db_*` / `vector_db_*` /
/// `graph_database_*` fields — never to a user-supplied verbatim `postgres://`
/// URL, which may already carry its own query string. Uses `&` if the built URL
/// somehow already has a query, `?` otherwise, so it never produces `??`.
fn with_sslmode(url: String, sslmode: &str) -> String {
    if sslmode.is_empty() {
        url
    } else if url.contains('?') {
        format!("{url}&sslmode={sslmode}")
    } else {
        format!("{url}?sslmode={sslmode}")
    }
}

/// Derive a libpq `sslmode` from the Python-chart `DATABASE_CONNECT_ARGS` JSON.
///
/// The chart passes asyncpg-style connect args, e.g. `{"ssl":"require"}`. We map
/// the `ssl` key onto the equivalent libpq `sslmode` value that sqlx understands:
/// a string is passed through when it is already a valid libpq mode; a boolean
/// `true` maps to `require`.
///
/// Returns `None` when the JSON is unparseable or has no `ssl` key. It never
/// *silently* downgrades a configured TLS requirement: an unrecognized string
/// (likely a typo) logs a warning and returns `None`, and an object/array `ssl`
/// (asyncpg accepts an `SSLContext`, which has no libpq-`sslmode` equivalent)
/// maps to `require` rather than dropping TLS — matching Python, which passes
/// `ssl` through verbatim and never quietly disables it.
fn sslmode_from_connect_args(raw: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    match parsed.get("ssl")? {
        serde_json::Value::Bool(true) => Some("require".to_string()),
        serde_json::Value::Bool(false) => Some("disable".to_string()),
        serde_json::Value::String(s) => {
            let s = s.trim().to_lowercase();
            match s.as_str() {
                "disable" | "allow" | "prefer" | "require" | "verify-ca" | "verify-full" => Some(s),
                // asyncpg accepts bare "true"/"false" strings too.
                "true" => Some("require".to_string()),
                "false" => Some("disable".to_string()),
                other => {
                    tracing::warn!(
                        ssl = other,
                        "DATABASE_CONNECT_ARGS `ssl` is not a recognized libpq sslmode; \
                         ignoring it. Set DB_SSLMODE explicitly to control TLS."
                    );
                    None
                }
            }
        }
        // An SSLContext-shaped object/array can't be expressed as a libpq
        // sslmode, but its presence means TLS is intended — require it rather
        // than silently connecting unencrypted; the custom context is not
        // carried through the URL.
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            tracing::warn!(
                "DATABASE_CONNECT_ARGS `ssl` is a custom TLS context, which cannot be \
                 expressed as a libpq sslmode; defaulting to sslmode=require."
            );
            Some("require".to_string())
        }
        _ => None,
    }
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

    pub llm_max_parallel_requests: u32,

    pub graph_prompt_path: String,

    // -- LLM fallback -----------------------------------------------------------
    /// Graph backend id. The default is derived from the graph factories the
    /// compiled features register
    /// ([`cognee_components::default_graph_provider`]): `ladybug` in every
    /// default build, else `postgres` wherever `pggraph` is compiled in, else
    /// the single registered backend if there is exactly one. A build that
    /// drops `ladybug` therefore still resolves on its own unless it registers
    /// two or more backends and neither preferred id.
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
    /// Maximum chunk size in tokens. `None` (the default) means "auto-calculate
    /// from the active embedding engine and the configured LLM ceiling" —
    /// Python's `chunk_size=None`. Set explicitly via `CHUNK_SIZE` /
    /// `COGNEE_CHUNK_SIZE`, `cognee config set chunk_size`, or `--chunk-size`.
    pub chunk_size: Option<u32>,
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
    /// libpq-style TLS mode appended to every Postgres URL cognee *builds*
    /// (relational + pgvector + Postgres-graph). Empty string means: append
    /// nothing (local no-TLS Postgres, or sqlite). Managed providers such as
    /// Neon require TLS, so set this to `require` (or `verify-full`) there.
    ///
    /// Populated from the `DB_SSLMODE` env var, or derived from the `ssl` key of
    /// the Python-chart `DATABASE_CONNECT_ARGS` JSON as a fallback. sqlx-postgres
    /// reads TLS only from this libpq `?sslmode=` URL parameter — it does not
    /// understand the asyncpg `?ssl=` form the Python chart passes.
    pub db_sslmode: String,

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

    /// Does exactly one process use the relational database?
    ///
    /// `None` (the default) means "derive it from the relational DB URL", and
    /// only an *in-memory* SQLite database derives `true` — see
    /// [`cognee_database::single_process_default`]. File-backed SQLite derives
    /// `false`, the shipped `sqlite:./cognee.db?mode=rwc` included: it is a
    /// file every cognee process in that directory opens, not a private
    /// handle. Set this explicitly — here or via `COGNEE_SINGLE_PROCESS` — to
    /// override in either direction; [`Settings::resolved_single_process`] is
    /// what reads it.
    ///
    /// Asserting it enables startup recovery that is only sound when no peer
    /// process exists: clearing the orphaned `pipeline_runs` row *and* the
    /// exclusive-run claim that a run killed mid-flight (SIGKILL, OOM, an
    /// Android process kill) leaves behind. Both refuse every later run on
    /// that dataset — the claim for a day, the row forever — and neither can
    /// be cleared by its dead holder. With one process per database every such
    /// leftover belongs to a dead predecessor; with more than one it may
    /// belong to a live peer and must not be touched.
    ///
    /// In practice this is an **opt-in**: the only URL that derives `true` is
    /// an in-memory database, which dies with its process and so can never
    /// hold a leftover to clear. Every real deployment that wants the recovery
    /// sets this — `cognee config set single_process true`, a binding's
    /// `set_config`, [`ConfigManager::set_single_process`], or
    /// `COGNEE_SINGLE_PROCESS=1`.
    pub single_process: Option<bool>,

    /// Make an unknown config key an error instead of a warning
    /// (`COGNEE_STRICT_CONFIG`). Off by default: a key can be unknown because
    /// it is misspelled, or because the component that owns it is not
    /// compiled into or registered with this build.
    pub strict_config: bool,

    /// Values of the component-owned config sections (see
    /// [`cognee_config_core::section`]), keyed by canonical key such as
    /// `graph.ladybug.max_db_size`.
    ///
    /// Flattened: in a config file or a binding's settings object these keys
    /// sit next to the `Settings` fields, and any key that is not a `Settings`
    /// field lands here. [`ConfigManager`] checks them against its schema;
    /// one that matches no registered section is an unknown key.
    #[serde(flatten)]
    pub sections: cognee_config_core::ConfigValues,
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
    ///
    /// This is a configuration boundary: it is the one place `Settings` opens
    /// the process environment. Use [`Settings::overlay_from`] to overlay from
    /// any other [`EnvSource`] (e.g. a `MapEnv` in tests).
    pub fn overlay_from_env(&mut self) {
        self.overlay_from_env_with_schema(builtin_schema());
    }

    /// [`Settings::load_from_env`] for a build whose component registry adds
    /// config sections of its own: `schema` must cover every section the
    /// process will use (see [`schema_with`]), so their env vars are read too.
    pub fn load_from_env_with_schema(schema: &ConfigSchema) -> Self {
        let mut s = Self::default();
        s.overlay_from_env_with_schema(schema);
        s
    }

    /// [`Settings::overlay_from_env`] with an explicit schema; see
    /// [`Settings::load_from_env_with_schema`].
    #[allow(
        clippy::disallowed_methods,
        reason = "config boundary: Settings is resolved from the process env here"
    )]
    pub fn overlay_from_env_with_schema(&mut self, schema: &ConfigSchema) {
        // Load .env (no-op if absent or if the vars are already in the environment).
        cognee_config_core::load_dotenv();
        self.overlay_from_with_schema(&cognee_config_core::ProcessEnv::new(), schema);
    }

    /// The typed component config section `T`: its values from
    /// [`Settings::sections`] (canonical keys or aliases) plus defaults.
    pub fn section<T: ConfigSection>(&self) -> T {
        T::from_values(&self.sections)
    }

    /// The LLM kind section (`llm.*`): provider, model, credentials, retries,
    /// timeouts and the other settings every LLM adapter shares.
    pub fn llm(&self) -> cognee_components::LlmConfig {
        self.section()
    }

    /// Whether exactly one process uses the relational database.
    ///
    /// The explicit [`Settings::single_process`] answer when the operator gave
    /// one, otherwise derived from [`Settings::resolved_relational_db_url`]:
    /// only an *in-memory* SQLite database, which no other process can open,
    /// derives `true`. File-backed SQLite — including the shipped default
    /// `sqlite:./cognee.db?mode=rwc` — is shared by every process started
    /// against it, so it must be asserted explicitly.
    ///
    /// Callers use this to gate recovery that is only sound with no peer
    /// process — see
    /// [`cognee_database::PipelineRunRepository::release_all_pipeline_run_claims`].
    pub fn resolved_single_process(&self) -> bool {
        cognee_database::resolve_single_process(
            &self.resolved_relational_db_url(),
            self.single_process,
        )
    }

    /// Returns the effective relational DB connection URL.
    ///
    /// When `db_provider` is `"postgres"`, builds
    /// `postgres://username:password@host:port/name` from the individual
    /// `db_*` fields (matching Python's `DB_PROVIDER`/`DB_HOST`/… env vars).
    /// Otherwise returns `relational_db_url` verbatim.
    pub fn resolved_relational_db_url(&self) -> String {
        if self.db_provider == "postgres" {
            with_sslmode(
                format!(
                    "postgres://{}:{}@{}:{}/{}",
                    self.db_username, self.db_password, self.db_host, self.db_port, self.db_name
                ),
                &self.db_sslmode,
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
    #[allow(
        clippy::disallowed_methods,
        reason = "config-unification: legacy direct env read, to be moved behind a config boundary"
    )]
    pub fn backend_context(&self) -> cognee_components::BackendBuildContext {
        use std::path::PathBuf;

        let graph_provider = self.graph_database_provider.to_lowercase();
        // Resolved for EVERY provider, not only the built-in Postgres ones.
        //
        // `ComponentRegistry` is an open extension point: an external crate may
        // register a Postgres-backed provider under any id it likes, and this
        // function cannot know which ids those are. Gating the resolution
        // on a hardcoded list of built-in ids made every such provider
        // unreachable — the factory received `None` and could only fail. Gating
        // it on the `pggraph` / `pgvector` cargo features made it worse, by
        // tying an external adapter to an unrelated built-in being compiled in.
        //
        // So the context offers the resolution and each factory decides whether
        // it wants it. Providers with nothing to do with Postgres simply never
        // read the field. The cost is assembling two strings that may go
        // unused; the benefit is that "register a provider" is sufficient.
        //
        // The `Result` is carried through rather than unwrapped so a factory can
        // restate the specific cause (e.g. "Missing required Postgres graph
        // credentials") in the error it returns, not just in a log line an SDK
        // user may never see.
        let graph_postgres_url = Some(self.resolved_graph_postgres_url());

        let vector_provider = self.vector_db_provider.to_lowercase();
        let vector_postgres_url = Some(self.resolved_vector_postgres_url());

        // Embedding endpoint/key fall back to the LLM provider's when no
        // embedding-specific values are set (shared OpenAI-compatible account).
        //
        // That sharing only makes sense when both sides speak the same protocol.
        // Bedrock does not: it takes AWS-signed InvokeModel bodies at a regional
        // host, and its `api_key` is a *bearer token*. Inheriting an
        // OpenAI-compatible `LLM_ENDPOINT` would POST InvokeModel at that host,
        // and inheriting `LLM_API_KEY` would hand that provider's credential to
        // AWS. `EMBEDDING_PROVIDER=bedrock` with a custom `LLM_ENDPOINT` /
        // `OPENAI_URL` is an ordinary configuration, so this is reachable — the
        // `api.openai.com` guard in the Bedrock engine only catches the default.
        //
        // Inherit only when the LLM side is Bedrock too, which is the case the
        // "shared account" rationale actually covers.
        let embedding_is_bedrock = self
            .embedding_provider
            .trim()
            .eq_ignore_ascii_case("bedrock");
        let inherit_from_llm =
            !embedding_is_bedrock || self.llm().provider.trim().eq_ignore_ascii_case("bedrock");

        let llm = self.llm();
        let mut endpoint_sources = vec![&self.embedding_endpoint];
        let mut api_key_sources = vec![&self.embedding_api_key];
        if inherit_from_llm {
            endpoint_sources.push(&llm.endpoint);
            api_key_sources.push(&llm.api_key);
        }
        let endpoint = endpoint_sources
            .into_iter()
            .find(|v| !v.is_empty())
            .cloned();
        let api_key = api_key_sources.into_iter().find(|v| !v.is_empty()).cloned();

        // MOCK_EMBEDDING: every truthy spelling (`true`/`1`/`yes`, plus the
        // explicit `deterministic`/`hash`) selects SHA-256-derived vectors;
        // `zero` is the only way to get the legacy all-zero vectors, which
        // cosine KNN backends drop (NaN distance) and therefore never retrieve.
        // Mirrors `cognee_embedding::EmbeddingConfig::from_env`.
        let mock_raw = std::env::var("MOCK_EMBEDDING")
            .ok()
            .map(|v| v.trim().to_lowercase());
        let mock_zero = matches!(mock_raw.as_deref(), Some("zero"));
        let mock = mock_zero
            || matches!(
                mock_raw.as_deref(),
                Some("true") | Some("1") | Some("yes") | Some("deterministic") | Some("hash")
            );
        let mock_mode = if mock_zero {
            cognee_embedding::MockVectorMode::Zero
        } else {
            cognee_embedding::MockVectorMode::Deterministic
        };

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
            sections: self.sections.clone(),
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
                rate_limit_enabled: self.embedding_rate_limit_enabled,
                rate_limit_requests: self.embedding_rate_limit_requests,
                rate_limit_interval: self.embedding_rate_limit_interval,
                mock,
                mock_mode,
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
                provider: llm.provider.to_lowercase(),
                model: llm.model.clone(),
                api_key: llm.api_key.clone(),
                endpoint: llm.endpoint.clone(),
                max_retries: llm.max_retries,
                network_retries: llm.network_retries,
                min_retry_seconds: llm.min_retry_seconds,
                max_parallel_requests: self.llm_max_parallel_requests,
                request_timeout_seconds: llm.request_timeout_seconds,
                connect_timeout_seconds: llm.connect_timeout_seconds,
                request_deadline_seconds: llm.request_deadline_seconds,
                rate_limit_enabled: llm.rate_limit_enabled,
                rate_limit_requests: llm.rate_limit_requests,
                rate_limit_interval: llm.rate_limit_interval,
                auto_rate_limit: llm.auto_rate_limit,
                max_completion_tokens: llm.max_completion_tokens,
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "GenerationOptions carries f32; a sampling temperature has no precision to lose"
                )]
                temperature: llm.temperature.map(|t| t as f32),
                llm_args: llm.args.clone(),
                api_version: llm.api_version.clone(),
                reasoning_override: cognee_components::parse_reasoning_override(&llm.reasoning),
                structured_output_mode: cognee_components::parse_structured_output_mode(
                    &llm.structured_output_mode,
                ),
                mock: llm.mock,
                cassette: llm.cassette.clone(),
                record_path: llm.record_path.clone(),
                debug_requests: llm.debug_requests,
            },
        }
    }

    /// Build a Postgres connection URL from the `graph_database_*` settings,
    /// falling back to the relational `db_*` fields when graph-specific creds
    /// are not fully configured (Python `get_graph_engine.py:332-367` parity).
    ///
    /// Always compiled: see [`Self::backend_context`] — the resolution is
    /// offered to whichever graph provider is selected, including externally
    /// registered ones, so it cannot depend on the built-in `pggraph` feature.
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
            // `debug`, not `warn`: `Settings::backend_context` resolves this for
            // every provider now, so a warning here fires on every deployment
            // -- including ones that never mention Postgres. The fallback is
            // also the documented single-shared-Postgres layout rather than a
            // misconfiguration. A genuine failure still returns `Err` below,
            // and that reaches the user through the factory.
            tracing::debug!(
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

        build_postgres_url(host, port, name, user, pass).map(|u| with_sslmode(u, &self.db_sslmode))
    }

    /// Build a Postgres connection URL from the `vector_db_*` settings.
    ///
    /// Always compiled, for the reason given on
    /// [`Self::resolved_graph_postgres_url`].
    pub(crate) fn resolved_vector_postgres_url(&self) -> Result<String, String> {
        if self.vector_db_url.starts_with("postgres://")
            || self.vector_db_url.starts_with("postgresql://")
        {
            return Ok(self.vector_db_url.clone());
        }

        // Python parity (`create_vector_engine.py` pgvector branch): use the
        // `VECTOR_DB_*` credentials only when the vector-specific set is present;
        // otherwise fall back WHOLESALE to the relational `db_*` configuration —
        // same host, port, AND database. That is exactly what the single-shared-
        // Postgres layout wants (Python even reuses the relational engine object
        // in that case), and it mirrors `resolved_graph_postgres_url` below.
        //
        // The previous per-field defaults (`localhost` / port `1234` / database
        // `cognee_vectors`) had no Python counterpart: a chart that set only
        // `VECTOR_DB_HOST` landed on port 1234 in a database named `cognee_vectors`
        // — a different database from the relational/graph stores on the same
        // host, silently defeating the single-DB goal. Gate on host+name+username
        // (not password, which may be empty for trust auth), matching the graph
        // path.
        let vector_creds_complete = !self.vector_db_host.is_empty()
            && !self.vector_db_name.is_empty()
            && !self.vector_db_username.is_empty();

        let (host, port, name, user, pass) = if vector_creds_complete {
            (
                self.vector_db_host.as_str(),
                self.vector_db_port,
                self.vector_db_name.as_str(),
                self.vector_db_username.as_str(),
                self.vector_db_password.as_str(),
            )
        } else {
            // `debug`, not `warn`: `Settings::backend_context` resolves this for
            // every provider now, so a warning here fires on every deployment
            // -- including ones that never mention Postgres. The fallback is
            // also the documented single-shared-Postgres layout rather than a
            // misconfiguration. A genuine failure still returns `Err` below,
            // and that reaches the user through the factory.
            tracing::debug!(
                "Postgres vector credentials not fully configured; falling back to the \
                 relational database configuration. Set VECTOR_DB_* explicitly to avoid this."
            );
            if self.db_host.is_empty() || self.db_name.is_empty() {
                return Err("Missing required Postgres vector credentials".into());
            }
            // A password set while the username is blank is a real
            // misconfiguration, not the default: defaulting the user to
            // `postgres` there would silently authenticate as the superuser with
            // that password. Fail fast so it surfaces at startup, not as an
            // opaque connect-time auth error.
            if self.db_username.is_empty() && !self.db_password.is_empty() {
                return Err(
                    "Missing required Postgres vector credentials: DB_PASSWORD is set but \
                     DB_USERNAME is empty"
                        .into(),
                );
            }
            // An empty username with an empty password is the out-of-the-box
            // default, so default it to `postgres` (Postgres's own superuser)
            // rather than erroring — the previous behaviour returned a usable URL
            // for the default config and pgvector deployments relied on it.
            // Crucially this stays on the relational host/port/DATABASE, so
            // vectors still land in the SAME database as the relational store; it
            // does NOT resurrect the old per-field defaults (port 1234 /
            // `cognee_vectors`) that silently split the stores across databases.
            let user = if self.db_username.is_empty() {
                "postgres"
            } else {
                self.db_username.as_str()
            };
            (
                self.db_host.as_str(),
                self.db_port,
                self.db_name.as_str(),
                user,
                self.db_password.as_str(),
            )
        };

        build_postgres_url(host, port, name, user, pass).map(|u| with_sslmode(u, &self.db_sslmode))
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
            Value::String(self.llm().provider.clone()),
        );
        m.insert("llm_model".into(), Value::String(self.llm().model.clone()));
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

            // Single-sourced with cognify's own in-flight default so the two
            // cannot drift: this setting is what the CLI and bindings push into
            // `CognifyConfig::max_parallel_extractions`, and a mismatch would
            // mean the configured value silently disagreed with the value used
            // by any consumer that does not thread it through (the HTTP routers
            // do not). See `DEFAULT_MAX_PARALLEL_EXTRACTIONS` for why this is
            // 1000 on desktop — matching the ~1000 in-flight ceiling litellm's
            // transport imposes on Python — and 128 on Android and iOS, where a
            // 1024-descriptor budget is shared with every open store.
            llm_max_parallel_requests: cognee_cognify::config::DEFAULT_MAX_PARALLEL_EXTRACTIONS
                as u32,
            graph_prompt_path: "generate_graph_prompt.txt".to_string(),

            // Derived from the graph factories the compiled features register
            // rather than hardcoded, for the same reason as the HTTP server's
            // `graph_provider`: a build without `ladybug` (an SDK or CLI
            // compiled `--no-default-features` with `pggraph`) would otherwise
            // default to a backend the registry cannot build, and fail on the
            // first graph access. `ladybug` still wins wherever it is compiled
            // in, which is every default build.
            graph_database_provider: cognee_components::default_graph_provider().to_string(),
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
            // existing configs continue to boot without edits. It is not a
            // fallback — `ComponentManager::init_vector_db` hands the provider id
            // to `ComponentRegistry::build_vector`, which ERRORS when no factory
            // is registered under it. Production deployments should explicitly set
            // `vector_db_provider="pgvector"` (and supply `vector_db_url`) for
            // durable storage. T5's earlier flip to `"pgvector"` broke OSS bindings
            // (their defaults don't enable the `pgvector` Cargo feature), so
            // `"lancedb"` remains the lowest-friction default — but note it is now
            // itself behind the `lancedb` feature (default-on in `cognee`, in
            // every binding, and in `cognee-http-server`). A consumer that
            // drops that
            // feature MUST set `vector_db_provider` explicitly; the registry's
            // unsupported-provider error names the feature to rebuild with.
            //
            // Android is the one first-party profile that does drop it:
            // `cognee/android-default` omits `lancedb` because the Arrow + lance
            // native stack does not cross-compile. Its effective backend has
            // always been in-memory brute-force anyway — when the feature IS
            // present, `LanceDbFactory::build` falls back to brute-force on
            // Android rather than opening a store. Naming brute-force directly
            // there keeps that behaviour and stops a stock Android build from
            // erroring on a provider whose factory was never registered. Every
            // other slim consumer keeps the loud failure described above.
            //
            // This default covers only a *default* `Settings`. An explicit
            // "lancedb" still reaches the registry from a persisted config.json
            // (the CLI serializes the whole struct, so any `config set` writes
            // the provider back) or from `VECTOR_DB_PROVIDER`; those are
            // absorbed by `ANDROID_LANCEDB_FALLBACK` in
            // `ComponentRegistry::resolve_vector_key`. Keep the two in step.
            #[cfg(all(target_os = "android", not(feature = "lancedb")))]
            vector_db_provider: "brute-force".to_string(),
            #[cfg(not(all(target_os = "android", not(feature = "lancedb"))))]
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
            chunk_size: None,
            chunk_overlap: 10,

            relational_db_url: "sqlite:./cognee.db?mode=rwc".to_string(),
            migration_db_url: String::new(),

            db_provider: "sqlite".to_string(),
            db_host: "localhost".to_string(),
            db_port: 5432,
            db_name: "cognee_db".to_string(),
            db_username: String::new(),
            db_password: String::new(),
            db_sslmode: String::new(),

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
            single_process: None,
            strict_config: false,
            sections: cognee_config_core::ConfigValues::new(),
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

impl From<cognee_config_core::KeyError> for ConfigError {
    fn from(error: cognee_config_core::KeyError) -> Self {
        match error {
            cognee_config_core::KeyError::Unknown(key) => ConfigError::UnknownKey(key),
            cognee_config_core::KeyError::Invalid { key, reason } => {
                ConfigError::TypeMismatch { key, reason }
            }
        }
    }
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
/// use cognee::config::{ConfigManager, Settings};
///
/// let cfg = ConfigManager::new(Settings::default());
/// assert_eq!(cfg.version(), 0);
///
/// cfg.set_llm_model("gpt-4o");
/// assert_eq!(cfg.version(), 1);
/// assert_eq!(cfg.read().llm().model, "gpt-4o");
/// ```
pub struct ConfigManager {
    // Lock order: `schema`, then `inner`, then `invalid`. A method that holds
    // more than one of them must take them in this order, or it can deadlock
    // against `extend_schema`.
    inner: Arc<RwLock<Settings>>,
    version: Arc<AtomicU64>,
    /// The component config sections this manager knows: the built-in ones,
    /// plus those of a custom registry added by
    /// [`ConfigManager::extend_schema`].
    schema: Arc<RwLock<ConfigSchema>>,
    /// Bulk-loaded values of a known key that had the wrong type, as
    /// `(canonical key, reason)`. They were dropped, and
    /// [`ConfigManager::check_strict`] reports them until the key is set again.
    invalid: Arc<Mutex<Vec<(String, String)>>>,
}

/// Bring settings that were loaded in bulk (deserialized from a config file or
/// a binding's settings object) into canonical form against `schema`, and
/// return the values of a known key that had the wrong type.
///
/// `Settings::sections` is a `#[serde(flatten)]` catch-all, so a `Settings`
/// key serde does not know by that name lands in it too: the
/// `#[serde(skip)]` fields (`llm_args`, `summarization_schema`) and key
/// aliases (`embedding_model`). Those are moved out and applied through the
/// key registry first, so only component section keys stay in `sections`.
fn normalize_bulk(schema: &ConfigSchema, settings: &mut Settings) -> Vec<(String, String)> {
    let mut invalid = Vec::new();
    let core: Vec<(String, &'static KeySpec)> = settings
        .sections
        .keys()
        .filter_map(|key| key_spec(key).map(|spec| (key.clone(), spec)))
        .collect();
    for (key, spec) in core {
        let Some(value) = settings.sections.remove(&key) else {
            continue;
        };
        if let Err(error) = (spec.set)(settings, &key, value) {
            let reason = match error {
                ConfigError::TypeMismatch { reason, .. } => reason,
                other => other.to_string(),
            };
            tracing::warn!(key, reason, "config value of the wrong type");
            invalid.push((spec.key.to_string(), reason));
        }
    }
    invalid.extend(schema.normalize_lenient(&mut settings.sections).invalid);
    invalid
}

impl ConfigManager {
    /// Create a new `ConfigManager` wrapping the given settings.
    ///
    /// Bulk-loaded values in `settings` are checked against the built-in
    /// schema: aliases become canonical keys, a value of the wrong type is
    /// dropped and reported by [`ConfigManager::check_strict`], and an
    /// unknown key is kept with a warning (an error under strict mode, again
    /// at `check_strict`).
    pub fn new(settings: Settings) -> Self {
        Self::with_schema(settings, builtin_schema().clone())
    }

    /// [`ConfigManager::new`] for a build whose component registry adds
    /// config sections of its own: `schema` is the full schema (see
    /// [`schema_with`]), so the values of those sections are recognized from
    /// the start rather than reported as unknown first.
    pub fn with_schema(settings: Settings, schema: ConfigSchema) -> Self {
        let mut settings = settings;
        let invalid = normalize_bulk(&schema, &mut settings);
        Self {
            inner: Arc::new(RwLock::new(settings)),
            version: Arc::new(AtomicU64::new(0)),
            schema: Arc::new(RwLock::new(schema)),
            invalid: Arc::new(Mutex::new(invalid)),
        }
    }

    /// Add the config sections of a custom component registry (see
    /// `ComponentRegistry::config_schema`), so their keys become settable
    /// here and section values that were unknown so far are checked.
    pub fn extend_schema(
        &self,
        other: &ConfigSchema,
    ) -> Result<(), cognee_config_core::SchemaError> {
        let mut schema = self.schema.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        let before = schema.sections().len();
        schema.merge(other)?;
        if schema.sections().len() == before {
            // Nothing new (e.g. the schema already came from `with_schema`):
            // the values were normalized against these sections already.
            return Ok(());
        }
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        let invalid = normalize_bulk(&schema, &mut s);
        self.invalid
            .lock()
            .expect("lock poison is unrecoverable") // lock poison is unrecoverable
            .extend(invalid);
        Ok(())
    }

    /// The whole configuration as one JSON object: every `Settings` field, and
    /// every setting of every known component section under its canonical key
    /// with its current or default value. Unknown section entries are kept.
    /// With `mask = Some(marker)`, non-empty section secrets are replaced by
    /// `marker` (callers mask `Settings` secrets themselves, e.g. with
    /// `cognee_bindings_common::redact_config_json`).
    pub fn snapshot(&self, mask: Option<&str>) -> serde_json::Value {
        // Lock order: `schema` before `inner` (see `ConfigManager`).
        let schema = self.schema.read().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        let s = self.read();
        let mut value = serde_json::to_value(&*s)
            .expect("Settings serializes to JSON: all fields are plain data");
        if let serde_json::Value::Object(map) = &mut value {
            for (key, v) in schema.snapshot(&s.sections, mask) {
                map.insert(key, v);
            }
        }
        value
    }

    /// A copy of the component config schema this manager knows.
    pub fn schema(&self) -> ConfigSchema {
        self.schema
            .read()
            .expect("lock poison is unrecoverable") // lock poison is unrecoverable
            .clone()
    }

    /// Fail if the configuration that was loaded in bulk (a config file, a
    /// binding's settings object) held a value of the wrong type for a known
    /// key, or, with `strict_config` on, a key that no registered setting
    /// owns. Otherwise `Ok`: unknown keys were already reported as warnings.
    ///
    /// Explicit [`ConfigManager::set`] calls apply the policy themselves; this
    /// covers keys that arrived in bulk, which are checked when the
    /// configuration is first used.
    pub fn check_strict(&self) -> Result<(), ConfigError> {
        let schema = self.schema.read().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        let s = self.read();
        if let Some((key, reason)) = self
            .invalid
            .lock()
            .expect("lock poison is unrecoverable") // lock poison is unrecoverable
            .first()
        {
            return Err(ConfigError::TypeMismatch {
                key: key.clone(),
                reason: reason.clone(),
            });
        }
        if !s.strict_config {
            return Ok(());
        }
        match schema.unknown_keys(&s.sections).first() {
            Some(key) => Err(ConfigError::UnknownKey((*key).to_string())),
            None => Ok(()),
        }
    }

    /// Forget a reported wrongly typed bulk value of `key` (canonical): the
    /// key was just set explicitly.
    fn clear_invalid(&self, key: &str) {
        self.invalid
            .lock()
            .expect("lock poison is unrecoverable") // lock poison is unrecoverable
            .retain(|(k, _)| k != key);
    }

    /// Every key whose value is a credential (a `Settings` row or a section
    /// setting marked `secret`), as it appears in the serialized `Settings`.
    /// Anything that shows the configuration must mask these.
    pub fn secret_keys(&self) -> Vec<&'static str> {
        let schema = self.schema.read().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        keys()
            .iter()
            .filter(|spec| spec.secret)
            .map(|spec| spec.key)
            .chain(
                schema
                    .key_defs()
                    .filter(|def| def.secret)
                    .map(|def| def.key),
            )
            .collect()
    }

    /// Convenience constructor: `Settings::load_from_env()` + wrap.
    pub fn from_env() -> Self {
        Self::new(Settings::load_from_env())
    }

    /// [`ConfigManager::from_env`] for a custom component registry: `extra` is
    /// the registry's schema (`ComponentRegistry::config_schema()`). Its
    /// sections are known *before* the environment is read, so their env vars
    /// are loaded too, which adding them later with
    /// [`ConfigManager::extend_schema`] cannot do.
    pub fn from_env_with_schema(
        extra: &ConfigSchema,
    ) -> Result<Self, cognee_config_core::SchemaError> {
        let schema = schema_with(extra)?;
        let settings = Settings::load_from_env_with_schema(&schema);
        Ok(Self::with_schema(settings, schema))
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
            schema: Arc::clone(&self.schema),
            invalid: Arc::clone(&self.invalid),
        }
    }
}

// -- Individual setter methods -----------------------------------------------

impl ConfigManager {
    // -- LLM -----------------------------------------------------------------

    pub fn set_llm_provider(&self, provider: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.provider".to_string(),
            serde_json::json!(provider.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_model(&self, model: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.model".to_string(),
            serde_json::json!(model.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_api_key(&self, key: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.api_key".to_string(),
            serde_json::json!(key.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_endpoint(&self, endpoint: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.endpoint".to_string(),
            serde_json::json!(endpoint.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    // -- LLM fallback --------------------------------------------------------

    pub fn set_llm_fallback_model(&self, model: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.fallback_model".to_string(),
            serde_json::json!(model.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_fallback_provider(&self, provider: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.fallback_provider".to_string(),
            serde_json::json!(provider.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_fallback_endpoint(&self, endpoint: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.fallback_endpoint".to_string(),
            serde_json::json!(endpoint.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_fallback_api_key(&self, key: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.fallback_api_key".to_string(),
            serde_json::json!(key.to_string()),
        );
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
        s.sections.insert(
            "llm.openai_compatible.transcription_model".to_string(),
            serde_json::json!(model.to_string()),
        );
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

    /// Assert (or deny) that exactly one process uses the relational database.
    ///
    /// The programmatic form of `COGNEE_SINGLE_PROCESS`, and the one an
    /// embedded consumer needs: the Python, C, TS and Java bindings configure
    /// through `set_config`, and an app that owns its SQLite file has no other
    /// way to say so. See [`Settings::single_process`] for what it enables.
    pub fn set_single_process(&self, single_process: bool) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.single_process = Some(single_process);
        drop(s);
        self.bump_version();
    }

    /// Restore the derived answer (`single_process = None`), the counterpart of
    /// [`Self::set_single_process`].
    pub fn clear_single_process(&self) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.single_process = None;
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
        s.chunk_size = Some(size);
        drop(s);
        self.bump_version();
    }

    /// Restore the auto-calculated chunk size (Python's `chunk_size=None`).
    pub fn clear_chunk_size(&self) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.chunk_size = None;
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
        s.sections.insert(
            "llm.api_version".to_string(),
            serde_json::json!(version.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    /// Set the reasoning-detection override (`auto` | `always` | `never`); see
    /// [`Settings::llm_reasoning`].
    pub fn set_llm_reasoning(&self, mode: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.reasoning".to_string(),
            serde_json::json!(mode.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    /// Set the structured-output request shape (`auto` | `json_schema` |
    /// `tools` | `functions` | `json`); see
    /// [`Settings::llm_structured_output_mode`]. `tools` / `functions` / `json`
    /// pin one shape; `json_schema` is a preference that demotes back to the
    /// cascade rather than a pin.
    pub fn set_llm_structured_output_mode(&self, mode: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.structured_output_mode".to_string(),
            serde_json::json!(mode.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_temperature(&self, temperature: f64) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.temperature".to_string(),
            serde_json::json!(Some(temperature)),
        );
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_streaming(&self, streaming: bool) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections
            .insert("llm.streaming".to_string(), serde_json::json!(streaming));
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_max_completion_tokens(&self, tokens: u32) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.max_completion_tokens".to_string(),
            serde_json::json!(tokens),
        );
        drop(s);
        self.bump_version();
    }

    pub fn set_llm_max_retries(&self, retries: u32) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections
            .insert("llm.max_retries".to_string(), serde_json::json!(retries));
        drop(s);
        self.bump_version();
    }

    /// Set the transport retry floor (`LLM_NETWORK_RETRIES`), the inner ladder
    /// underneath [`set_llm_max_retries`](Self::set_llm_max_retries).
    pub fn set_llm_network_retries(&self, retries: u32) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.network_retries".to_string(),
            serde_json::json!(retries),
        );
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
        s.sections
            .insert("llm.mock".to_string(), serde_json::json!(mock));
        drop(s);
        self.bump_version();
    }

    /// Set the cassette path used by the replay mock (`MOCK_LLM_CASSETTE`).
    pub fn set_llm_cassette(&self, cassette: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.cassette".to_string(),
            serde_json::json!(cassette.to_string()),
        );
        drop(s);
        self.bump_version();
    }

    /// Set the recording cassette output path (`COGNEE_RECORD_LLM`); empty = unset.
    pub fn set_llm_record_path(&self, path: &str) {
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        s.sections.insert(
            "llm.record_path".to_string(),
            serde_json::json!(path.to_string()),
        );
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
        m.insert(
            "llm_provider".into(),
            Value::String(s.llm().provider.clone()),
        );
        m.insert("llm_model".into(), Value::String(s.llm().model.clone()));
        m.insert("llm_api_key".into(), Value::String(mask(&s.llm().api_key)));
        m.insert(
            "llm_endpoint".into(),
            Value::String(s.llm().endpoint.clone()),
        );
        m.insert(
            "llm_api_version".into(),
            Value::String(s.llm().api_version.clone()),
        );
        m.insert(
            "llm_reasoning".into(),
            Value::String(s.llm().reasoning.clone()),
        );
        m.insert(
            "llm_structured_output_mode".into(),
            Value::String(s.llm().structured_output_mode.clone()),
        );
        m.insert(
            "llm_temperature".into(),
            s.llm()
                .temperature
                .and_then(serde_json::Number::from_f64)
                .map_or(Value::Null, Value::Number),
        );
        m.insert(
            "llm_max_completion_tokens".into(),
            Value::Number(s.llm().max_completion_tokens.into()),
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
        m.insert(
            "single_process".into(),
            // `null` = derived from the relational URL, not "false". The
            // difference is load-bearing: `false` is an operator refusing the
            // startup sweep, `null` is nobody having said anything.
            s.single_process.map_or(Value::Null, Value::Bool),
        );

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
        m.insert(
            "chunk_size".into(),
            // `null` = auto-calculated at pipeline time (Python `chunk_size=None`).
            s.chunk_size
                .map_or(Value::Null, |v| Value::Number(v.into())),
        );
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

impl ConfigManager {
    /// Bulk-update LLM config from a map. Matches Python `config.set_llm_config()`.
    pub fn set_llm_config(
        &self,
        values: &HashMap<String, serde_json::Value>,
    ) -> Result<(), ConfigError> {
        // The keys this bulk setter has always accepted; each is dispatched
        // through the generic setter (aliases of `llm.*` section keys, plus
        // the `Settings` key `llm_max_parallel_requests`).
        const LLM_KEYS: &[&str] = &[
            "llm_provider",
            "llm_model",
            "llm_api_key",
            "llm_endpoint",
            "llm_api_version",
            "llm_reasoning",
            "llm_structured_output_mode",
            "llm_temperature",
            "llm_max_completion_tokens",
            "llm_streaming",
            "llm_max_retries",
            "llm_network_retries",
            "llm_min_retry_seconds",
            "llm_request_timeout_seconds",
            "llm_connect_timeout_seconds",
            "llm_request_deadline_seconds",
            "auto_rate_limit",
            "llm_max_parallel_requests",
            "llm_mock",
            "llm_cassette",
            "llm_record_path",
        ];
        if let Some(key) = values.keys().find(|k| !LLM_KEYS.contains(&k.as_str())) {
            return Err(ConfigError::UnknownKey(key.clone()));
        }
        for (key, value) in values {
            self.set(key, value.clone())?;
        }
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
    /// Accepts every key in the [registry](keys), including its aliases.
    /// Returns `ConfigError::UnknownKey` for unrecognized keys and
    /// `ConfigError::TypeMismatch` when the JSON value type doesn't match.
    ///
    /// `system_root_directory` goes through
    /// [`ConfigManager::set_system_root_directory`], so the graph and vector
    /// paths derived from the old root move with it.
    pub fn set(&self, key: &str, value: serde_json::Value) -> Result<(), ConfigError> {
        if let Some(spec) = key_spec(key) {
            if spec.key == "system_root_directory" {
                self.set_system_root_directory(as_string(key, &value)?.as_str());
                return Ok(());
            }
            let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
            (spec.set)(&mut s, key, value)?;
            drop(s);
            self.clear_invalid(spec.key);
            self.bump_version();
            return Ok(());
        }
        let schema = self.schema.read().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        if schema.key_def(key).is_some() {
            let canonical = schema
                .set(&mut s.sections, key, &value)
                .map_err(ConfigError::from)?;
            drop(s);
            drop(schema);
            self.clear_invalid(canonical);
            self.bump_version();
            return Ok(());
        }
        let strict = s.strict_config;
        drop(s);
        cognee_config_core::unknown_key(key, strict).map_err(ConfigError::from)
    }

    /// Apply a JSON object of settings in bulk, e.g. a binding's constructor
    /// settings object.
    ///
    /// `Settings` keys and known section keys are set as by
    /// [`ConfigManager::set`]; a value of the wrong type is an error. A key
    /// this manager does not know (yet) is *kept* in `Settings::sections`
    /// with a warning rather than dropped: it may belong to a component
    /// registry added later with [`ConfigManager::extend_schema`], which
    /// checks it then. Strict mode is enforced by
    /// [`ConfigManager::check_strict`] once the full schema is known.
    pub fn apply_patch(
        &self,
        patch: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), ConfigError> {
        for (key, value) in patch {
            let known = key_spec(key).is_some()
                || self
                    .schema
                    .read()
                    .expect("lock poison is unrecoverable") // lock poison is unrecoverable
                    .key_def(key)
                    .is_some();
            if known {
                self.set(key, value.clone())?;
            } else {
                // Lenient here even in strict mode: see the doc comment.
                let _ = cognee_config_core::unknown_key(key, false);
                let mut s = self.inner.write().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
                s.sections.insert(key.clone(), value.clone());
                drop(s);
                self.bump_version();
            }
        }
        Ok(())
    }

    /// The current value of `key` (a `Settings` key, a section key, or an
    /// alias of either) as JSON, unmasked. `None` for an unknown key.
    pub fn get(&self, key: &str) -> Option<serde_json::Value> {
        // Lock order: `schema` before `inner` (see `ConfigManager`).
        let schema = self.schema.read().expect("lock poison is unrecoverable"); // lock poison is unrecoverable
        let s = self.read();
        if let Some(spec) = key_spec(key) {
            return Some((spec.get)(&s));
        }
        schema.get(&s.sections, key)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
#[allow(
    clippy::disallowed_methods,
    reason = "tests drive configuration through the process env"
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

        assert_eq!(s.llm().max_completion_tokens, 4096);
    }

    #[test]
    #[serial_test::serial]
    fn overlay_chunk_size_primary_and_alias() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        for var in ["COGNEE_CHUNK_SIZE", "CHUNK_SIZE"] {
            unsafe { std::env::remove_var("COGNEE_CHUNK_SIZE") };
            unsafe { std::env::remove_var("CHUNK_SIZE") };
            unsafe { std::env::set_var(var, "2048") };
            let mut s = Settings::default();
            s.overlay_from_env();
            unsafe { std::env::remove_var(var) };
            assert_eq!(s.chunk_size, Some(2048), "{var} was not honoured");
        }
    }

    #[test]
    #[serial_test::serial]
    fn chunk_size_defaults_to_auto() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::remove_var("COGNEE_CHUNK_SIZE") };
        unsafe { std::env::remove_var("CHUNK_SIZE") };
        let mut s = Settings::default();
        s.overlay_from_env();
        assert_eq!(
            s.chunk_size, None,
            "unset must stay None so the pipeline auto-calculates (Python chunk_size=None)"
        );
    }

    /// The two retry counts are separate knobs with separate Python sources.
    ///
    /// They shared one value until SDK-624, which multiplied the loops into each
    /// other — `max_retries x (network_retries + 1) x llm_request_timeout_seconds`
    /// — and forced `llm_max_retries` to be kept at 2 to bound the product.
    #[test]
    fn retry_defaults_mirror_pythons_two_independent_counts() {
        let s = Settings::default();
        assert_eq!(
            s.llm().max_retries,
            3,
            "structured-output re-asks mirror Python's _MAX_VALIDATION_RETRIES = 3"
        );
        assert_eq!(
            s.llm().network_retries,
            2,
            "the transport ladder mirrors Python's stop_after_attempt(2)"
        );
    }

    /// The shipped defaults must not trip the deadline guard-rail in
    /// `cognee_components::builtins::llm`, which warns when the aggregate budget
    /// cannot contain `llm_max_retries x llm_min_retry_seconds`.
    ///
    /// This is a real regression, not a hypothetical: the http-server default
    /// was once lowered 3 -> 2 purely because its own defaults warned at every
    /// startup. A default configuration that warns about itself trains operators
    /// to ignore the warning.
    #[test]
    fn the_default_deadline_contains_the_default_retry_ladder() {
        let s = Settings::default();
        let ladder = s.llm().max_retries.max(1) * s.llm().min_retry_seconds;
        assert!(
            s.llm().request_deadline_seconds >= ladder,
            "LLM_REQUEST_DEADLINE_SECONDS default ({}) must contain the default \
             ladder ({} = {} x {}s), or every startup warns about the values it \
             shipped with",
            s.llm().request_deadline_seconds,
            ladder,
            s.llm().max_retries,
            s.llm().min_retry_seconds,
        );
    }

    /// ...and must also outlast one provider-overload episode.
    ///
    /// The deadline clock includes time spent paced and queued for an in-flight
    /// permit — deliberately, unlike the retry floor, which subtracts it. So a
    /// default at or below `OVERLOAD_COOLDOWN` means every call queued behind a
    /// 429/503 episode aborts after its first dispatch, turning the paced
    /// recovery the pacer exists to perform into hard failures. Caught reviewing
    /// PR #215, when the default was briefly 720s against a 900s cooldown.
    #[test]
    fn the_default_deadline_outlasts_one_overload_cooldown() {
        let s = Settings::default();
        let cooldown = cognee_utils::pacing::OVERLOAD_COOLDOWN.as_secs();
        assert!(
            u64::from(s.llm().request_deadline_seconds) > cooldown,
            "LLM_REQUEST_DEADLINE_SECONDS default ({}s) must outlast one \
             OVERLOAD_COOLDOWN ({cooldown}s), or a call paced behind a provider \
             overload episode spends its whole budget queueing and fails after a \
             single dispatch",
            s.llm().request_deadline_seconds,
        );
    }

    /// Every entry point that reaches one retry knob must reach the other.
    ///
    /// `set_llm_network_retries` first shipped reachable from the bulk
    /// `set_llm_config` map and the CLI config store but **not** from the
    /// generic `set(key, value)` dispatcher, so the public API that Python and
    /// the C binding both go through answered `UnknownKey` for a key the CLI
    /// accepted. Caught in review of PR #215.
    #[test]
    fn llm_network_retries_is_reachable_from_every_config_entry_point() {
        let cm = ConfigManager::new(Settings::default());

        cm.set("llm_network_retries", serde_json::json!(5))
            .expect("the generic dispatcher must accept the key the CLI accepts");
        assert_eq!(cm.read().llm().network_retries, 5);

        cm.set_llm_config(
            &[("llm_network_retries".to_string(), serde_json::json!(6))]
                .into_iter()
                .collect(),
        )
        .expect("the bulk LLM setter must accept it too");
        assert_eq!(cm.read().llm().network_retries, 6);

        cm.set_llm_network_retries(7);
        assert_eq!(cm.read().llm().network_retries, 7);
    }

    /// The same class of gap as the test above, for `single_process`.
    ///
    /// It shipped in review as env-var-only: the docs told embedders to set
    /// `Settings::single_process`, and the generic dispatcher every binding
    /// goes through answered `UnknownKey`. Since the whole point of the
    /// setting is to let an embedded consumer — Python, C, TS, Java — opt into
    /// startup recovery, env-only made it unreachable for its own audience.
    #[test]
    fn single_process_is_reachable_from_every_config_entry_point() {
        let cm = ConfigManager::new(Settings::default());
        assert_eq!(cm.read().single_process, None, "unset by default");

        cm.set("single_process", serde_json::json!(true))
            .expect("the generic dispatcher must accept a bool");
        assert_eq!(cm.read().single_process, Some(true));

        cm.set("single_process", serde_json::json!(false))
            .expect("and the other way");
        assert_eq!(cm.read().single_process, Some(false));

        // `null` restores the derivation, like `chunk_size` / `llm_temperature`.
        cm.set("single_process", serde_json::Value::Null)
            .expect("null must clear it, not fail");
        assert_eq!(cm.read().single_process, None);

        cm.set_single_process(true);
        assert_eq!(cm.read().single_process, Some(true));
        cm.clear_single_process();
        assert_eq!(cm.read().single_process, None);
    }

    /// A string value must mean exactly what the same text means in
    /// `COGNEE_SINGLE_PROCESS`, including the blank case — the two parsers
    /// disagreeing on whitespace is the bug this shares a parser to prevent.
    #[test]
    fn single_process_strings_parse_as_the_env_var_does() {
        let cm = ConfigManager::new(Settings::default());

        for truthy in ["1", "true", " yes ", "ON"] {
            cm.set("single_process", serde_json::json!(truthy))
                .expect("truthy string");
            assert_eq!(
                cm.read().single_process,
                Some(true),
                "{truthy:?} must read as true"
            );
        }
        for falsy in ["0", "no", "off", "maybe"] {
            cm.set("single_process", serde_json::json!(falsy))
                .expect("falsy string");
            assert_eq!(
                cm.read().single_process,
                Some(false),
                "{falsy:?} must read as an explicit false"
            );
        }
        // Blank is "no answer given", not a silent `false` — the same
        // whitespace case both env paths now agree on.
        cm.set_single_process(true);
        cm.set("single_process", serde_json::json!("   "))
            .expect("blank string");
        assert_eq!(cm.read().single_process, None, "blank means derive");
    }

    /// A JSON *number* must mean what the same digit means as a string.
    ///
    /// Callers reach this dispatcher with already-parsed JSON —
    /// `cognee-cli config set single_process 1` parses its argument before
    /// dispatching, and a JS or Python binding passing the `1` the docs list
    /// as accepted sends a number. Both used to fall through to `as_bool` and
    /// be rejected with "expects a boolean", so the documented value failed on
    /// the documented spelling while the suite stayed green: it only ever
    /// tested `Value::String("1")`, which is not what either caller sends.
    #[test]
    fn single_process_accepts_the_json_numbers_the_docs_list() {
        let cm = ConfigManager::new(Settings::default());

        cm.set("single_process", serde_json::json!(1))
            .expect("`1` is documented as accepted");
        assert_eq!(cm.read().single_process, Some(true));

        cm.set("single_process", serde_json::json!(0))
            .expect("`0` is documented as accepted");
        assert_eq!(cm.read().single_process, Some(false));

        // And a number agrees with the string spelling of itself, which is the
        // invariant the shared parser exists to hold.
        for raw in ["1", "0"] {
            cm.set("single_process", serde_json::json!(raw))
                .expect("string form");
            let as_string = cm.read().single_process;
            cm.set(
                "single_process",
                serde_json::Value::Number(raw.parse::<u64>().expect("digit").into()),
            )
            .expect("number form");
            assert_eq!(
                cm.read().single_process,
                as_string,
                "`{raw}` and \"{raw}\" must not mean different things"
            );
        }
    }

    /// The config snapshot must keep "derived" and "explicitly false" apart —
    /// reporting `false` for an unset value would tell an operator the sweep
    /// was refused when nobody had said anything.
    #[test]
    fn single_process_snapshot_distinguishes_null_from_false() {
        let cm = ConfigManager::new(Settings::default());
        assert_eq!(
            cm.get_settings().get("single_process"),
            Some(&serde_json::Value::Null)
        );

        cm.set_single_process(false);
        assert_eq!(
            cm.get_settings().get("single_process"),
            Some(&serde_json::Value::Bool(false))
        );
    }

    #[test]
    #[serial_test::serial]
    fn overlay_picks_up_llm_network_retries() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::set_var("LLM_NETWORK_RETRIES", "7") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LLM_NETWORK_RETRIES") };

        assert_eq!(s.llm().network_retries, 7);
        assert_eq!(
            s.llm().max_retries,
            Settings::default().llm().max_retries,
            "the transport knob must not move the structured-output one — \
             sharing a value is the defect SDK-624 fixed"
        );
    }

    /// `overlay_from` reads only the `EnvSource` it is given — no process-env
    /// mutation, so no `#[serial]` — and keeps `overlay_from_env`'s rules:
    /// aliases fall back, empty values count as unset.
    #[test]
    fn overlay_from_reads_only_the_given_env_source() {
        let env = MapEnv::new()
            .with("OPENAI_MODEL", "alias-model")
            .with("LLM_NETWORK_RETRIES", "7")
            .with("LLM_PROVIDER", "")
            .with(cognee_database::SINGLE_PROCESS_ENV, "true");
        let mut s = Settings::default();
        s.overlay_from(&env);

        assert_eq!(s.llm().model, "alias-model");
        assert_eq!(s.llm().network_retries, 7);
        assert_eq!(s.llm().provider, Settings::default().llm().provider);
        assert_eq!(s.single_process, Some(true));
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

        assert_eq!(s.llm().max_completion_tokens, 2048);
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
                s.llm().streaming,
                expected,
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
        assert_eq!(cm.read().llm().model, "gpt-4o");

        cm.set_llm_api_key("sk-test");
        assert_eq!(cm.version(), 2);
        assert_eq!(cm.read().llm().api_key, "sk-test");
    }

    #[test]
    fn config_manager_clone_shares_state() {
        let cm1 = ConfigManager::new(Settings::default());
        let cm2 = cm1.clone();

        cm1.set_llm_model("shared-model");
        assert_eq!(cm2.read().llm().model, "shared-model");
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
        assert_eq!(cm.read().llm().model, "test-model");
    }

    #[test]
    fn config_manager_generic_set_u32() {
        let cm = ConfigManager::new(Settings::default());
        cm.set("chunk_size", serde_json::json!(2048))
            .expect("set should succeed");
        assert_eq!(cm.read().chunk_size, Some(2048));
    }

    /// Lenient by default: an unknown key is a warning, changes nothing and
    /// does not bump the version.
    #[test]
    fn config_manager_generic_set_unknown_key_is_lenient_by_default() {
        let cm = ConfigManager::new(Settings::default());
        cm.set("nonexistent_key", serde_json::json!("value"))
            .expect("lenient by default");
        assert_eq!(cm.version(), 0);
        assert!(cm.read().sections.is_empty());
    }

    /// With `strict_config` on, an unknown key is an error, from `set` and
    /// for keys that arrived in bulk (`check_strict`).
    #[test]
    fn config_manager_generic_set_unknown_key_errors_when_strict() {
        let cm = ConfigManager::new(Settings::default());
        cm.set("strict_config", serde_json::json!(true)).unwrap();
        match cm.set("nonexistent_key", serde_json::json!("value")) {
            Err(ConfigError::UnknownKey(k)) => assert_eq!(k, "nonexistent_key"),
            other => panic!("expected UnknownKey, got: {other:?}"),
        }

        let mut settings = Settings {
            strict_config: true,
            ..Settings::default()
        };
        settings
            .sections
            .insert("stale.key".into(), serde_json::json!(1));
        let cm = ConfigManager::new(settings);
        assert!(matches!(cm.check_strict(), Err(ConfigError::UnknownKey(k)) if k == "stale.key"));
        assert_eq!(
            cm.read().sections.get("stale.key"),
            Some(&serde_json::json!(1)),
            "kept"
        );
    }

    cognee_config_core::config_section! {
        /// A section only a custom registry knows.
        pub struct CustomSection in "test.custom" {
            /// A knob.
            pub x: u32 = 1 => { env: ["COGNEE_TEST_CUSTOM_X"], alias: ["custom_x"], secret: false },
        }
    }

    /// A custom registry's section env vars are read when env is loaded with
    /// the full schema, and not by the built-in loader.
    #[test]
    fn custom_registry_sections_load_from_env_with_the_full_schema() {
        let mut registry = cognee_components::ComponentRegistry::with_builtins();
        registry.register_config_section(
            <CustomSection as cognee_config_core::ConfigSection>::schema(),
        );
        let extra = registry.config_schema().unwrap();
        let schema = schema_with(&extra).unwrap();
        let env = MapEnv::new().with("COGNEE_TEST_CUSTOM_X", "5");

        let mut builtin_only = Settings::default();
        builtin_only.overlay_from(&env);
        assert!(
            builtin_only.sections.is_empty(),
            "the built-in loader does not know it"
        );

        let mut settings = Settings::default();
        settings.overlay_from_with_schema(&env, &schema);
        let cm = crate::ComponentManager::with_registry(ConfigManager::new(settings), registry);
        assert_eq!(cm.config().get("test.custom.x"), Some(serde_json::json!(5)));
        assert_eq!(cm.config().get("custom_x"), Some(serde_json::json!(5)));
        cm.config().set("custom_x", serde_json::json!(9)).unwrap();
        let section: CustomSection = cm.config().read().backend_context().section();
        assert_eq!(section.x, 9);
    }

    fn custom_schema() -> ConfigSchema {
        let mut extra = ConfigSchema::new();
        extra
            .add(<CustomSection as cognee_config_core::ConfigSection>::schema())
            .unwrap();
        schema_with(&extra).unwrap()
    }

    /// A bulk-loaded section value of the wrong type is an error at the first
    /// fallible step, strict mode or not, until the key is set again.
    #[test]
    fn wrongly_typed_bulk_section_value_is_reported() {
        let settings: Settings =
            serde_json::from_value(serde_json::json!({ "custom_x": "big" })).unwrap();
        let cm = ConfigManager::with_schema(settings, custom_schema());
        match cm.check_strict() {
            Err(ConfigError::TypeMismatch { key, .. }) => assert_eq!(key, "test.custom.x"),
            other => panic!("expected a type mismatch, got {other:?}"),
        }
        assert_eq!(
            cm.get("test.custom.x"),
            Some(serde_json::json!(1)),
            "default"
        );
        cm.set("custom_x", serde_json::json!(3)).unwrap();
        cm.check_strict().unwrap();
    }

    /// `Settings` keys that serde does not know by that name (the
    /// `#[serde(skip)]` fields and key aliases) land in the flattened
    /// `sections` map; `ConfigManager::new` applies them as settings instead
    /// of treating them as unknown section keys.
    #[test]
    fn bulk_loaded_settings_keys_are_not_unknown_section_keys() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "strict_config": true,
            "llm_args": { "max_tokens": 100 },
            "embedding_model": "x",
        }))
        .unwrap();
        let cm = ConfigManager::new(settings);
        cm.check_strict().unwrap();
        assert_eq!(
            cm.get("llm_args"),
            Some(serde_json::json!({ "max_tokens": 100 }))
        );
        let s = cm.read();
        assert_eq!(s.embedding_model_name, "x");
        assert!(
            !s.sections.contains_key("embedding_model"),
            "{:?}",
            s.sections
        );
    }

    /// A wrongly typed bulk value of a `Settings` key routed through the
    /// flatten map is reported like a section value.
    #[test]
    fn wrongly_typed_bulk_settings_alias_is_reported() {
        let settings: Settings =
            serde_json::from_value(serde_json::json!({ "embedding_model": 5 })).unwrap();
        let cm = ConfigManager::new(settings);
        assert!(matches!(
            cm.check_strict(),
            Err(ConfigError::TypeMismatch { .. })
        ));
    }

    /// Settings built with the full schema recognize a custom section's values
    /// from the start: nothing is reported as unknown, and re-adding the same
    /// sections is a no-op.
    #[test]
    fn with_schema_knows_custom_sections_up_front() {
        let mut settings = Settings {
            strict_config: true,
            ..Settings::default()
        };
        settings
            .sections
            .insert("test.custom.x".into(), serde_json::json!(5));
        let schema = custom_schema();
        let cm = ConfigManager::with_schema(settings, schema.clone());
        cm.check_strict().unwrap();
        cm.extend_schema(&schema).unwrap();
        cm.check_strict().unwrap();
        assert_eq!(cm.get("test.custom.x"), Some(serde_json::json!(5)));
    }

    /// `check_strict`/`get` and `extend_schema` take the two locks in the same
    /// order, so they cannot deadlock against each other.
    #[test]
    fn extend_schema_does_not_deadlock_against_readers() {
        let cm = ConfigManager::new(Settings {
            strict_config: true,
            ..Settings::default()
        });
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let cm = cm.clone();
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        let _ = cm.check_strict();
                        let _ = cm.get("test.custom.x");
                    }
                })
            })
            .collect();
        for _ in 0..200 {
            let mut extra = ConfigSchema::new();
            extra
                .add(<CustomSection as cognee_config_core::ConfigSection>::schema())
                .unwrap();
            cm.extend_schema(&extra).unwrap();
        }
        for reader in readers {
            reader.join().unwrap();
        }
    }

    /// `secret_keys` covers the registry rows and secret section settings.
    #[test]
    fn secret_keys_cover_registry_rows() {
        let keys = ConfigManager::new(Settings::default()).secret_keys();
        assert!(keys.contains(&"db_password"));
        assert!(keys.contains(&"llm.fallback_api_key"));
        assert!(!keys.contains(&"llm.model"));
    }

    /// A key owned by a component config section is settable by its
    /// canonical key and its plain alias, and reaches the build context.
    #[cfg(feature = "ladybug")]
    #[test]
    fn section_keys_are_settable_and_reach_the_build_context() {
        let cm = ConfigManager::new(Settings::default());
        cm.set("graph_max_db_size", serde_json::json!(2048))
            .unwrap();
        assert_eq!(
            cm.get("graph.ladybug.max_db_size"),
            Some(serde_json::json!(2048))
        );
        assert!(matches!(
            cm.set("graph.ladybug.max_db_size", serde_json::json!("big")),
            Err(ConfigError::TypeMismatch { .. })
        ));
        let ctx = cm.read().backend_context();
        let section: cognee_graph::LadybugConfig = ctx.section();
        assert_eq!(section.max_db_size, 2048);
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
        assert_eq!(s.llm().model, "gpt-4o");
        assert_eq!(s.llm().provider, "openai");
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
        assert_eq!(cm.read().llm().api_version, "2024-02-15");

        cm.set_llm_temperature(0.7);
        expected_version += 1;
        assert!(
            cm.read()
                .llm()
                .temperature
                .is_some_and(|t| (t - 0.7).abs() < f64::EPSILON)
        );

        cm.set_llm_streaming(true);
        expected_version += 1;
        assert!(cm.read().llm().streaming);

        cm.set_llm_max_completion_tokens(2048);
        expected_version += 1;
        assert_eq!(cm.read().llm().max_completion_tokens, 2048);

        cm.set_llm_max_retries(5);
        expected_version += 1;
        assert_eq!(cm.read().llm().max_retries, 5);

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
        assert!(
            cm.read()
                .llm()
                .temperature
                .is_some_and(|t| (t - 0.5).abs() < f64::EPSILON)
        );

        cm.set("llm_streaming", serde_json::json!(true))
            .expect("llm_streaming should be settable");
        assert!(cm.read().llm().streaming);

        cm.set("llm_max_retries", serde_json::json!(7))
            .expect("llm_max_retries should be settable");
        assert_eq!(cm.read().llm().max_retries, 7);

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
        assert!(s.llm().streaming);
        assert_eq!(s.llm().max_retries, 9);
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

        assert_eq!(s.llm().rate_limit_requests, 120);
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
            s.llm().args.get("max_tokens"),
            Some(&serde_json::json!(16384))
        );
        assert_eq!(s.llm().args.get("top_p"), Some(&serde_json::json!(0.9)));
        // The lowered backend context carries the same map through to the factory.
        assert_eq!(s.backend_context().llm.llm_args, s.llm().args);
    }

    /// A Bedrock embedding provider must not inherit an OpenAI-compatible
    /// `LLM_ENDPOINT` / `LLM_API_KEY`.
    ///
    /// The endpoint would receive AWS `InvokeModel` bodies, and the key would be
    /// sent to AWS as a bearer token — the Bedrock engine's own guard only
    /// rejects the `api.openai.com` default, so a custom proxy passed straight
    /// through.
    #[test]
    fn bedrock_embeddings_do_not_inherit_a_foreign_llm_endpoint_or_key() {
        let mut s = Settings {
            sections: [
                (
                    "llm.provider".to_string(),
                    serde_json::json!("openai".to_string()),
                ),
                (
                    "llm.endpoint".to_string(),
                    serde_json::json!("https://llm-proxy.example.com/v1".to_string()),
                ),
                (
                    "llm.api_key".to_string(),
                    serde_json::json!("sk-openai-key".to_string()),
                ),
            ]
            .into(),
            embedding_provider: "bedrock".to_string(),
            embedding_endpoint: String::new(),
            embedding_api_key: String::new(),
            ..Default::default()
        };

        let ctx = s.backend_context();
        assert_eq!(
            ctx.embedding.endpoint, None,
            "a non-Bedrock LLM endpoint must not become the Bedrock api_base",
        );
        assert_eq!(
            ctx.embedding.api_key, None,
            "a non-Bedrock LLM key must not become the Bedrock bearer token",
        );

        // The sharing rationale still applies when both sides are Bedrock.
        s.sections.insert(
            "llm.provider".to_string(),
            serde_json::json!("bedrock".to_string()),
        );
        let shared = s.backend_context();
        assert_eq!(
            shared.embedding.api_key.as_deref(),
            Some("sk-openai-key"),
            "a Bedrock LLM key is still shared with Bedrock embeddings",
        );

        // And a non-Bedrock embedding provider keeps the original fallback.
        s.embedding_provider = "openai".to_string();
        s.sections.insert(
            "llm.provider".to_string(),
            serde_json::json!("openai".to_string()),
        );
        let inherited = s.backend_context();
        assert_eq!(
            inherited.embedding.endpoint.as_deref(),
            Some("https://llm-proxy.example.com/v1"),
            "the shared-account fallback must survive for OpenAI-compatible pairs",
        );
    }

    #[test]
    #[serial_test::serial]
    fn overlay_ignores_malformed_llm_args() {
        // A non-object / malformed value is ignored (left empty), not fatal.
        unsafe { std::env::set_var("LLM_ARGS", "not json") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LLM_ARGS") };
        assert!(s.llm().args.is_empty());

        unsafe { std::env::set_var("LLM_ARGS", "[1, 2, 3]") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("LLM_ARGS") };
        assert!(s.llm().args.is_empty());
    }

    #[test]
    fn default_values_are_correct() {
        let s = Settings::default();
        assert!(s.llm().args.is_empty());
        assert_eq!(s.cache_backend, "fs");
        assert_eq!(s.cache_host, "localhost");
        assert_eq!(s.cache_port, 6379);
        assert_eq!(s.session_ttl_seconds, 604800);
        assert!(s.enable_caching);
        assert!(!s.auto_feedback);
        assert!(!s.enable_access_control);
        assert_eq!(s.log_level, "info");
        assert!(!s.llm().rate_limit_enabled);
        assert_eq!(s.llm().rate_limit_requests, 60);
        assert_eq!(s.llm().rate_limit_interval, 60);
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
            sections: [("llm.api_key".to_string(), serde_json::json!("sk-secret"))].into(),
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
        // The graph default is derived from the compiled graph backends, so pin
        // it only where the derivation has an answer to give. `ladybug` is in
        // this crate's default features, which is the configuration every
        // consumer of the documented default builds.
        #[cfg(feature = "ladybug")]
        assert_eq!(settings.graph_database_provider, "ladybug");
        #[cfg(all(not(feature = "ladybug"), feature = "pggraph"))]
        assert_eq!(settings.graph_database_provider, "postgres");
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
        assert_eq!(s.llm().fallback_model, "gpt-4o-mini");
        assert_eq!(s.llm().fallback_provider, "openai");
        assert_eq!(s.llm().fallback_endpoint, "https://fallback.example.com/v1");
        assert_eq!(s.llm().fallback_api_key, "fallback-key");
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
        assert_eq!(
            cfg.read()
                .section::<cognee_components::OpenAiCompatibleConfig>()
                .transcription_model,
            "whisper-1"
        );
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

    // -- sslmode on Postgres URLs (Fix 2) -----------------------------------

    #[test]
    fn relational_postgres_url_has_no_sslmode_by_default() {
        let s = Settings {
            db_provider: "postgres".to_string(),
            db_host: "db.internal".to_string(),
            db_port: 5432,
            db_name: "cognee".to_string(),
            db_username: "postgres".to_string(),
            db_password: "pw".to_string(),
            db_sslmode: String::new(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_relational_db_url(),
            "postgres://postgres:pw@db.internal:5432/cognee"
        );
    }

    #[test]
    fn relational_postgres_url_appends_sslmode_when_set() {
        let s = Settings {
            db_provider: "postgres".to_string(),
            db_host: "db.internal".to_string(),
            db_port: 5432,
            db_name: "cognee".to_string(),
            db_username: "postgres".to_string(),
            db_password: "pw".to_string(),
            db_sslmode: "require".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_relational_db_url(),
            "postgres://postgres:pw@db.internal:5432/cognee?sslmode=require"
        );
    }

    #[test]
    fn sqlite_relational_url_is_untouched_by_sslmode() {
        // Non-postgres provider must never gain a `?sslmode=` — it would break
        // the sqlite connection string.
        let s = Settings {
            db_provider: "sqlite".to_string(),
            db_sslmode: "require".to_string(),
            relational_db_url: "sqlite:./cognee.db?mode=rwc".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_relational_db_url(),
            "sqlite:./cognee.db?mode=rwc"
        );
    }

    #[test]
    #[serial_test::serial]
    fn db_sslmode_env_overrides() {
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::remove_var("DATABASE_CONNECT_ARGS") };
        unsafe { std::env::set_var("DB_SSLMODE", "verify-full") };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("DB_SSLMODE") };
        assert_eq!(s.db_sslmode, "verify-full");
    }

    #[test]
    #[serial_test::serial]
    fn db_sslmode_derived_from_connect_args() {
        // The Python chart passes asyncpg connect args; we map ssl -> sslmode.
        // SAFETY: test is serial — no other thread reads/writes env concurrently.
        unsafe { std::env::remove_var("DB_SSLMODE") };
        unsafe { std::env::set_var("DATABASE_CONNECT_ARGS", r#"{"ssl":"require"}"#) };
        let mut s = Settings::default();
        s.overlay_from_env();
        unsafe { std::env::remove_var("DATABASE_CONNECT_ARGS") };
        assert_eq!(s.db_sslmode, "require");
    }

    #[test]
    fn sslmode_from_connect_args_maps_forms() {
        assert_eq!(
            sslmode_from_connect_args(r#"{"ssl":"require"}"#).as_deref(),
            Some("require")
        );
        assert_eq!(
            sslmode_from_connect_args(r#"{"ssl":true}"#).as_deref(),
            Some("require")
        );
        assert_eq!(
            sslmode_from_connect_args(r#"{"ssl":"verify-full"}"#).as_deref(),
            Some("verify-full")
        );
        // No ssl key, unknown value, or bad JSON => None (append nothing).
        assert_eq!(sslmode_from_connect_args(r#"{"foo":1}"#), None);
        // Unknown string (likely a typo) => None, but warns rather than crashing.
        assert_eq!(sslmode_from_connect_args(r#"{"ssl":"bogus"}"#), None);
        assert_eq!(sslmode_from_connect_args("not json"), None);
        // An object/array ssl (asyncpg SSLContext) can't map to a libpq sslmode,
        // but TLS is clearly intended — require it, never silently drop (Fix #6).
        assert_eq!(
            sslmode_from_connect_args(r#"{"ssl":{"ca":"/etc/ca.pem"}}"#).as_deref(),
            Some("require")
        );
    }

    /// An externally registered provider must receive the resolved Postgres
    /// URLs, under whatever id it chose.
    ///
    /// `ComponentRegistry` lets any crate register a provider, so the set of
    /// Postgres-backed provider ids is open and this crate cannot enumerate it.
    /// `backend_context` used to resolve only for the built-in ids
    /// (`postgres`/`postgresql`, `pgvector`), which handed every external
    /// Postgres-backed adapter a `None` it could only turn into "requires a
    /// resolved PostgreSQL URL" — the provider was unreachable through
    /// configuration on every entry point. Pin that it is resolved for an id
    /// this crate has never heard of.
    #[test]
    fn backend_context_resolves_postgres_urls_for_an_external_provider() {
        let s = Settings {
            graph_database_provider: "some-external-provider".to_string(),
            vector_db_provider: "some-external-provider".to_string(),
            db_provider: "postgres".to_string(),
            db_host: "shared-pg".to_string(),
            db_port: 5432,
            db_name: "cognee_db".to_string(),
            db_username: "postgres".to_string(),
            db_password: "pw".to_string(),
            ..Settings::default()
        };
        let ctx = s.backend_context();
        assert_eq!(
            ctx.graph_postgres_url,
            Some(Ok(
                "postgres://postgres:pw@shared-pg:5432/cognee_db".to_string()
            )),
            "an external graph provider must be offered the resolved URL"
        );
        assert_eq!(
            ctx.vector_postgres_url,
            Some(Ok(
                "postgres://postgres:pw@shared-pg:5432/cognee_db".to_string()
            )),
            "an external vector provider must be offered the resolved URL"
        );
        // The provider id is passed through untouched (lowercased) so the
        // registry can find the factory that was registered under it.
        assert_eq!(ctx.graph_provider, "some-external-provider");
        assert_eq!(ctx.vector_provider, "some-external-provider");
    }

    /// A failed resolution must reach the factory as `Some(Err(cause))`, not
    /// `None`, so the factory can restate the real reason instead of the
    /// generic "requires a resolved PostgreSQL URL".
    #[test]
    fn backend_context_carries_the_resolution_failure_cause() {
        // Blank relational host with no vector-specific credentials: nothing
        // left to fall back to (`vector_postgres_url_errs_when_relational_host_
        // or_name_missing` pins that this is an error rather than a bogus URL).
        let s = Settings {
            graph_database_provider: "some-external-provider".to_string(),
            vector_db_provider: "some-external-provider".to_string(),
            db_host: String::new(),
            ..Settings::default()
        };
        let ctx = s.backend_context();
        assert!(
            matches!(ctx.vector_postgres_url, Some(Err(_))),
            "an unresolvable URL must carry its cause, not vanish into None"
        );
    }

    // -- vector Postgres URL: all-or-nothing relational fallback (Fixes 1-3) --

    #[cfg(feature = "pgvector")]
    #[test]
    fn vector_postgres_url_uses_full_vector_creds() {
        // When the full VECTOR_DB_* set is present, use it verbatim.
        let s = Settings {
            vector_db_provider: "pgvector".to_string(),
            vector_db_host: "vec.internal".to_string(),
            vector_db_port: 6543,
            vector_db_name: "vectors".to_string(),
            vector_db_username: "vuser".to_string(),
            vector_db_password: "vpw".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_vector_postgres_url().expect("url builds"),
            "postgres://vuser:vpw@vec.internal:6543/vectors"
        );
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn vector_postgres_url_falls_back_to_relational_for_single_db() {
        // The single-shared-Postgres case: only db_* is configured, no
        // VECTOR_DB_*. The vector store must share the relational host, PORT and
        // DATABASE — NOT port 1234 or a `cognee_vectors` database (Fixes 1 & 2).
        // This is the path the pg_shared_db_single_stack test could not exercise.
        let s = Settings {
            vector_db_provider: "pgvector".to_string(),
            db_provider: "postgres".to_string(),
            db_host: "shared-pg".to_string(),
            db_port: 5432,
            db_name: "cognee_db".to_string(),
            db_username: "postgres".to_string(),
            db_password: "pw".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_vector_postgres_url().expect("url builds"),
            "postgres://postgres:pw@shared-pg:5432/cognee_db",
            "vector store must land on the SAME host/port/database as relational"
        );
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn vector_postgres_url_partial_vector_creds_use_relational() {
        // VECTOR_DB_HOST alone is incomplete → fall back wholesale to relational
        // (Python's all-or-nothing rule), not a per-field mix landing on 1234.
        let s = Settings {
            vector_db_provider: "pgvector".to_string(),
            vector_db_host: "ignored-partial".to_string(),
            db_host: "shared-pg".to_string(),
            db_port: 5432,
            db_name: "cognee_db".to_string(),
            db_username: "postgres".to_string(),
            db_password: "pw".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_vector_postgres_url().expect("url builds"),
            "postgres://postgres:pw@shared-pg:5432/cognee_db"
        );
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn vector_postgres_url_defaults_username_for_out_of_box_config() {
        // The out-of-the-box config leaves db_username empty. Rather than
        // erroring, resolve a usable URL by defaulting the username to
        // `postgres`, while staying on the relational host/port/DATABASE
        // (localhost/5432/cognee_db) — NOT the old per-field defaults
        // (port 1234 / `cognee_vectors`) that split the stores.
        let s = Settings {
            vector_db_provider: "pgvector".to_string(),
            db_username: String::new(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_vector_postgres_url().expect("url builds"),
            "postgres://postgres@localhost:5432/cognee_db"
        );
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn vector_postgres_url_errs_when_password_set_but_username_empty() {
        // A password with a blank username is a misconfiguration, not the
        // default: fail fast rather than silently authenticating as `postgres`.
        let s = Settings {
            vector_db_provider: "pgvector".to_string(),
            db_username: String::new(),
            db_password: "secret".to_string(),
            ..Settings::default()
        };
        assert!(s.resolved_vector_postgres_url().is_err());
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn vector_postgres_url_errs_when_relational_host_or_name_missing() {
        // If even the relational host/name are blank there is nothing to fall
        // back to, so surface an explicit error rather than a bogus URL.
        let s = Settings {
            vector_db_provider: "pgvector".to_string(),
            db_host: String::new(),
            ..Settings::default()
        };
        assert!(s.resolved_vector_postgres_url().is_err());
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn vector_postgres_url_full_url_shortcut_is_verbatim() {
        // A full postgres:// URL in vector_db_url is passed through untouched.
        let s = Settings {
            vector_db_provider: "pgvector".to_string(),
            vector_db_url: "postgres://u:p@explicit:6543/vecs?sslmode=require".to_string(),
            db_sslmode: "disable".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_vector_postgres_url().expect("url builds"),
            "postgres://u:p@explicit:6543/vecs?sslmode=require"
        );
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn vector_postgres_url_appends_sslmode() {
        // sslmode is appended on the relational-fallback (single-DB) path too.
        let s = Settings {
            vector_db_provider: "pgvector".to_string(),
            db_host: "shared-pg".to_string(),
            db_port: 5432,
            db_name: "cognee_db".to_string(),
            db_username: "postgres".to_string(),
            db_password: "pw".to_string(),
            db_sslmode: "require".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_vector_postgres_url().expect("url builds"),
            "postgres://postgres:pw@shared-pg:5432/cognee_db?sslmode=require"
        );
    }

    #[cfg(feature = "pggraph")]
    #[test]
    fn graph_postgres_url_appends_sslmode() {
        let s = Settings {
            graph_database_provider: "postgres".to_string(),
            graph_database_host: "graph.internal".to_string(),
            graph_database_port: 5432,
            graph_database_name: "cognee".to_string(),
            graph_database_username: "postgres".to_string(),
            graph_database_password: "pw".to_string(),
            db_sslmode: "require".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            s.resolved_graph_postgres_url().expect("url builds"),
            "postgres://postgres:pw@graph.internal:5432/cognee?sslmode=require"
        );
    }
}
