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
