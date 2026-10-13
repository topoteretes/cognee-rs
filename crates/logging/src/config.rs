//! Env-var-driven configuration for `cognee-logging`.
//!
//! [`LoggingConfig::from_env`] parses every env var documented in
//! `docs/telemetry/06-file-logging-rotation.md` §"Env Var Catalog"
//! into a plain-old data struct. It does **not** install a subscriber,
//! resolve filesystem paths, or emit `tracing` events — those concerns
//! land in tasks 06-03 / 06-04 / 06-05.
//!
//! Every field is also settable from code: assign it directly, or use
//! [`LoggingConfig::set`] with a string key (what the language bindings'
//! `setup_logging(options)` entry points do). Logging configures a
//! process-wide singleton (the `tracing` subscriber), so this is *process*
//! config: it is resolved once, before any SDK handle exists, and
//! [`LoggingConfig::from_env_source`] is its only window onto the
//! environment.
//!
//! Note: `COGNEE_LOG_MAX_BYTES` is captured (as [`LoggingConfig::max_bytes`])
//! but not honoured. Per decision 1 in the parent doc, size-based rotation
//! is deferred for v1 and the variable is accepted as a documented no-op.
//! The init layer warns when it is set; keeping `from_env` free of
//! `tracing::*` calls lets unit tests run without a subscriber.

use cognee_config_core::EnvSource;
use std::path::PathBuf;
use thiserror::Error;

/// Output format for both stdout and file sinks (decision 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Python-byte-exact `<ts> [<LEVEL>] <msg> k=v ... [logger]`.
    Plain,
    /// JSON Lines via `tracing-subscriber::fmt::layer().json()`.
    Json,
}

/// Which standard stream the console layer writes to.
///
/// Defaults to [`ConsoleStream::Stdout`], preserving the behaviour every
/// existing consumer had. The CLI selects [`ConsoleStream::Stderr`]: it emits
/// *data* on stdout (notably `--output-format json`), and diagnostics
/// interleaved with that data make the output unparseable — `serde_json` reads
/// the `2026` of a log timestamp as a number and fails with "trailing
/// characters at line 1 column 5". Keeping logs on stderr is also the
/// conventional split for a command-line tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleStream {
    /// Write console logs to stdout.
    Stdout,
    /// Write console logs to stderr, leaving stdout for data.
    Stderr,
}

/// Time-based rotation cadence. Size-based deferred per decision 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogRotation {
    /// Rotate once per day at UTC midnight.
    Daily,
    /// Rotate at the top of every hour.
    Hourly,
    /// Rotate every minute (mostly useful for tests).
    Minutely,
    /// Never rotate.
    Never,
}

/// Errors raised by [`LoggingConfig::from_env`].
#[derive(Debug, Error)]
pub enum LoggingConfigError {
    /// Env var was set to a value outside its accepted enum domain.
    #[error("invalid value for {var}: {value:?} (expected one of: {expected})")]
    InvalidValue {
        /// Env-var name that carried the invalid value.
        var: &'static str,
        /// Raw value that failed to parse.
        value: String,
        /// Human-readable list of accepted values.
        expected: &'static str,
    },
    /// Env var was set to a non-integer where an integer was required.
    #[error("invalid integer for {var}: {source}")]
    InvalidInt {
        /// Env-var name that carried the non-integer value.
        var: &'static str,
        /// Underlying parse error from `<usize as FromStr>`.
        #[source]
        source: std::num::ParseIntError,
    },
    /// [`LoggingConfig::set`] was given a key outside [`LOGGING_SETTING_KEYS`].
    #[error("unknown logging setting {0:?} (expected one of: {keys})", keys = LOGGING_SETTING_KEYS.join(", "))]
    UnknownKey(String),
}

/// The keys [`LoggingConfig::set`] accepts. `log_level` and
/// `logs_root_directory` share their names with the `cognee::Settings` keys
/// they correspond to.
pub const LOGGING_SETTING_KEYS: &[&str] = &[
    "log_level",
    "logs_root_directory",
    "log_file",
    "log_file_name",
    "log_rotation",
    "log_format",
    "log_backup_count",
    "log_max_files",
    "log_max_bytes",
    "console_stream",
];

/// Parsed view of every env var that influences logging setup.
///
/// All filesystem-side resolution (default logs dir, fallback to
/// `/tmp/cognee_logs`, multi-process `LOG_FILE_NAME` propagation)
/// happens later in task 06-03; this struct only captures the raw
/// user-provided overrides.
///
/// Defaults applied by [`Self::from_env`] when env vars are unset:
///
/// | Field | Default |
/// |---|---|
/// | `file_enabled` | `true` |
/// | `logs_dir_override` | `None` |
/// | `log_file_name` | `None` |
/// | `rotation` | [`LogRotation::Daily`] |
/// | `format` | [`LogFormat::Plain`] |
/// | `backup_count` | `5` |
/// | `max_files` | `10` (decision 14) |
/// | `level_filter` | `None` (init layer substitutes the default filter) |
#[derive(Debug, Clone)]
pub struct LoggingConfig {
    /// `COGNEE_LOG_FILE` toggle. `false`/`0`/`no` (case-insensitive)
    /// disables the file sink entirely.
    pub file_enabled: bool,
    /// User-provided `COGNEE_LOGS_DIR` override, captured as a raw
    /// `PathBuf`. Falls back to `~/.cognee/logs` → `/tmp/cognee_logs`
    /// during resolution in task 06-03.
    pub logs_dir_override: Option<PathBuf>,
    /// Pre-resolved file path from the `LOG_FILE_NAME` env var, used
    /// to inherit a single log file across child processes (Python
    /// parity, decision 5). `None` → generate at init time.
    pub log_file_name: Option<PathBuf>,
    /// Time-based rotation cadence from `COGNEE_LOG_ROTATION`.
    pub rotation: LogRotation,
    /// Output format from `COGNEE_LOG_FORMAT` (decision 3).
    pub format: LogFormat,
    /// `COGNEE_LOG_BACKUP_COUNT`: hint for `tracing-appender`'s
    /// `max_log_files` builder (default `5`).
    pub backup_count: usize,
    /// `COGNEE_LOG_MAX_FILES` (decision 14): retention ceiling
    /// enforced by the startup cleanup pass (task 06-03). Default `10`.
    pub max_files: usize,
    /// Which standard stream the console layer writes to. Not env-driven:
    /// it is a property of the embedding program, not the deployment.
    pub console_stream: ConsoleStream,
    /// Resolved `EnvFilter` directive string built from `RUST_LOG`
    /// (preferred) or `LOG_LEVEL` (fallback). `None` means neither
    /// env var was set, and the init layer should substitute the
    /// default library-noise-suppressing filter (decision 6).
    pub level_filter: Option<String>,
    /// Raw `COGNEE_LOG_MAX_BYTES`. Accepted for Python parity but ignored:
    /// size-based rotation is not supported yet (decision 1), so
    /// [`crate::init_logging`] only warns when this is set.
    pub max_bytes: Option<String>,
}

impl LoggingConfig {
    /// Build a [`LoggingConfig`] with all defaults — equivalent to
    /// what [`Self::from_env`] returns on an empty environment.
    ///
    /// Used by binaries / bindings as the fallback when
    /// `from_env()` rejects a malformed user-provided env var: rather
    /// than aborting startup we keep logging on with documented
    /// defaults.
    pub fn defaults() -> Self {
        Self {
            file_enabled: true,
            logs_dir_override: None,
            log_file_name: None,
            rotation: LogRotation::Daily,
            format: LogFormat::Plain,
            backup_count: 5,
            max_files: 10,
            console_stream: ConsoleStream::Stdout,
            level_filter: None,
            max_bytes: None,
        }
    }

    /// Build a [`LoggingConfig`] by reading the env-var surface
    /// documented on the struct.
    ///
    /// Returns an error only when an env var carries a malformed
    /// value (e.g. `COGNEE_LOG_ROTATION=weekly`,
    /// `COGNEE_LOG_BACKUP_COUNT=abc`). Unset env vars fall back to
    /// the documented defaults.
    ///
    /// This is a configuration boundary: it is where logging opens the
    /// process environment. [`LoggingConfig::from_env_source`] reads the
    /// same variables from any [`EnvSource`].
    pub fn from_env() -> Result<Self, LoggingConfigError> {
        Self::from_env_source(&process_env())
    }

    /// The level filter alone, from the process environment: `RUST_LOG`, else
    /// `LOG_LEVEL` (Python level names mapped, see [`normalize_level_filter`]).
    ///
    /// For callers that only need the level — the bindings' import-time bridge
    /// subscribers — so that a malformed, unrelated logging variable
    /// (`COGNEE_LOG_ROTATION=weekly`) cannot discard a valid `RUST_LOG`.
    pub fn level_filter_from_env() -> Option<String> {
        Self::level_filter_from_env_source(&process_env())
    }

    /// [`LoggingConfig::level_filter_from_env`], reading from `env`.
    pub fn level_filter_from_env_source(env: &dyn EnvSource) -> Option<String> {
        read_nonempty(env, "RUST_LOG")
            .or_else(|| read_nonempty(env, "LOG_LEVEL").map(|level| normalize_level_filter(&level)))
    }

    /// [`LoggingConfig::from_env`], reading from `env` instead of the
    /// process environment.
    pub fn from_env_source(env: &dyn EnvSource) -> Result<Self, LoggingConfigError> {
        let file_enabled = parse_bool(env, "COGNEE_LOG_FILE", true);

        let logs_dir_override = read_nonempty(env, "COGNEE_LOGS_DIR").map(PathBuf::from);
        let log_file_name = read_nonempty(env, "LOG_FILE_NAME").map(PathBuf::from);

        let rotation = match read_nonempty(env, "COGNEE_LOG_ROTATION") {
            None => LogRotation::Daily,
            Some(raw) => parse_rotation("COGNEE_LOG_ROTATION", raw)?,
        };

        let format = match read_nonempty(env, "COGNEE_LOG_FORMAT") {
            None => LogFormat::Plain,
            Some(raw) => parse_format("COGNEE_LOG_FORMAT", raw)?,
        };

        let backup_count = parse_usize(env, "COGNEE_LOG_BACKUP_COUNT", 5)?;
        let max_files = parse_usize(env, "COGNEE_LOG_MAX_FILES", 10)?;
        let max_bytes = read_nonempty(env, "COGNEE_LOG_MAX_BYTES");

        // Decision 7: RUST_LOG wins over LOG_LEVEL. When only
        // LOG_LEVEL is set, surface it verbatim so the init layer
        // (task 06-05) can feed it into `EnvFilter::new` as a bare
        // level. When both are unset, leave `None` so init substitutes
        // `default_filter()` (decision 6).
        let level_filter = Self::level_filter_from_env_source(env);

        Ok(Self {
            file_enabled,
            logs_dir_override,
            log_file_name,
            rotation,
            format,
            backup_count,
            max_files,
            console_stream: ConsoleStream::Stdout,
            level_filter,
            max_bytes,
        })
    }

    /// Set one setting from code, by key (see [`LOGGING_SETTING_KEYS`]).
    /// Wins over the environment the config was resolved from.
    ///
    /// | Key | Field | Value |
    /// |---|---|---|
    /// | `log_level` | [`level_filter`](Self::level_filter) | an `EnvFilter` directive (`debug`, `info,cognee=trace`); empty restores the default filter |
    /// | `logs_root_directory` | [`logs_dir_override`](Self::logs_dir_override) | a directory; empty restores the default |
    /// | `log_file` | [`file_enabled`](Self::file_enabled) | `false`/`0`/`no` disable, anything else enables |
    /// | `log_file_name` | [`log_file_name`](Self::log_file_name) | a path; empty restores the default |
    /// | `log_rotation` | [`rotation`](Self::rotation) | `daily`, `hourly`, `minutely`, `never` |
    /// | `log_format` | [`format`](Self::format) | `plain`, `json` |
    /// | `log_backup_count` | [`backup_count`](Self::backup_count) | an integer |
    /// | `log_max_files` | [`max_files`](Self::max_files) | an integer |
    /// | `log_max_bytes` | [`max_bytes`](Self::max_bytes) | accepted, ignored (warns) |
    /// | `console_stream` | [`console_stream`](Self::console_stream) | `stdout`, `stderr` |
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), LoggingConfigError> {
        let non_empty = || (!value.is_empty()).then(|| value.to_string());
        match key {
            "log_level" => self.level_filter = non_empty().map(|v| normalize_level_filter(&v)),
            "logs_root_directory" => self.logs_dir_override = non_empty().map(PathBuf::from),
            "log_file" => self.file_enabled = parse_bool_value(value),
            "log_file_name" => self.log_file_name = non_empty().map(PathBuf::from),
            "log_rotation" => self.rotation = parse_rotation("log_rotation", value.to_string())?,
            "log_format" => self.format = parse_format("log_format", value.to_string())?,
            "log_backup_count" => self.backup_count = parse_usize_value("log_backup_count", value)?,
            "log_max_files" => self.max_files = parse_usize_value("log_max_files", value)?,
            "log_max_bytes" => self.max_bytes = non_empty(),
            "console_stream" => {
                self.console_stream = match value.to_ascii_lowercase().as_str() {
                    "stdout" => ConsoleStream::Stdout,
                    "stderr" => ConsoleStream::Stderr,
                    _ => {
                        return Err(LoggingConfigError::InvalidValue {
                            var: "console_stream",
                            value: value.to_string(),
                            expected: "stdout, stderr",
                        });
                    }
                }
            }
            other => return Err(LoggingConfigError::UnknownKey(other.to_string())),
        }
        Ok(())
    }
}

/// The process environment, opened at this crate's one config boundary.
#[allow(
    clippy::disallowed_methods,
    reason = "config boundary: LoggingConfig is resolved from the process env here"
)]
fn process_env() -> cognee_config_core::ProcessEnv {
    cognee_config_core::ProcessEnv::new()
}

/// Map a bare Python logging level name to its `EnvFilter` spelling, so a
/// `LOG_LEVEL` shared with the Python SDK means the same thing here.
///
/// `EnvFilter` reads a bare word it does not know as a *target* name, so
/// `LOG_LEVEL=WARNING` used to become "everything from the `WARNING` target,
/// nothing else" and silenced every cognee log. Case-insensitive:
///
/// | Python | `EnvFilter` |
/// |---|---|
/// | `CRITICAL`, `FATAL`, `ERROR` | `error` |
/// | `WARNING`, `WARN` | `warn` |
/// | `INFO` | `info` |
/// | `DEBUG` | `debug` |
/// | `NOTSET`, `TRACE` | `trace` |
///
/// Anything else (`off`, a full directive such as `info,cognee=debug`) is
/// returned unchanged.
pub fn normalize_level_filter(value: &str) -> String {
    let trimmed = value.trim();
    let mapped = match trimmed.to_ascii_lowercase().as_str() {
        "critical" | "fatal" | "error" => "error",
        "warning" | "warn" => "warn",
        "info" => "info",
        "debug" => "debug",
        "notset" | "trace" => "trace",
        _ => return trimmed.to_string(),
    };
    mapped.to_string()
}

fn parse_rotation(var: &'static str, raw: String) -> Result<LogRotation, LoggingConfigError> {
    match raw.to_ascii_lowercase().as_str() {
        "daily" => Ok(LogRotation::Daily),
        "hourly" => Ok(LogRotation::Hourly),
        "minutely" => Ok(LogRotation::Minutely),
        "never" => Ok(LogRotation::Never),
        _ => Err(LoggingConfigError::InvalidValue {
            var,
            value: raw,
            expected: "daily, hourly, minutely, never",
        }),
    }
}

fn parse_format(var: &'static str, raw: String) -> Result<LogFormat, LoggingConfigError> {
    match raw.to_ascii_lowercase().as_str() {
        "plain" => Ok(LogFormat::Plain),
        "json" => Ok(LogFormat::Json),
        _ => Err(LoggingConfigError::InvalidValue {
            var,
            value: raw,
            expected: "plain, json",
        }),
    }
}

fn parse_usize_value(var: &'static str, value: &str) -> Result<usize, LoggingConfigError> {
    value
        .parse::<usize>()
        .map_err(|source| LoggingConfigError::InvalidInt { var, source })
}

/// Python-compatible boolean (`logging_utils.py:501`): anything other than
/// `false`/`0`/`no` (case-insensitive) is truthy.
fn parse_bool_value(value: &str) -> bool {
    !matches!(value.to_ascii_lowercase().as_str(), "false" | "0" | "no")
}

/// Read an env var, returning `None` for unset *or* empty values.
///
/// Python treats `""` and unset the same way (`os.getenv(name)` is
/// truthy-checked); we follow suit so e.g. `COGNEE_LOGS_DIR=` does
/// not produce a `PathBuf::from("")`.
fn read_nonempty(env: &dyn EnvSource, name: &str) -> Option<String> {
    env.get(name).filter(|value| !value.is_empty())
}

/// Parse a boolean env var with the Python-compatible semantics in
/// `logging_utils.py:501`: anything other than `false`/`0`/`no`
/// (case-insensitive) is treated as truthy. Unset → `default`.
fn parse_bool(env: &dyn EnvSource, name: &str, default: bool) -> bool {
    match read_nonempty(env, name) {
        None => default,
        Some(value) => parse_bool_value(&value),
    }
}

/// Parse a `usize` env var, surfacing parse failures as
/// [`LoggingConfigError::InvalidInt`].
fn parse_usize(
    env: &dyn EnvSource,
    name: &'static str,
    default: usize,
) -> Result<usize, LoggingConfigError> {
    match read_nonempty(env, name) {
        None => Ok(default),
        Some(value) => parse_usize_value(name, &value),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
#[allow(
    clippy::disallowed_methods,
    reason = "tests drive configuration through the process env"
)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::ffi::OsString;

    /// Env vars consulted by `LoggingConfig::from_env`. The test
    /// guard snapshots and restores all of these around each test to
    /// keep mutations from leaking between cases (and from leaking
    /// the caller's real env into the test).
    const TRACKED_VARS: &[&str] = &[
        "COGNEE_LOG_FILE",
        "COGNEE_LOGS_DIR",
        "LOG_FILE_NAME",
        "COGNEE_LOG_ROTATION",
        "COGNEE_LOG_FORMAT",
        "COGNEE_LOG_BACKUP_COUNT",
        "COGNEE_LOG_MAX_FILES",
        "RUST_LOG",
        "LOG_LEVEL",
    ];

    /// RAII guard: snapshots `TRACKED_VARS` on construction, clears
    /// them, then restores them on drop. Avoids `temp_env`/`figment`
    /// deps and makes the test self-contained.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let saved: Vec<(&'static str, Option<OsString>)> = TRACKED_VARS
                .iter()
                .map(|name| (*name, env::var_os(name)))
                .collect();
            for name in TRACKED_VARS {
                // SAFETY: this is the env-mutation seam for tests
                // and every test using this guard is
                // `#[serial_test::serial]`, so no other thread is
                // racing on env state.
                unsafe {
                    env::remove_var(name);
                }
            }
            Self { saved }
        }

        fn set(&self, name: &'static str, value: &str) {
            // Must be a tracked var, otherwise restore will not
            // unset it.
            assert!(
                TRACKED_VARS.contains(&name),
                "test set untracked env var {name}"
            );
            // SAFETY: see `new` — serial tests, single thread.
            unsafe {
                env::set_var(name, value);
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                // SAFETY: see `EnvGuard::new`.
                unsafe {
                    match value {
                        Some(v) => env::set_var(name, v),
                        None => env::remove_var(name),
                    }
                }
            }
        }
    }

    #[test]
    #[serial]
    fn defaults_when_env_empty() {
        let _g = EnvGuard::new();
        let cfg = LoggingConfig::from_env().expect("defaults must parse");
        assert!(cfg.file_enabled);
        assert!(cfg.logs_dir_override.is_none());
        assert!(cfg.log_file_name.is_none());
        assert_eq!(cfg.rotation, LogRotation::Daily);
        assert_eq!(cfg.format, LogFormat::Plain);
        assert_eq!(cfg.backup_count, 5);
        assert_eq!(cfg.max_files, 10);
        assert!(cfg.level_filter.is_none());
    }

    #[test]
    #[serial]
    fn cognee_log_file_falsy_values_disable_file_sink() {
        for raw in ["false", "FALSE", "False", "0", "no", "NO", "No"] {
            let g = EnvGuard::new();
            g.set("COGNEE_LOG_FILE", raw);
            let cfg = LoggingConfig::from_env().expect("parses");
            assert!(
                !cfg.file_enabled,
                "COGNEE_LOG_FILE={raw:?} should disable file logging"
            );
            drop(g);
        }
    }

    #[test]
    #[serial]
    fn cognee_log_file_truthy_values_enable_file_sink() {
        for raw in ["true", "1", "yes", "on", "anything-else"] {
            let g = EnvGuard::new();
            g.set("COGNEE_LOG_FILE", raw);
            let cfg = LoggingConfig::from_env().expect("parses");
            assert!(
                cfg.file_enabled,
                "COGNEE_LOG_FILE={raw:?} should enable file logging"
            );
            drop(g);
        }
    }

    #[test]
    #[serial]
    fn cognee_logs_dir_is_captured_as_pathbuf() {
        let g = EnvGuard::new();
        g.set("COGNEE_LOGS_DIR", "/tmp/foo");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert_eq!(cfg.logs_dir_override, Some(PathBuf::from("/tmp/foo")));
    }

    #[test]
    #[serial]
    fn empty_logs_dir_is_treated_as_unset() {
        let g = EnvGuard::new();
        g.set("COGNEE_LOGS_DIR", "");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert!(cfg.logs_dir_override.is_none());
    }

    #[test]
    #[serial]
    fn log_file_name_is_captured_as_pathbuf() {
        let g = EnvGuard::new();
        g.set("LOG_FILE_NAME", "/tmp/foo/x.log");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert_eq!(cfg.log_file_name, Some(PathBuf::from("/tmp/foo/x.log")));
    }

    #[test]
    #[serial]
    fn rotation_accepts_all_variants_case_insensitive() {
        for (raw, expected) in [
            ("daily", LogRotation::Daily),
            ("DAILY", LogRotation::Daily),
            ("hourly", LogRotation::Hourly),
            ("Hourly", LogRotation::Hourly),
            ("minutely", LogRotation::Minutely),
            ("never", LogRotation::Never),
        ] {
            let g = EnvGuard::new();
            g.set("COGNEE_LOG_ROTATION", raw);
            let cfg = LoggingConfig::from_env().expect("parses");
            assert_eq!(cfg.rotation, expected, "raw={raw:?}");
            drop(g);
        }
    }

    #[test]
    #[serial]
    fn invalid_rotation_value_errors() {
        let g = EnvGuard::new();
        g.set("COGNEE_LOG_ROTATION", "weekly");
        let err = LoggingConfig::from_env().expect_err("invalid value should error");
        match err {
            LoggingConfigError::InvalidValue { var, value, .. } => {
                assert_eq!(var, "COGNEE_LOG_ROTATION");
                assert_eq!(value, "weekly");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
        drop(g);
    }

    #[test]
    #[serial]
    fn format_accepts_plain_and_json() {
        let g = EnvGuard::new();
        g.set("COGNEE_LOG_FORMAT", "json");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert_eq!(cfg.format, LogFormat::Json);
        drop(g);

        let g = EnvGuard::new();
        g.set("COGNEE_LOG_FORMAT", "PLAIN");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert_eq!(cfg.format, LogFormat::Plain);
        drop(g);
    }

    #[test]
    #[serial]
    fn invalid_format_value_errors() {
        let g = EnvGuard::new();
        g.set("COGNEE_LOG_FORMAT", "yaml");
        let err = LoggingConfig::from_env().expect_err("invalid value should error");
        assert!(matches!(
            err,
            LoggingConfigError::InvalidValue {
                var: "COGNEE_LOG_FORMAT",
                ..
            }
        ));
        drop(g);
    }

    #[test]
    #[serial]
    fn backup_count_parses_integer() {
        let g = EnvGuard::new();
        g.set("COGNEE_LOG_BACKUP_COUNT", "3");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert_eq!(cfg.backup_count, 3);
    }

    #[test]
    #[serial]
    fn backup_count_non_integer_errors() {
        let g = EnvGuard::new();
        g.set("COGNEE_LOG_BACKUP_COUNT", "abc");
        let err = LoggingConfig::from_env().expect_err("non-int should error");
        assert!(matches!(
            err,
            LoggingConfigError::InvalidInt {
                var: "COGNEE_LOG_BACKUP_COUNT",
                ..
            }
        ));
        drop(g);
    }

    #[test]
    #[serial]
    fn max_files_parses_integer() {
        let g = EnvGuard::new();
        g.set("COGNEE_LOG_MAX_FILES", "20");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert_eq!(cfg.max_files, 20);
    }

    #[test]
    #[serial]
    fn rust_log_populates_level_filter() {
        let g = EnvGuard::new();
        g.set("RUST_LOG", "debug,foo=warn");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert_eq!(cfg.level_filter.as_deref(), Some("debug,foo=warn"));
        drop(g);
    }

    #[test]
    #[serial]
    fn log_level_used_when_rust_log_unset() {
        let g = EnvGuard::new();
        g.set("LOG_LEVEL", "DEBUG");
        let cfg = LoggingConfig::from_env().expect("parses");
        // Python level names are normalized to EnvFilter spelling.
        assert_eq!(cfg.level_filter.as_deref(), Some("debug"));
        drop(g);
    }

    #[test]
    #[serial]
    fn rust_log_wins_over_log_level() {
        let g = EnvGuard::new();
        g.set("RUST_LOG", "trace");
        g.set("LOG_LEVEL", "DEBUG");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert_eq!(cfg.level_filter.as_deref(), Some("trace"));
        drop(g);
    }

    #[test]
    #[serial]
    fn empty_rust_log_falls_back_to_log_level() {
        // Mirrors the empty-string behavior of `read_nonempty` —
        // `RUST_LOG=""` should not shadow `LOG_LEVEL`.
        let g = EnvGuard::new();
        g.set("RUST_LOG", "");
        g.set("LOG_LEVEL", "warn");
        let cfg = LoggingConfig::from_env().expect("parses");
        assert_eq!(cfg.level_filter.as_deref(), Some("warn"));
        drop(g);
    }

    #[test]
    #[serial]
    fn config_is_clone_and_debug() {
        // Compile-time check that `LoggingConfig` keeps `Clone +
        // Debug` (the runbook's only hard public-API requirement at
        // this stage).
        let _g = EnvGuard::new();
        let cfg = LoggingConfig::from_env().expect("parses");
        let cloned = cfg.clone();
        let _ = format!("{cloned:?}");
    }

    /// `from_env_source` reads only the source it is given, so it needs no
    /// process-env mutation and no `#[serial]`.
    #[test]
    fn from_env_source_reads_only_the_given_source() {
        let env = cognee_config_core::MapEnv::new()
            .with("COGNEE_LOG_FILE", "false")
            .with("COGNEE_LOG_ROTATION", "Hourly")
            .with("COGNEE_LOG_BACKUP_COUNT", "3")
            .with("COGNEE_LOGS_DIR", "")
            .with("LOG_LEVEL", "debug");
        let cfg = LoggingConfig::from_env_source(&env).expect("parses");
        assert!(!cfg.file_enabled);
        assert_eq!(cfg.rotation, LogRotation::Hourly);
        assert_eq!(cfg.backup_count, 3);
        assert_eq!(cfg.logs_dir_override, None, "empty means unset");
        assert_eq!(cfg.level_filter.as_deref(), Some("debug"));
    }

    /// Python level names (a `LOG_LEVEL` shared with the Python SDK) map to
    /// `EnvFilter` levels instead of being read as a target name.
    #[test]
    fn python_level_names_are_normalized() {
        for (raw, want) in [
            ("WARNING", "warn"),
            ("warning", "warn"),
            ("CRITICAL", "error"),
            ("FATAL", "error"),
            ("ERROR", "error"),
            ("INFO", "info"),
            ("DEBUG", "debug"),
            ("NOTSET", "trace"),
            ("info,cognee=debug", "info,cognee=debug"),
        ] {
            let env = cognee_config_core::MapEnv::new().with("LOG_LEVEL", raw);
            let cfg = LoggingConfig::from_env_source(&env).expect("parses");
            assert_eq!(cfg.level_filter.as_deref(), Some(want), "LOG_LEVEL={raw}");
            let mut cfg = LoggingConfig::defaults();
            cfg.set("log_level", raw).expect("known key");
            assert_eq!(
                cfg.level_filter.as_deref(),
                Some(want),
                "set log_level={raw}"
            );
        }
        // RUST_LOG is an EnvFilter directive already: passed through as is.
        let env = cognee_config_core::MapEnv::new().with("RUST_LOG", "warn");
        assert_eq!(
            LoggingConfig::level_filter_from_env_source(&env).as_deref(),
            Some("warn")
        );
    }

    /// The level alone survives a malformed, unrelated logging variable.
    #[test]
    fn level_filter_ignores_unrelated_bad_vars() {
        let env = cognee_config_core::MapEnv::new()
            .with("RUST_LOG", "debug")
            .with("COGNEE_LOG_ROTATION", "weekly")
            .with("COGNEE_LOG_BACKUP_COUNT", "abc");
        assert!(LoggingConfig::from_env_source(&env).is_err());
        assert_eq!(
            LoggingConfig::level_filter_from_env_source(&env).as_deref(),
            Some("debug")
        );
    }

    /// The resulting filter actually lets `warn` events through for a
    /// cognee target (the regression: a bare `WARNING` became a target name).
    #[test]
    fn normalized_warning_enables_warn_for_every_target() {
        use tracing_subscriber::layer::SubscriberExt;
        let level = normalize_level_filter("WARNING");
        let subscriber = tracing_subscriber::registry().with(crate::env_filter(Some(&level)));
        tracing::subscriber::with_default(subscriber, || {
            assert!(tracing::enabled!(target: "cognee_cognify", tracing::Level::WARN));
            assert!(!tracing::enabled!(target: "cognee_cognify", tracing::Level::INFO));
        });
    }

    #[test]
    fn max_bytes_is_captured_not_honoured() {
        let env = cognee_config_core::MapEnv::new().with("COGNEE_LOG_MAX_BYTES", "1048576");
        let cfg = LoggingConfig::from_env_source(&env).expect("parses");
        assert_eq!(cfg.max_bytes.as_deref(), Some("1048576"));
    }

    /// `set` wins over the environment, by key, for every field.
    #[test]
    fn set_overrides_env_values() {
        let env = cognee_config_core::MapEnv::new()
            .with("LOG_LEVEL", "warn")
            .with("COGNEE_LOGS_DIR", "/from/env");
        let mut cfg = LoggingConfig::from_env_source(&env).expect("parses");
        cfg.set("log_level", "debug").expect("known key");
        cfg.set("logs_root_directory", "/from/code")
            .expect("known key");
        cfg.set("log_file", "no").expect("known key");
        cfg.set("log_file_name", "/tmp/x.log").expect("known key");
        cfg.set("log_rotation", "Hourly").expect("known key");
        cfg.set("log_format", "json").expect("known key");
        cfg.set("log_backup_count", "2").expect("known key");
        cfg.set("log_max_files", "3").expect("known key");
        cfg.set("console_stream", "stderr").expect("known key");
        assert_eq!(cfg.level_filter.as_deref(), Some("debug"));
        assert_eq!(cfg.logs_dir_override, Some(PathBuf::from("/from/code")));
        assert!(!cfg.file_enabled);
        assert_eq!(cfg.log_file_name, Some(PathBuf::from("/tmp/x.log")));
        assert_eq!(cfg.rotation, LogRotation::Hourly);
        assert_eq!(cfg.format, LogFormat::Json);
        assert_eq!(cfg.backup_count, 2);
        assert_eq!(cfg.max_files, 3);
        assert_eq!(cfg.console_stream, ConsoleStream::Stderr);

        // Empty restores the default for the optional ones.
        cfg.set("log_level", "").expect("known key");
        assert_eq!(cfg.level_filter, None);

        assert!(matches!(
            cfg.set("log_rotation", "weekly"),
            Err(LoggingConfigError::InvalidValue {
                var: "log_rotation",
                ..
            })
        ));
        assert!(matches!(
            cfg.set("log_max_files", "x"),
            Err(LoggingConfigError::InvalidInt {
                var: "log_max_files",
                ..
            })
        ));
        assert!(matches!(
            cfg.set("nope", "x"),
            Err(LoggingConfigError::UnknownKey(k)) if k == "nope"
        ));
    }
}
