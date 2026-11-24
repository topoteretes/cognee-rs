//! One bounded stream of per-chunk LLM calls, with a fail-fast stop.
//!
//! Every per-chunk LLM call of a cognify run goes through here: the default
//! `KnowledgeGraph` extraction, the runtime custom-schema extraction and the
//! summaries. They used to send them in sequential `chunks_per_batch` batches,
//! each one finishing before the next could start, so one slow chunk held up
//! every chunk behind the batch boundary. [`dispatch_calls`] runs every chunk
//! through one `buffer_unordered(max_parallel)` stream instead: the cap on
//! in-flight calls is the only limit on dispatch.
//!
//! The inputs are built lazily, when a call starts, so a stage holds
//! O(`max_parallel`) copies of chunk text rather than one per chunk.
//!
//! # Delivery, stop and cancellation (SDK-1694 O4b)
//!
//! - Each outcome is handed to a **sink the moment it returns**, so the
//!   per-document assembler sees results as they arrive.
//!   [`dispatch_chunk_calls`] is the collecting form, for callers that want
//!   every outcome at once, in index order.
//! - The **stop flag is injected** ([`StopFlag`]), so the extraction stream,
//!   the summary stream and the writer of one stage share one: a failure in
//!   any of them stops the others from starting calls.
//! - A call never started comes back as [`ChunkCall::Skipped`]. A skipped call
//!   is the **only** "never reached" signal: skips arrive whenever the stream
//!   reaches them, not in any particular position.
//! - **Dropping the dispatch future aborts every call in flight**: each
//!   spawned call is held by an abort-on-drop handle, so a cancelled stage
//!   (an HTTP client gone, a `timeout`, a sibling's error in a `try_join`)
//!   stops paying for calls whose results nobody will read.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::stream::{self, StreamExt};
use tokio::task::JoinHandle;
use tracing::info;

use crate::error::CognifyError;

/// How often [`dispatch_calls`] logs its progress at most.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(30);

/// What happened to one chunk's call.
#[derive(Debug)]
pub(crate) enum ChunkCall<T> {
    /// The call was dispatched and returned, successfully or not. A panic
    /// inside the call is an `Err` too.
    Done(Result<T, CognifyError>),
    /// The call was never dispatched: the stage's [`StopFlag`] was raised
    /// when this one's turn came.
    Skipped,
}

/// A stop shared by everything one stage dispatches, and by its writer.
///
/// Raised by a failing call of a stream that stops on failure, or by the
/// writer when a save fails; read by every stream that stops on failure,
/// before it starts a call. Cheap to clone: every clone is the same flag.
#[derive(Debug, Clone, Default)]
pub(crate) struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn raise(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn is_raised(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// How one stream dispatches.
#[derive(Debug, Clone)]
pub(crate) struct Dispatch {
    /// At most this many calls in flight (0 counts as 1).
    pub(crate) max_parallel: usize,
    /// The stage's stop.
    pub(crate) stop: StopFlag,
    /// Start no call once `stop` is raised.
    pub(crate) honour_stop: bool,
    /// Raise `stop` when one of this stream's calls fails.
    pub(crate) raise_on_failure: bool,
    /// Names the stream in the progress line.
    pub(crate) what: &'static str,
}

impl Dispatch {
    /// A stream with a flag of its own that stops on its own failures when
    /// `fail_fast` is set, and never otherwise.
    pub(crate) fn standalone(max_parallel: usize, fail_fast: bool, what: &'static str) -> Self {
        Self {
            max_parallel,
            stop: StopFlag::new(),
            honour_stop: fail_fast,
            raise_on_failure: fail_fast,
            what,
        }
    }
}

/// A spawned call, aborted when dropped before it finished.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        // A no-op for a task that already finished.
        self.0.abort();
    }
}

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

/// Run `make(index)` for every `index` in `0..count`, at most
/// `dispatch.max_parallel` at a time, and hand each outcome to `sink` the
/// moment it returns.
///
/// * `admit(index)` is awaited before a call's stop check and before `make`:
///   the per-document assembler waits there until the call's document may
///   start (the cap on unsaved documents). A call waiting to be admitted holds
///   its slot; calls are admitted in index order.
/// * `make` is called only when a call is about to start, so it is the place
///   to clone a chunk's text: at most `max_parallel` copies exist at once, and
///   none is made for a call that never starts. Each future it returns runs on
///   its own `tokio::spawn`, so calls use the whole multi-threaded runtime.
/// * The stop: with `honour_stop`, no call starts once the flag is raised —
///   the check runs on a call's first poll, after admission and before `make`
///   builds its inputs. With `raise_on_failure`, a failing call raises the
///   flag inside its spawned task, the moment it returns: storing it later,
///   when the stream next polls that call's wrapper, would let a sibling that
///   finished in between open a slot for a call the failure should have
///   stopped. A panic is caught there too (`catch_unwind`), so it raises the
///   flag and becomes that chunk's failure instead of unwinding the stage.
///   Calls already in flight are not cancelled by a stop: they finish and
///   their outcomes, failures included, reach the sink.
/// * `task_error` turns a panic message, or a task that was cancelled, into
///   the chunk's error.
///
/// Dropping the returned future aborts the calls in flight (see the module
/// docs). Returns once every index has reached the sink.
///
/// `buffer_unordered`, not `buffered`: the latter's `FuturesOrdered` counts
/// completed-but-undrained outputs against its limit, so one slow call pins
/// its slot and every slot filled behind it until it returns — head-of-line
/// blocking, which is exactly what removing the batch barrier is for. A sink
/// that needs the chunk order back restores it from the index.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_calls<T, A, AFut, F, Fut, S>(
    count: usize,
    dispatch: &Dispatch,
    admit: A,
    make: F,
    task_error: fn(String) -> CognifyError,
    mut sink: S,
) where
    T: Send + 'static,
    A: Fn(usize) -> AFut,
    AFut: Future<Output = ()>,
    F: FnMut(usize) -> Fut + Send,
    Fut: Future<Output = Result<T, CognifyError>> + Send + 'static,
    S: FnMut(usize, ChunkCall<T>),
{
    let Dispatch {
        max_parallel,
        stop,
        honour_stop,
        raise_on_failure,
        what,
    } = dispatch;
    // Shared by every call's wrapper so that `make` runs inside the wrapper,
    // after its check. The wrappers are polled by this one task, so the lock
    // is never contended; it is held only while `make` builds the inputs.
    let make = Mutex::new(make);
    let admit = &admit;

    let mut calls = stream::iter(0..count)
        .map(|index| {
            let stop = stop.clone();
            let make = &make;
            async move {
                admit(index).await;
                if *honour_stop && stop.is_raised() {
                    return (index, ChunkCall::Skipped);
                }
                let call = {
                    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
                    let mut make = make.lock().expect("lock poison is unrecoverable");
                    make(index)
                };
                let raise = *raise_on_failure;
                let result = AbortOnDrop(tokio::spawn(async move {
                    let result = AssertUnwindSafe(call)
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|panic| Err(task_error(panic_message(panic.as_ref()))));
                    if raise && result.is_err() {
                        stop.raise();
                    }
                    result
                }))
                .await
                // Only a cancelled task gets here: a panic was caught above.
                .unwrap_or_else(|error| Err(task_error(error.to_string())));
                (index, ChunkCall::Done(result))
            }
        })
        .buffer_unordered((*max_parallel).max(1));

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
        sink(index, outcome);
    }
}

/// [`dispatch_calls`] with a flag of its own and no admission, collecting
/// every outcome **in index order**, whatever order the calls finished in.
///
/// With `fail_fast`, once any call has failed no further call starts; every
/// call never started comes back as [`ChunkCall::Skipped`]. Without it every
/// call starts.
pub(crate) async fn dispatch_chunk_calls<T, F, Fut>(
    count: usize,
    max_parallel: usize,
    fail_fast: bool,
    what: &'static str,
    make: F,
    task_error: fn(String) -> CognifyError,
) -> Vec<ChunkCall<T>>
where
    T: Send + 'static,
    F: FnMut(usize) -> Fut + Send,
    Fut: Future<Output = Result<T, CognifyError>> + Send + 'static,
{
    let mut outcomes: Vec<(usize, ChunkCall<T>)> = Vec::with_capacity(count);
    dispatch_calls(
        count,
        &Dispatch::standalone(max_parallel, fail_fast, what),
        |_| std::future::ready(()),
        make,
        task_error,
        |index, outcome| outcomes.push((index, outcome)),
    )
    .await;
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

    /// Outcomes reach the sink as they return, not in index order: a fast
    /// late chunk is delivered before a slow early one.
    #[tokio::test]
    async fn outcomes_reach_the_sink_as_they_return() {
        let mut delivered: Vec<usize> = Vec::new();
        dispatch_calls(
            3,
            &Dispatch::standalone(3, false, "test"),
            |_| std::future::ready(()),
            |index| async move {
                // Chunk 0 is the slowest, chunk 2 the fastest.
                tokio::time::sleep(Duration::from_millis(20 * (3 - index as u64))).await;
                Ok(index)
            },
            task_error,
            |index, outcome| {
                assert!(matches!(outcome, ChunkCall::Done(Ok(value)) if value == index));
                delivered.push(index);
            },
        )
        .await;
        assert_eq!(delivered, vec![2, 1, 0]);
    }

    /// Two streams sharing one flag: a failure in the first stops the second
    /// from starting any call, whichever raised it.
    #[tokio::test]
    async fn a_shared_flag_stops_the_other_stream() {
        let stop = StopFlag::new();
        let extraction = Dispatch {
            max_parallel: 1,
            stop: stop.clone(),
            honour_stop: true,
            raise_on_failure: true,
            what: "extraction",
        };
        let summaries = Dispatch {
            raise_on_failure: false,
            what: "summaries",
            ..extraction.clone()
        };
        let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        let started = Arc::new(AtomicUsize::new(0));
        let mut skipped = 0usize;
        let failing = dispatch_calls(
            1,
            &extraction,
            |_| std::future::ready(()),
            |_| async { Err::<(), _>(failure()) },
            task_error,
            |_, _| {
                let _ = gate_tx.send(true);
            },
        );
        let waiting = dispatch_calls(
            4,
            &summaries,
            // Nothing starts until the failure is in.
            |_| {
                let mut gate = gate_rx.clone();
                async move {
                    let _ = gate.wait_for(|open| *open).await;
                }
            },
            |_| {
                let started = Arc::clone(&started);
                async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            task_error,
            |_, outcome| {
                if matches!(outcome, ChunkCall::Skipped) {
                    skipped += 1;
                }
            },
        );
        futures::join!(failing, waiting);
        assert!(stop.is_raised());
        assert_eq!(started.load(Ordering::SeqCst), 0, "no summary call started");
        assert_eq!(skipped, 4);
    }

    /// A stream that does not raise the flag on failure leaves it down, so a
    /// tolerated failure stops nothing.
    #[tokio::test]
    async fn a_stream_that_does_not_raise_leaves_the_flag_down() {
        let dispatch = Dispatch {
            raise_on_failure: false,
            ..Dispatch::standalone(1, true, "test")
        };
        let mut done = 0usize;
        dispatch_calls(
            3,
            &dispatch,
            |_| std::future::ready(()),
            |index| async move {
                if index == 0 {
                    Err(failure())
                } else {
                    Ok(index)
                }
            },
            task_error,
            |_, outcome| {
                if matches!(outcome, ChunkCall::Done(_)) {
                    done += 1;
                }
            },
        )
        .await;
        assert!(!dispatch.stop.is_raised());
        assert_eq!(done, 3, "every call ran");
    }

    /// Calls wait in `admit` until they may start, in index order.
    #[tokio::test]
    async fn calls_start_only_once_admitted() {
        let (open_tx, open_rx) = tokio::sync::watch::channel(0usize);
        let dispatch = Dispatch::standalone(3, false, "test");
        let started = Arc::new(Mutex::new(Vec::new()));
        let run = dispatch_calls(
            3,
            &dispatch,
            |index| {
                let mut open = open_rx.clone();
                async move {
                    let _ = open.wait_for(|admitted| *admitted > index).await;
                }
            },
            |index| {
                let started = Arc::clone(&started);
                async move {
                    started.lock().unwrap().push(index);
                    Ok(index)
                }
            },
            task_error,
            |_, _| {},
        );
        let admit = async {
            for admitted in 1..=3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                assert_eq!(started.lock().unwrap().len(), admitted - 1);
                open_tx.send(admitted).unwrap();
            }
        };
        futures::join!(run, admit);
        assert_eq!(*started.lock().unwrap(), vec![0, 1, 2]);
    }

    /// Dropping the dispatch future aborts the calls in flight: they never
    /// finish.
    #[tokio::test]
    async fn dropping_the_dispatch_aborts_calls_in_flight() {
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let finished = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        struct CountDrop(Arc<AtomicUsize>);
        impl Drop for CountDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dispatch = Dispatch::standalone(2, false, "test");
        let run = dispatch_calls(
            2,
            &dispatch,
            |_| std::future::ready(()),
            |_| {
                let started = started_tx.clone();
                let finished = Arc::clone(&finished);
                let guard = CountDrop(Arc::clone(&dropped));
                async move {
                    let _guard = guard;
                    let _ = started.send(());
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    finished.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            task_error,
            |_, _| {},
        );
        tokio::select! {
            () = run => panic!("the calls sleep for a minute"),
            _ = async {
                started_rx.recv().await;
                started_rx.recv().await;
            } => {}
        }
        // The aborted tasks drop their futures on the runtime.
        for _ in 0..100 {
            if dropped.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 2, "both calls were aborted");
        assert_eq!(finished.load(Ordering::SeqCst), 0);
    }
}
