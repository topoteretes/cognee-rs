//! C FFI entrypoint for cognee's file-based logging subsystem.
//!
//! Exposes a single argument-less function, `cognee_setup_logging()`,
//! that reads configuration from environment variables — see
//! [`cognee_logging::LoggingConfig::from_env`] — and installs the
//! global `tracing` subscriber.
//!
//! Return codes:
//! - `0` — success (including the idempotent no-op when called twice).
//! - `1` — internal lock poisoning (should not happen in practice).
//! - `2` — invalid configuration (an env var has a malformed value).
//!
//! The returned [`LogGuards`] are stashed in a process-global
//! [`OnceLock`] so the non-blocking file appender's worker thread
//! lives for the full lifetime of the C host process.

use std::ffi::c_int;
use std::sync::{Mutex, OnceLock};

use cognee_logging::{LogGuards, LoggingConfig, init_logging};

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
    let slot = GUARDS.get_or_init(|| Mutex::new(None));
    let mut lock = match slot.lock() {
        Ok(l) => l,
        // lock poison is unrecoverable
        Err(_) => return 1,
    };
    if lock.is_some() {
        return 0; // idempotent
    }

    let cfg = match LoggingConfig::from_env() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("cognee_setup_logging: {err}");
            return 2;
        }
    };
    let guards = init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    *lock = Some(guards);
    0
}
