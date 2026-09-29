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

use crate::services::CogneeServices;
use crate::{HandleState, SdkError};

/// Checkpoint the graph after an op that mutated it, and never let the result
/// change the op's own.
///
/// Every op that writes nodes or edges ends here. An embedded graph (ladybug)
/// keeps a run's writes in an un-checkpointed `.wal` and only folds them into
/// the main database file at a checkpoint; a host without an Android-style
/// `onPause` reaches one only by luck. Cognify checkpointing while a `forget`
/// does not is the shape that actually hurts: the delete is the write a user
/// explicitly asked for, and the one whose loss resurrects content they removed.
///
/// Deliberately *after* the op's result is in hand and deliberately infallible.
/// `GraphDBTrait::flush` reports a skipped or failed checkpoint as `Ok(false)`
/// and logs why; `Err` means it could not be attempted at all. Neither may turn
/// a completed delete into a failed one — the state a missed checkpoint leaves
/// behind is the state every such write was already in before this existed.
///
/// It is explicitly not a close: the handle stays warm, because closing the
/// store under a live pipeline is the worse bug this must not reintroduce.
pub(crate) async fn checkpoint_graph_after(svc: &CogneeServices, op: &'static str) {
    match svc.graph_db.flush().await {
        Ok(true) => tracing::debug!(op, "graph checkpointed"),
        Ok(false) => tracing::debug!(
            op,
            "graph checkpoint skipped; the write is committed but still only in the WAL"
        ),
        Err(e) => tracing::warn!(error = %e, op, "could not checkpoint the graph"),
    }
}

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
///   fails on a read-only database, and it is deliberately skipped when a graph
///   read is in flight (starting one there stalls every query for five seconds
///   and then fails anyway). Neither is an error the caller can act on, and
///   neither should surface to a user who merely pressed Home. The outcome is
///   reported in the payload, not as an `Err`. An `Err` here means the op could
///   not be dispatched at all.
/// - **`"flushed"` is the outcome, not the dispatch.** It is `true` only when
///   the store really is checkpointed. A skipped or failed checkpoint reports
///   `{"flushed": false, "warm": true}` — the call succeeded, the durability did
///   not happen, and a caller reading `flushed` gets the truth about its data.
/// - **Not a close.** The handle stays warm and fully usable; the next op
///   serves normally.
/// - **Idempotent**, and cheap when there is nothing to flush.
///
/// Returns `{"flushed": bool, "warm": bool, "ms": u64}` — `ms` measured so a
/// host that calls this on a UI lifecycle callback can see what it costs.
pub async fn flush(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let started = Instant::now();

    // Deliberately not `services`, which would build: see the contract above.
    // Equally deliberately `services_if_cached` rather than `services_if_warm` —
    // a bundle whose config version has moved is still cached, still holds the
    // live graph adapter, and is exactly the bundle whose un-checkpointed writes
    // are about to be stranded when it is replaced.
    let Some(svc) = state.services_if_cached().await else {
        return Ok(json!({
            "flushed": false,
            "warm": false,
            "ms": started.elapsed().as_millis() as u64,
        }));
    };

    let flushed = match svc.graph_db.flush().await {
        // The adapter reports the outcome, not the dispatch: `false` means no
        // checkpoint happened (read-only store, failed CHECKPOINT, or reads that
        // never went quiet). It has already logged why.
        Ok(flushed) => flushed,
        Err(e) => {
            // Reaching here means the flush could not even be attempted. Still
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
