//! Command runtime + teardown: the one place a CLI command's async work is
//! driven, and the only place its resources are released.
//!
//! Every command used to build its own runtime, run its work, and let the runtime
//! drop — which is where two defects lived:
//!
//! 1. **The release ran on the wrong runtime.** Closing the components from
//!    `main` after the command returned meant a *fresh* runtime, while the
//!    connections' in-flight `return_to_pool` tasks belonged to the command's
//!    runtime and had died with it. The relational close then could not settle,
//!    and SQLite kept the `-wal`/`-shm` sidecars that topoteretes/cognee-rs#132
//!    is about.
//! 2. **The telemetry flush was inert.** `send_telemetry` dispatches a detached
//!    task onto the current runtime; dropping that runtime cancels the POST and
//!    zeroes the in-flight counter, so a flush issued afterwards had nothing left
//!    to wait for and reported success. Measured against a stub collector: 0 of 1
//!    events delivered, while looking exactly like a working flush.
//!
//! Both are the same mistake — doing teardown work outside the runtime that owns
//! the work — so both are fixed by doing it here, inside `block_on`, before the
//! runtime goes away.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cognee::ComponentManager;

use crate::error::CliError;

/// How long the component close may take before the exit gives up on it.
///
/// A pool close waits for its connections to come back and an embedded graph
/// checkpoints synchronously, so this is not instant — but it must be finite: a
/// store that never finishes would otherwise cost the caller their exit code. What
/// a timeout costs is the sidecars we were trying to remove, which the next run
/// recovers.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long already-dispatched telemetry POSTs get to leave the process.
///
/// Small on purpose: an exit code must never wait on an analytics collector.
const TELEMETRY_FLUSH_TIMEOUT: Duration = Duration::from_millis(500);

/// How long the runtime's own shutdown may take once the work is done.
///
/// `Runtime::shutdown_timeout` rather than a plain drop, because a drop **joins
/// every blocking task** with no bound. When [`CLOSE_TIMEOUT`] fires, the
/// `spawn_blocking` destructors the close started (an ONNX session joining its
/// worker threads, an embedded graph checkpointing) are still running, and a plain
/// drop would sit and wait for exactly the work the timeout just gave up on —
/// making the bound decorative. Anything still running past this point is
/// abandoned to process exit.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Nesting depth of [`defer_teardown`] guards. Non-zero means some caller is
/// replaying several commands against one warm manager and will tear down itself.
static DEFERRED: AtomicUsize = AtomicUsize::new(0);

/// Suppress the per-command teardown until the returned guard drops.
///
/// For `run-sequence`, which parses a file of commands and runs them in order
/// against one manager: tearing down after every step would be correct but would
/// make each step pay a full re-warm — a fresh TLS handshake per HTTP engine and,
/// for the ONNX provider, a re-read of the model file — and that cost lands inside
/// the very timing the sequence file is describing. The caller tears down once at
/// the end instead ([`release_blocking`]).
#[must_use = "the teardown stays deferred only while the guard is alive"]
pub fn defer_teardown() -> DeferGuard {
    DEFERRED.fetch_add(1, Ordering::AcqRel);
    DeferGuard
}

/// Restores per-command teardown when dropped. See [`defer_teardown`].
pub struct DeferGuard;

impl Drop for DeferGuard {
    fn drop(&mut self) {
        DEFERRED.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Release the components from a synchronous context, on a runtime built for the
/// purpose.
///
/// The fallback path: it is what `main` calls after dispatch, so a command that
/// deferred its teardown (or never had one, like `config`) still releases exactly
/// once. Idempotent and cheap when the work is already done — closing an empty
/// cache is a no-op and an idle telemetry queue flushes instantly.
///
/// Prefer [`run_command`], which does the same work on the runtime that owns the
/// connections: their in-flight returns belong to that runtime, and a fresh one
/// cannot schedule them.
pub fn release_blocking(cm: &ComponentManager) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::debug!(%error, "could not build a runtime to release the components");
            return;
        }
    };
    runtime.block_on(release(cm));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
}

/// Drive one command's async work to completion, then release its resources — all
/// on the same runtime, which is then shut down under a bound.
///
/// Returns the command's own result: the teardown never changes the exit code, and
/// a teardown failure is logged rather than surfaced. The command's outcome is what
/// the caller asked about.
pub fn run_command<F, T>(cm: Arc<ComponentManager>, future: F) -> Result<T, CliError>
where
    F: Future<Output = Result<T, CliError>>,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    let outcome = runtime.block_on(async move {
        let outcome = future.await;
        // Release whether the command succeeded or not: a failed run has usually
        // opened the database too. Unless a caller deferred it — see
        // `defer_teardown`.
        //
        // Read `DEFERRED` *here*, after the command has finished, not before it
        // starts. A snapshot taken up front is stale the moment a `DeferGuard`
        // outlives the load but not the command: the decision would then skip a
        // release that is no longer deferred, which is the silent
        // never-torn-down case this module exists to remove. Today's only caller
        // (`run_sequence`) holds its guard across the whole dispatch, so the two
        // readings agree — this is about not depending on that.
        // Only the *close* is deferrable. The flush is not, and conflating the
        // two silently discarded telemetry: `run-sequence` (the sole caller of
        // `defer_teardown`) would skip `release` entirely per step, so each
        // step's detached POSTs were still on this runtime when
        // `shutdown_timeout` below cancelled them — and `main`'s fallback
        // `release_blocking` then flushed a *fresh* runtime with nothing left to
        // wait for. A 50-step sequence delivered zero events, which is exactly
        // the defect this module's header claims to have fixed. Whatever
        // dispatched a POST has to be the runtime that waits for it.
        if DEFERRED.load(Ordering::Acquire) == 0 {
            release(&cm).await;
        } else {
            flush_telemetry().await;
        }
        outcome
    });

    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    outcome
}

/// Close the components and flush telemetry, in that order, on the current
/// runtime.
///
/// Order matters: the close is the part with a user-visible consequence (files on
/// disk), and telemetry must never delay or fail it.
async fn release(cm: &ComponentManager) {
    if tokio::time::timeout(CLOSE_TIMEOUT, cm.close())
        .await
        .is_err()
    {
        tracing::debug!(
            "closing the components timed out after {CLOSE_TIMEOUT:?}; \
             a database's WAL sidecars may be left for the next run to recover"
        );
    }

    flush_telemetry().await;
}

/// Wait for the telemetry this runtime dispatched, bounded.
///
/// Split out of [`release`] because the two halves have different deferrability:
/// a caller replaying several commands against one warm manager can postpone the
/// *close* (see [`defer_teardown`]) but must never postpone the *flush* — the
/// POSTs are detached on the runtime that is about to be shut down, so nothing
/// else can wait for them.
async fn flush_telemetry() {
    if !cognee::cognee_telemetry::flush(TELEMETRY_FLUSH_TIMEOUT).await {
        tracing::debug!(
            "telemetry still in flight after {TELEMETRY_FLUSH_TIMEOUT:?}; \
             dropping the remainder rather than delaying the exit"
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// The runtime shutdown is bounded, so a blocking task that outlives the
    /// command cannot hold the exit open.
    ///
    /// A component destructor runs on the blocking pool (`spawn_blocking`), and
    /// when the close times out those destructors are still running. With a plain
    /// `drop(runtime)` the exit then waits for them without a bound — the timeout
    /// the caller asked for buys nothing. This asserts the bound is real.
    #[test]
    fn a_stuck_blocking_task_does_not_hold_the_exit_open() {
        let started = std::time::Instant::now();
        {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                // Stands in for a destructor that outlives the close: the pattern
                // `ComponentManager::close` uses for every slot.
                tokio::task::spawn_blocking(|| {
                    std::thread::sleep(Duration::from_secs(60));
                });
                // Let it start before we tear the runtime down.
                tokio::task::yield_now().await;
            });
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(30),
            "the runtime shutdown must be bounded, took {elapsed:?} — a plain drop \
             joins blocking tasks with no bound, which makes CLOSE_TIMEOUT decorative"
        );
    }
}
