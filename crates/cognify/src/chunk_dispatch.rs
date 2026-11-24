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
//! loop, with one difference: the inputs are built lazily, when a call starts,
//! so the stage holds O(`max_parallel`) copies of chunk text rather than one
//! per chunk.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::stream::{self, StreamExt};
use tracing::info;

use crate::error::CognifyError;

/// How often [`dispatch_chunk_calls`] logs its progress at most.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(30);

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
/// * `make` is called only when a call is about to start, so it is the place
///   to clone a chunk's text: at most `max_parallel` copies exist at once, and
///   none is made for a call that never starts. Each future it returns runs on
///   its own `tokio::spawn`, so calls use the whole multi-threaded runtime.
/// * `fail_fast`: once any call has failed, no further call starts. There is
///   one check and one store, each at the only point where it is not too late:
///   - the check runs on a call's first poll, the moment it would start, and
///     before `make` builds its inputs;
///   - the store runs inside the spawned task, the moment a failing call
///     returns. Storing it later, when the stream next polls that call's
///     wrapper, would let a sibling that finished in between open a slot for a
///     call the failure should have stopped. A panic is caught there too
///     (`catch_unwind`), so it raises the flag and becomes that chunk's
///     failure instead of unwinding the stage.
///
///   Calls already in flight are not cancelled: they finish and their
///   outcomes, failures included, are returned. Every call never started comes
///   back as [`ChunkCall::Skipped`]. Without `fail_fast` every call starts.
/// * `task_error` turns a panic message, or a task that was cancelled, into
///   the chunk's error.
/// * `what` names the stage in the progress line, logged at most every
///   [`PROGRESS_INTERVAL`].
///
/// Dropping the returned future (the stage itself being cancelled) stops
/// dispatch but does not abort the spawned calls already in flight: they run
/// to completion and their results are dropped. Aborting them belongs with the
/// per-document assembler (SDK-1694 O4b), which owns the stream's lifetime.
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
    what: &str,
    make: F,
    task_error: fn(String) -> CognifyError,
) -> Vec<ChunkCall<T>>
where
    T: Send + 'static,
    F: FnMut(usize) -> Fut + Send,
    Fut: Future<Output = Result<T, CognifyError>> + Send + 'static,
{
    let stopped = Arc::new(AtomicBool::new(false));
    // Shared by every call's wrapper so that `make` runs inside the wrapper,
    // after its check. The wrappers are polled by this one task, so the lock
    // is never contended; it is held only while `make` builds the inputs.
    let make = Mutex::new(make);
    let mut outcomes: Vec<(usize, ChunkCall<T>)> = Vec::with_capacity(count);

    let mut calls = stream::iter(0..count)
        .map(|index| {
            let stopped = Arc::clone(&stopped);
            let make = &make;
            async move {
                if fail_fast && stopped.load(Ordering::Acquire) {
                    return (index, ChunkCall::Skipped);
                }
                let call = {
                    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
                    let mut make = make.lock().expect("lock poison is unrecoverable");
                    make(index)
                };
                let result = tokio::spawn(async move {
                    let result = AssertUnwindSafe(call)
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|panic| Err(task_error(panic_message(panic.as_ref()))));
                    if result.is_err() {
                        stopped.store(true, Ordering::Release);
                    }
                    result
                })
                .await
                // Only a cancelled task gets here: a panic was caught above.
                .unwrap_or_else(|error| Err(task_error(error.to_string())));
                (index, ChunkCall::Done(result))
            }
        })
        .buffer_unordered(max_parallel.max(1));

    let mut returned = 0usize;
    let mut last_log = Instant::now();
    while let Some((index, outcome)) = calls.next().await {
        if matches!(outcome, ChunkCall::Done(_)) {
            returned += 1;
            if last_log.elapsed() >= PROGRESS_INTERVAL {
                last_log = Instant::now();
                info!("{what}: {returned} of {count} chunks returned");
            }
        }
        outcomes.push((index, outcome));
    }
    drop(calls);

    outcomes.sort_by_key(|(index, _)| *index);
    outcomes.into_iter().map(|(_, outcome)| outcome).collect()
}

/// The message a panic was raised with, as `std` prints it.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    let message = panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a non-string payload".to_string());
    format!("task panicked: {message}")
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

    fn task_error(message: String) -> CognifyError {
        CognifyError::Execute(message)
    }

    fn failure() -> CognifyError {
        CognifyError::Execute("fails".to_string())
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
            task_error,
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

    /// After a failure, no later call has its inputs built or is started.
    /// Pins the check: it must run before `make`, on the call's first poll.
    #[tokio::test]
    async fn nothing_is_built_or_started_after_a_failure() {
        let built = AtomicUsize::new(0);
        let started = Arc::new(AtomicUsize::new(0));
        let outcomes = dispatch_chunk_calls(
            5,
            1,
            true,
            "test",
            |index| {
                built.fetch_add(1, Ordering::SeqCst);
                let started = Arc::clone(&started);
                async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    if index == 0 {
                        Err(failure())
                    } else {
                        Ok(index)
                    }
                }
            },
            task_error,
        )
        .await;

        assert_eq!(
            built.load(Ordering::SeqCst),
            1,
            "no inputs built after the stop"
        );
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "no call started after the stop"
        );
        assert!(matches!(outcomes[0], ChunkCall::Done(Err(_))));
        assert!(
            outcomes[1..]
                .iter()
                .all(|outcome| matches!(outcome, ChunkCall::Skipped))
        );
    }

    /// The reviewer's case, on the multi-threaded runtime: a failure and a
    /// success return between two polls of the stream, the success's slot
    /// opens, and still no new call starts. Pins that the flag is raised by
    /// the failing call itself; repeated to give a scheduling race a chance.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_slot_freed_after_a_failure_starts_no_call() {
        for _ in 0..50 {
            // Chunk 1 starts, then chunk 0 fails, then chunk 1 returns: the
            // failure is in before chunk 1's slot opens. Without the first
            // handshake chunk 0 could fail before chunk 1 started, and
            // chunk 1 would (rightly) be skipped.
            let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
            let (failed_tx, failed_rx) = tokio::sync::oneshot::channel::<()>();
            let mut first = Some((started_rx, failed_tx));
            let mut second = Some((started_tx, failed_rx));
            let started = Arc::new(AtomicUsize::new(0));
            let outcomes = dispatch_chunk_calls(
                6,
                2,
                true,
                "test",
                |index| {
                    let started = Arc::clone(&started);
                    let first = (index == 0).then(|| first.take()).flatten();
                    let second = (index == 1).then(|| second.take()).flatten();
                    async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        if let Some((started_rx, failed_tx)) = first {
                            let _ = started_rx.await;
                            let _ = failed_tx.send(());
                            return Err(failure());
                        }
                        if let Some((started_tx, failed_rx)) = second {
                            let _ = started_tx.send(());
                            let _ = failed_rx.await;
                        }
                        Ok(index)
                    }
                },
                task_error,
            )
            .await;

            assert_eq!(started.load(Ordering::SeqCst), 2, "only chunks 0 and 1 ran");
            assert!(matches!(outcomes[0], ChunkCall::Done(Err(_))));
            assert!(matches!(outcomes[1], ChunkCall::Done(Ok(1))));
            assert!(
                outcomes[2..]
                    .iter()
                    .all(|outcome| matches!(outcome, ChunkCall::Skipped))
            );
        }
    }

    /// A panicking call is that chunk's failure, and stops dispatch under
    /// fail-fast like any other failure.
    #[tokio::test]
    async fn a_panicking_call_is_a_failure_and_stops_dispatch() {
        let outcomes = dispatch_chunk_calls(
            3,
            1,
            true,
            "test",
            |index| async move {
                if index == 0 {
                    panic!("boom");
                }
                Ok(index)
            },
            task_error,
        )
        .await;
        match &outcomes[0] {
            ChunkCall::Done(Err(error)) => assert!(error.to_string().contains("boom")),
            other => panic!("expected the panic as a failure, got {other:?}"),
        }
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
            "test",
            |index| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if index == 0 {
                        Err(failure())
                    } else {
                        Ok(index)
                    }
                }
            },
            task_error,
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
