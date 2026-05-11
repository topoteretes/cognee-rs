//! Env-var-driven configuration for `cognee-logging`.
//!
//! [`LoggingConfig::from_env`] parses every env var documented in
//! `docs/telemetry/06-file-logging-rotation.md` §"Env Var Catalog"
//! into a plain-old data struct. It does **not** install a subscriber,
//! resolve filesystem paths, or emit `tracing` events — those concerns
//! land in tasks 06-03 / 06-04 / 06-05.
//!
//! Note: `COGNEE_LOG_MAX_BYTES` is intentionally **not** parsed here.
//! Per decision 1 in the parent doc, size-based rotation is deferred
//! for v1 and the variable is accepted as a documented no-op. The
//! init layer (task 06-05) is responsible for any warn-on-set
//! behavior; keeping `from_env` free of `tracing::*` calls lets unit
//! tests run without a subscriber.

use std::env;
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
}

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
    /// Resolved `EnvFilter` directive string built from `RUST_LOG`
    /// (preferred) or `LOG_LEVEL` (fallback). `None` means neither
    /// env var was set, and the init layer should substitute the
    /// default library-noise-suppressing filter (decision 6).
    pub level_filter: Option<String>,
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
            level_filter: None,
        }
    }

    /// Build a [`LoggingConfig`] by reading the env-var surface
    /// documented on the struct.
    ///
    /// Returns an error only when an env var carries a malformed
    /// value (e.g. `COGNEE_LOG_ROTATION=weekly`,
    /// `COGNEE_LOG_BACKUP_COUNT=abc`). Unset env vars fall back to
    /// the documented defaults.
    pub fn from_env() -> Result<Self, LoggingConfigError> {
        let file_enabled = parse_bool("COGNEE_LOG_FILE", true);

        let logs_dir_override = read_nonempty("COGNEE_LOGS_DIR").map(PathBuf::from);
        let log_file_name = read_nonempty("LOG_FILE_NAME").map(PathBuf::from);

        let rotation = match read_nonempty("COGNEE_LOG_ROTATION") {
            None => LogRotation::Daily,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "daily" => LogRotation::Daily,
                "hourly" => LogRotation::Hourly,
                "minutely" => LogRotation::Minutely,
                "never" => LogRotation::Never,
                _ => {
                    return Err(LoggingConfigError::InvalidValue {
                        var: "COGNEE_LOG_ROTATION",
                        value: raw,
                        expected: "daily, hourly, minutely, never",
                    });
                }
            },
        };

        let format = match read_nonempty("COGNEE_LOG_FORMAT") {
            None => LogFormat::Plain,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "plain" => LogFormat::Plain,
                "json" => LogFormat::Json,
                _ => {
                    return Err(LoggingConfigError::InvalidValue {
                        var: "COGNEE_LOG_FORMAT",
                        value: raw,
                        expected: "plain, json",
                    });
                }
            },
        };

        let backup_count = parse_usize("COGNEE_LOG_BACKUP_COUNT", 5)?;
        let max_files = parse_usize("COGNEE_LOG_MAX_FILES", 10)?;

        // Decision 7: RUST_LOG wins over LOG_LEVEL. When only
        // LOG_LEVEL is set, surface it verbatim so the init layer
        // (task 06-05) can feed it into `EnvFilter::new` as a bare
        // level. When both are unset, leave `None` so init substitutes
        // `default_filter()` (decision 6).
        let level_filter = read_nonempty("RUST_LOG").or_else(|| read_nonempty("LOG_LEVEL"));

        Ok(Self {
            file_enabled,
            logs_dir_override,
            log_file_name,
            rotation,
            format,
            backup_count,
            max_files,
            level_filter,
        })
    }
}

/// Read an env var, returning `None` for unset *or* empty values.
///
/// Python treats `""` and unset the same way (`os.getenv(name)` is
/// truthy-checked); we follow suit so e.g. `COGNEE_LOGS_DIR=` does
/// not produce a `PathBuf::from("")`.
fn read_nonempty(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

/// Parse a boolean env var with the Python-compatible semantics in
/// `logging_utils.py:501`: anything other than `false`/`0`/`no`
/// (case-insensitive) is treated as truthy. Unset → `default`.
fn parse_bool(name: &str, default: bool) -> bool {
    match read_nonempty(name) {
        None => default,
        Some(value) => {
            let lower = value.to_ascii_lowercase();
            !matches!(lower.as_str(), "false" | "0" | "no")
        }
    }
}

/// Parse a `usize` env var, surfacing parse failures as
/// [`LoggingConfigError::InvalidInt`].
fn parse_usize(name: &'static str, default: usize) -> Result<usize, LoggingConfigError> {
    match read_nonempty(name) {
        None => Ok(default),
        Some(value) => value
            .parse::<usize>()
            .map_err(|source| LoggingConfigError::InvalidInt { var: name, source }),
    }
}

#[cfg(test)]
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
        assert_eq!(cfg.level_filter.as_deref(), Some("DEBUG"));
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
}
