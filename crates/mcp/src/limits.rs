//! Per-engineer resource limits for transient workers.

use serde::{Deserialize, Serialize};

use crate::config::{ConfigError, EnvSource};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub engine_owners: u32,
    pub llm_lanes: u32,
    pub drain_timeout_seconds: u32,
    pub llm_timeout_seconds: u32,
    pub embedding_timeout_seconds: u32,
    pub max_llm_calls: u32,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub embedding_batch_size: u32,
    pub max_events_per_drain: u32,
    pub improve_every: u32,
    pub lease_stale_seconds: u32,
    pub max_attempts: u32,
    pub spool_high_water_bytes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            engine_owners: 1,
            llm_lanes: 1,
            drain_timeout_seconds: 120,
            llm_timeout_seconds: 45,
            embedding_timeout_seconds: 30,
            max_llm_calls: 8,
            max_input_tokens: 48_000,
            max_output_tokens: 8_000,
            embedding_batch_size: 64,
            max_events_per_drain: 50,
            improve_every: 20,
            lease_stale_seconds: 180,
            max_attempts: 5,
            spool_high_water_bytes: 512 * 1024 * 1024,
        }
    }
}

impl ResourceLimits {
    pub(crate) fn from_env(env: &impl EnvSource) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        Ok(Self {
            engine_owners: read_u32(env, "APEX_COGNEE_ENGINE_OWNERS", defaults.engine_owners)?,
            llm_lanes: read_u32(env, "APEX_COGNEE_LLM_LANES", defaults.llm_lanes)?,
            drain_timeout_seconds: read_u32(
                env,
                "APEX_COGNEE_DRAIN_TIMEOUT_SECONDS",
                defaults.drain_timeout_seconds,
            )?,
            llm_timeout_seconds: read_u32(
                env,
                "APEX_COGNEE_LLM_TIMEOUT_SECONDS",
                defaults.llm_timeout_seconds,
            )?,
            embedding_timeout_seconds: read_u32(
                env,
                "APEX_COGNEE_EMBEDDING_TIMEOUT_SECONDS",
                defaults.embedding_timeout_seconds,
            )?,
            max_llm_calls: read_u32(env, "APEX_COGNEE_MAX_LLM_CALLS", defaults.max_llm_calls)?,
            max_input_tokens: read_u32(
                env,
                "APEX_COGNEE_MAX_INPUT_TOKENS",
                defaults.max_input_tokens,
            )?,
            max_output_tokens: read_u32(
                env,
                "APEX_COGNEE_MAX_OUTPUT_TOKENS",
                defaults.max_output_tokens,
            )?,
            embedding_batch_size: read_u32(
                env,
                "APEX_COGNEE_EMBEDDING_BATCH_SIZE",
                defaults.embedding_batch_size,
            )?,
            max_events_per_drain: read_u32(
                env,
                "APEX_COGNEE_MAX_EVENTS_PER_DRAIN",
                defaults.max_events_per_drain,
            )?,
            improve_every: read_u32(env, "APEX_COGNEE_IMPROVE_EVERY", defaults.improve_every)?,
            lease_stale_seconds: read_u32(
                env,
                "APEX_COGNEE_LEASE_STALE_SECONDS",
                defaults.lease_stale_seconds,
            )?,
            max_attempts: read_u32(env, "APEX_COGNEE_MAX_ATTEMPTS", defaults.max_attempts)?,
            spool_high_water_bytes: read_u64(
                env,
                "APEX_COGNEE_SPOOL_HIGH_WATER_BYTES",
                defaults.spool_high_water_bytes,
            )?,
        })
    }
}

fn read_u32(env: &impl EnvSource, key: &'static str, default: u32) -> Result<u32, ConfigError> {
    let Some(value) = env.get(key) else {
        return Ok(default);
    };
    value
        .parse::<u32>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .ok_or(ConfigError::InvalidPositiveInteger(key))
}

fn read_u64(env: &impl EnvSource, key: &'static str, default: u64) -> Result<u64, ConfigError> {
    let Some(value) = env.get(key) else {
        return Ok(default);
    };
    value
        .parse::<u64>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .ok_or(ConfigError::InvalidPositiveInteger(key))
}
