//! Shared async lifecycle operations: `flush`.
//!
//! The half of teardown that is not a teardown. `close` releases resources and
//! makes the handle unusable; `flush` makes what has been written durable and
//! leaves everything running. An embedded host (a phone) needs the second far
//! more often than the first, and must never be tempted to use the first for
//! the second — closing the engine under a live pipeline shuts the database pool
//! beneath it and wedges the dataset for 24 h, which is how this whole area got
//! its scar tissue.

use std::time::Instant;

use serde_json::json;

use crate::{HandleState, SdkError};

/// Make everything written so far durable, without closing anything.
///
/// Today this is the graph store, which is the only component that buffers
/// anything a restart would lose: an embedded ladybug graph keeps committed
/// transactions in an un-checkpointed `.wal` and only folds them into the main
/// database file at a checkpoint. The relational pool runs with
/// `synchronous = FULL` and the vector store flushes after every write, so both
/// are already durable by the time an op returns.
///
/// # Contract
///
/// - **Never builds the engine.** A handle that was never warmed returns
///   `{"flushed": false, "warm": false}` immediately, having touched nothing. A
///   caller on a latency-sensitive path (an app being backgrounded) can call
///   this unconditionally without risking a cold ONNX load.
/// - **Never fails because the flush did not happen.** A checkpoint legitimately
///   fails on a read-only database and whenever a read is in flight; neither is
///   an error the caller can act on, and neither should surface to a user who
///   merely pressed Home. The outcome is reported in the payload, not as an
///   `Err`. An `Err` here means the op could not be dispatched at all.
/// - **Not a close.** The handle stays warm and fully usable; the next op
///   serves normally.
/// - **Idempotent**, and cheap when there is nothing to flush.
///
/// Returns `{"flushed": bool, "warm": bool, "ms": u64}` — `ms` measured so a
/// host that calls this on a UI lifecycle callback can see what it costs.
pub async fn flush(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let started = Instant::now();

    // Deliberately `services_if_warm`, not `services`: see the contract above.
    let Some(svc) = state.services_if_warm().await else {
        return Ok(json!({
            "flushed": false,
            "warm": false,
            "ms": started.elapsed().as_millis() as u64,
        }));
    };

    let flushed = match svc.graph_db.flush().await {
        Ok(()) => true,
        Err(e) => {
            // The adapter already downgrades a failed checkpoint to a warning;
            // reaching here means the flush could not even be attempted. Still
            // not an error for the caller — there is nothing it could do — but
            // it must be visible.
            tracing::warn!(error = %e, "graph flush could not be attempted");
            false
        }
    };

    let ms = started.elapsed().as_millis() as u64;
    tracing::debug!(flushed, ms, "flush complete");
    Ok(json!({ "flushed": flushed, "warm": true, "ms": ms }))
}
