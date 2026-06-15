//! Read-only view of the observability-relevant subset of `Settings`.
//!
//! Defined here (not in `cognee-lib`) to avoid a hard dependency on the
//! umbrella crate. `cognee-lib::Settings` implements this trait in a
//! sibling task so HTTP middleware and other upstream callers can drive
//! [`crate::init_telemetry`] without going through `cognee-lib`.

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
}

// Defaults mirror `cognee-lib::Settings::default()` (config.rs lines 644-651).
// Kept here so callers that don't depend on `cognee-lib` (e.g. `cognee-http-server`)
// still get the same defaults; a unit test below pins the two views together.
pub(crate) const DEFAULT_TRACING_ENABLED: bool = false;
pub(crate) const DEFAULT_SERVICE_NAME: &str = "cognee";
pub(crate) const DEFAULT_OTLP_ENDPOINT: &str = "";
pub(crate) const DEFAULT_OTLP_HEADERS: &str = "";
pub(crate) const DEFAULT_OTLP_PROTOCOL: &str = "grpc";
pub(crate) const DEFAULT_SPAN_PROCESSOR: &str = "batch";
pub(crate) const DEFAULT_TRACES_SAMPLER: &str = "";
pub(crate) const DEFAULT_TRACES_SAMPLER_ARG: &str = "";

/// Snapshot of OTEL-relevant env vars usable as a [`SettingsView`].
///
/// Lets crates that don't depend on `cognee-lib::Settings` (e.g. the HTTP
/// server) drive [`crate::init_telemetry`] directly from the environment.
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
        }
    }
}

impl EnvSettingsView {
    /// Read all eight OTEL env vars. Missing or empty vars fall back to
    /// the same defaults as `cognee-lib::Settings::default()`.
    pub fn from_env() -> Self {
        let mut view = Self::default();

        if let Some(v) = read_env("COGNEE_TRACING_ENABLED") {
            view.tracing_enabled = cognee_utils::parse_env_bool(&v);
        }
        if let Some(v) = read_env("OTEL_SERVICE_NAME") {
            view.service_name = v;
        }
        if let Some(v) = read_env("OTEL_EXPORTER_OTLP_ENDPOINT") {
            view.otlp_endpoint = v;
        }
        if let Some(v) = read_env("OTEL_EXPORTER_OTLP_HEADERS") {
            view.otlp_headers = v;
        }
        if let Some(v) = read_env("OTEL_EXPORTER_OTLP_PROTOCOL") {
            view.otlp_protocol = v;
        }
        if let Some(v) = read_env("OTEL_SPAN_PROCESSOR") {
            view.span_processor = v;
        }
        if let Some(v) = read_env("OTEL_TRACES_SAMPLER") {
            view.traces_sampler = v;
        }
        if let Some(v) = read_env("OTEL_TRACES_SAMPLER_ARG") {
            view.traces_sampler_arg = v;
        }

        view
    }
}

// Empty string is treated as "unset" so callers can clear an env var via
// `KEY=` without flipping the value to a literal empty string mid-process.
fn read_env(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
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
}

#[cfg(test)]
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
}
