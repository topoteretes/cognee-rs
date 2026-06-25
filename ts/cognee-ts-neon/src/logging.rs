//! JS / Node entrypoint for cognee's file-based logging subsystem.
//!
//! Exposes a single argument-less `setupLogging()` function. All
//! configuration is read from environment variables — see
//! [`cognee_logging::LoggingConfig::from_env`].
//!
//! The returned [`LogGuards`] are stashed in a process-global
//! [`OnceLock`] so the non-blocking file appender's worker thread
//! lives for the duration of the Node process and second-and-later
//! calls are no-ops.

use std::sync::{Mutex, OnceLock};

use cognee_logging::{LogGuards, LoggingConfig, init_logging};
use neon::prelude::*;

static GUARDS: OnceLock<Mutex<Option<LogGuards>>> = OnceLock::new();

/// Initialize cognee's logging subsystem from environment variables.
///
/// All configuration is via env vars (`COGNEE_LOG_*`, `LOG_FILE_NAME`,
/// `LOG_LEVEL`, `RUST_LOG`); set them before calling. Calling this
/// function more than once is a no-op (idempotent).
pub fn setup_logging(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let slot = GUARDS.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(cx.undefined()); // idempotent
    }

    let cfg = LoggingConfig::from_env()
        .or_else(|err| cx.throw_error(format!("invalid logging config: {err}")))?;
    let guards = init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    *lock = Some(guards);
    Ok(cx.undefined())
}
