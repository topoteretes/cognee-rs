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

    pub embedding_model_path: String,
    pub embedding_tokenizer_path: String,
    pub embedding_model_name: String,
    pub embedding_dimensions: u32,
    pub embedding_max_sequence_length: u32,
    pub embedding_batch_size: u32,
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
        if let Some(v) = str_var("LLM_MAX_TOKENS")
            && let Ok(n) = v.parse::<u32>()
        {
            self.llm_max_completion_tokens = n;
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

            embedding_model_path: "./target/models/BGE-Small-v1.5-model_quantized.onnx".to_string(),
            embedding_tokenizer_path: "./target/models/bge-small-tokenizer.json".to_string(),
            embedding_model_name: "BGE-Small-v1.5".to_string(),
            embedding_dimensions: 384,
            embedding_max_sequence_length: 512,
            embedding_batch_size: 32,
        }
    }
}
