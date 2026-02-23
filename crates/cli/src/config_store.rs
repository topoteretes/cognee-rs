use crate::error::CliError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

const CURRENT_VERSION: u32 = 1;

pub const DEFAULT_SYSTEM_PROMPT_PATH: &str = "answer_simple_question.txt";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDocument {
    pub version: u32,
    #[serde(default)]
    pub settings: Settings,
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

    pub default_system_prompt_path: String,

    pub embedding_model_path: String,
    pub embedding_tokenizer_path: String,
    pub embedding_model_name: String,
    pub embedding_dimensions: u32,
    pub embedding_max_sequence_length: u32,
    pub embedding_batch_size: u32,
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

impl Default for ConfigDocument {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            settings: Settings::default(),
        }
    }
}

pub fn config_file_path() -> Result<PathBuf, CliError> {
    let base_dir = dirs::config_dir().ok_or_else(|| {
        CliError::Runtime("Could not resolve user config directory for cognee-cli".to_string())
    })?;

    Ok(base_dir.join("cognee-rust").join("config.json"))
}

pub fn load_config() -> Result<ConfigDocument, CliError> {
    let path = config_file_path()?;

    if !path.exists() {
        return Ok(ConfigDocument::default());
    }

    let content = fs::read_to_string(&path)
        .map_err(|error| CliError::Runtime(format!("Failed to read config file: {error}")))?;

    serde_json::from_str::<ConfigDocument>(&content)
        .map_err(|error| CliError::Runtime(format!("Failed to parse config file: {error}")))
}

pub fn save_config(config: &ConfigDocument) -> Result<(), CliError> {
    let path = config_file_path()?;
    let directory = path.parent().ok_or_else(|| {
        CliError::Runtime("Could not resolve parent directory for config file".to_string())
    })?;

    fs::create_dir_all(directory).map_err(|error| {
        CliError::Runtime(format!("Failed to create config directory: {error}"))
    })?;

    let serialized = serde_json::to_string_pretty(config)
        .map_err(|error| CliError::Runtime(format!("Failed to serialize config: {error}")))?;

    let temp_path = path.with_extension("json.tmp");
    let mut temp_file = fs::File::create(&temp_path).map_err(|error| {
        CliError::Runtime(format!("Failed to create temp config file: {error}"))
    })?;
    temp_file
        .write_all(serialized.as_bytes())
        .map_err(|error| CliError::Runtime(format!("Failed to write temp config file: {error}")))?;

    fs::rename(&temp_path, &path)
        .map_err(|error| CliError::Runtime(format!("Failed to replace config file: {error}")))
}

pub fn known_keys() -> Vec<&'static str> {
    vec![
        "default_user_id",
        "default_dataset_name",
        "system_root_directory",
        "data_root_directory",
        "cache_root_directory",
        "logs_root_directory",
        "monitoring_tool",
        "classification_model",
        "summarization_model",
        "graph_model",
        "llm_provider",
        "llm_model",
        "llm_api_key",
        "llm_endpoint",
        "llm_api_version",
        "llm_temperature",
        "llm_streaming",
        "llm_max_completion_tokens",
        "llm_max_retries",
        "llm_max_parallel_requests",
        "graph_prompt_path",
        "graph_database_provider",
        "graph_database_url",
        "graph_database_name",
        "graph_database_username",
        "graph_database_password",
        "graph_database_port",
        "graph_database_key",
        "graph_file_path",
        "graph_filename",
        "vector_db_provider",
        "vector_db_url",
        "vector_db_port",
        "vector_db_name",
        "vector_db_key",
        "chunk_strategy",
        "chunk_engine",
        "chunk_size",
        "chunk_overlap",
        "relational_db_url",
        "migration_db_url",
        "default_system_prompt_path",
        "embedding_model_path",
        "embedding_tokenizer_path",
        "embedding_model_name",
        "embedding_dimensions",
        "embedding_max_sequence_length",
        "embedding_batch_size",
    ]
}

pub fn as_flat_map(settings: &Settings) -> BTreeMap<&'static str, Value> {
    BTreeMap::from([
        (
            "default_user_id",
            Value::String(settings.default_user_id.clone()),
        ),
        (
            "default_dataset_name",
            Value::String(settings.default_dataset_name.clone()),
        ),
        (
            "system_root_directory",
            Value::String(settings.system_root_directory.clone()),
        ),
        (
            "data_root_directory",
            Value::String(settings.data_root_directory.clone()),
        ),
        (
            "cache_root_directory",
            Value::String(settings.cache_root_directory.clone()),
        ),
        (
            "logs_root_directory",
            Value::String(settings.logs_root_directory.clone()),
        ),
        (
            "monitoring_tool",
            Value::String(settings.monitoring_tool.clone()),
        ),
        (
            "classification_model",
            Value::String(settings.classification_model.clone()),
        ),
        (
            "summarization_model",
            Value::String(settings.summarization_model.clone()),
        ),
        ("graph_model", Value::String(settings.graph_model.clone())),
        ("llm_provider", Value::String(settings.llm_provider.clone())),
        ("llm_model", Value::String(settings.llm_model.clone())),
        ("llm_api_key", Value::String(settings.llm_api_key.clone())),
        ("llm_endpoint", Value::String(settings.llm_endpoint.clone())),
        (
            "llm_api_version",
            Value::String(settings.llm_api_version.clone()),
        ),
        ("llm_temperature", Value::from(settings.llm_temperature)),
        ("llm_streaming", Value::from(settings.llm_streaming)),
        (
            "llm_max_completion_tokens",
            Value::from(settings.llm_max_completion_tokens),
        ),
        ("llm_max_retries", Value::from(settings.llm_max_retries)),
        (
            "llm_max_parallel_requests",
            Value::from(settings.llm_max_parallel_requests),
        ),
        (
            "graph_prompt_path",
            Value::String(settings.graph_prompt_path.clone()),
        ),
        (
            "graph_database_provider",
            Value::String(settings.graph_database_provider.clone()),
        ),
        (
            "graph_database_url",
            Value::String(settings.graph_database_url.clone()),
        ),
        (
            "graph_database_name",
            Value::String(settings.graph_database_name.clone()),
        ),
        (
            "graph_database_username",
            Value::String(settings.graph_database_username.clone()),
        ),
        (
            "graph_database_password",
            Value::String(settings.graph_database_password.clone()),
        ),
        (
            "graph_database_port",
            Value::from(settings.graph_database_port),
        ),
        (
            "graph_database_key",
            Value::String(settings.graph_database_key.clone()),
        ),
        (
            "graph_file_path",
            Value::String(settings.graph_file_path.clone()),
        ),
        (
            "graph_filename",
            Value::String(settings.graph_filename.clone()),
        ),
        (
            "vector_db_provider",
            Value::String(settings.vector_db_provider.clone()),
        ),
        (
            "vector_db_url",
            Value::String(settings.vector_db_url.clone()),
        ),
        ("vector_db_port", Value::from(settings.vector_db_port)),
        (
            "vector_db_name",
            Value::String(settings.vector_db_name.clone()),
        ),
        (
            "vector_db_key",
            Value::String(settings.vector_db_key.clone()),
        ),
        (
            "chunk_strategy",
            Value::String(settings.chunk_strategy.clone()),
        ),
        ("chunk_engine", Value::String(settings.chunk_engine.clone())),
        ("chunk_size", Value::from(settings.chunk_size)),
        ("chunk_overlap", Value::from(settings.chunk_overlap)),
        (
            "relational_db_url",
            Value::String(settings.relational_db_url.clone()),
        ),
        (
            "migration_db_url",
            Value::String(settings.migration_db_url.clone()),
        ),
        (
            "default_system_prompt_path",
            Value::String(settings.default_system_prompt_path.clone()),
        ),
        (
            "embedding_model_path",
            Value::String(settings.embedding_model_path.clone()),
        ),
        (
            "embedding_tokenizer_path",
            Value::String(settings.embedding_tokenizer_path.clone()),
        ),
        (
            "embedding_model_name",
            Value::String(settings.embedding_model_name.clone()),
        ),
        (
            "embedding_dimensions",
            Value::from(settings.embedding_dimensions),
        ),
        (
            "embedding_max_sequence_length",
            Value::from(settings.embedding_max_sequence_length),
        ),
        (
            "embedding_batch_size",
            Value::from(settings.embedding_batch_size),
        ),
    ])
}

pub fn set_value(settings: &mut Settings, key: &str, value: Value) -> Result<(), CliError> {
    match key {
        "default_user_id" => settings.default_user_id = expect_string(key, value)?,
        "default_dataset_name" => settings.default_dataset_name = expect_string(key, value)?,
        "system_root_directory" => settings.system_root_directory = expect_string(key, value)?,
        "data_root_directory" => settings.data_root_directory = expect_string(key, value)?,
        "cache_root_directory" => settings.cache_root_directory = expect_string(key, value)?,
        "logs_root_directory" => settings.logs_root_directory = expect_string(key, value)?,
        "monitoring_tool" => settings.monitoring_tool = expect_string(key, value)?,
        "classification_model" => settings.classification_model = expect_string(key, value)?,
        "summarization_model" => settings.summarization_model = expect_string(key, value)?,
        "graph_model" => settings.graph_model = expect_string(key, value)?,
        "llm_provider" => settings.llm_provider = expect_string(key, value)?,
        "llm_model" => settings.llm_model = expect_string(key, value)?,
        "llm_api_key" => settings.llm_api_key = expect_string(key, value)?,
        "llm_endpoint" => settings.llm_endpoint = expect_string(key, value)?,
        "llm_api_version" => settings.llm_api_version = expect_string(key, value)?,
        "llm_temperature" => settings.llm_temperature = expect_f64(key, value)?,
        "llm_streaming" => settings.llm_streaming = expect_bool(key, value)?,
        "llm_max_completion_tokens" => settings.llm_max_completion_tokens = expect_u32(key, value)?,
        "llm_max_retries" => settings.llm_max_retries = expect_u32(key, value)?,
        "llm_max_parallel_requests" => settings.llm_max_parallel_requests = expect_u32(key, value)?,
        "graph_prompt_path" => settings.graph_prompt_path = expect_string(key, value)?,
        "graph_database_provider" => settings.graph_database_provider = expect_string(key, value)?,
        "graph_database_url" => settings.graph_database_url = expect_string(key, value)?,
        "graph_database_name" => settings.graph_database_name = expect_string(key, value)?,
        "graph_database_username" => settings.graph_database_username = expect_string(key, value)?,
        "graph_database_password" => settings.graph_database_password = expect_string(key, value)?,
        "graph_database_port" => settings.graph_database_port = expect_u16(key, value)?,
        "graph_database_key" => settings.graph_database_key = expect_string(key, value)?,
        "graph_file_path" => settings.graph_file_path = expect_string(key, value)?,
        "graph_filename" => settings.graph_filename = expect_string(key, value)?,
        "vector_db_provider" => settings.vector_db_provider = expect_string(key, value)?,
        "vector_db_url" => settings.vector_db_url = expect_string(key, value)?,
        "vector_db_port" => settings.vector_db_port = expect_u16(key, value)?,
        "vector_db_name" => settings.vector_db_name = expect_string(key, value)?,
        "vector_db_key" => settings.vector_db_key = expect_string(key, value)?,
        "chunk_strategy" => settings.chunk_strategy = expect_string(key, value)?,
        "chunk_engine" => settings.chunk_engine = expect_string(key, value)?,
        "chunk_size" => settings.chunk_size = expect_u32(key, value)?,
        "chunk_overlap" => settings.chunk_overlap = expect_u32(key, value)?,
        "relational_db_url" => settings.relational_db_url = expect_string(key, value)?,
        "migration_db_url" => settings.migration_db_url = expect_string(key, value)?,
        "default_system_prompt_path" => {
            settings.default_system_prompt_path = expect_string(key, value)?
        }
        "embedding_model_path" => settings.embedding_model_path = expect_string(key, value)?,
        "embedding_tokenizer_path" => {
            settings.embedding_tokenizer_path = expect_string(key, value)?
        }
        "embedding_model_name" => settings.embedding_model_name = expect_string(key, value)?,
        "embedding_dimensions" => settings.embedding_dimensions = expect_u32(key, value)?,
        "embedding_max_sequence_length" => {
            settings.embedding_max_sequence_length = expect_u32(key, value)?
        }
        "embedding_batch_size" => settings.embedding_batch_size = expect_u32(key, value)?,
        _ => {
            return Err(CliError::Validation(format!(
                "Unknown config key '{key}'. Use 'cognee-cli config list' to see supported keys."
            )));
        }
    }

    Ok(())
}

pub fn unset_key(settings: &mut Settings, key: &str) -> Result<(), CliError> {
    let defaults = Settings::default();

    match key {
        "default_user_id" => settings.default_user_id = defaults.default_user_id,
        "default_dataset_name" => settings.default_dataset_name = defaults.default_dataset_name,
        "system_root_directory" => settings.system_root_directory = defaults.system_root_directory,
        "data_root_directory" => settings.data_root_directory = defaults.data_root_directory,
        "cache_root_directory" => settings.cache_root_directory = defaults.cache_root_directory,
        "logs_root_directory" => settings.logs_root_directory = defaults.logs_root_directory,
        "monitoring_tool" => settings.monitoring_tool = defaults.monitoring_tool,
        "classification_model" => settings.classification_model = defaults.classification_model,
        "summarization_model" => settings.summarization_model = defaults.summarization_model,
        "graph_model" => settings.graph_model = defaults.graph_model,
        "llm_provider" => settings.llm_provider = defaults.llm_provider,
        "llm_model" => settings.llm_model = defaults.llm_model,
        "llm_api_key" => settings.llm_api_key = defaults.llm_api_key,
        "llm_endpoint" => settings.llm_endpoint = defaults.llm_endpoint,
        "llm_api_version" => settings.llm_api_version = defaults.llm_api_version,
        "llm_temperature" => settings.llm_temperature = defaults.llm_temperature,
        "llm_streaming" => settings.llm_streaming = defaults.llm_streaming,
        "llm_max_completion_tokens" => {
            settings.llm_max_completion_tokens = defaults.llm_max_completion_tokens
        }
        "llm_max_retries" => settings.llm_max_retries = defaults.llm_max_retries,
        "llm_max_parallel_requests" => {
            settings.llm_max_parallel_requests = defaults.llm_max_parallel_requests
        }
        "graph_prompt_path" => settings.graph_prompt_path = defaults.graph_prompt_path,
        "graph_database_provider" => {
            settings.graph_database_provider = defaults.graph_database_provider
        }
        "graph_database_url" => settings.graph_database_url = defaults.graph_database_url,
        "graph_database_name" => settings.graph_database_name = defaults.graph_database_name,
        "graph_database_username" => {
            settings.graph_database_username = defaults.graph_database_username
        }
        "graph_database_password" => {
            settings.graph_database_password = defaults.graph_database_password
        }
        "graph_database_port" => settings.graph_database_port = defaults.graph_database_port,
        "graph_database_key" => settings.graph_database_key = defaults.graph_database_key,
        "graph_file_path" => settings.graph_file_path = defaults.graph_file_path,
        "graph_filename" => settings.graph_filename = defaults.graph_filename,
        "vector_db_provider" => settings.vector_db_provider = defaults.vector_db_provider,
        "vector_db_url" => settings.vector_db_url = defaults.vector_db_url,
        "vector_db_port" => settings.vector_db_port = defaults.vector_db_port,
        "vector_db_name" => settings.vector_db_name = defaults.vector_db_name,
        "vector_db_key" => settings.vector_db_key = defaults.vector_db_key,
        "chunk_strategy" => settings.chunk_strategy = defaults.chunk_strategy,
        "chunk_engine" => settings.chunk_engine = defaults.chunk_engine,
        "chunk_size" => settings.chunk_size = defaults.chunk_size,
        "chunk_overlap" => settings.chunk_overlap = defaults.chunk_overlap,
        "relational_db_url" => settings.relational_db_url = defaults.relational_db_url,
        "migration_db_url" => settings.migration_db_url = defaults.migration_db_url,
        "default_system_prompt_path" => {
            settings.default_system_prompt_path = defaults.default_system_prompt_path
        }
        "embedding_model_path" => settings.embedding_model_path = defaults.embedding_model_path,
        "embedding_tokenizer_path" => {
            settings.embedding_tokenizer_path = defaults.embedding_tokenizer_path
        }
        "embedding_model_name" => settings.embedding_model_name = defaults.embedding_model_name,
        "embedding_dimensions" => settings.embedding_dimensions = defaults.embedding_dimensions,
        "embedding_max_sequence_length" => {
            settings.embedding_max_sequence_length = defaults.embedding_max_sequence_length
        }
        "embedding_batch_size" => settings.embedding_batch_size = defaults.embedding_batch_size,
        _ => {
            return Err(CliError::Validation(format!(
                "Unknown config key '{key}'. Use 'cognee-cli config list' to see supported keys."
            )));
        }
    }

    Ok(())
}

fn expect_string(key: &str, value: Value) -> Result<String, CliError> {
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| CliError::Validation(format!("Config key '{key}' expects a string value")))
}

fn expect_u32(key: &str, value: Value) -> Result<u32, CliError> {
    value
        .as_u64()
        .and_then(|raw| u32::try_from(raw).ok())
        .ok_or_else(|| {
            CliError::Validation(format!("Config key '{key}' expects a positive integer"))
        })
}

fn expect_u16(key: &str, value: Value) -> Result<u16, CliError> {
    value
        .as_u64()
        .and_then(|raw| u16::try_from(raw).ok())
        .ok_or_else(|| {
            CliError::Validation(format!(
                "Config key '{key}' expects an integer in range 0..65535"
            ))
        })
}

fn expect_f64(key: &str, value: Value) -> Result<f64, CliError> {
    value
        .as_f64()
        .ok_or_else(|| CliError::Validation(format!("Config key '{key}' expects a numeric value")))
}

fn expect_bool(key: &str, value: Value) -> Result<bool, CliError> {
    value
        .as_bool()
        .ok_or_else(|| CliError::Validation(format!("Config key '{key}' expects true/false")))
}
