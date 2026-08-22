//! Explicit APEX environment configuration for capture and graph writes.

use std::fmt;
use std::path::PathBuf;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(feature = "engine")]
use crate::embedding_generation::{EmbeddingFingerprint, EmbeddingGeneration};
use crate::layout::StateLayout;
use crate::limits::ResourceLimits;
use crate::secret::SecretString;

pub trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ModelConfig {
    pub provider: String,
    pub endpoint: String,
    pub model: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct EmbeddingConfig {
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    pub dimensions: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentConfig {
    pub layout: StateLayout,
    pub dataset: String,
    pub llm: ModelConfig,
    pub embedding: Option<EmbeddingConfig>,
    pub limits: ResourceLimits,
    pub allow_forget_all: bool,
    proxy_key: SecretString,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("HOME is required when APEX_COGNEE_ROOT is unset")]
    MissingHome,
    #[error("{0} must be a positive integer in range")]
    InvalidPositiveInteger(&'static str),
    #[error("{0} must be true or false")]
    InvalidBoolean(&'static str),
    #[error("{0} is required before graph writes")]
    MissingGraphSetting(&'static str),
    #[error("embedding generation fingerprint does not match the requested configuration")]
    EmbeddingGenerationMismatch,
    #[error("embedding generation belongs to a different private root")]
    EmbeddingGenerationRootMismatch,
}

impl AgentConfig {
    pub fn from_env(env: &impl EnvSource) -> Result<Self, ConfigError> {
        let root = match nonempty(env.get("APEX_COGNEE_ROOT")) {
            Some(root) => PathBuf::from(root),
            None => {
                let home = nonempty(env.get("HOME")).ok_or(ConfigError::MissingHome)?;
                PathBuf::from(home).join(".apex/cognee")
            }
        };
        let dataset =
            nonempty(env.get("APEX_COGNEE_DATASET")).unwrap_or_else(|| "agent_sessions".to_owned());
        let llm = ModelConfig {
            provider: env_value(env, "APEX_COGNEE_LLM_PROVIDER"),
            endpoint: env_value(env, "APEX_COGNEE_LLM_ENDPOINT"),
            model: env_value(env, "APEX_COGNEE_LLM_MODEL"),
        };

        let embedding_provider = env_value(env, "APEX_COGNEE_EMBEDDING_PROVIDER");
        let embedding_endpoint = env_value(env, "APEX_COGNEE_EMBEDDING_ENDPOINT");
        let embedding_model = env_value(env, "APEX_COGNEE_EMBEDDING_MODEL");
        let embedding_dimensions = read_optional_u32(env, "APEX_COGNEE_EMBEDDING_DIMENSIONS")?;
        let embedding = if embedding_provider.is_empty()
            && embedding_endpoint.is_empty()
            && embedding_model.is_empty()
            && embedding_dimensions.is_none()
        {
            None
        } else {
            Some(EmbeddingConfig {
                provider: embedding_provider,
                endpoint: embedding_endpoint,
                model: embedding_model,
                dimensions: embedding_dimensions.unwrap_or(0),
            })
        };

        let proxy_key = nonempty(env.get("APEX_COGNEE_PROXY_KEY"))
            .or_else(|| nonempty(env.get("APEX_LLM_PROXY_KEY")))
            .unwrap_or_default();
        let allow_forget_all = read_bool(env, "APEX_COGNEE_ALLOW_FORGET_ALL", false)?;

        Ok(Self {
            layout: StateLayout::under(root),
            dataset,
            llm,
            embedding,
            limits: ResourceLimits::from_env(env)?,
            allow_forget_all,
            proxy_key: SecretString::new(proxy_key),
        })
    }

    pub fn proxy_key(&self) -> &SecretString {
        &self.proxy_key
    }

    #[cfg(feature = "engine")]
    pub fn cognee_settings(
        &self,
        generation: &EmbeddingGeneration,
    ) -> Result<cognee::config::Settings, ConfigError> {
        require(&self.llm.provider, "APEX_COGNEE_LLM_PROVIDER")?;
        require(&self.llm.endpoint, "APEX_COGNEE_LLM_ENDPOINT")?;
        require(&self.llm.model, "APEX_COGNEE_LLM_MODEL")?;
        if self.proxy_key.is_empty() {
            return Err(ConfigError::MissingGraphSetting(
                "APEX_COGNEE_PROXY_KEY or APEX_LLM_PROXY_KEY",
            ));
        }
        let embedding = self
            .embedding
            .as_ref()
            .ok_or(ConfigError::MissingGraphSetting(
                "APEX_COGNEE_EMBEDDING_PROVIDER",
            ))?;
        require(&embedding.provider, "APEX_COGNEE_EMBEDDING_PROVIDER")?;
        require(&embedding.endpoint, "APEX_COGNEE_EMBEDDING_ENDPOINT")?;
        require(&embedding.model, "APEX_COGNEE_EMBEDDING_MODEL")?;
        if embedding.dimensions == 0 {
            return Err(ConfigError::MissingGraphSetting(
                "APEX_COGNEE_EMBEDDING_DIMENSIONS",
            ));
        }
        if generation.fingerprint() != &EmbeddingFingerprint::from_config(embedding) {
            return Err(ConfigError::EmbeddingGenerationMismatch);
        }
        if generation.private_root() != self.layout.root.as_path() {
            return Err(ConfigError::EmbeddingGenerationRootMismatch);
        }

        let layout = &self.layout;
        Ok(cognee::config::Settings {
            system_root_directory: layout.system.display().to_string(),
            data_root_directory: generation.data().display().to_string(),
            cache_root_directory: layout.cache.display().to_string(),
            logs_root_directory: layout.status.join("logs").display().to_string(),
            db_provider: "sqlite".into(),
            relational_db_url: generation.sqlite_url(),
            vector_db_provider: "lancedb".into(),
            vector_db_url: generation.vector().display().to_string(),
            graph_database_provider: "ladybug".into(),
            graph_file_path: generation.graph().display().to_string(),
            cache_backend: "seaorm".into(),
            default_dataset_name: self.dataset.clone(),
            llm_provider: self.llm.provider.clone(),
            llm_model: self.llm.model.clone(),
            llm_endpoint: self.llm.endpoint.clone(),
            llm_api_key: self.proxy_key.expose().to_owned(),
            user_agent: apex_user_agent(),
            llm_max_parallel_requests: 1,
            llm_max_retries: 0,
            embedding_provider: embedding.provider.clone(),
            embedding_model_name: embedding.model.clone(),
            embedding_dimensions: embedding.dimensions,
            embedding_endpoint: embedding.endpoint.clone(),
            embedding_api_key: self.proxy_key.expose().to_owned(),
            embedding_batch_size: self.limits.embedding_batch_size,
            ..Default::default()
        })
    }
}

#[cfg(feature = "engine")]
pub(crate) fn apex_user_agent() -> String {
    format!(
        "Apex/{} ({}; {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

impl fmt::Debug for ModelConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelConfig")
            .field("provider", &self.provider)
            .field("endpoint", &endpoint_class(&self.endpoint))
            .field("model", &self.model)
            .finish()
    }
}

impl Serialize for ModelConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("ModelConfig", 3)?;
        state.serialize_field("provider", &self.provider)?;
        state.serialize_field("endpoint", &endpoint_class(&self.endpoint))?;
        state.serialize_field("model", &self.model)?;
        state.end()
    }
}

impl fmt::Debug for EmbeddingConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmbeddingConfig")
            .field("provider", &self.provider)
            .field("endpoint", &endpoint_class(&self.endpoint))
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

impl Serialize for EmbeddingConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("EmbeddingConfig", 4)?;
        state.serialize_field("provider", &self.provider)?;
        state.serialize_field("endpoint", &endpoint_class(&self.endpoint))?;
        state.serialize_field("model", &self.model)?;
        state.serialize_field("dimensions", &self.dimensions)?;
        state.end()
    }
}

pub(crate) fn endpoint_class(endpoint: &str) -> String {
    let Ok(parsed) = url::Url::parse(endpoint) else {
        return opaque_endpoint_class(endpoint);
    };
    if parsed.host_str().is_none() {
        return opaque_endpoint_class(endpoint);
    }
    parsed.origin().ascii_serialization()
}

fn opaque_endpoint_class(endpoint: &str) -> String {
    let digest = Sha256::digest(endpoint.as_bytes());
    format!("opaque:{}", lowercase_hex(&digest))
}

pub(crate) fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|candidate| !candidate.is_empty())
}

fn env_value(env: &impl EnvSource, key: &str) -> String {
    nonempty(env.get(key)).unwrap_or_default()
}

fn read_optional_u32(env: &impl EnvSource, key: &'static str) -> Result<Option<u32>, ConfigError> {
    let Some(value) = env.get(key) else {
        return Ok(None);
    };
    value
        .parse::<u32>()
        .map(Some)
        .map_err(|_| ConfigError::InvalidPositiveInteger(key))
}

fn read_bool(env: &impl EnvSource, key: &'static str, default: bool) -> Result<bool, ConfigError> {
    match env.get(key).as_deref() {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(ConfigError::InvalidBoolean(key)),
    }
}

#[cfg(feature = "engine")]
fn require(value: &str, key: &'static str) -> Result<(), ConfigError> {
    if value.is_empty() {
        Err(ConfigError::MissingGraphSetting(key))
    } else {
        Ok(())
    }
}
