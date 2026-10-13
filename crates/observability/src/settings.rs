//! Read-only view of the observability-relevant subset of `Settings`.
//!
//! Defined here (not in `cognee`) to avoid a hard dependency on the
//! umbrella crate. `cognee::Settings` implements this trait in a
//! sibling task so HTTP middleware and other upstream callers can drive
//! [`crate::init_telemetry`] without going through `cognee`.

use cognee_config_core::EnvSource;

/// Borrow-only adapter over the OTEL fields of cognee `Settings`.
///
/// All accessors return `&str` / `bool` so implementations can avoid
/// cloning. `Send + Sync` so the trait object can travel across async
/// task boundaries.
pub trait SettingsView: Send + Sync {
    /// Mirrors `Settings.cognee_tracing_enabled`.
    fn tracing_enabled(&self) -> bool;
    /// Mirrors `Settings.otel_service_name`.
    fn service_name(&self) -> &str;
    /// Mirrors `Settings.otel_exporter_otlp_endpoint`.
    fn otlp_endpoint(&self) -> &str;
    /// Mirrors `Settings.otel_exporter_otlp_headers`.
    fn otlp_headers(&self) -> &str;
    /// Mirrors `Settings.otel_exporter_otlp_protocol`.
    fn otlp_protocol(&self) -> &str;
    /// Mirrors `Settings.otel_span_processor`.
    fn span_processor(&self) -> &str;
    /// Mirrors `Settings.otel_traces_sampler`.
    fn traces_sampler(&self) -> &str;
    /// Mirrors `Settings.otel_traces_sampler_arg`.
    fn traces_sampler_arg(&self) -> &str;
    /// Mirrors `Settings.otel_deployment_environment`: the
    /// `deployment.environment.name` resource attribute (env: `ENV`).
    ///
    /// Provided so implementors outside this workspace keep compiling; they
    /// report [`DEFAULT_DEPLOYMENT_ENVIRONMENT`] unless they override it.
    fn deployment_environment(&self) -> &str {
        DEFAULT_DEPLOYMENT_ENVIRONMENT
    }
}

/// Default `deployment.environment.name` when `ENV` is unset.
pub const DEFAULT_DEPLOYMENT_ENVIRONMENT: &str = "development";

/// The settings keys [`EnvSettingsView::set`] accepts — the same names as the
/// matching `cognee::Settings` fields.
pub const OTEL_SETTING_KEYS: &[&str] = &[
    "cognee_tracing_enabled",
    "otel_service_name",
    "otel_exporter_otlp_endpoint",
    "otel_exporter_otlp_headers",
    "otel_exporter_otlp_protocol",
    "otel_span_processor",
    "otel_traces_sampler",
    "otel_traces_sampler_arg",
    "otel_deployment_environment",
];

/// [`EnvSettingsView::set`] was given a key outside [`OTEL_SETTING_KEYS`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown OpenTelemetry setting {0:?} (expected one of: {keys})", keys = OTEL_SETTING_KEYS.join(", "))]
pub struct UnknownOtelSetting(pub String);

// Defaults mirror `cognee::Settings::default()` (config.rs lines 644-651).
// Kept here so callers that don't depend on `cognee` still get the same
// defaults; a unit test below pins the two views together. The original such
// caller, `cognee-http-server`, is not part of this repository and depends
// on this crate from outside this workspace.
pub(crate) const DEFAULT_TRACING_ENABLED: bool = false;
pub(crate) const DEFAULT_SERVICE_NAME: &str = "cognee";
pub(crate) const DEFAULT_OTLP_ENDPOINT: &str = "";
pub(crate) const DEFAULT_OTLP_HEADERS: &str = "";
pub(crate) const DEFAULT_OTLP_PROTOCOL: &str = "grpc";
pub(crate) const DEFAULT_SPAN_PROCESSOR: &str = "batch";
pub(crate) const DEFAULT_TRACES_SAMPLER: &str = "";
pub(crate) const DEFAULT_TRACES_SAMPLER_ARG: &str = "";

/// Snapshot of OTEL-relevant settings usable as a [`SettingsView`]: the
/// process-config counterpart of the `otel_*` fields of `cognee::Settings`.
///
/// Lets callers that initialise tracing before any `Settings` exists (the
/// language bindings, an HTTP server) drive [`crate::init_telemetry`]:
/// defaults, overlaid with the environment ([`EnvSettingsView::from_env`]),
/// overlaid with explicit values from code ([`EnvSettingsView::set`]).
#[derive(Debug, Clone)]
pub struct EnvSettingsView {
    tracing_enabled: bool,
    service_name: String,
    otlp_endpoint: String,
    otlp_headers: String,
    otlp_protocol: String,
    span_processor: String,
    traces_sampler: String,
    traces_sampler_arg: String,
    deployment_environment: String,
}

impl Default for EnvSettingsView {
    fn default() -> Self {
        Self {
            tracing_enabled: DEFAULT_TRACING_ENABLED,
            service_name: DEFAULT_SERVICE_NAME.to_string(),
            otlp_endpoint: DEFAULT_OTLP_ENDPOINT.to_string(),
            otlp_headers: DEFAULT_OTLP_HEADERS.to_string(),
            otlp_protocol: DEFAULT_OTLP_PROTOCOL.to_string(),
            span_processor: DEFAULT_SPAN_PROCESSOR.to_string(),
            traces_sampler: DEFAULT_TRACES_SAMPLER.to_string(),
            traces_sampler_arg: DEFAULT_TRACES_SAMPLER_ARG.to_string(),
            deployment_environment: DEFAULT_DEPLOYMENT_ENVIRONMENT.to_string(),
        }
    }
}

impl EnvSettingsView {
    /// Read the OTEL env vars (`COGNEE_TRACING_ENABLED`, `OTEL_*`, `ENV`).
    /// Missing or empty vars fall back to the same defaults as
    /// `cognee::Settings::default()`.
    ///
    /// This is a configuration boundary: it is where tracing setup opens the
    /// process environment. [`EnvSettingsView::from_env_source`] reads the same
    /// variables from any [`EnvSource`].
    pub fn from_env() -> Self {
        Self::from_env_over(Self::default())
    }

    /// [`EnvSettingsView::from_env`] over caller-chosen defaults, e.g. a
    /// binding's own default service name:
    /// `EnvSettingsView::from_env_over(EnvSettingsView::default().with_service_name("cognee.python-binding"))`.
    /// A variable that is set wins over the default; an unset one keeps it.
    #[allow(
        clippy::disallowed_methods,
        reason = "config boundary: the OTel settings are resolved from the process env here"
    )]
    pub fn from_env_over(defaults: Self) -> Self {
        defaults.overlay(&cognee_config_core::ProcessEnv::new())
    }

    /// [`EnvSettingsView::from_env`], reading from `env` instead of the
    /// process environment.
    pub fn from_env_source(env: &dyn EnvSource) -> Self {
        Self::default().overlay(env)
    }

    /// Overlay the variables set in `env` onto `self`.
    #[must_use]
    pub fn overlay(mut self, env: &dyn EnvSource) -> Self {
        if let Some(v) = read_env(env, "COGNEE_TRACING_ENABLED") {
            self.tracing_enabled = cognee_utils::parse_env_bool(&v);
        }
        for (var, field) in [
            ("OTEL_SERVICE_NAME", &mut self.service_name),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &mut self.otlp_endpoint),
            ("OTEL_EXPORTER_OTLP_HEADERS", &mut self.otlp_headers),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", &mut self.otlp_protocol),
            ("OTEL_SPAN_PROCESSOR", &mut self.span_processor),
            ("OTEL_TRACES_SAMPLER", &mut self.traces_sampler),
            ("OTEL_TRACES_SAMPLER_ARG", &mut self.traces_sampler_arg),
            ("ENV", &mut self.deployment_environment),
        ] {
            if let Some(v) = read_env(env, var) {
                *field = v;
            }
        }
        self
    }

    /// Replace the default service name (`cognee`). Meant to be applied to
    /// the defaults, before the environment, so `OTEL_SERVICE_NAME` still wins.
    #[must_use]
    pub fn with_service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = name.into();
        self
    }

    /// Set one setting from code, by its `cognee::Settings` key (see
    /// [`OTEL_SETTING_KEYS`]). `cognee_tracing_enabled` takes a boolean
    /// spelled `true|1|yes|on` / anything else; every other key takes the
    /// value verbatim. Wins over the environment.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), UnknownOtelSetting> {
        let field = match key {
            "cognee_tracing_enabled" => {
                self.tracing_enabled = cognee_utils::parse_env_bool(value);
                return Ok(());
            }
            "otel_service_name" => &mut self.service_name,
            "otel_exporter_otlp_endpoint" => &mut self.otlp_endpoint,
            "otel_exporter_otlp_headers" => &mut self.otlp_headers,
            "otel_exporter_otlp_protocol" => &mut self.otlp_protocol,
            "otel_span_processor" => &mut self.span_processor,
            "otel_traces_sampler" => &mut self.traces_sampler,
            "otel_traces_sampler_arg" => &mut self.traces_sampler_arg,
            "otel_deployment_environment" => &mut self.deployment_environment,
            other => return Err(UnknownOtelSetting(other.to_string())),
        };
        *field = value.to_string();
        Ok(())
    }
}

// Empty string is treated as "unset" so callers can clear an env var via
// `KEY=` without flipping the value to a literal empty string mid-process.
fn read_env(env: &dyn EnvSource, key: &str) -> Option<String> {
    env.get(key).filter(|v| !v.is_empty())
}

impl SettingsView for EnvSettingsView {
    fn tracing_enabled(&self) -> bool {
        self.tracing_enabled
    }
    fn service_name(&self) -> &str {
        &self.service_name
    }
    fn otlp_endpoint(&self) -> &str {
        &self.otlp_endpoint
    }
    fn otlp_headers(&self) -> &str {
        &self.otlp_headers
    }
    fn otlp_protocol(&self) -> &str {
        &self.otlp_protocol
    }
    fn span_processor(&self) -> &str {
        &self.span_processor
    }
    fn traces_sampler(&self) -> &str {
        &self.traces_sampler
    }
    fn traces_sampler_arg(&self) -> &str {
        &self.traces_sampler_arg
    }
    fn deployment_environment(&self) -> &str {
        &self.deployment_environment
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
#[allow(
    clippy::disallowed_methods,
    reason = "tests drive configuration through the process env"
)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env-mutating tests share process state; serialize them to avoid
    // cross-test interference.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_otel_env() {
        for key in [
            "COGNEE_TRACING_ENABLED",
            "OTEL_SERVICE_NAME",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "OTEL_EXPORTER_OTLP_PROTOCOL",
            "OTEL_SPAN_PROCESSOR",
            "OTEL_TRACES_SAMPLER",
            "OTEL_TRACES_SAMPLER_ARG",
            "ENV",
        ] {
            // Safety: tests serialize via ENV_LOCK; no other thread reads
            // the env concurrently here.
            unsafe { std::env::remove_var(key) };
        }
    }

    #[test]
    fn from_env_empty_matches_defaults() {
        let _g = ENV_LOCK.lock().expect("env lock poisoned");
        clear_otel_env();

        let view = EnvSettingsView::from_env();
        assert!(!view.tracing_enabled());
        assert_eq!(view.service_name(), DEFAULT_SERVICE_NAME);
        assert_eq!(view.otlp_endpoint(), DEFAULT_OTLP_ENDPOINT);
        assert_eq!(view.otlp_headers(), DEFAULT_OTLP_HEADERS);
        assert_eq!(view.otlp_protocol(), DEFAULT_OTLP_PROTOCOL);
        assert_eq!(view.span_processor(), DEFAULT_SPAN_PROCESSOR);
        assert_eq!(view.traces_sampler(), DEFAULT_TRACES_SAMPLER);
        assert_eq!(view.traces_sampler_arg(), DEFAULT_TRACES_SAMPLER_ARG);
        assert_eq!(
            view.deployment_environment(),
            DEFAULT_DEPLOYMENT_ENVIRONMENT
        );
    }

    #[test]
    fn tracing_enabled_truthy_values() {
        let _g = ENV_LOCK.lock().expect("env lock poisoned");
        for v in ["true", "TRUE", "1", "yes", "YES"] {
            clear_otel_env();
            unsafe { std::env::set_var("COGNEE_TRACING_ENABLED", v) };
            let view = EnvSettingsView::from_env();
            assert!(view.tracing_enabled(), "{v} should enable tracing");
        }
    }

    #[test]
    fn tracing_enabled_falsy_values() {
        let _g = ENV_LOCK.lock().expect("env lock poisoned");
        for v in ["false", "0", "no", "off", "anything-else"] {
            clear_otel_env();
            unsafe { std::env::set_var("COGNEE_TRACING_ENABLED", v) };
            let view = EnvSettingsView::from_env();
            assert!(!view.tracing_enabled(), "{v} should not enable tracing");
        }
    }

    #[test]
    fn from_env_reads_all_fields() {
        let _g = ENV_LOCK.lock().expect("env lock poisoned");
        clear_otel_env();
        unsafe {
            std::env::set_var("COGNEE_TRACING_ENABLED", "true");
            std::env::set_var("OTEL_SERVICE_NAME", "my-svc");
            std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4317");
            std::env::set_var("OTEL_EXPORTER_OTLP_HEADERS", "x-key=val");
            std::env::set_var("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf");
            std::env::set_var("OTEL_SPAN_PROCESSOR", "simple");
            std::env::set_var("OTEL_TRACES_SAMPLER", "traceidratio");
            std::env::set_var("OTEL_TRACES_SAMPLER_ARG", "0.5");
        }

        let view = EnvSettingsView::from_env();
        assert!(view.tracing_enabled());
        assert_eq!(view.service_name(), "my-svc");
        assert_eq!(view.otlp_endpoint(), "http://collector:4317");
        assert_eq!(view.otlp_headers(), "x-key=val");
        assert_eq!(view.otlp_protocol(), "http/protobuf");
        assert_eq!(view.span_processor(), "simple");
        assert_eq!(view.traces_sampler(), "traceidratio");
        assert_eq!(view.traces_sampler_arg(), "0.5");

        clear_otel_env();
    }

    /// `from_env_source` reads only the source it is given, so it needs no
    /// process-env mutation and no `#[serial]`.
    #[test]
    fn from_env_source_reads_only_the_given_source() {
        let env = cognee_config_core::MapEnv::new()
            .with("COGNEE_TRACING_ENABLED", "yes")
            .with("OTEL_SERVICE_NAME", "svc")
            .with("OTEL_EXPORTER_OTLP_ENDPOINT", "");
        let view = EnvSettingsView::from_env_source(&env);
        assert!(view.tracing_enabled());
        assert_eq!(view.service_name(), "svc");
        assert_eq!(
            view.otlp_endpoint(),
            EnvSettingsView::default().otlp_endpoint(),
            "empty means unset"
        );
    }

    #[test]
    fn env_feeds_deployment_environment() {
        let env = cognee_config_core::MapEnv::new().with("ENV", "staging");
        assert_eq!(
            EnvSettingsView::from_env_source(&env).deployment_environment(),
            "staging"
        );
    }

    /// Precedence: caller default < environment < explicit `set`.
    #[test]
    fn default_service_name_env_and_set_precedence() {
        let base = EnvSettingsView::default().with_service_name("cognee.test-binding");
        let unset = base.clone().overlay(&cognee_config_core::MapEnv::new());
        assert_eq!(unset.service_name(), "cognee.test-binding");

        let env = cognee_config_core::MapEnv::new().with("OTEL_SERVICE_NAME", "from-env");
        let mut view = base.overlay(&env);
        assert_eq!(view.service_name(), "from-env");

        view.set("otel_service_name", "from-code")
            .expect("known key");
        view.set("cognee_tracing_enabled", "true")
            .expect("known key");
        view.set("otel_deployment_environment", "prod")
            .expect("known key");
        assert_eq!(view.service_name(), "from-code");
        assert!(view.tracing_enabled());
        assert_eq!(view.deployment_environment(), "prod");
        assert_eq!(
            view.set("otel_nope", "x"),
            Err(UnknownOtelSetting("otel_nope".into()))
        );
    }
}
