//! JS / Node entrypoint for cognee's file-based logging subsystem.
//!
//! `setupLogging(options?)` installs the global `tracing` subscriber. Logging
//! is process config: it is initialised once, before any `Cognee` handle
//! exists, so it does not read a handle's settings. It resolves defaults <
//! environment (`COGNEE_LOG_*`, `LOG_FILE_NAME`, `LOG_LEVEL`, `RUST_LOG`) <
//! `options`, through the path every binding shares
//! ([`cognee_bindings_common::process_config`]).
//!
//! The returned [`LogGuards`] are stashed in a process-global
//! [`OnceLock`] so the non-blocking file appender's worker thread
//! lives for the duration of the Node process and second-and-later
//! calls are no-ops.

use std::sync::{Mutex, OnceLock};

use cognee_logging::{LogGuards, init_logging};
use neon::prelude::*;

use crate::json::read_opts;

static GUARDS: OnceLock<Mutex<Option<LogGuards>>> = OnceLock::new();

/// Initialize cognee's logging subsystem. `options` (optional object, snake_
/// or camelCase keys) wins over the environment. Calling this function more
/// than once is a no-op (idempotent).
pub fn setup_logging(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let slot = GUARDS.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if lock.is_some() {
        return Ok(cx.undefined()); // idempotent
    }

    let options = read_opts(&mut cx, 0)?;
    let cfg = cognee_bindings_common::process_config::logging_config(Some(&options))
        .or_else(|err| cx.throw_error(err))?;
    let guards = init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    *lock = Some(guards);
    Ok(cx.undefined())
}
