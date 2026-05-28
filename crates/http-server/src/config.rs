//! HTTP server configuration.
//!
//! `HttpServerConfig` holds all tuneable parameters.  `from_env()` reads the
//! documented environment variables and overlays them on the struct defaults.
//! Only the standalone binary calls `from_env()`; library embedders construct
//! `HttpServerConfig` directly.

use std::{str::FromStr, time::Duration};

use secrecy::SecretString;

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
    /// Whether to require authentication on API routes.
    /// Env: `REQUIRE_AUTHENTICATION`. Default: `true`.
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
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 8000,
            cors_allowed_origins: Vec::new(),
            ui_app_url: "http://localhost:3000".into(),
            env: Environment::Prod,
            require_authentication: true,
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

        Ok(cfg)
    }
}

impl HttpServerConfig {
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
mod tests {
    use super::*;

    #[test]
    fn test_defaults() {
        let cfg = HttpServerConfig::default();
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 8000);
        assert_eq!(cfg.ui_app_url, "http://localhost:3000");
        assert_eq!(cfg.body_limit, 100 * 1024 * 1024);
        assert_eq!(cfg.jwt_lifetime, Duration::from_secs(3600));
        assert!(cfg.require_authentication);
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
}
