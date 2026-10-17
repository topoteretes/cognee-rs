//! The `Settings` key registry: one row per setting.
//!
//! Every surface that addresses a setting by name goes through this table:
//!
//! * the env binding, [`Settings::overlay_from`],
//! * the generic setter, [`ConfigManager::set`](super::ConfigManager::set),
//!   which the Python, C, TypeScript and Java bindings call,
//! * the CLI's `config list | get | set | unset`.
//!
//! So a setting added here is settable from env, from code and from the CLI
//! at once, and the three cannot drift apart. Adding a setting is a field on
//! [`Settings`] (with its default) plus one row below; see
//! `docs/configuration.md` § "Adding a setting".
//!
//! A row names the field, its parse rules (`kind`), the env vars that bind it
//! in priority order (an alias is read only when every earlier variable is
//! unset or empty), any alternate setting keys, and whether the value is a
//! secret.

use serde_json::Value;

use std::sync::OnceLock;

use cognee_config_core::ConfigSchema;

use super::{ConfigError, EnvSource, Settings, sslmode_from_connect_args};

/// One setting: its key, how it is read from env and JSON, and how it is
/// shown and reset.
pub struct KeySpec {
    /// The setting key: the `Settings` field name, and its serde name.
    pub key: &'static str,
    /// Alternate keys accepted by the setters (e.g. `embedding_model`).
    pub key_aliases: &'static [&'static str],
    /// Env vars that bind this setting, in priority order. Empty when the
    /// setting has no env binding.
    pub env: &'static [&'static str],
    /// Whether the value is a credential that must not be shown unmasked.
    pub secret: bool,
    /// Whether the value is part of `Settings`' serialized form, and so
    /// survives a save/load of a config file. `false` only for the two
    /// `#[serde(skip)]` fields.
    pub persisted: bool,
    pub(super) overlay: fn(&mut Settings, &dyn EnvSource),
    pub(super) set: fn(&mut Settings, &str, Value) -> Result<(), ConfigError>,
    pub(super) get: fn(&Settings) -> Value,
    pub(super) reset: fn(&mut Settings, &Settings),
}

impl std::fmt::Debug for KeySpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeySpec")
            .field("key", &self.key)
            .field("key_aliases", &self.key_aliases)
            .field("env", &self.env)
            .field("secret", &self.secret)
            .field("persisted", &self.persisted)
            .finish_non_exhaustive()
    }
}

/// Every setting, in `Settings` field order.
pub fn keys() -> &'static [KeySpec] {
    KEYS
}

/// The row for `key`, matching the setting key or one of its aliases.
pub fn key_spec(key: &str) -> Option<&'static KeySpec> {
    KEYS.iter()
        .find(|spec| spec.key == key || spec.key_aliases.contains(&key))
}

// -- Per-kind parse rules -----------------------------------------------------
//
// These are the rules `overlay_from_env` and `ConfigManager::set` have always
// applied; `tests/config_key_golden.rs` pins them.

/// The first variable of `names` that is set to a non-empty value.
fn first_set(env: &dyn EnvSource, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| env.get(name).filter(|v| !v.is_empty()))
}

fn mismatch(key: &str, reason: &str) -> ConfigError {
    ConfigError::TypeMismatch {
        key: key.to_string(),
        reason: reason.to_string(),
    }
}

mod kind {
    use super::{ConfigError, EnvSource, Value, first_set, mismatch};

    pub(super) fn overlay_str(field: &mut String, env: &dyn EnvSource, names: &[&str]) {
        if let Some(v) = first_set(env, names) {
            *field = v;
        }
    }

    /// An integer: a value that does not parse leaves the field untouched,
    /// and does not fall through to a later alias.
    pub(super) fn overlay_parsed<T: std::str::FromStr>(
        field: &mut T,
        env: &dyn EnvSource,
        names: &[&str],
    ) {
        if let Some(n) = first_set(env, names).and_then(|v| v.parse::<T>().ok()) {
            *field = n;
        }
    }

    pub(super) fn overlay_opt_parsed<T: std::str::FromStr>(
        field: &mut Option<T>,
        env: &dyn EnvSource,
        names: &[&str],
    ) {
        if let Some(n) = first_set(env, names).and_then(|v| v.parse::<T>().ok()) {
            *field = Some(n);
        }
    }

    /// `true | 1 | yes | on` (trimmed, case-insensitive) is true; any other
    /// non-empty value is false.
    pub(super) fn overlay_bool(field: &mut bool, env: &dyn EnvSource, names: &[&str]) {
        if let Some(v) = first_set(env, names) {
            *field = cognee_utils::parse_env_bool(&v);
        }
    }

    pub(super) fn as_string(key: &str, value: &Value) -> Result<String, ConfigError> {
        value
            .as_str()
            .map(ToString::to_string)
            .ok_or_else(|| mismatch(key, "expected a string"))
    }

    #[allow(
        dead_code,
        reason = "used by the `u64` row kind; no row needs it today"
    )]
    pub(super) fn as_u64(key: &str, value: &Value) -> Result<u64, ConfigError> {
        value
            .as_u64()
            .ok_or_else(|| mismatch(key, "expected a positive integer (u64)"))
    }

    pub(super) fn as_u32(key: &str, value: &Value) -> Result<u32, ConfigError> {
        value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| mismatch(key, "expected a positive integer (u32)"))
    }

    #[allow(
        dead_code,
        reason = "used by the `opt_f64` row kind; no row needs it today"
    )]
    pub(super) fn as_f64(key: &str, value: &Value) -> Result<f64, ConfigError> {
        value
            .as_f64()
            .ok_or_else(|| mismatch(key, "expected a number"))
    }

    pub(super) fn as_bool(key: &str, value: &Value) -> Result<bool, ConfigError> {
        value
            .as_bool()
            .ok_or_else(|| mismatch(key, "expected a boolean"))
    }
}

/// One row per plain setting:
/// `row!(field: kind [secret], env ["PRIMARY", "ALIAS"…] [aliases ["key"…]])`.
/// `kind` picks the parse rules: `str`, `u32`, `u64`, `bool`,
/// `opt_u32` (JSON `null` clears it) and `opt_f64` (likewise).
macro_rules! row {
    (@overlay str, $f:ident) => {
        |s, env| kind::overlay_str(&mut s.$f, env, ROW_ENV)
    };
    (@overlay bool, $f:ident) => {
        |s, env| kind::overlay_bool(&mut s.$f, env, ROW_ENV)
    };
    (@overlay opt_u32, $f:ident) => {
        |s, env| kind::overlay_opt_parsed::<u32>(&mut s.$f, env, ROW_ENV)
    };
    (@overlay opt_f64, $f:ident) => {
        |s, env| kind::overlay_opt_parsed::<f64>(&mut s.$f, env, ROW_ENV)
    };
    (@overlay $int:ident, $f:ident) => {
        |s, env| kind::overlay_parsed::<$int>(&mut s.$f, env, ROW_ENV)
    };
    (@set str, $f:ident) => {
        |s, key, v| {
            s.$f = kind::as_string(key, &v)?;
            Ok(())
        }
    };
    (@set u32, $f:ident) => {
        |s, key, v| {
            s.$f = kind::as_u32(key, &v)?;
            Ok(())
        }
    };
    (@set u64, $f:ident) => {
        |s, key, v| {
            s.$f = kind::as_u64(key, &v)?;
            Ok(())
        }
    };
    (@set bool, $f:ident) => {
        |s, key, v| {
            s.$f = kind::as_bool(key, &v)?;
            Ok(())
        }
    };
    (@set opt_u32, $f:ident) => {
        |s, key, v| {
            s.$f = if v.is_null() {
                None
            } else {
                Some(kind::as_u32(key, &v)?)
            };
            Ok(())
        }
    };
    (@set opt_f64, $f:ident) => {
        |s, key, v| {
            s.$f = if v.is_null() {
                None
            } else {
                Some(kind::as_f64(key, &v)?)
            };
            Ok(())
        }
    };
    ($f:ident: $kind:ident $($secret:ident)?, env [$($env:literal),*] $(aliases [$($alias:literal),*])?) => {{
        const ROW_ENV: &[&str] = &[$($env),*];
        KeySpec {
            key: stringify!($f),
            key_aliases: &[$($($alias),*)?],
            env: ROW_ENV,
            secret: row!(@secret $($secret)?),
            persisted: true,
            overlay: row!(@overlay $kind, $f),
            set: row!(@set $kind, $f),
            get: |s| serde_json::to_value(&s.$f).unwrap_or(Value::Null),
            reset: |s, d| s.$f.clone_from(&d.$f),
        }
    }};
    (@secret) => {
        false
    };
    (@secret secret) => {
        true
    };
}

use kind::{as_bool, as_string};

/// The setting key of backend access control.
pub(super) const ACCESS_CONTROL_KEY: &str = "enable_access_control";
/// Its env var, the Python SDK's name.
const ACCESS_CONTROL_ENV: &[&str] = &["ENABLE_BACKEND_ACCESS_CONTROL"];

/// Why `enable_access_control = true` is refused when the application has not
/// declared that it provides access control.
const ACCESS_CONTROL_NOT_PROVIDED: &str = "cognee-rs does not enforce backend access \
     control from configuration: it applies only when the embedding application supplies \
     an AclDb (SearchBuilder/SearchOrchestrator::with_acl_db, the ingestion builder's \
     with_acl_db, DatasetManager::with_acl), and this application has not declared that \
     it does (ConfigManager::set_application_access_control). Unset \
     ENABLE_BACKEND_ACCESS_CONTROL / enable_access_control or set it to false";

/// The error for `enable_access_control = true` without an application that
/// provides access control.
pub(super) fn access_control_not_provided() -> ConfigError {
    ConfigError::Unsupported {
        key: ACCESS_CONTROL_KEY.to_string(),
        reason: ACCESS_CONTROL_NOT_PROVIDED.to_string(),
    }
}

/// The canonical `monitoring_tool`: `none` (also for an empty value), or why
/// the value is not supported. Python's `Observer` names `llmlite` and
/// `langsmith` too, but its `get_observe` rejects every tool but `none`;
/// tracing goes through OpenTelemetry instead (`COGNEE_TRACING_ENABLED`,
/// `OTEL_*`).
fn monitoring_tool(value: &str) -> Result<String, String> {
    let tool = value.trim().to_ascii_lowercase();
    if tool.is_empty() || tool == "none" {
        Ok("none".to_string())
    } else {
        Err(format!(
            "unsupported monitoring tool '{tool}': only `none` is supported; trace \
             through OpenTelemetry (COGNEE_TRACING_ENABLED, OTEL_EXPORTER_OTLP_ENDPOINT)"
        ))
    }
}

/// The registry. Field order follows `Settings`.
static KEYS: &[KeySpec] = &[
    // -- System paths, users & datasets -------------------------------------
    row!(default_user_id: str, env ["COGNEE_DEFAULT_USER_ID"]),
    row!(default_dataset_name: str, env ["COGNEE_DEFAULT_DATASET_NAME"]),
    row!(system_root_directory: str, env ["COGNEE_SYSTEM_ROOT_DIRECTORY"]),
    row!(data_root_directory: str, env ["COGNEE_DATA_ROOT_DIRECTORY"]),
    row!(cache_root_directory: str, env ["CACHE_ROOT_DIRECTORY"]),
    row!(logs_root_directory: str, env ["COGNEE_LOGS_DIR"]),
    KeySpec {
        key: "monitoring_tool",
        key_aliases: &[],
        env: &["MONITORING_TOOL"],
        secret: false,
        persisted: true,
        // Python's `get_observe` supports only `none` and raises
        // `UnsupportedObserverError` for any other tool. Setting one fails the
        // same way; from env, an unsupported value is ignored with a warning,
        // as a value that does not parse is for every other key.
        overlay: |s, env| {
            if let Some(v) = first_set(env, &["MONITORING_TOOL"]) {
                match monitoring_tool(&v) {
                    Ok(tool) => s.monitoring_tool = tool,
                    Err(reason) => tracing::warn!(value = v, "MONITORING_TOOL: {reason}; ignored"),
                }
            }
        },
        set: |s, key, v| {
            s.monitoring_tool =
                monitoring_tool(&as_string(key, &v)?).map_err(|r| mismatch(key, &r))?;
            Ok(())
        },
        get: |s| Value::String(s.monitoring_tool.clone()),
        reset: |s, d| s.monitoring_tool = d.monitoring_tool.clone(),
    },
    // -- Cognify models -----------------------------------------------------
    row!(classification_model: str, env []),
    row!(summarization_model: str, env []),
    row!(graph_model: str, env []),
    KeySpec {
        key: "summarization_schema",
        key_aliases: &[],
        env: &[],
        secret: false,
        persisted: false,
        overlay: |_, _| {},
        // `null` clears it; anything else must pass the cognify schema check.
        set: |s, key, v| {
            if v.is_null() {
                s.summarization_schema = None;
                return Ok(());
            }
            cognee_cognify::config::validate_summary_schema(&v)
                .map_err(|e| mismatch(key, &e.to_string()))?;
            s.summarization_schema = Some(v);
            Ok(())
        },
        get: |s| s.summarization_schema.clone().unwrap_or(Value::Null),
        reset: |s, d| s.summarization_schema = d.summarization_schema.clone(),
    },
    // -- LLM ----------------------------------------------------------------
    row!(llm_max_parallel_requests: u32, env ["LLM_MAX_PARALLEL_REQUESTS"]),
    // Python's `LLMConfig.graph_prompt_path` (a `BaseSettings` field, so
    // `GRAPH_PROMPT_PATH` binds it there too).
    row!(graph_prompt_path: str, env ["GRAPH_PROMPT_PATH"]),
    // -- Chunking -----------------------------------------------------------
    row!(chunk_strategy: str, env []),
    row!(chunk_engine: str, env []),
    // Rust extension, no Python counterpart. `null` restores auto-sizing.
    row!(chunk_size: opt_u32, env ["COGNEE_CHUNK_SIZE", "CHUNK_SIZE"]),
    row!(chunk_overlap: u32, env ["COGNEE_CHUNK_OVERLAP", "CHUNK_OVERLAP"]),
    // -- Relational database ------------------------------------------------
    KeySpec {
        key: "db_sslmode",
        key_aliases: &[],
        env: &["DB_SSLMODE", "DATABASE_CONNECT_ARGS"],
        secret: false,
        persisted: true,
        // The explicit libpq `DB_SSLMODE` wins; otherwise the mode is derived
        // from the `ssl` key of the Python chart's `DATABASE_CONNECT_ARGS`.
        overlay: |s, env| {
            if let Some(v) = first_set(env, &["DB_SSLMODE"]) {
                s.db_sslmode = v;
            } else if let Some(mode) = first_set(env, &["DATABASE_CONNECT_ARGS"])
                .and_then(|v| sslmode_from_connect_args(&v))
            {
                s.db_sslmode = mode;
            }
        },
        set: |s, key, v| {
            s.db_sslmode = as_string(key, &v)?;
            Ok(())
        },
        get: |s| Value::String(s.db_sslmode.clone()),
        reset: |s, d| s.db_sslmode = d.db_sslmode.clone(),
    },
    row!(default_system_prompt_path: str, env []),
    // -- Session / cache ----------------------------------------------------
    // `cache_backend`, `session_ttl_seconds` and `enable_caching` are aliases
    // of the `session` section's `store`, `ttl_seconds` and `caching`.
    row!(auto_feedback: bool, env ["AUTO_FEEDBACK"]),
    // -- Authentication / ACL -----------------------------------------------
    row!(default_user_email: str, env ["DEFAULT_USER_EMAIL"]),
    row!(default_user_password: str secret, env ["DEFAULT_USER_PASSWORD"]),
    // Reserved: cognee-rs itself never acts on it. It stays a known setting so
    // the Python SDK's env var is not an unknown key (Python-parity configs
    // carry `ENABLE_BACKEND_ACCESS_CONTROL=false`), but `true` is accepted only
    // when the application declares that it provides access control
    // (`ConfigManager::set_application_access_control`). This row's plain
    // `set` cannot know that, so it refuses `true`; `ConfigManager::set` lets
    // it through for such an application. A `true` from env or a loaded
    // config is stored, and `ConfigManager::check_supported` reports it.
    KeySpec {
        key: ACCESS_CONTROL_KEY,
        key_aliases: &[],
        env: ACCESS_CONTROL_ENV,
        secret: false,
        persisted: true,
        overlay: |s, env| {
            kind::overlay_bool(&mut s.enable_access_control, env, ACCESS_CONTROL_ENV);
        },
        set: |s, key, v| {
            if as_bool(key, &v)? {
                return Err(access_control_not_provided());
            }
            s.enable_access_control = false;
            Ok(())
        },
        get: |s| Value::Bool(s.enable_access_control),
        reset: |s, d| s.enable_access_control = d.enable_access_control,
    },
    // -- Search history -----------------------------------------------------
    row!(log_search_history: bool, env ["COGNEE_LOG_SEARCH_HISTORY"]),
    // -- Logging ------------------------------------------------------------
    row!(log_level: str, env ["LOG_LEVEL"]),
    // -- Observability ------------------------------------------------------
    row!(cognee_tracing_enabled: bool, env ["COGNEE_TRACING_ENABLED"]),
    row!(otel_service_name: str, env ["OTEL_SERVICE_NAME"]),
    row!(otel_exporter_otlp_endpoint: str, env ["OTEL_EXPORTER_OTLP_ENDPOINT"]),
    row!(otel_exporter_otlp_headers: str secret, env ["OTEL_EXPORTER_OTLP_HEADERS"]),
    row!(otel_exporter_otlp_protocol: str, env ["OTEL_EXPORTER_OTLP_PROTOCOL"]),
    row!(otel_span_processor: str, env ["OTEL_SPAN_PROCESSOR"]),
    row!(otel_traces_sampler: str, env ["OTEL_TRACES_SAMPLER"]),
    row!(otel_traces_sampler_arg: str, env ["OTEL_TRACES_SAMPLER_ARG"]),
    row!(otel_deployment_environment: str, env ["ENV"]),
    // -- Feature flags ------------------------------------------------------
    row!(enable_last_accessed: bool, env ["ENABLE_LAST_ACCESSED"]),
    row!(strict_config: bool, env ["COGNEE_STRICT_CONFIG"]),
    KeySpec {
        key: "single_process",
        key_aliases: &[],
        env: &[cognee_database::SINGLE_PROCESS_ENV],
        secret: false,
        persisted: true,
        // Both paths go through `parse_single_process_override`, so the env var
        // and `cognee_database::single_process_from_env` cannot drift. The raw
        // value is used (no empty filter): a blank value means "derive".
        overlay: |s, env| {
            if let Some(v) = env
                .get(cognee_database::SINGLE_PROCESS_ENV)
                .and_then(|raw| cognee_database::parse_single_process_override(&raw))
            {
                s.single_process = Some(v);
            }
        },
        // `null` restores the derived answer. A string or a JSON number goes
        // through the env var's parser, so `"1"`, `1` and
        // `COGNEE_SINGLE_PROCESS=1` mean the same, and a blank string means
        // "derive" rather than a silent `false`.
        set: |s, key, v| {
            s.single_process = if v.is_null() {
                None
            } else if v.is_string() || v.is_number() {
                let raw = v
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string());
                cognee_database::parse_single_process_override(&raw)
            } else {
                Some(as_bool(key, &v)?)
            };
            Ok(())
        },
        get: |s| serde_json::to_value(s.single_process).unwrap_or(Value::Null),
        reset: |s, d| s.single_process = d.single_process,
    },
];

/// [`builtin_schema`], once something has built it. `Settings`' `Debug` reads
/// it with `get()`, so formatting never builds the component registry.
static BUILTIN_SCHEMA: OnceLock<ConfigSchema> = OnceLock::new();

/// The configuration schema of a default build: the `Settings` keys and env
/// vars reserved, plus the sections of every factory
/// `ComponentRegistry::with_builtins()` registers for the enabled features.
///
/// A custom registry adds its sections to a [`ConfigManager`](super::ConfigManager)
/// with [`ConfigManager::extend_schema`](super::ConfigManager::extend_schema)
/// (which `ComponentManager::with_registry` does).
pub fn builtin_schema() -> &'static ConfigSchema {
    BUILTIN_SCHEMA.get_or_init(|| {
        let mut schema = core_schema();
        let builtins = cognee_components::ComponentRegistry::with_builtins()
            .config_schema()
            .expect("built-in config sections do not collide (pinned by builtin_schema_has_no_collisions)");
        schema
            .merge(&builtins)
            .expect("built-in config sections do not claim Settings keys (pinned by builtin_schema_has_no_collisions)");
        schema
    })
}

/// The built-in schema plus the sections of `extra` (a custom registry's
/// `config_schema()`): what a process using that registry must load env with.
pub fn schema_with(extra: &ConfigSchema) -> Result<ConfigSchema, cognee_config_core::SchemaError> {
    let mut schema = builtin_schema().clone();
    schema.merge(extra)?;
    Ok(schema)
}

/// An empty schema with every `Settings` key, key alias and primary env var
/// reserved, so no section can claim them.
pub(super) fn core_schema() -> ConfigSchema {
    let mut schema = ConfigSchema::new();
    schema
        .reserve(
            "Settings",
            KEYS.iter()
                .flat_map(|spec| std::iter::once(spec.key).chain(spec.key_aliases.iter().copied())),
            // Primary env vars only, the same rule sections follow: a fallback
            // (`LLM_API_KEY` behind `embedding_api_key`) may be another
            // owner's primary.
            KEYS.iter().filter_map(|spec| spec.env.first().copied()),
        )
        .expect("Settings keys are unique (pinned by env_vars_and_keys_are_unique)");
    schema
}

impl Settings {
    /// Set the setting `key` (a `Settings` key, a built-in section key, or an
    /// alias of either) from a JSON value, with the same parse rules as
    /// [`ConfigManager::set`](super::ConfigManager::set).
    ///
    /// This is the plain assignment: unlike
    /// [`ConfigManager::set_system_root_directory`](super::ConfigManager::set_system_root_directory)
    /// it does not move the paths derived from the system root. An unknown key
    /// is a warning, or [`ConfigError::UnknownKey`] when `strict_config` is on.
    pub fn set_key(&mut self, key: &str, value: Value) -> Result<(), ConfigError> {
        if let Some(spec) = key_spec(key) {
            return (spec.set)(self, key, value);
        }
        let schema = builtin_schema();
        if schema.key_def(key).is_some() {
            schema.set(&mut self.sections, key, &value)?;
            return Ok(());
        }
        cognee_config_core::unknown_key(key, self.strict_config).map_err(ConfigError::from)
    }

    /// Fail if a setting holds a value cognee-rust accepts as a key but does
    /// not support. Today that is `enable_access_control = true`
    /// (`ENABLE_BACKEND_ACCESS_CONTROL`) when `application_access_control` is
    /// `false`: cognee-rs applies dataset ACLs only through an `AclDb` the
    /// embedding application attaches, so a configuration asking for access
    /// control is an error unless the application declares that it provides
    /// it. `false` (the default) is always accepted.
    ///
    /// [`ConfigManager::check_supported`](super::ConfigManager::check_supported)
    /// calls this with the manager's
    /// [`application_access_control`](super::ConfigManager::application_access_control)
    /// switch.
    pub fn check_supported(&self, application_access_control: bool) -> Result<(), ConfigError> {
        if self.enable_access_control && !application_access_control {
            return Err(access_control_not_provided());
        }
        Ok(())
    }

    /// The current value of setting `key` (or an alias) as JSON, unmasked.
    /// `None` for an unknown key.
    pub fn get_key(&self, key: &str) -> Option<Value> {
        match key_spec(key) {
            Some(spec) => Some((spec.get)(self)),
            None => builtin_schema().get(&self.sections, key),
        }
    }

    /// Restore setting `key` (or an alias) to its default, or remove a stale
    /// section entry that no component owns. Any other unknown key is a
    /// warning, or [`ConfigError::UnknownKey`] when `strict_config` is on.
    pub fn reset_key(&mut self, key: &str) -> Result<(), ConfigError> {
        if let Some(spec) = key_spec(key) {
            (spec.reset)(self, &Settings::default());
            return Ok(());
        }
        if builtin_schema().remove(&mut self.sections, key).is_some() {
            return Ok(());
        }
        // A stale entry no component owns (e.g. written by a build with more
        // features): removing it is how a config is repaired, so it is allowed
        // even in strict mode.
        if self.sections.remove(key).is_some() {
            return Ok(());
        }
        cognee_config_core::unknown_key(key, self.strict_config).map_err(ConfigError::from)
    }

    /// Overlay the variables of `env` on top of `self`: the `Settings` keys
    /// and every built-in section.
    ///
    /// Same rules as [`Settings::overlay_from_env`] (only set, non-empty
    /// variables change a value), but reading from `env` instead of the
    /// process environment, and without loading `.env`.
    pub fn overlay_from(&mut self, env: &dyn EnvSource) {
        self.overlay_from_with_schema(env, builtin_schema());
    }

    /// [`Settings::overlay_from`] for a build whose component registry adds
    /// sections of its own: `schema` must include the built-in sections too
    /// (see [`schema_with`]).
    pub fn overlay_from_with_schema(&mut self, env: &dyn EnvSource, schema: &ConfigSchema) {
        for spec in KEYS {
            (spec.overlay)(self, env);
        }
        schema.overlay_env(&mut self.sections, env);
    }
}

/// One value of [`Settings`]' `Debug` output, as compact JSON: a secret by
/// [`mask_secret`](cognee_config_core::mask_secret) (the marker when set,
/// itself when `null` or `""`), any other value with embedded credentials
/// masked by [`redact_credentials`](cognee_config_core::redact_credentials).
struct ShownValue<'a> {
    value: &'a Value,
    secret: bool,
}

impl std::fmt::Debug for ShownValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.secret && cognee_config_core::secret_is_set(self.value) {
            return f.write_str(cognee_config_core::REDACTED);
        }
        let json = self.value.to_string();
        f.write_str(&cognee_config_core::redact_credentials(&json))
    }
}

/// The `sections` map of [`Settings`]' `Debug` output, secrets masked.
struct ShownSections<'a>(&'a cognee_config_core::ConfigValues);

impl std::fmt::Debug for ShownSections<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Only an already built schema: building it here would construct the
        // component registry inside a formatter.
        let schema = BUILTIN_SCHEMA.get();
        let mut map = f.debug_map();
        for (key, value) in self.0 {
            // A key of a built-in section (or an alias of one) is masked by its
            // `secret` flag. A key no built-in section owns (a custom
            // registry's, a stale one, or any key before the built-in schema
            // exists) may be a credential: a string is masked unless empty.
            let secret = match schema.and_then(|schema| schema.key_def(key)) {
                Some(def) => def.secret,
                None => value.is_string(),
            };
            map.entry(&key, &ShownValue { value, secret });
        }
        map.finish()
    }
}

/// Never prints a credential, and has no side effects: every setting is shown
/// as JSON; a setting marked `secret` (a registry row, or a built-in section
/// key) shows as [`REDACTED`](cognee_config_core::REDACTED) when set and as its
/// `null` / `""` otherwise; any other value has embedded credentials (a URL
/// password, `password=…`) masked. A `sections` entry no built-in section owns
/// is masked when it is a non-empty string, since only the registry that owns
/// it knows whether it is a secret.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("Settings");
        for spec in KEYS {
            let value = (spec.get)(self);
            out.field(
                spec.key,
                &ShownValue {
                    value: &value,
                    secret: spec.secret,
                },
            );
        }
        out.field("sections", &ShownSections(&self.sections));
        out.finish()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// Every serialized `Settings` field, plus the `#[serde(skip)]` one,
    /// has exactly one row, and no row names a field that does not exist.
    #[test]
    fn every_settings_field_has_exactly_one_row() {
        let mut fields: Vec<String> = serde_json::to_value(Settings::default())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        fields.push("summarization_schema".to_string());
        fields.sort();

        let mut rows: Vec<String> = KEYS.iter().map(|s| s.key.to_string()).collect();
        rows.sort();
        let mut deduped = rows.clone();
        deduped.dedup();
        assert_eq!(rows, deduped, "a key has two rows");
        assert_eq!(rows, fields, "registry rows and Settings fields differ");
    }

    /// No env var and no setting key binds two rows.
    #[test]
    fn env_vars_and_keys_are_unique() {
        let mut names: Vec<&str> = KEYS
            .iter()
            .flat_map(|s| std::iter::once(s.key).chain(s.key_aliases.iter().copied()))
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "a setting key or key alias is reused");

        // An env var may serve as a *fallback* for another row
        // (`LLM_API_KEY` backs `embedding_api_key`), but may be the primary
        // variable of only one row.
        let mut primaries: Vec<&str> = KEYS.iter().filter_map(|s| s.env.first().copied()).collect();
        let total = primaries.len();
        primaries.sort_unstable();
        primaries.dedup();
        assert_eq!(
            primaries.len(),
            total,
            "an env var is the primary of two rows"
        );
    }

    /// The built-in sections neither collide with each other nor claim a
    /// `Settings` key or env var.
    #[test]
    fn builtin_schema_has_no_collisions() {
        let mut schema = core_schema();
        let builtins = cognee_components::ComponentRegistry::with_builtins()
            .config_schema()
            .unwrap();
        schema.merge(&builtins).unwrap();
    }

    fn assert_access_control_rejected(result: Result<(), ConfigError>) {
        match result {
            Err(ConfigError::Unsupported { key, reason }) => {
                assert_eq!(key, "enable_access_control");
                assert!(reason.contains("AclDb"), "{reason}");
                assert!(
                    reason.contains("set_application_access_control"),
                    "{reason}"
                );
                assert!(reason.contains("ENABLE_BACKEND_ACCESS_CONTROL"), "{reason}");
            }
            other => panic!("expected the access-control rejection, got {other:?}"),
        }
    }

    fn env_settings(raw: &str) -> Settings {
        let mut s = Settings::default();
        s.overlay_from(&crate::config::MapEnv::new().with("ENABLE_BACKEND_ACCESS_CONTROL", raw));
        s
    }

    /// `ENABLE_BACKEND_ACCESS_CONTROL=true` is an error at first use, without
    /// `strict_config`; `false` and unset are accepted.
    #[test]
    fn access_control_from_env_true_is_rejected() {
        use crate::config::ConfigManager;
        for raw in ["true", "1", "yes", "ON"] {
            let s = env_settings(raw);
            assert!(!s.strict_config);
            assert_access_control_rejected(s.check_supported(false));
            let cm = ConfigManager::new(s);
            assert_access_control_rejected(cm.check_supported());
            assert_access_control_rejected(cm.check_strict());
        }
        for raw in ["false", "0", "no", ""] {
            let s = env_settings(raw);
            assert!(!s.enable_access_control, "{raw:?}");
            ConfigManager::new(s).check_strict().unwrap();
        }
    }

    /// `set` / `set_key` refuse `true` at once and accept `false`.
    #[test]
    fn access_control_set_true_is_rejected() {
        use crate::config::ConfigManager;
        let cm = ConfigManager::new(Settings::default());
        assert_access_control_rejected(cm.set("enable_access_control", Value::Bool(true)));
        assert!(!cm.read().enable_access_control);
        assert_eq!(cm.version(), 0, "a rejected set changes nothing");
        cm.set("enable_access_control", Value::Bool(false)).unwrap();
        cm.check_strict().unwrap();
        // Still a boolean key: a wrong type is the usual type mismatch.
        assert!(matches!(
            cm.set("enable_access_control", Value::from("true")),
            Err(ConfigError::TypeMismatch { reason, .. }) if reason == "expected a boolean"
        ));

        let mut s = Settings::default();
        assert_access_control_rejected(s.set_key("enable_access_control", Value::Bool(true)));
        s.set_key("enable_access_control", Value::Bool(false))
            .unwrap();
        assert!(!s.enable_access_control);
    }

    /// A config file or binding settings object carrying `true` fails at first
    /// use (or at once through `apply_patch`); `false` loads.
    #[test]
    fn access_control_from_json_true_is_rejected() {
        use crate::config::ConfigManager;
        let s: Settings =
            serde_json::from_value(serde_json::json!({ "enable_access_control": true })).unwrap();
        assert_access_control_rejected(ConfigManager::new(s).check_strict());

        let s: Settings =
            serde_json::from_value(serde_json::json!({ "enable_access_control": false })).unwrap();
        ConfigManager::new(s).check_strict().unwrap();

        let cm = ConfigManager::new(Settings::default());
        let patch = serde_json::json!({ "enable_access_control": true });
        assert_access_control_rejected(cm.apply_patch(patch.as_object().unwrap()));
        let patch = serde_json::json!({ "enable_access_control": false });
        cm.apply_patch(patch.as_object().unwrap()).unwrap();
        cm.check_strict().unwrap();
    }

    /// An application that provides access control owns the flag: `true` is
    /// accepted from env, `set`, `apply_patch` and a loaded config, and can
    /// be turned off again.
    #[test]
    fn access_control_true_is_accepted_when_the_application_provides_it() {
        use crate::config::ConfigManager;
        let cm = ConfigManager::new(env_settings("true"));
        assert_access_control_rejected(cm.check_strict());
        cm.set_application_access_control(true);
        assert!(cm.application_access_control());
        cm.check_strict().unwrap();
        cm.check_supported().unwrap();
        assert!(cm.read().enable_access_control);
        // Shared by clones.
        assert!(cm.clone().application_access_control());

        let cm = ConfigManager::new(Settings::default());
        cm.set_application_access_control(true);
        cm.set("enable_access_control", Value::Bool(true)).unwrap();
        assert_eq!(cm.get("enable_access_control"), Some(Value::Bool(true)));
        cm.set("enable_access_control", Value::Bool(false)).unwrap();
        let patch = serde_json::json!({ "enable_access_control": true });
        cm.apply_patch(patch.as_object().unwrap()).unwrap();
        assert!(cm.read().enable_access_control);
        cm.check_strict().unwrap();
        assert!(matches!(
            cm.set("enable_access_control", Value::from("yes")),
            Err(ConfigError::TypeMismatch { .. })
        ));

        // Withdrawn again: the stored `true` is reported.
        cm.set_application_access_control(false);
        assert_access_control_rejected(cm.check_strict());
    }

    /// `apply_patch` is atomic: a failing key leaves every other key of the
    /// patch unapplied and the version unchanged.
    #[test]
    fn apply_patch_is_atomic() {
        use crate::config::ConfigManager;
        let cm = ConfigManager::new(Settings::default());
        let before = cm.read().chunk_overlap;
        let patch = serde_json::json!({
            "chunk_overlap": before + 7,
            "llm.model": "patched-model",
            "custom.unknown": 1,
            "enable_access_control": true,
        });
        assert_access_control_rejected(cm.apply_patch(patch.as_object().unwrap()));
        assert_eq!(cm.version(), 0);
        assert_eq!(cm.read().chunk_overlap, before);
        assert_ne!(cm.read().llm().model, "patched-model");
        assert!(!cm.read().sections.contains_key("custom.unknown"));

        let patch = serde_json::json!({ "chunk_overlap": before + 7, "llm.model": "m" });
        cm.apply_patch(patch.as_object().unwrap()).unwrap();
        assert_eq!(cm.version(), 1, "one bump per patch");
        assert_eq!(cm.read().chunk_overlap, before + 7);
        assert_eq!(cm.read().llm().model, "m");
    }

    /// `{:?}` of `Settings` never prints a credential: a secret row, a
    /// built-in section secret (canonical or alias), or a string under a key
    /// no built-in section owns.
    #[test]
    fn settings_debug_redacts_secrets() {
        let mut s = Settings {
            default_user_password: "pw-row-secret".to_string(),
            otel_exporter_otlp_headers: "authorization=hdr-row-secret".to_string(),
            ..Settings::default()
        };
        s.sections
            .insert("llm.api_key".into(), Value::from("sk-section-secret"));
        s.sections
            .insert("embedding_api_key".into(), Value::from("sk-alias-secret"));
        s.sections
            .insert("custom.api_key".into(), Value::from("sk-unknown-secret"));
        s.sections
            .insert("custom.enabled".into(), Value::Bool(true));
        s.sections
            .insert("llm.model".into(), Value::from("gpt-4o-mini"));
        // Not secrets, but they embed one.
        s.sections.insert(
            "relational.url".into(),
            Value::from("postgres://cognee:url-pw-secret@db:5432/cognee"),
        );
        s.sections.insert(
            "graph.url".into(),
            Value::from("host=db user=cognee password=dsn-pw-secret dbname=g"),
        );
        // Only an already built schema is consulted by `Debug`.
        let _ = builtin_schema();

        for out in [format!("{s:?}"), format!("{s:#?}")] {
            for secret in [
                "pw-row-secret",
                "hdr-row-secret",
                "sk-section-secret",
                "sk-alias-secret",
                "sk-unknown-secret",
                "url-pw-secret",
                "dsn-pw-secret",
            ] {
                assert!(!out.contains(secret), "{secret} leaked: {out}");
            }
            assert!(out.contains("gpt-4o-mini"), "non-secret shown: {out}");
            assert!(out.contains(cognee_config_core::REDACTED), "{out}");
        }
        let out = format!("{s:?}");
        assert!(out.contains("\"custom.enabled\": true"), "{out}");
        assert!(out.contains("enable_access_control: false"), "{out}");
        assert!(
            out.contains("postgres://cognee:***REDACTED***@db:5432/cognee"),
            "{out}"
        );
        // An unset (empty) string secret shows as itself, not as the marker.
        let out = format!("{:?}", Settings::default());
        assert!(out.contains("default_user_password: \"\""), "{out}");
        assert!(!out.contains(cognee_config_core::REDACTED), "{out}");
    }

    /// Every row's get/set round-trips its default value.
    #[test]
    fn every_row_round_trips_its_default() {
        let default = Settings::default();
        for spec in KEYS {
            let mut s = Settings::default();
            let value = (spec.get)(&default);
            (spec.set)(&mut s, spec.key, value.clone())
                .unwrap_or_else(|e| panic!("{}: {e}", spec.key));
            assert_eq!((spec.get)(&s), value, "{}", spec.key);
        }
    }
}
