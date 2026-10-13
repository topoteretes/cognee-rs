//! Shared logging setup for the cognee Rust SDK.
//!
//! `cognee-logging` is the single home of file-based logging with
//! rotation, the Python-compatible plain text formatter, and the
//! default library-noise-suppressing `EnvFilter`. Binaries and bindings
//! (Python / JS / C) call `init_logging` to install a global subscriber;
//! library crates **must not** depend on this crate. In this workspace the
//! binary is `cognee-cli`.

#![deny(missing_docs)]

mod config;
mod formatter;
mod init;
mod paths;

pub use config::{
    ConsoleStream, LOGGING_SETTING_KEYS, LogFormat, LogRotation, LoggingConfig, LoggingConfigError,
    normalize_level_filter,
};
pub use formatter::PythonPlainFormatter;
pub use init::{BoxedLayer, LogGuards, default_filter, env_filter, init_logging};
pub use paths::{cleanup_old_logs, propagate_log_file_name, resolve_logs_dir};
