//! Default stderr `tracing` subscriber for the Neon binding (gap 07).
//!
//! Installed automatically when the `cdylib` is first loaded by Node,
//! before any exported function is registered. Honours
//! `COGNEE_BINDING_SUPPRESS_LOGS=<any non-empty>` as an opt-out for
//! hosts that own their own logger.
//!
//! Composes with the explicit `setupLogging()` (gap 06) which adds
//! the rotating file appender on top via `tracing-subscriber`'s
//! `try_init` semantics — whichever subscriber claims the global
//! slot first wins, the loser becomes a no-op. The default subscriber
//! is the "events are never silently dropped" baseline.

use std::sync::Once;

use tracing_subscriber::{EnvFilter, fmt};

static INIT: Once = Once::new();

/// Install the default stderr `fmt` subscriber.
///
/// Idempotent: subsequent calls are no-ops (guarded by [`Once`]).
/// Returns silently when `COGNEE_BINDING_SUPPRESS_LOGS` is set to any
/// non-empty value, or when another subscriber has already claimed the
/// global `tracing` slot (e.g. `setupLogging()` ran first).
pub(crate) fn install() {
    INIT.call_once(|| {
        if std::env::var_os("COGNEE_BINDING_SUPPRESS_LOGS")
            .filter(|v| !v.is_empty())
            .is_some()
        {
            return;
        }

        // Reuse cognee-logging's default filter so Node hosts see the
        // same `info,ort=warn,reqwest=warn,…` baseline as the CLI
        // binary. The crate is already a dependency for `setupLogging`
        // (gap 06 task 08), so calling the helper avoids duplicating
        // the literal and prevents drift.
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(cognee_logging::default_filter()));

        // `try_init` rather than `init` — if `setupLogging()` (gap 06)
        // or any other code installed a subscriber first, we soft-fail
        // and let that subscriber own the global slot. Matches PyO3
        // semantics from 07-02.
        let _ = fmt()
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_env_filter(filter)
            .try_init();
    });
}
