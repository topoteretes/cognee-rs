//! Python entrypoint for cognee's file-based logging subsystem.
//!
//! `setup_logging(options=None)` on the `_native` extension module installs
//! the global `tracing` subscriber. Logging is process config: it is
//! initialised once, before any `Cognee` handle exists, so it does not read a
//! handle's settings. It resolves defaults < environment (`COGNEE_LOG_*`,
//! `LOG_FILE_NAME`, `LOG_LEVEL`, `RUST_LOG`) < `options`, through the path
//! every binding shares ([`cognee_bindings_common::process_config`]).
//!
//! The returned [`LogGuards`] are stashed in a process-global
//! [`OnceLock`] so the non-blocking file appender's worker thread is
//! kept alive for the lifetime of the Python interpreter, and so
//! second and later calls are idempotent no-ops.

use std::sync::{Mutex, OnceLock};

use cognee_logging::{LogGuards, init_logging};
use pyo3::prelude::*;

use crate::json::py_to_serde;

static GUARDS: OnceLock<Mutex<Option<LogGuards>>> = OnceLock::new();

/// Initialize cognee's logging subsystem.
///
/// `options` (optional dict) wins over the environment. Keys: `log_level`,
/// `logs_root_directory`, `log_file`, `log_file_name`, `log_rotation`,
/// `log_format`, `log_backup_count`, `log_max_files`, `console_stream`.
///
/// Calling this function more than once is a no-op (first call wins);
/// the worker thread that flushes log lines to disk is held in a
/// process-global singleton.
#[pyfunction]
#[pyo3(signature = (options=None))]
pub fn setup_logging(options: Option<Bound<'_, PyAny>>) -> PyResult<()> {
    let slot = GUARDS.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(()); // idempotent
    }

    let options = options.as_ref().map(py_to_serde).transpose()?;
    let cfg = cognee_bindings_common::process_config::logging_config(options.as_ref())
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    let guards = init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    *lock = Some(guards);
    Ok(())
}
