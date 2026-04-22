//! Shared configuration types for cognee-rust.

use serde::{Deserialize, Serialize};

pub const DEFAULT_SYSTEM_PROMPT_PATH: &str = "answer_simple_question.txt";

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
    pub graph_prompt_path: String,

    pub graph_database_provider: String,
    pub graph_database_url: String,
    pub graph_database_name: String,
    pub graph_database_username: String,
    pub graph_database_password: String,
    pub graph_database_port: u16,
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
        if let Some(v) = str_var("LLM_STREAMING") {
            let v = v.to_lowercase();
            self.llm_streaming = v == "true" || v == "1" || v == "yes";
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
            let v = v.to_lowercase();
            self.enable_caching = v == "true" || v == "1" || v == "yes";
        }
        if let Some(v) = str_var("AUTO_FEEDBACK") {
            let v = v.to_lowercase();
            self.auto_feedback = v == "true" || v == "1" || v == "yes";
        }

        // -- Authentication / ACL ------------------------------------------------
        if let Some(v) = str_var("DEFAULT_USER_EMAIL") {
            self.default_user_email = v;
        }
        if let Some(v) = str_var("DEFAULT_USER_PASSWORD") {
            self.default_user_password = v;
        }
        if let Some(v) = str_var("ENABLE_BACKEND_ACCESS_CONTROL") {
            let v = v.to_lowercase();
            self.enable_access_control = v == "true" || v == "1" || v == "yes";
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
            let v = v.to_lowercase();
            self.llm_rate_limit_enabled = v == "true" || v == "1" || v == "yes";
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
            let v = v.to_lowercase();
            self.embedding_rate_limit_enabled = v == "true" || v == "1" || v == "yes";
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
            let v = v.to_lowercase();
            self.cognee_tracing_enabled = v == "true" || v == "1" || v == "yes";
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

        // -- Feature flags -------------------------------------------------------
        if let Some(v) = str_var("ENABLE_LAST_ACCESSED") {
            let v = v.to_lowercase();
            self.enable_last_accessed = v == "true" || v == "1" || v == "yes";
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_user_id: "00000000-0000-0000-0000-000000000000".to_string(),
            default_dataset_name: "main_dataset".to_string(),
            system_root_directory: "./.cognee_system".to_string(),
            data_root_directory: "./.data_storage".to_string(),
            cache_root_directory: "./.cognee_cache".to_string(),
            logs_root_directory: "./logs".to_string(),
            monitoring_tool: "none".to_string(),

            classification_model: String::new(),
            summarization_model: String::new(),
            graph_model: "KnowledgeGraph".to_string(),

            llm_provider: "openai".to_string(),
            llm_model: "gpt-5-mini".to_string(),
            llm_api_key: String::new(),
            llm_endpoint: String::new(),
            llm_api_version: String::new(),
            llm_temperature: 0.0,
            llm_streaming: false,
            llm_max_completion_tokens: 16384,
            llm_max_retries: 2,
            llm_max_parallel_requests: 20,
            graph_prompt_path: "generate_graph_prompt.txt".to_string(),

            graph_database_provider: "kuzu".to_string(),
            graph_database_url: String::new(),
            graph_database_name: String::new(),
            graph_database_username: String::new(),
            graph_database_password: String::new(),
            graph_database_port: 123,
            graph_database_key: String::new(),
            graph_file_path: String::new(),
            graph_filename: String::new(),

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

            relational_db_url: "sqlite:./cognee.db".to_string(),
            migration_db_url: String::new(),

            db_provider: "sqlite".to_string(),
            db_host: "localhost".to_string(),
            db_port: 5432,
            db_name: "cognee_db".to_string(),
            db_username: String::new(),
            db_password: String::new(),

            default_system_prompt_path: DEFAULT_SYSTEM_PROMPT_PATH.to_string(),

            embedding_provider: "onnx".to_string(),
            embedding_model_path: "./target/models/BGE-Small-v1.5-model_quantized.onnx".to_string(),
            embedding_tokenizer_path: "./target/models/bge-small-tokenizer.json".to_string(),
            embedding_model_name: "BGE-Small-v1.5".to_string(),
            embedding_dimensions: 384,
            embedding_max_sequence_length: 512,
            embedding_batch_size: 32,

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
            default_user_email: String::new(),
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

            // Feature flags
            enable_last_accessed: false,
        }
    }
}

#[cfg(test)]
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
    fn default_values_are_correct() {
        let s = Settings::default();
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
        assert!(!s.enable_last_accessed);
        assert_eq!(s.embedding_provider, "onnx");
    }
}
