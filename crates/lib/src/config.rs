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
