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
// Future modules — declared by sibling tasks:
// mod paths;        // 06-03: resolve_logs_dir + propagate_log_file_name + cleanup_old_logs
// mod formatter;    // 06-04: PythonPlainFormatter
// mod init;         // 06-05: init_logging + LogGuards + default_filter

pub use config::{LogFormat, LogRotation, LoggingConfig, LoggingConfigError};
