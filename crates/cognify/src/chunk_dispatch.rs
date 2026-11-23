//! One bounded stream of per-chunk LLM calls, with a fail-fast stop.
//!
//! Both LLM graph-extraction paths — the default `KnowledgeGraph` path
//! ([`crate::tasks::extract_graph_from_data`]) and the runtime custom-schema
//! path ([`crate::tasks::extract_dynamic_graphs_from_data`]) — send one call
//! per chunk. They used to send them in sequential `chunks_per_batch` batches,
//! each one finishing before the next could start, so one slow chunk held up
//! every chunk behind the batch boundary. [`dispatch_chunk_calls`] runs every
//! chunk through one `buffer_unordered(max_parallel)` stream instead: the cap
//! on in-flight calls is the only limit on dispatch.
//!
//! Same shape as `SummaryExtractor::summarize_chunks`, which never had a batch
//! loop, with one difference: the inputs are built lazily, when a slot opens,
//! so the stage holds O(`max_parallel`) copies of chunk text rather than one
//! per chunk.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::stream::{self, StreamExt};
use tokio::task::JoinError;
use tracing::info;

use crate::error::CognifyError;

/// What happened to one chunk's call.
#[derive(Debug)]
pub(crate) enum ChunkCall<T> {
    /// The call was dispatched and returned, successfully or not. A panic
    /// inside the call is an `Err` too.
    Done(Result<T, CognifyError>),
    /// The call was never dispatched: under fail-fast another chunk had
    /// already failed when this one's turn came.
    Skipped,
}

/// Run `make(index)` for every `index` in `0..count`, at most `max_parallel` at
/// a time, and return every outcome **in index order**, whatever order the
/// calls finished in.
///
/// * `make` is called lazily, when the stream has room for the call, so it is
///   the place to clone a chunk's text: at most `max_parallel` copies exist at
///   once. Each future it returns is run on its own `tokio::spawn`, so calls
///   use the whole multi-threaded runtime and a panicking call becomes that
///   chunk's failure (via `join_error`) instead of unwinding the stage.
/// * `fail_fast`: once any call has failed, no further call is dispatched.
///   The check runs when a call is about to start, the last moment it can
///   still be avoided. Calls already in flight are not cancelled: they finish
///   and their outcomes, failures included, are returned. Every call never
///   started comes back as [`ChunkCall::Skipped`]. Without `fail_fast` every
///   call is dispatched.
/// * `progress_every` and `what` only shape an `info!` progress line, logged
///   every `progress_every` returned calls.
///
/// `buffer_unordered`, not `buffered`: the latter's `FuturesOrdered` counts
/// completed-but-undrained outputs against its limit, so one slow call pins
/// its slot and every slot filled behind it until it returns — head-of-line
/// blocking, which is exactly what removing the batch barrier is for. Order is
/// restored afterwards from the index each call carries; graph building keeps
/// the first declaration of an entity in chunk order, and failure records must
/// be deterministic for a given input.
pub(crate) async fn dispatch_chunk_calls<T, F, Fut>(
    count: usize,
    max_parallel: usize,
    fail_fast: bool,
    progress_every: usize,
    what: &str,
    mut make: F,
    join_error: fn(JoinError) -> CognifyError,
) -> Vec<ChunkCall<T>>
where
    T: Send + 'static,
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<T, CognifyError>> + Send + 'static,
{
    let stopped = Arc::new(AtomicBool::new(false));
    let progress_every = progress_every.max(1);
    let mut outcomes: Vec<(usize, ChunkCall<T>)> = Vec::with_capacity(count);

    let mut calls = stream::iter(0..count)
        .map(|index| {
            // Checked twice: here, so a stopped run does not even build the
            // call's inputs, and again when the call is first polled, which
            // is the moment it would start.
            let call = (!(fail_fast && stopped.load(Ordering::Acquire))).then(|| make(index));
            let stopped = Arc::clone(&stopped);
            async move {
                let Some(call) = call else {
                    return (index, ChunkCall::Skipped);
                };
                if fail_fast && stopped.load(Ordering::Acquire) {
                    return (index, ChunkCall::Skipped);
                }
                // The flag is raised inside the spawned task, the moment the
                // call returns, rather than when this wrapper is next polled:
                // a sibling that finishes in between must not open a slot for
                // a call the failure should have stopped.
                let task_stopped = Arc::clone(&stopped);
                let result = tokio::spawn(async move {
                    let result = call.await;
                    if result.is_err() {
                        task_stopped.store(true, Ordering::Release);
                    }
                    result
                })
                .await
                .map_err(join_error)
                .and_then(|result| result);
                // A panic never reaches the store above.
                if result.is_err() {
                    stopped.store(true, Ordering::Release);
                }
                (index, ChunkCall::Done(result))
            }
        })
        .buffer_unordered(max_parallel.max(1));

    let mut returned = 0usize;
    while let Some((index, outcome)) = calls.next().await {
        if matches!(outcome, ChunkCall::Done(_)) {
            returned += 1;
            if returned.is_multiple_of(progress_every) {
                info!("{what}: {returned} of {count} chunks returned");
            }
        }
        outcomes.push((index, outcome));
    }
    drop(calls);

    outcomes.sort_by_key(|(index, _)| *index);
    outcomes.into_iter().map(|(_, outcome)| outcome).collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn join_error(error: JoinError) -> CognifyError {
        CognifyError::Execute(error.to_string())
    }

    /// Outcomes come back in index order even when the calls finish in
    /// reverse, and the in-flight cap holds.
    #[tokio::test]
    async fn outcomes_are_in_index_order_and_in_flight_is_capped() {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let outcomes = dispatch_chunk_calls(
            8,
            3,
            false,
            100,
            "test",
            |index| {
                let in_flight = Arc::clone(&in_flight);
                let peak = Arc::clone(&peak);
                async move {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    // Earlier chunks take longer, so they finish later.
                    tokio::time::sleep(Duration::from_millis(5 * (8 - index as u64))).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    Ok(index)
                }
            },
            join_error,
        )
        .await;

        let values: Vec<usize> = outcomes
            .into_iter()
            .map(|outcome| match outcome {
                ChunkCall::Done(Ok(value)) => value,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(values, (0..8).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 3);
    }

    /// A panicking call is that chunk's failure, and stops dispatch under
    /// fail-fast like any other failure.
    #[tokio::test]
    async fn a_panicking_call_is_a_failure_and_stops_dispatch() {
        let outcomes = dispatch_chunk_calls(
            3,
            1,
            true,
            100,
            "test",
            |index| async move {
                if index == 0 {
                    panic!("boom");
                }
                Ok(index)
            },
            join_error,
        )
        .await;
        assert!(matches!(outcomes[0], ChunkCall::Done(Err(_))));
        assert!(matches!(outcomes[1], ChunkCall::Skipped));
        assert!(matches!(outcomes[2], ChunkCall::Skipped));
    }

    /// Without fail-fast every call is dispatched, failures or not.
    #[tokio::test]
    async fn run_to_end_dispatches_every_call() {
        let calls = Arc::new(AtomicUsize::new(0));
        let outcomes = dispatch_chunk_calls(
            4,
            1,
            false,
            100,
            "test",
            |index| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if index == 0 {
                        Err(CognifyError::Execute("first fails".to_string()))
                    } else {
                        Ok(index)
                    }
                }
            },
            join_error,
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert!(
            outcomes
                .iter()
                .all(|outcome| matches!(outcome, ChunkCall::Done(_)))
        );
    }
}
