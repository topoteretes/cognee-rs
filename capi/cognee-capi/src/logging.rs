//! C FFI entrypoints for cognee's file-based logging subsystem.
//!
//! * `cognee_setup_logging()` — configuration from environment variables
//!   only.
//! * `cognee_setup_logging_with_options(options_json)` — the same, with a
//!   JSON object of options that wins over the environment.
//!
//! Logging is process config: it is initialised once, before any `CgSdk`
//! handle exists, so it does not read a handle's settings. Both resolve
//! defaults < environment < options through the path every binding shares
//! ([`cognee_bindings_common::process_config`]).
//!
//! Return codes:
//! - `0` — success (including the idempotent no-op when called twice).
//! - `1` — internal lock poisoning (should not happen in practice).
//! - `2` — invalid configuration (a malformed env var, invalid options JSON,
//!   an unknown key or an invalid value); the message is available from
//!   `cg_last_error_message()`.
//!
//! The returned [`LogGuards`] are stashed in a process-global
//! [`OnceLock`] so the non-blocking file appender's worker thread
//! lives for the full lifetime of the C host process.

use std::ffi::{c_char, c_int};
use std::sync::{Mutex, OnceLock};

use cognee_logging::{LogGuards, init_logging};

use crate::error::set_last_error;

static GUARDS: OnceLock<Mutex<Option<LogGuards>>> = OnceLock::new();

/// Initialize cognee's logging subsystem from environment variables.
///
/// Returns 0 on success (including idempotent re-call), non-zero on
/// configuration error (an invalid env-var value).
///
/// Safe to call multiple times; the second and later calls are
/// no-ops and return 0.
///
/// # Safety
///
/// This function takes no arguments and is safe to call from any C
/// thread. All synchronization is internal.
#[unsafe(no_mangle)]
pub extern "C" fn cognee_setup_logging() -> c_int {
    setup(None)
}

/// Initialize cognee's logging subsystem with options that win over the
/// environment.
///
/// `options_json` is a JSON object (or `NULL`) with any of the keys
/// `log_level`, `logs_root_directory`, `log_file`, `log_file_name`,
/// `log_rotation`, `log_format`, `log_backup_count`, `log_max_files`,
/// `console_stream`. Returns as [`cognee_setup_logging`]; the first call of
/// either function wins.
///
/// # Safety
///
/// `options_json` must be `NULL` or a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cognee_setup_logging_with_options(options_json: *const c_char) -> c_int {
    // SAFETY: forwarded caller contract.
    match unsafe { crate::telemetry_otlp::options_from_c(options_json) } {
        Ok(options) => setup(options.as_ref()),
        Err(()) => 2,
    }
}

fn setup(options: Option<&serde_json::Value>) -> c_int {
    let slot = GUARDS.get_or_init(|| Mutex::new(None));
    let mut lock = match slot.lock() {
        Ok(l) => l,
        // lock poison is unrecoverable
        Err(_) => return 1,
    };
    if lock.is_some() {
        return 0; // idempotent
    }

    let cfg = match cognee_bindings_common::process_config::logging_config(options) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("cognee_setup_logging: {err}");
            set_last_error(err);
            return 2;
        }
    };
    let guards = init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    *lock = Some(guards);
    0
}
