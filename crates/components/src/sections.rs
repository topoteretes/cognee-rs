//! Config sections owned by the component kinds and the built-in adapters.
//!
//! A **kind** section holds the settings every adapter of that kind shares
//! (`llm.model`, `llm.api_key`, …); [`ComponentRegistry`](crate::ComponentRegistry)
//! registers it whenever the kind exists. An **adapter** section holds the
//! settings only one adapter reads; that adapter's factory returns it from
//! `config_sections()`, so it exists only when the factory is registered.
//!
//! Keys are namespaced (`<section>.<field>`); each keeps the Python SDK's flat
//! name as an alias (`llm_model`), and the env vars it has always had.

use cognee_config_core::config_section;
use serde_json::{Map, Value};

use crate::context::AwsInputs;

config_section! {
    /// Settings shared by every LLM adapter. Registered by the LLM kind.
    pub struct LlmConfig in "llm" {
        /// Provider id selecting the LLM factory (`openai`, `anthropic`, `azure`,
        /// `bedrock`, `ollama`, …).
        pub provider: String = "openai".to_string() => {
            env: ["LLM_PROVIDER"], alias: ["llm_provider"], secret: false,
        },
        /// Model name, passed to the provider.
        pub model: String = "openai/gpt-5-mini".to_string() => {
            env: ["LLM_MODEL", "OPENAI_MODEL"], alias: ["llm_model"], secret: false,
        },
        /// API key.
        pub api_key: String = String::new() => {
            env: ["LLM_API_KEY", "OPENAI_TOKEN"], alias: ["llm_api_key"], secret: true,
        },
        /// Endpoint (base URL); empty uses the provider's default.
        pub endpoint: String = String::new() => {
            env: ["LLM_ENDPOINT", "OPENAI_URL"], alias: ["llm_endpoint"], secret: false,
        },
        /// API version (Azure `api-version`).
        pub api_version: String = String::new() => {
            env: ["LLM_API_VERSION"], alias: ["llm_api_version"], secret: false,
        },
        /// Reasoning-model detection: `auto`, `always` or `never`.
        pub reasoning: String = "auto".to_string() => {
            env: ["LLM_REASONING"], alias: ["llm_reasoning"], secret: false,
        },
        /// Structured-output request shape: `auto`, `tools`, `functions`, `json`
        /// or `json_schema`.
        pub structured_output_mode: String = "auto".to_string() => {
            env: ["LLM_STRUCTURED_OUTPUT_MODE"], alias: ["llm_structured_output_mode"], secret: false,
        },
        /// Sampling temperature; unset sends none (Python's `model_fields_set` gate).
        pub temperature: Option<f64> = None => {
            env: ["LLM_TEMPERATURE"], alias: ["llm_temperature"], secret: false,
        },
        /// Stream responses.
        pub streaming: bool = false => {
            env: ["LLM_STREAMING"], alias: ["llm_streaming"], secret: false,
        },
        /// Completion-token ceiling sent with every request.
        pub max_completion_tokens: u32 = cognee_llm::OpenAIAdapter::DEFAULT_MAX_COMPLETION_TOKENS => {
            env: ["LLM_MAX_COMPLETION_TOKENS", "LLM_MAX_TOKENS"], alias: ["llm_max_completion_tokens"], secret: false,
        },
        /// Attempts at one structured-output call (first ask plus re-asks).
        pub max_retries: u32 = 3 => {
            env: ["LLM_MAX_RETRIES"], alias: ["llm_max_retries"], secret: false,
        },
        /// Minimum transport attempts for one HTTP request.
        pub network_retries: u32 = 2 => {
            env: ["LLM_NETWORK_RETRIES"], alias: ["llm_network_retries"], secret: false,
        },
        /// Minimum seconds a transient failure is retried for.
        pub min_retry_seconds: u32 = 240 => {
            env: ["LLM_MIN_RETRY_SECONDS"], alias: ["llm_min_retry_seconds"], secret: false,
        },
        /// Per-HTTP-request timeout in seconds.
        pub request_timeout_seconds: u32 = default_secs(cognee_llm::OpenAIAdapter::DEFAULT_REQUEST_TIMEOUT) => {
            env: ["LLM_REQUEST_TIMEOUT_SECONDS"], alias: ["llm_request_timeout_seconds"], secret: false,
        },
        /// TCP connect timeout in seconds.
        pub connect_timeout_seconds: u32 = default_secs(cognee_llm::OpenAIAdapter::DEFAULT_CONNECT_TIMEOUT) => {
            env: ["LLM_CONNECT_TIMEOUT_SECONDS"], alias: ["llm_connect_timeout_seconds"], secret: false,
        },
        /// Wall-clock ceiling on one structured-output call in seconds; 0 disables.
        pub request_deadline_seconds: u32 = default_secs(cognee_llm::OpenAIAdapter::DEFAULT_REQUEST_DEADLINE) => {
            env: ["LLM_REQUEST_DEADLINE_SECONDS"], alias: ["llm_request_deadline_seconds"], secret: false,
        },
        /// Extra parameters merged into every request (Python `llm_args`).
        pub args: Map<String, Value> = Map::new() => {
            env: ["LLM_ARGS"], alias: ["llm_args"], secret: false,
        },
        /// Pace dispatch at `rate_limit_requests` per `rate_limit_interval`.
        pub rate_limit_enabled: bool = false => {
            env: ["LLM_RATE_LIMIT_ENABLED"], alias: ["llm_rate_limit_enabled"], secret: false,
        },
        /// Requests per pacing interval.
        pub rate_limit_requests: u32 = 60 => {
            env: ["LLM_RATE_LIMIT_REQUESTS"], alias: ["llm_rate_limit_requests"], secret: false,
        },
        /// Pacing interval in seconds.
        pub rate_limit_interval: u32 = 60 => {
            env: ["LLM_RATE_LIMIT_INTERVAL"], alias: ["llm_rate_limit_interval"], secret: false,
        },
        /// Let provider overload switch pacing on by itself for a cooldown.
        pub auto_rate_limit: bool = true => {
            env: ["AUTO_RATE_LIMIT"], alias: ["auto_rate_limit"], secret: false,
        },
        /// Use the record/replay mock LLM instead of the provider.
        pub mock: bool = false => {
            env: ["MOCK_LLM"], alias: ["llm_mock"], secret: false,
        },
        /// Cassette replayed by the mock LLM.
        pub cassette: String = String::new() => {
            env: ["MOCK_LLM_CASSETTE"], alias: ["llm_cassette"], secret: false,
        },
        /// Record real responses to this cassette path.
        pub record_path: String = String::new() => {
            env: ["COGNEE_RECORD_LLM"], alias: ["llm_record_path"], secret: false,
        },
        /// Dump every request and response body to stderr.
        pub debug_requests: bool = false => {
            env: ["COGNEE_DEBUG_LLM_REQUEST"], alias: [], secret: false,
        },
        /// Fallback model (not used yet; see SDK-1070).
        pub fallback_model: String = String::new() => {
            env: [], alias: ["llm_fallback_model"], secret: false,
        },
        /// Fallback provider (not used yet; see SDK-1070).
        pub fallback_provider: String = String::new() => {
            env: [], alias: ["llm_fallback_provider"], secret: false,
        },
        /// Fallback endpoint (not used yet; see SDK-1070).
        pub fallback_endpoint: String = String::new() => {
            env: [], alias: ["llm_fallback_endpoint"], secret: false,
        },
        /// Fallback API key (not used yet; see SDK-1070).
        pub fallback_api_key: String = String::new() => {
            env: [], alias: ["llm_fallback_api_key"], secret: true,
        },
    }
}

const fn default_secs(d: std::time::Duration) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the adapter defaults are minutes, far below u32::MAX seconds"
    )]
    let secs = d.as_secs() as u32;
    secs
}

config_section! {
    /// The OpenAI-compatible LLM adapter (OpenAI, Ollama, Mistral, Gemini,
    /// custom endpoints, Azure).
    pub struct OpenAiCompatibleConfig in "llm.openai_compatible" {
        /// Model used for image description; unset uses `llm.model`.
        pub vision_model: Option<String> = None => {
            env: ["LLM_VISION_MODEL"], alias: ["llm_vision_model"], secret: false,
        },
        /// Model used for audio transcription; blank uses the default.
        pub transcription_model: String = cognee_llm::OpenAIAdapter::DEFAULT_TRANSCRIPTION_MODEL.to_string() => {
            env: ["TRANSCRIPTION_MODEL"], alias: ["transcription_model"], secret: false,
        },
    }
}

config_section! {
    /// The native Anthropic Messages adapter.
    pub struct AnthropicConfig in "llm.anthropic" {
        /// Base URL override. Deliberately not `llm.endpoint`, which defaults
        /// to the OpenAI-shaped `OPENAI_URL` alias.
        pub base_url: Option<String> = None => {
            env: ["ANTHROPIC_BASE_URL", "ANTHROPIC_API_BASE"], alias: ["anthropic_base_url"], secret: false,
        },
    }
}

impl AnthropicConfig {
    /// The base URL, trimmed; empty means unset.
    pub fn base_url(&self) -> Option<String> {
        self.base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

config_section! {
    /// AWS credentials and endpoints for the Bedrock LLM and embedding
    /// adapters. Each field falls back to the litellm/boto3 env var of the
    /// same name inside the adapter when unset here.
    pub struct AwsConfig in "aws" {
        /// Region. No env binding on purpose: filling it from `AWS_REGION_NAME`
        /// would push the region embedded in a model ARN below the env vars,
        /// inverting Python's precedence. The adapter applies the env rungs.
        pub region: Option<String> = None => {
            env: [], alias: ["aws_region_name"], secret: false,
        },
        /// Access key id.
        pub access_key_id: Option<String> = None => {
            env: ["AWS_ACCESS_KEY_ID"], alias: ["aws_access_key_id"], secret: true,
        },
        /// Secret access key.
        pub secret_access_key: Option<String> = None => {
            env: ["AWS_SECRET_ACCESS_KEY"], alias: ["aws_secret_access_key"], secret: true,
        },
        /// Session token.
        pub session_token: Option<String> = None => {
            env: ["AWS_SESSION_TOKEN"], alias: ["aws_session_token"], secret: true,
        },
        /// Profile name (`AWS_PROFILE_NAME`, not `AWS_PROFILE`).
        pub profile_name: Option<String> = None => {
            env: ["AWS_PROFILE_NAME"], alias: ["aws_profile_name"], secret: false,
        },
        /// IAM role ARN to assume.
        pub role_name: Option<String> = None => {
            env: ["AWS_ROLE_NAME"], alias: ["aws_role_name"], secret: false,
        },
        /// STS session name.
        pub session_name: Option<String> = None => {
            env: ["AWS_SESSION_NAME"], alias: ["aws_session_name"], secret: false,
        },
        /// Web identity token.
        pub web_identity_token: Option<String> = None => {
            env: ["AWS_WEB_IDENTITY_TOKEN"], alias: ["aws_web_identity_token"], secret: true,
        },
        /// STS endpoint.
        pub sts_endpoint: Option<String> = None => {
            env: ["AWS_STS_ENDPOINT"], alias: ["aws_sts_endpoint"], secret: false,
        },
        /// External id for role assumption.
        pub external_id: Option<String> = None => {
            env: ["AWS_EXTERNAL_ID"], alias: ["aws_external_id"], secret: false,
        },
        /// Bedrock runtime endpoint.
        pub bedrock_runtime_endpoint: Option<String> = None => {
            env: ["AWS_BEDROCK_RUNTIME_ENDPOINT"], alias: ["aws_bedrock_runtime_endpoint"], secret: false,
        },
        /// Bedrock API-key bearer token.
        pub bearer_token: Option<String> = None => {
            env: ["AWS_BEARER_TOKEN_BEDROCK"], alias: ["aws_bearer_token_bedrock"], secret: true,
        },
    }
}

impl AwsConfig {
    /// The adapter inputs: each value trimmed, an empty one treated as unset.
    pub fn to_inputs(&self) -> AwsInputs {
        let clean = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        AwsInputs {
            region: clean(&self.region),
            access_key_id: clean(&self.access_key_id),
            secret_access_key: clean(&self.secret_access_key),
            session_token: clean(&self.session_token),
            profile_name: clean(&self.profile_name),
            role_name: clean(&self.role_name),
            session_name: clean(&self.session_name),
            web_identity_token: clean(&self.web_identity_token),
            sts_endpoint: clean(&self.sts_endpoint),
            external_id: clean(&self.external_id),
            bedrock_runtime_endpoint: clean(&self.bedrock_runtime_endpoint),
            bearer_token: clean(&self.bearer_token),
        }
    }
}

#[cfg(target_os = "android")]
fn default_embedding() -> (&'static str, &'static str, u32) {
    ("onnx", "BGE-Small-v1.5", 384)
}
#[cfg(not(target_os = "android"))]
fn default_embedding() -> (&'static str, &'static str, u32) {
    ("openai", "text-embedding-3-small", 1536)
}

config_section! {
    /// Settings shared by every embedding provider. Registered by the
    /// embedding kind. The default is local ONNX (BGE-Small) on Android and
    /// OpenAI `text-embedding-3-small` elsewhere, matching the Python SDK.
    pub struct EmbeddingConfig in "embedding" {
        /// Provider id selecting the embedding factory (`openai`,
        /// `openai_compatible`, `ollama`, `onnx`/`fastembed`, `bedrock`, `mock`).
        pub provider: String = default_embedding().0.to_string() => {
            env: ["EMBEDDING_PROVIDER"], alias: ["embedding_provider"], secret: false,
        },
        /// Model name.
        pub model: String = default_embedding().1.to_string() => {
            env: ["EMBEDDING_MODEL"], alias: ["embedding_model_name", "embedding_model"], secret: false,
        },
        /// Vector dimensions (must match the model).
        pub dimensions: u32 = default_embedding().2 => {
            env: ["EMBEDDING_DIMENSIONS"], alias: ["embedding_dimensions"], secret: false,
        },
        /// Endpoint; unset inherits `llm.endpoint` unless only the embedder is
        /// Bedrock.
        pub endpoint: String = String::new() => {
            env: ["EMBEDDING_ENDPOINT"], alias: ["embedding_endpoint"], secret: false,
        },
        /// API key; unset inherits `llm.api_key` unless only the embedder is
        /// Bedrock.
        pub api_key: String = String::new() => {
            env: ["EMBEDDING_API_KEY", "LLM_API_KEY"], alias: ["embedding_api_key"], secret: true,
        },
        /// Texts per embedding request.
        pub batch_size: u32 = 36 => {
            env: ["EMBEDDING_BATCH_SIZE"], alias: ["embedding_batch_size"], secret: false,
        },
        /// Maximum input tokens per text for the local ONNX provider
        /// (`onnx`/`fastembed`): it truncates input and is the embedding term
        /// of cognify's auto chunk size. Remote providers ignore it and use
        /// `embedding.max_completion_tokens` instead.
        pub max_sequence_length: u32 = 512 => {
            env: ["EMBEDDING_MAX_SEQUENCE_LENGTH"], alias: ["embedding_max_sequence_length"], secret: false,
        },
        /// Input-token limit of the remote embedding API (`openai`,
        /// `openai_compatible`, `ollama`, `bedrock`); for those providers it is
        /// also the embedding term of cognify's auto chunk size.
        pub max_completion_tokens: u32 = 8191 => {
            env: ["EMBEDDING_MAX_COMPLETION_TOKENS"], alias: ["embedding_max_completion_tokens"], secret: false,
        },
        /// Pace dispatch at `rate_limit_requests` per `rate_limit_interval`.
        pub rate_limit_enabled: bool = false => {
            env: ["EMBEDDING_RATE_LIMIT_ENABLED"], alias: ["embedding_rate_limit_enabled"], secret: false,
        },
        /// Requests per pacing interval.
        pub rate_limit_requests: u32 = 60 => {
            env: ["EMBEDDING_RATE_LIMIT_REQUESTS"], alias: ["embedding_rate_limit_requests"], secret: false,
        },
        /// Pacing interval in seconds.
        pub rate_limit_interval: u32 = 60 => {
            env: ["EMBEDDING_RATE_LIMIT_INTERVAL"], alias: ["embedding_rate_limit_interval"], secret: false,
        },
        /// Mock embedder: `zero` gives all-zero vectors; `true`, `1`, `yes`,
        /// `deterministic` or `hash` give deterministic hash vectors; anything
        /// else (or unset) uses the real provider.
        pub mock: String = String::new() => {
            env: ["MOCK_EMBEDDING"], alias: ["mock_embedding"], secret: false,
        },
    }
}

impl EmbeddingConfig {
    /// The mock mode, or `None` for the real provider.
    pub fn mock_mode(&self) -> Option<cognee_embedding::MockVectorMode> {
        match self.mock.trim().to_lowercase().as_str() {
            "zero" => Some(cognee_embedding::MockVectorMode::Zero),
            "true" | "1" | "yes" | "deterministic" | "hash" => {
                Some(cognee_embedding::MockVectorMode::Deterministic)
            }
            _ => None,
        }
    }
}

config_section! {
    /// The local ONNX (fastembed) embedding provider (`onnx` feature).
    pub struct OnnxEmbeddingConfig in "embedding.onnx" {
        /// Model file.
        pub model_path: String = "./target/models/BGE-Small-v1.5-model_quantized.onnx".to_string() => {
            env: ["EMBEDDING_MODEL_PATH", "COGNEE_E2E_EMBED_MODEL_PATH"], alias: ["embedding_model_path"], secret: false,
        },
        /// Tokenizer file.
        pub tokenizer_path: String = "./target/models/bge-small-tokenizer.json".to_string() => {
            env: ["EMBEDDING_TOKENIZER_PATH", "COGNEE_E2E_TOKENIZER_PATH"], alias: ["embedding_tokenizer_path"], secret: false,
        },
        /// Inference batch size (leading tensor dimension), independent of
        /// `embedding.batch_size`.
        pub batch_size: u32 = 32 => {
            env: ["EMBEDDING_ONNX_BATCH_SIZE"], alias: ["embedding_onnx_batch_size"], secret: false,
        },
    }
}

config_section! {
    /// The OpenAI-compatible embedding provider (OpenAI, Azure, vLLM, TEI, …).
    pub struct OpenAiCompatibleEmbeddingConfig in "embedding.openai_compatible" {
        /// API version (Azure `api-version`).
        pub api_version: Option<String> = None => {
            env: ["EMBEDDING_API_VERSION"], alias: ["embedding_api_version"], secret: false,
        },
    }
}

config_section! {
    /// The Ollama embedding provider.
    pub struct OllamaEmbeddingConfig in "embedding.ollama" {
        /// Hugging Face tokenizer id used to count tokens for this model.
        pub huggingface_tokenizer: Option<String> = None => {
            env: ["HUGGINGFACE_TOKENIZER"], alias: ["huggingface_tokenizer"], secret: false,
        },
    }
}

/// Trim an optional string; empty means unset.
pub(crate) fn non_empty(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The default vector backend: `lancedb`, except on an Android build without
/// the `lancedb` feature, whose effective backend has always been in-memory
/// brute-force.
///
/// `lancedb` is kept as the literal default so existing configs boot without
/// edits. It is not a fallback: the registry ERRORS when no factory is
/// registered under the provider id, naming the feature to rebuild with, so a
/// consumer that drops the `lancedb` feature (default-on in `cognee` and every
/// binding) must set `vector.provider` explicitly. Production deployments
/// should set `pgvector` (with `vector.url`) for durable storage.
///
/// Android is the one first-party profile that drops `lancedb`
/// (`cognee/android-default`: the Arrow + lance native stack does not
/// cross-compile). Its effective backend has always been brute-force anyway —
/// when the feature IS present, `LanceDbFactory::build` falls back to
/// brute-force on Android — so naming brute-force here keeps that behaviour
/// and stops a stock Android build from erroring on a provider whose factory
/// was never registered.
///
/// This covers only the *default*. An explicit `lancedb` still arrives from a
/// persisted config.json or `VECTOR_DB_PROVIDER`; that is absorbed by
/// `ANDROID_LANCEDB_FALLBACK` in `ComponentRegistry::resolve_vector_key`. Keep
/// the two in step.
fn default_vector_provider() -> String {
    if cfg!(all(target_os = "android", not(feature = "lancedb"))) {
        "brute-force".to_string()
    } else {
        "lancedb".to_string()
    }
}

config_section! {
    /// Settings shared by every vector database backend. Registered by the
    /// vector kind.
    pub struct VectorConfig in "vector" {
        /// Backend id selecting the vector factory (`lancedb`, `pgvector`,
        /// `brute-force`, …). `lancedb` is kept as the default so existing
        /// configs boot unchanged; it errors, rather than falling back, when its
        /// feature is missing (except on Android, see `ANDROID_LANCEDB_FALLBACK`).
        pub provider: String = default_vector_provider() => {
            env: ["VECTOR_DB_PROVIDER"], alias: ["vector_db_provider"], secret: false,
        },
        /// URL or path; empty derives `{system_root}/vectors`.
        pub url: String = String::new() => {
            env: ["VECTOR_DB_URL"], alias: ["vector_db_url"], secret: false,
        },
        /// Port.
        pub port: u16 = 1234 => {
            env: ["VECTOR_DB_PORT"], alias: ["vector_db_port"], secret: false,
        },
        /// Database name.
        pub name: String = String::new() => {
            env: ["VECTOR_DB_NAME"], alias: ["vector_db_name"], secret: false,
        },
        /// API key.
        pub key: String = String::new() => {
            env: ["VECTOR_DB_KEY"], alias: ["vector_db_key"], secret: true,
        },
        /// Username.
        pub username: String = String::new() => {
            env: ["VECTOR_DB_USERNAME"], alias: ["vector_db_username"], secret: false,
        },
        /// Password.
        pub password: String = String::new() => {
            env: ["VECTOR_DB_PASSWORD"], alias: ["vector_db_password"], secret: true,
        },
        /// Host.
        pub host: String = String::new() => {
            env: ["VECTOR_DB_HOST"], alias: ["vector_db_host"], secret: false,
        },
    }
}

config_section! {
    /// Settings shared by every graph database backend. Registered by the
    /// graph kind.
    pub struct GraphConfig in "graph" {
        /// Backend id selecting the graph factory. The default is derived from
        /// the compiled backends (see `default_graph_provider`).
        pub provider: String = crate::default_graph_provider().to_string() => {
            env: ["GRAPH_DATABASE_PROVIDER"], alias: ["graph_database_provider"], secret: false,
        },
        /// URL.
        pub url: String = String::new() => {
            env: ["GRAPH_DATABASE_URL"], alias: ["graph_database_url"], secret: false,
        },
        /// Database name.
        pub name: String = String::new() => {
            env: ["GRAPH_DATABASE_NAME"], alias: ["graph_database_name"], secret: false,
        },
        /// Username.
        pub username: String = String::new() => {
            env: ["GRAPH_DATABASE_USERNAME"], alias: ["graph_database_username"], secret: false,
        },
        /// Password.
        pub password: String = String::new() => {
            env: ["GRAPH_DATABASE_PASSWORD"], alias: ["graph_database_password"], secret: true,
        },
        /// Port.
        pub port: u16 = 123 => {
            env: ["GRAPH_DATABASE_PORT"], alias: ["graph_database_port"], secret: false,
        },
        /// Host.
        pub host: String = String::new() => {
            env: ["GRAPH_DATABASE_HOST"], alias: ["graph_database_host"], secret: false,
        },
        /// API key.
        pub key: String = String::new() => {
            env: ["GRAPH_DATABASE_KEY"], alias: ["graph_database_key"], secret: true,
        },
        /// Embedded graph file path; empty derives `{system_root}/graph`.
        pub file_path: String = String::new() => {
            env: ["GRAPH_FILE_PATH"], alias: ["graph_file_path"], secret: false,
        },
        /// Graph file name (not used yet; see SDK-1070).
        pub filename: String = String::new() => {
            env: [], alias: ["graph_filename"], secret: false,
        },
    }
}

config_section! {
    /// File storage. Registered by the storage kind.
    pub struct StorageConfig in "storage" {
        /// Backend id selecting the storage factory (`local`; object stores
        /// such as S3 come from an external factory).
        pub backend: String = "local".to_string() => {
            env: ["STORAGE_BACKEND"], alias: ["storage_backend"], secret: false,
        },
        /// Bucket for object-store backends. No OSS backend reads it.
        pub bucket_name: String = String::new() => {
            env: ["STORAGE_BUCKET_NAME"], alias: ["storage_bucket_name"], secret: false,
        },
    }
}

config_section! {
    /// Ontology grounding of extracted entities. Registered by the ontology
    /// kind.
    pub struct OntologyConfig in "ontology" {
        /// Ontology file(s) (`.owl`, `.ttl`, `.rdf`, …); empty disables
        /// ontology grounding (the no-op resolver).
        pub file_path: String = String::new() => {
            env: ["ONTOLOGY_FILE_PATH"], alias: ["ontology_file_path"], secret: false,
        },
        /// Resolver id selecting the ontology factory (`rdflib`).
        pub resolver: String = "rdflib".to_string() => {
            env: ["ONTOLOGY_RESOLVER"], alias: ["ontology_resolver"], secret: false,
        },
        /// Entity-name matching strategy (`fuzzy`).
        pub matching_strategy: String = "fuzzy".to_string() => {
            env: ["ONTOLOGY_MATCHING_STRATEGY"], alias: ["ontology_matching_strategy"], secret: false,
        },
    }
}

config_section! {
    /// Graph extraction during cognify. Registered by the graph-extraction
    /// kind.
    pub struct GraphExtractionConfig in "graph_extraction" {
        /// The default extractor kind: `llm` (the built-in extractor) or a
        /// registered backend such as `gliner`. A per-call `graphBackend`
        /// selection overrides it.
        pub backend: String = "llm".to_string() => {
            env: ["GRAPH_EXTRACTOR"], alias: ["graph_extractor"], secret: false,
        },
    }
}

config_section! {
    /// The relational database. Registered by the relational kind.
    pub struct RelationalConfig in "relational" {
        /// Backend id selecting the relational factory (`sqlite`, `postgres`).
        /// For `postgres` the URL is built from the connection parts below;
        /// otherwise `url` is used verbatim.
        pub provider: String = "sqlite".to_string() => {
            env: ["DB_PROVIDER"], alias: ["db_provider"], secret: false,
        },
        /// Connection URL (`sqlite:…`, `postgres://…`) when not built from parts.
        pub url: String = "sqlite:./cognee.db?mode=rwc".to_string() => {
            env: ["DATABASE_URL"], alias: ["relational_db_url"], secret: false,
        },
        /// Source database URL for `cognee-cli migrate`.
        pub migration_url: String = String::new() => {
            env: [], alias: ["migration_db_url"], secret: false,
        },
        /// Server host (`postgres`).
        pub host: String = "localhost".to_string() => {
            env: ["DB_HOST"], alias: ["db_host"], secret: false,
        },
        /// Server port (`postgres`).
        pub port: u16 = 5432 => {
            env: ["DB_PORT"], alias: ["db_port"], secret: false,
        },
        /// Database name (`postgres`).
        pub name: String = "cognee_db".to_string() => {
            env: ["DB_NAME"], alias: ["db_name"], secret: false,
        },
        /// User name (`postgres`).
        pub username: String = String::new() => {
            env: ["DB_USERNAME"], alias: ["db_username"], secret: false,
        },
        /// Password (`postgres`).
        pub password: String = String::new() => {
            env: ["DB_PASSWORD"], alias: ["db_password"], secret: true,
        },
    }
}
