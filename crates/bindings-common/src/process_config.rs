//! Process configuration for the language bindings: logging, OpenTelemetry
//! export and product analytics.
//!
//! Each of these configures a **process-wide** singleton (the `tracing`
//! subscriber, the OTLP exporter, the analytics dispatcher), so it is resolved
//! once per process and is not part of a handle's `Settings`. The bindings
//! initialise logging and OTLP from module-level functions that run before any
//! handle exists; this module is the one path they all share, so every binding
//! resolves these settings the same way:
//!
//! **defaults < environment < explicit options passed from code**
//!
//! Options are a JSON object keyed by the same names as the matching
//! `cognee::Settings` keys where one exists (`log_level`,
//! `logs_root_directory`, `cognee_tracing_enabled`, `otel_*`), so a settings
//! object can be passed through. camelCase spellings (`logLevel`,
//! `otelServiceName`) are accepted too, for the TypeScript and Java bindings.
//! String, boolean and number values are accepted; `null` entries are ignored.
//!
//! Product analytics need no options here: the bindings call
//! [`cognee::telemetry::set_enabled`] / [`cognee::telemetry::is_enabled`]
//! directly.

use cognee_logging::LoggingConfig;
use cognee_observability::EnvSettingsView;
use serde_json::Value;

/// Parse an optional JSON argument handed over a binding boundary (a JNI
/// string, a C `const char*`): empty, all-whitespace and `null` mean "none".
///
/// # Errors
///
/// The text is not valid JSON.
pub fn parse_optional_json(raw: &str) -> Result<Option<Value>, serde_json::Error> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "null" {
        return Ok(None);
    }
    serde_json::from_str(raw).map(Some)
}

/// The resolved logging configuration for a binding's `setup_logging`:
/// [`LoggingConfig::from_env`] (the process-config boundary) overlaid with
/// `options`.
///
/// # Errors
///
/// A malformed environment variable, a non-object `options`, an unknown key or
/// an invalid value, as a message suitable for the binding's exception.
pub fn logging_config(options: Option<&Value>) -> Result<LoggingConfig, String> {
    let cfg = LoggingConfig::from_env().map_err(|e| format!("invalid logging config: {e}"))?;
    apply_logging_options(cfg, options)
}

/// Overlay `options` onto `cfg` (see the module docs for the accepted shape).
///
/// # Errors
///
/// As [`logging_config`], minus the environment.
pub fn apply_logging_options(
    mut cfg: LoggingConfig,
    options: Option<&Value>,
) -> Result<LoggingConfig, String> {
    for (key, value) in entries(options)? {
        cfg.set(&key, &value)
            .map_err(|e| format!("invalid logging option: {e}"))?;
    }
    Ok(cfg)
}

/// The resolved OpenTelemetry settings for a binding's OTLP setup:
/// `default_service_name` (the binding's own, e.g. `cognee.python-binding`),
/// overlaid with the environment (the process-config boundary), overlaid with
/// `options`.
///
/// The binding default is a *default*: `OTEL_SERVICE_NAME` and an explicit
/// `otel_service_name` option both win over it. The process environment is
/// never modified.
///
/// # Errors
///
/// A non-object `options` or an unknown key, as a message suitable for the
/// binding's exception.
pub fn otel_settings(
    default_service_name: &str,
    options: Option<&Value>,
) -> Result<EnvSettingsView, String> {
    let view = EnvSettingsView::from_env_over(
        EnvSettingsView::default().with_service_name(default_service_name),
    );
    apply_otel_options(view, options)
}

/// Overlay `options` onto `view` (see the module docs for the accepted shape).
///
/// # Errors
///
/// As [`otel_settings`].
pub fn apply_otel_options(
    mut view: EnvSettingsView,
    options: Option<&Value>,
) -> Result<EnvSettingsView, String> {
    for (key, value) in entries(options)? {
        view.set(&key, &value)
            .map_err(|e| format!("invalid OpenTelemetry option: {e}"))?;
    }
    Ok(view)
}

/// Flatten an options object into `(snake_case key, string value)` pairs.
fn entries(options: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let map = match options {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(map)) => map,
        Some(other) => {
            return Err(format!(
                "options must be an object (or null), got {}",
                type_name(other)
            ));
        }
    };
    let mut out = Vec::with_capacity(map.len());
    for (key, value) in map {
        let value = match value {
            Value::Null => continue,
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            other => {
                return Err(format!(
                    "option {key:?} must be a string, boolean or number, got {}",
                    type_name(other)
                ));
            }
        };
        out.push((to_snake_case(key), value));
    }
    Ok(out)
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// `logsRootDirectory` -> `logs_root_directory`; snake_case passes through.
fn to_snake_case(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use cognee_config_core::MapEnv;
    use cognee_observability::SettingsView;
    use serde_json::json;

    #[test]
    fn logging_options_win_over_env_in_either_spelling() {
        let env = MapEnv::new()
            .with("LOG_LEVEL", "warn")
            .with("COGNEE_LOGS_DIR", "/env");
        let cfg = LoggingConfig::from_env_source(&env).expect("parses");
        let cfg = apply_logging_options(
            cfg,
            Some(&json!({
                "log_level": "debug",
                "logsRootDirectory": "/code",
                "logFile": false,
                "log_backup_count": 3,
                "log_format": null,
            })),
        )
        .expect("valid options");
        assert_eq!(cfg.level_filter.as_deref(), Some("debug"));
        assert_eq!(
            cfg.logs_dir_override.as_deref(),
            Some(std::path::Path::new("/code"))
        );
        assert!(!cfg.file_enabled);
        assert_eq!(cfg.backup_count, 3);
        assert_eq!(cfg.format, cognee_logging::LogFormat::Plain, "null ignored");
    }

    #[test]
    fn logging_options_reject_unknown_keys_and_bad_shapes() {
        let cfg = LoggingConfig::defaults();
        let err = apply_logging_options(cfg.clone(), Some(&json!({"nope": 1}))).unwrap_err();
        assert!(err.contains("unknown logging setting"), "{err}");
        let err = apply_logging_options(cfg.clone(), Some(&json!([1]))).unwrap_err();
        assert!(err.contains("must be an object"), "{err}");
        let err = apply_logging_options(cfg, Some(&json!({"log_level": ["x"]}))).unwrap_err();
        assert!(err.contains("must be a string"), "{err}");
    }

    /// Binding default < `OTEL_SERVICE_NAME` < explicit option.
    #[test]
    fn otel_service_name_precedence() {
        let base = EnvSettingsView::default().with_service_name("cognee.test-binding");

        let view = apply_otel_options(base.clone().overlay(&MapEnv::new()), None).expect("ok");
        assert_eq!(view.service_name(), "cognee.test-binding");

        let env = MapEnv::new().with("OTEL_SERVICE_NAME", "from-env");
        let view = apply_otel_options(base.clone().overlay(&env), None).expect("ok");
        assert_eq!(view.service_name(), "from-env");

        let view = apply_otel_options(
            base.overlay(&env),
            Some(&json!({
                "otelServiceName": "from-code",
                "cognee_tracing_enabled": true,
                "otel_exporter_otlp_endpoint": "http://127.0.0.1:4317",
            })),
        )
        .expect("ok");
        assert_eq!(view.service_name(), "from-code");
        assert!(view.tracing_enabled());
        assert_eq!(view.otlp_endpoint(), "http://127.0.0.1:4317");
    }

    #[test]
    fn otel_options_reject_unknown_keys() {
        let err = apply_otel_options(EnvSettingsView::default(), Some(&json!({"otel_x": "1"})))
            .unwrap_err();
        assert!(err.contains("unknown OpenTelemetry setting"), "{err}");
    }

    #[test]
    fn optional_json_treats_blank_and_null_as_none() {
        for raw in ["", "  ", "null", " null "] {
            assert_eq!(parse_optional_json(raw).expect("valid"), None, "{raw:?}");
        }
        assert_eq!(
            parse_optional_json(r#"{"a":1}"#).expect("valid"),
            Some(json!({"a": 1}))
        );
        assert!(parse_optional_json("{").is_err());
    }

    #[test]
    fn camel_case_keys_are_normalised() {
        assert_eq!(to_snake_case("logsRootDirectory"), "logs_root_directory");
        assert_eq!(to_snake_case("log_level"), "log_level");
        assert_eq!(
            to_snake_case("cogneeTracingEnabled"),
            "cognee_tracing_enabled"
        );
    }
}
