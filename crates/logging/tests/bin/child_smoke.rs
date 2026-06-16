//! Tiny helper binary for `tests/multi_process_inheritance.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "binary test helper — panics are acceptable failures"
)]
//!
//! Calls [`cognee_logging::init_logging`] with whatever
//! [`cognee_logging::LoggingConfig::from_env`] produces (the parent
//! test sets `COGNEE_LOGS_DIR` and `LOG_FILE_NAME` for us via env
//! inheritance), emits one `info!` line tagged with this child's PID,
//! and returns.
//!
//! Decision 5: by reading `LOG_FILE_NAME` from the inherited env, this
//! child appends to the parent's file. No coordination beyond the env
//! var is required.

fn main() {
    let cfg = cognee_logging::LoggingConfig::from_env()
        .expect("child process: LoggingConfig::from_env must parse the parent's env");
    let _guard =
        cognee_logging::init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    tracing::info!(pid = std::process::id(), "child emitted");
    // Drop `_guard` here so the non-blocking writer flushes the line
    // before the process exits.
}
