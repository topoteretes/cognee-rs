//! Process-wide configuration of product analytics ([`TelemetryConfig`]).
//!
//! Product analytics configure a process-wide singleton (the dispatcher behind
//! [`crate::send_telemetry`]), so their settings are **process config**, not
//! per-handle `Settings`: one [`TelemetryConfig`] governs every event the
//! process sends, whichever SDK handle or code path emits it.
//!
//! # Precedence
//!
//! 1. **Defaults** — [`TelemetryConfig::default`]: analytics **on** (Python SDK
//!    parity, opt-out).
//! 2. **Process environment** — resolved once, by [`TelemetryConfig::from_env`],
//!    the first time the process configuration is needed (the first event, a
//!    binding's import-time arming, or an [`is_enabled`] call).
//! 3. **Explicit code** — [`configure`] replaces the whole configuration and
//!    [`set_enabled`] flips the master switch. Either one wins over the
//!    environment, and stays in force until the next explicit call.
//!
//! Because the environment is read once, changing `TELEMETRY_DISABLED` after the
//! first event has no effect; call [`set_enabled`] instead.
//!
//! # Environment variables (read only by [`TelemetryConfig::from_env_source`])
//!
//! | Var | Field | Default |
//! |---|---|---|
//! | `TELEMETRY_DISABLED` | [`enabled`](TelemetryConfig::enabled) = `false` when set to any non-empty value | unset |
//! | `ENV` | [`enabled`](TelemetryConfig::enabled) = `false` when `test` or `dev` | unset |
//! | `COGNEE_HOST_SDK` | [`host_sdk`](TelemetryConfig::host_sdk) | unset |
//! | `TELEMETRY_REQUEST_TIMEOUT` | [`request_timeout_secs`](TelemetryConfig::request_timeout_secs) | `5` |
//! | `TRACKING_ID` | [`tracking_id`](TelemetryConfig::tracking_id) | unset |
//! | `TELEMETRY_API_KEY_TRACKING_SALT` | [`api_key_tracking_salt`](TelemetryConfig::api_key_tracking_salt) | well-known default |
//! | `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS` | [`endpoint_override`](TelemetryConfig::endpoint_override), honoured only while `COGNEE_TELEMETRY_INTEGRATION_TEST` is non-empty | unset |
//!
//! The LLM API key hashed into `api_key_tracking_id` is **not** read from the
//! environment: it is the resolved LLM key, registered with
//! [`crate::set_llm_api_key`] (cognee's `ComponentManager` does that for every
//! handle), so a key set from code or from the CLI config store is the one
//! reported.

use std::sync::{OnceLock, RwLock};

use cognee_config_core::EnvSource;

/// Default HTTP timeout for one analytics POST, in seconds.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 5;

/// The collector analytics are posted to (Python SDK parity, so cross-SDK
/// identity grouping works).
pub const DEFAULT_ENDPOINT: &str = "https://test.prometh.ai";

/// Process-wide product-analytics configuration.
///
/// Build one with [`TelemetryConfig::default`] or [`TelemetryConfig::from_env`],
/// adjust the fields, and install it with [`configure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryConfig {
    /// Master switch. `true` by default (opt-out, matching the Python SDK).
    ///
    /// Env: `false` when `TELEMETRY_DISABLED` is non-empty or `ENV` is `test` /
    /// `dev`.
    pub enabled: bool,
    /// Name of a host SDK that embeds this one and sends its own analytics
    /// (`COGNEE_HOST_SDK`). When set, events from a **language binding** (one
    /// that called [`crate::env::arm_binding_emission`]) are suppressed so the
    /// same operation is not counted twice. Pure-Rust embedders (the CLI) are not
    /// affected.
    pub host_sdk: Option<String>,
    /// Total HTTP timeout for one POST, in seconds (`TELEMETRY_REQUEST_TIMEOUT`).
    /// Clamped to `[1, 60]` when used.
    pub request_timeout_secs: u64,
    /// Fixed `anonymous_id` (`TRACKING_ID`), instead of the project-local
    /// `.anon_id` file.
    pub tracking_id: Option<String>,
    /// PBKDF2 salt for `api_key_tracking_id` (`TELEMETRY_API_KEY_TRACKING_SALT`).
    /// `None` uses the well-known default shared with the Python SDK.
    pub api_key_tracking_salt: Option<String>,
    /// Collector URL override, for tests and integration harnesses. `None` posts
    /// to [`DEFAULT_ENDPOINT`].
    ///
    /// Env: `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS`, honoured only while
    /// `COGNEE_TELEMETRY_INTEGRATION_TEST` is non-empty, so a stray variable in a
    /// production environment cannot redirect analytics.
    pub endpoint_override: Option<String>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host_sdk: None,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
            tracking_id: None,
            api_key_tracking_salt: None,
            endpoint_override: None,
        }
    }
}

impl TelemetryConfig {
    /// Resolve the configuration from the process environment.
    ///
    /// This is a configuration boundary: it is where product analytics open the
    /// process environment. [`TelemetryConfig::from_env_source`] reads the same
    /// variables from any [`EnvSource`].
    #[allow(
        clippy::disallowed_methods,
        reason = "config boundary: product-analytics configuration is resolved from the process env here"
    )]
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_source(&cognee_config_core::ProcessEnv::new())
    }

    /// [`TelemetryConfig::from_env`], reading from `env` instead of the process
    /// environment. Empty values are treated as unset.
    #[must_use]
    pub fn from_env_source(env: &dyn EnvSource) -> Self {
        let read = |key: &str| env.get(key).filter(|v| !v.is_empty());

        // Python utils.py: `if os.getenv("TELEMETRY_DISABLED")` (any non-empty
        // value, even "false") and `ENV in ("test", "dev")`.
        let disabled = read("TELEMETRY_DISABLED").is_some()
            || matches!(read("ENV").as_deref(), Some("test" | "dev"));

        let request_timeout_secs = read("TELEMETRY_REQUEST_TIMEOUT")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS);

        // The override is a test hook: unit tests of this crate may use it
        // freely; anything else must also opt in with the integration flag.
        let override_allowed = cfg!(test) || read("COGNEE_TELEMETRY_INTEGRATION_TEST").is_some();
        let endpoint_override = if override_allowed {
            read("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS")
        } else {
            None
        };

        Self {
            enabled: !disabled,
            host_sdk: read("COGNEE_HOST_SDK"),
            request_timeout_secs,
            tracking_id: read("TRACKING_ID"),
            api_key_tracking_salt: read("TELEMETRY_API_KEY_TRACKING_SALT"),
            endpoint_override,
        }
    }

    /// The request timeout actually used, clamped to `[1, 60]` seconds.
    ///
    /// The clamp is a hardening choice: the non-runtime fallback dispatch is
    /// synchronous, and an upper bound keeps a misconfiguration from blocking
    /// shutdown indefinitely.
    #[must_use]
    pub fn effective_request_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.request_timeout_secs.clamp(1, 60))
    }

    /// The collector URL events are posted to.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        self.endpoint_override
            .as_deref()
            .unwrap_or(DEFAULT_ENDPOINT)
    }
}

static STATE: OnceLock<RwLock<TelemetryConfig>> = OnceLock::new();

/// The process-wide slot, resolved from the environment on first use.
fn state() -> &'static RwLock<TelemetryConfig> {
    STATE.get_or_init(|| RwLock::new(TelemetryConfig::from_env()))
}

/// Replace the process-wide analytics configuration. Wins over the environment.
///
/// Applies to every event sent afterwards, including events already queued but
/// not yet posted: a POST checks the switch again right before it leaves the
/// process.
#[allow(
    clippy::unwrap_used,
    reason = "RwLock poisoning only follows a panic while holding the lock; no meaningful recovery"
)]
pub fn configure(config: TelemetryConfig) {
    if let Err(rejected) = STATE.set(RwLock::new(config)) {
        // Already initialised: overwrite in place.
        let config = rejected
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // lock poison is unrecoverable
        *state().write().unwrap() = config;
    }
}

/// Turn product analytics on or off for the whole process. Wins over the
/// environment (`TELEMETRY_DISABLED`, `ENV`); every other setting is kept.
///
/// Disabling also drops events that are queued but not yet posted.
#[allow(
    clippy::unwrap_used,
    reason = "RwLock poisoning only follows a panic while holding the lock; no meaningful recovery"
)]
pub fn set_enabled(enabled: bool) {
    // lock poison is unrecoverable
    state().write().unwrap().enabled = enabled;
}

/// A snapshot of the process-wide analytics configuration.
#[allow(
    clippy::unwrap_used,
    reason = "RwLock poisoning only follows a panic while holding the lock; no meaningful recovery"
)]
#[must_use]
pub fn current_config() -> TelemetryConfig {
    // lock poison is unrecoverable
    state().read().unwrap().clone()
}

/// Whether an event sent now would actually be posted.
///
/// `true` only when all of the following hold:
/// * the `telemetry` cargo feature is compiled in;
/// * [`TelemetryConfig::enabled`] is set;
/// * no host SDK suppresses this process's binding emissions (see
///   [`TelemetryConfig::host_sdk`]).
#[allow(
    clippy::unwrap_used,
    reason = "RwLock poisoning only follows a panic while holding the lock; no meaningful recovery"
)]
#[must_use]
pub fn is_enabled() -> bool {
    if !cfg!(feature = "telemetry") {
        return false;
    }
    // lock poison is unrecoverable
    let config = state().read().unwrap();
    config.enabled && !(crate::env::is_binding_armed() && config.host_sdk.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_config_core::MapEnv;

    #[test]
    fn defaults_are_opt_out() {
        let cfg = TelemetryConfig::from_env_source(&MapEnv::new());
        assert_eq!(cfg, TelemetryConfig::default());
        assert!(cfg.enabled);
        assert_eq!(cfg.endpoint(), DEFAULT_ENDPOINT);
        assert_eq!(cfg.effective_request_timeout().as_secs(), 5);
    }

    #[test]
    fn telemetry_disabled_any_non_empty_value_disables() {
        for v in ["1", "true", "false", "yes"] {
            let cfg =
                TelemetryConfig::from_env_source(&MapEnv::new().with("TELEMETRY_DISABLED", v));
            assert!(!cfg.enabled, "TELEMETRY_DISABLED={v:?}");
        }
        // Python's `if os.getenv(...)` treats empty as falsy.
        let cfg = TelemetryConfig::from_env_source(&MapEnv::new().with("TELEMETRY_DISABLED", ""));
        assert!(cfg.enabled);
    }

    #[test]
    fn env_test_and_dev_disable() {
        for (v, enabled) in [("test", false), ("dev", false), ("production", true)] {
            let cfg = TelemetryConfig::from_env_source(&MapEnv::new().with("ENV", v));
            assert_eq!(cfg.enabled, enabled, "ENV={v}");
        }
    }

    #[test]
    fn timeout_parses_and_clamps() {
        for (raw, secs) in [("0", 1), ("120", 60), ("10", 10), ("junk", 5)] {
            let cfg = TelemetryConfig::from_env_source(
                &MapEnv::new().with("TELEMETRY_REQUEST_TIMEOUT", raw),
            );
            assert_eq!(cfg.effective_request_timeout().as_secs(), secs, "{raw}");
        }
    }

    #[test]
    fn identity_and_host_overrides_are_read() {
        let cfg = TelemetryConfig::from_env_source(
            &MapEnv::new()
                .with("TRACKING_ID", "fixed")
                .with("TELEMETRY_API_KEY_TRACKING_SALT", "salt")
                .with("COGNEE_HOST_SDK", "python"),
        );
        assert_eq!(cfg.tracking_id.as_deref(), Some("fixed"));
        assert_eq!(cfg.api_key_tracking_salt.as_deref(), Some("salt"));
        assert_eq!(cfg.host_sdk.as_deref(), Some("python"));
    }

    #[test]
    fn endpoint_override_is_read_in_unit_tests() {
        let cfg = TelemetryConfig::from_env_source(
            &MapEnv::new().with("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS", "http://127.0.0.1:1"),
        );
        assert_eq!(cfg.endpoint(), "http://127.0.0.1:1");
    }
}
