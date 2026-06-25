//! Python entrypoint for cognee's file-based logging subsystem.
//!
//! Exposes a single argument-less `setup_logging()` function on the
//! `_native` extension module. All configuration is taken from
//! environment variables (`COGNEE_LOG_*`, `LOG_FILE_NAME`, `LOG_LEVEL`,
//! `RUST_LOG`) — see [`cognee_logging::LoggingConfig::from_env`].
//!
//! The returned [`LogGuards`] are stashed in a process-global
//! [`OnceLock`] so the non-blocking file appender's worker thread is
//! kept alive for the lifetime of the Python interpreter, and so
//! second and later calls are idempotent no-ops.

use std::sync::{Mutex, OnceLock};

use cognee_logging::{LogGuards, LoggingConfig, init_logging};
use pyo3::prelude::*;

static GUARDS: OnceLock<Mutex<Option<LogGuards>>> = OnceLock::new();

/// Initialize cognee's logging subsystem from environment variables.
///
/// All configuration is via env vars (set them *before* calling this
/// function): `COGNEE_LOG_FILE`, `COGNEE_LOGS_DIR`, `LOG_FILE_NAME`,
/// `COGNEE_LOG_ROTATION`, `COGNEE_LOG_FORMAT`,
/// `COGNEE_LOG_BACKUP_COUNT`, `LOG_LEVEL`, `RUST_LOG`.
///
/// Calling this function more than once is a no-op (first call wins);
/// the worker thread that flushes log lines to disk is held in a
/// process-global singleton.
#[pyfunction]
pub fn setup_logging() -> PyResult<()> {
    let slot = GUARDS.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(()); // idempotent
    }

    let cfg = LoggingConfig::from_env().map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("invalid logging config: {e}"))
    })?;
    let guards = init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    *lock = Some(guards);
    Ok(())
}
