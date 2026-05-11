//! Shared logging setup for the cognee Rust SDK.
//!
//! `cognee-logging` is the single home of file-based logging with
//! rotation, the Python-compatible plain text formatter, and the
//! default library-noise-suppressing `EnvFilter`. Binaries
//! (`cognee-cli`, `cognee-http-server`) and bindings (Python / JS /
//! C) call `init_logging` to install a global subscriber; library
//! crates **must not** depend on this crate.

#![deny(missing_docs)]

mod config;
mod formatter;
mod paths;
// Future modules — declared by sibling tasks:
// mod init;         // 06-05: init_logging + LogGuards + default_filter

pub use config::{LogFormat, LogRotation, LoggingConfig, LoggingConfigError};
pub use formatter::PythonPlainFormatter;
pub use paths::{cleanup_old_logs, propagate_log_file_name, resolve_logs_dir};
