//! Side-channel for streaming answer tokens out of a deep LLM call.
//!
//! Port of Python's `cognee/infrastructure/llm/streaming/token_sink.py`
//! (read its module docstring; the ownership and close rules below are the same
//! and are load-bearing). The call chain from a recall caller down to the
//! adapter is single-value at every hop, so tokens travel sideways: the adapter
//! pushes each delta into a sink while still returning the complete string, and
//! every existing signature stays as it was. Streaming is never load-bearing: if
//! nothing is listening, or the request takes a path that cannot stream, the
//! caller gets the identical payload with zero events.
//!
//! Two task-locals, both load-bearing (Python's two ContextVars):
//!
//! * the *requested* sink ([`with_token_sink`]) is set once per request by
//!   whoever created the sink;
//! * the *active* sink is promoted only around the concurrent session-turn
//!   answer lane ([`promote_answer_lane`]). Tokio task-locals are scoped to the
//!   wrapped future, so the sibling analysis lane (a separate `tokio::spawn` or a
//!   sibling `join!` arm) can never see it: a request-wide flag would interleave
//!   `SessionTurnAnalysis` output into the user's tokens.
//!
//! **Ownership is decided by the first delta, not by entering the scope.** Several
//! lanes may be promoted at once (one answer per authorised dataset) and which of
//! them *can* stream is unknown until one does. The first lane to emit owns the
//! stream; later lanes' deltas are dropped.
//!
//! **The creator closes the sink.** [`generate_answer`] closes it early when its
//! answer streamed (so a consumer sees the answer end without waiting for
//! persistence); every other exit leaves it open because a later call in the same
//! request may still stream. A consumer iterating a sink nobody closes waits
//! forever, so the transport closes it when the request future ends.
//!
//! **Deltas are a preview, not the payload.** A consumer must treat the returned
//! value as authoritative. `append_references` runs after the answer call, so
//! citations never appear as deltas; sessionless and sequential recalls stream
//! nothing; and a concurrent turn whose analysis finds the message was
//! feedback-only replaces the answer after the sink closed, so the deltas are then
//! a different answer. Render the final payload over what was streamed, never
//! append to it.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::error::LlmResult;
use crate::llm_trait::Llm;
use crate::types::{GenerationOptions, GenerationResponse, Message};

/// Deltas are a preview and the final payload is authoritative, so buffering is
/// bounded: a sink nobody drains would otherwise retain one event per token for
/// the life of the request (Python `MAX_BUFFERED_EVENTS`).
pub const MAX_BUFFERED_EVENTS: usize = 2048;

/// One event on the sink's queue (Python `StreamEvent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// One token (or token group) of the answer.
    Delta(String),
    /// A phase change, e.g. `generating`.
    Stage(String),
    /// Discard what was rendered so far: a retried LLM call restarts generation.
    Reset,
    /// The last token has been generated; work after this is persistence.
    AnswerDone,
    /// A failure, already sanitised. `status` is the HTTP status the same
    /// failure would have produced on the JSON path.
    Error {
        /// Safe, payload-free message.
        message: String,
        /// HTTP status of the equivalent JSON-path failure, when known.
        status: Option<u16>,
    },
}

impl StreamEvent {
    /// Dropping one of these is worse than the memory it costs: a lost `Error`
    /// ends the stream cleanly on a failed request and a lost `Reset` lets the
    /// consumer concatenate two copies of a retried answer.
    fn undroppable(&self) -> bool {
        !matches!(self, Self::Delta(_))
    }
}

/// What an adapter pushes streamed text into. Implemented by the sink's per-lane
/// handle; adapters depend on this trait rather than on the sink.
pub trait DeltaSink: Send + Sync {
    /// An attempt is starting. A retry re-streams from the beginning, so the
    /// owning lane emits `reset` first (Python `begin_attempt`).
    fn begin_attempt(&self);
    /// Offer one token.
    fn delta(&self, text: &str);
}

static NEXT_PRODUCER: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct State {
    queue: VecDeque<StreamEvent>,
    owner: Option<u64>,
    emitted: bool,
    detached: bool,
    closed: bool,
    dropped: bool,
    last_stage: Option<String>,
}

/// Fan-out channel for answer tokens. Never affects the returned value.
pub struct TokenSink {
    state: Mutex<State>,
    notify: Notify,
    max_buffered: usize,
    consumer_taken: AtomicBool,
}

impl std::fmt::Debug for TokenSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSink").finish_non_exhaustive()
    }
}

impl Default for TokenSink {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenSink {
    /// A sink with the default buffer bound.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_buffered(MAX_BUFFERED_EVENTS)
    }

    /// A sink with an explicit buffer bound (tests).
    #[must_use]
    pub fn with_max_buffered(max_buffered: usize) -> Self {
        Self {
            state: Mutex::new(State::default()),
            notify: Notify::new(),
            max_buffered,
            consumer_taken: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // lock poison is unrecoverable
        #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
        self.state.lock().expect("token sink state poisoned")
    }

    /// Whether this producer is the one whose deltas reach the consumer.
    #[must_use]
    pub fn owns(&self, producer: u64) -> bool {
        self.lock().owner == Some(producer)
    }

    /// Whether the stream has already been terminated.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Whether the buffer overflowed and preview frames were discarded. A
    /// consumer that fell behind has silent gaps in what it rendered.
    #[must_use]
    pub fn dropped_events(&self) -> bool {
        self.lock().dropped
    }

    /// Whether the consumer has detached.
    #[must_use]
    pub fn is_detached(&self) -> bool {
        self.lock().detached
    }

    /// Whether any delta was produced (whether or not it was delivered).
    #[must_use]
    pub fn emitted_any(&self) -> bool {
        self.lock().emitted
    }

    /// Mark the start of an LLM attempt for `producer`, resetting partial output.
    /// Only the owning lane may reset; otherwise a second dataset's answer call
    /// would tell the consumer to discard the first one's answer mid-stream.
    pub fn begin_attempt(&self, producer: u64) {
        let mut state = self.lock();
        if state.emitted && state.owner == Some(producer) {
            self.push(&mut state, StreamEvent::Reset);
        }
    }

    /// Offer one token from `producer`. The first producer to get here owns the
    /// sink for the rest of the request; other lanes' deltas are dropped.
    pub fn put_delta(&self, producer: u64, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut state = self.lock();
        match state.owner {
            None => state.owner = Some(producer),
            Some(owner) if owner != producer => return,
            Some(_) => {}
        }
        // Set before the detached check: a consumer that left mid-answer does not
        // change the fact that this producer streamed.
        state.emitted = true;
        self.push(&mut state, StreamEvent::Delta(text.to_string()));
    }

    /// Announce a phase change, skipping a repeat of the current phase.
    pub fn mark_stage(&self, stage: &str) {
        let mut state = self.lock();
        if state.last_stage.as_deref() == Some(stage) {
            return;
        }
        state.last_stage = Some(stage.to_string());
        self.push(&mut state, StreamEvent::Stage(stage.to_string()));
    }

    /// The last token has been generated.
    pub fn answer_done(&self) {
        let mut state = self.lock();
        self.push(&mut state, StreamEvent::AnswerDone);
    }

    /// Report a failure to the consumer, then terminate the stream. Takes an
    /// already-safe message: provider errors embed the rendered prompt.
    pub fn fail(&self, message: impl Into<String>, status: Option<u16>) {
        {
            let mut state = self.lock();
            self.push(
                &mut state,
                StreamEvent::Error {
                    message: message.into(),
                    status,
                },
            );
        }
        self.close();
    }

    /// Terminate the stream. Idempotent.
    pub fn close(&self) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        state.closed = true;
        drop(state);
        self.notify.notify_one();
    }

    /// The consumer is gone; stop buffering but let the producer finish.
    /// Deliberately not cancellation: the LLM call keeps running and the session
    /// write still lands.
    pub fn detach(&self) {
        let mut state = self.lock();
        state.detached = true;
        state.queue.clear();
    }

    fn push(&self, state: &mut State, event: StreamEvent) {
        if state.detached || state.closed {
            return;
        }
        if !event.undroppable() && state.queue.len() >= self.max_buffered {
            // Drop rather than block: a slow or absent consumer must never apply
            // backpressure to the LLM call it is watching.
            if !state.dropped {
                state.dropped = true;
                tracing::warn!(
                    max_buffered = self.max_buffered,
                    "token sink buffer full, dropping stream events; the returned answer is unaffected"
                );
            }
            return;
        }
        state.queue.push_back(event);
        self.notify.notify_one();
    }

    /// The consumer side. Single-consumer by construction: a second call returns
    /// `None`, since two readers over one queue would split the events and only
    /// one would see the end of the stream.
    #[must_use]
    pub fn stream(self: &Arc<Self>) -> Option<TokenStream> {
        if self.consumer_taken.swap(true, Ordering::SeqCst) {
            return None;
        }
        Some(TokenStream {
            sink: Arc::clone(self),
        })
    }
}

/// The consumer end of a [`TokenSink`].
#[derive(Debug)]
pub struct TokenStream {
    sink: Arc<TokenSink>,
}

impl TokenStream {
    /// The next event, or `None` once the sink is closed and drained. Cancel-safe.
    pub async fn next(&mut self) -> Option<StreamEvent> {
        loop {
            {
                let mut state = self.sink.lock();
                if let Some(event) = state.queue.pop_front() {
                    return Some(event);
                }
                if state.closed {
                    return None;
                }
            }
            self.sink.notify.notified().await;
        }
    }
}

/// One promoted answer call's handle on the sink: its producer identity is what
/// lets the sink tell the lane that owns the stream from a concurrent lane whose
/// deltas must be dropped.
struct LaneSink {
    sink: Arc<TokenSink>,
    producer: u64,
}

impl DeltaSink for LaneSink {
    fn begin_attempt(&self) {
        self.sink.begin_attempt(self.producer);
    }

    fn delta(&self, text: &str) {
        self.sink.put_delta(self.producer, text);
    }
}

tokio::task_local! {
    static REQUESTED: Arc<TokenSink>;
    static ACTIVE: Arc<TokenSink>;
}

/// Run `future` with `sink` as the request's requested sink. Set once, at
/// request entry, by the sink's creator. Tokio task-locals are not inherited by
/// `tokio::spawn`, which is what keeps spawned analysis work out of the stream.
pub async fn with_token_sink<F: Future>(sink: Arc<TokenSink>, future: F) -> F::Output {
    REQUESTED.scope(sink, future).await
}

/// The sink this request asked for, if any.
#[must_use]
pub fn requested_token_sink() -> Option<Arc<TokenSink>> {
    REQUESTED.try_with(Arc::clone).ok()
}

/// The sink the current future may stream into: `Some` only inside
/// [`promote_answer_lane`].
#[must_use]
pub fn active_token_sink() -> Option<Arc<TokenSink>> {
    ACTIVE.try_with(Arc::clone).ok()
}

/// Promote the requested sink (if any) around `future`, the concurrent
/// session-turn answer lane. Nothing outside the wrapped future sees it, so the
/// analysis lane stays out of the user's stream. A no-op without a requested sink.
pub async fn promote_answer_lane<F: Future>(future: F) -> F::Output {
    match requested_token_sink() {
        Some(sink) => ACTIVE.scope(sink, future).await,
        None => future.await,
    }
}

/// The one plain-text completion a listening client may watch (Python
/// `generate_answer` + `answer_scope`).
///
/// Behaves exactly like [`Llm::generate`] unless every gate holds: a sink is
/// promoted for this task, the adapter's resolved `llm.streaming` setting is on,
/// the adapter supports streaming, and the sink is not already closed. Callers
/// must use it only for plain-text answers; a structured response never streams.
///
/// On the way out it ends the stream **when this lane actually streamed**:
/// `answer_done` + close on success, a sanitised `error` on failure. Any other
/// exit leaves the sink open, because a later call in the same request may still
/// stream.
pub async fn generate_answer(
    llm: &dyn Llm,
    messages: Vec<Message>,
    options: Option<GenerationOptions>,
) -> LlmResult<GenerationResponse> {
    let Some(sink) = active_token_sink() else {
        return llm.generate(messages, options).await;
    };
    if !llm.streaming_enabled() || !llm.supports_streaming() || sink.is_closed() {
        return llm.generate(messages, options).await;
    }

    let producer = NEXT_PRODUCER.fetch_add(1, Ordering::Relaxed);
    sink.mark_stage("generating");
    let lane = LaneSink {
        sink: Arc::clone(&sink),
        producer,
    };
    let result = llm.generate_streaming(messages, options, &lane).await;
    match &result {
        Ok(_) => {
            if sink.owns(producer) && sink.emitted_any() {
                sink.answer_done();
                sink.close();
            }
        }
        Err(error) => {
            tracing::warn!(error_kind = error.log_kind(), "answer streaming failed");
            if sink.owns(producer) {
                sink.fail(
                    format!("{} during answer generation", error.log_kind()),
                    error.http_status(),
                );
            }
        }
    }
    result
}
