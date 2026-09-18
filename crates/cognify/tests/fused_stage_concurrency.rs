#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Does cognify's fused stage *actually* run graph extraction and
//! summarization at the same time?
//!
//! `040385e2` ("fuse graph extraction and summarization into one concurrent
//! stage") replaced two sequential pipeline stages with a single
//! [`cognee_cognify::make_extract_graph_and_summarize_task`] built on
//! `TypedTask::try_parallel`. The barrier test that shipped with it covers the
//! *generic* combinator in `cognee-core` — two toy `async_fn` branches over an
//! `i32`. Nothing pinned the real cognify stage, where each branch drives its
//! own `buffer_unordered` fan-out over real chunks and both contend for the
//! process-wide `cognee_llm::in_flight` ceiling. A regression that re-serialised
//! the two halves — awaiting one branch before building the other's future,
//! collapsing the fan-out onto a shared permit, or simply reverting the wiring
//! in `build_cognify_pipeline` — would leave the `cognee-core` test green.
//!
//! These three tests close that gap, deliberately using two independent
//! techniques so neither has to be trusted alone:
//!
//! 1. [`fused_stage_runs_graph_and_summary_concurrently`] — a *rendezvous*.
//!    Every LLM call announces its own kind and then blocks until the other
//!    kind has been dispatched. The stage can only finish if a graph call and a
//!    summary call were in flight simultaneously; serial execution deadlocks.
//!    This is an existence proof of overlap, not a measurement, so it cannot be
//!    flaky on a loaded machine — a regression hits the timeout instead.
//!
//! 2. [`sequential_composition_of_the_same_branches_deadlocks`] — the negative
//!    control for (1). It runs the *same two branch tasks* over the *same
//!    rendezvous LLM* one after the other, the shape the pipeline had before
//!    the fusion, and asserts that it hangs. Without this, test (1) passing
//!    proves nothing: a rendezvous that both a fused and a serial pipeline
//!    satisfy would be measuring nothing at all.
//!
//! 3. [`full_cognify_overlaps_graph_and_summary_llm_calls`] — the telemetry
//!    reproduction. It runs the whole public `cognify()` entry point and
//!    records a `(kind, start, end)` interval per LLM call, which is the same
//!    artifact a span exporter would show. It then asserts the graph and
//!    summary intervals overlap, and prints the timeline on failure so the
//!    result can be read directly rather than inferred.
//!
//! All three are offline and deterministic: mock LLM, storage, graph, vector
//! and embedding backends, and in-memory SQLite. No network, no credentials.
//!
//! Run with:
//!   cargo test --package cognee-cognify --test fused_stage_concurrency -- --nocapture

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use uuid::Uuid;

use cognee_cognify::{
    CognifyConfig, ExtractedChunks, SummarizedData, cognify, make_extract_graph_and_summarize_task,
    make_extract_graph_task, make_summarize_text_task,
};
use cognee_core::{
    CancellationHandle, CpuPool, RayonThreadPool, TaskContext, TaskContextBuilder, TypedTask,
};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::{EmbeddingEngine, MockEmbeddingEngine};
use cognee_graph::GraphDBTrait;
use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_llm::{Llm, LlmResult};
use cognee_models::{Data, Dataset, DocumentChunk};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockVectorDB};
use cognee_vector::VectorDB;

/// How long a rendezvous is given before it is declared a deadlock.
///
/// Generous on purpose. A fused stage clears it in milliseconds, so the only
/// thing this bounds is how long a *failing* run takes to report.
const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the negative control waits before concluding the sequential shape
/// really is stuck. Short, because it is expected to expire every time.
const DEADLOCK_CONFIRM: Duration = Duration::from_secs(3);

// ───────────────────────────── rendezvous plumbing ──────────────────────────

/// A one-way gate: every waiter parks until the first [`Gate::open`], then all
/// of them — and everyone arriving later — pass immediately.
///
/// A `Semaphore` rather than a `Notify` because the flag must be *sticky*: a
/// caller that opens the gate before its counterpart starts waiting must still
/// release that later waiter. `Notify` only stores a single permit and
/// `notify_waiters` wakes nobody who has not yet registered, both of which turn
/// this into a race. A `Barrier` would work only if the exact number of LLM
/// calls were known up front; this stays correct for any count.
struct Gate {
    permits: Semaphore,
    opened: AtomicBool,
}

impl Gate {
    fn new() -> Self {
        Self {
            permits: Semaphore::new(0),
            opened: AtomicBool::new(false),
        }
    }

    /// Idempotent — `add_permits` is called once however many callers open it,
    /// so the permit count cannot overflow.
    fn open(&self) {
        if !self.opened.swap(true, Ordering::SeqCst) {
            self.permits.add_permits(Semaphore::MAX_PERMITS / 2);
        }
    }

    /// Park until the gate is open. The permit is returned on drop, so the gate
    /// does not erode as waiters pass through it.
    async fn wait(&self) {
        let _permit = self
            .permits
            .acquire()
            .await
            .expect("the gate semaphore is never closed");
    }
}

/// Which half of the fused stage issued a call, told apart exactly the way
/// `cognee_test_utils::MockLlm` tells them apart: summarization carries
/// `SummarizedContent`'s schema, which declares a top-level `summary` property,
/// and graph extraction carries `KnowledgeGraph`'s, which does not.
fn is_summary_schema(schema: &Value) -> bool {
    schema
        .get("properties")
        .and_then(|properties| properties.get("summary"))
        .is_some()
}

/// An `Llm` that makes serial execution observable as a hang.
///
/// Every call opens the gate for its own kind and then blocks on the other
/// kind's gate. Two calls of *different* kinds release each other; any number
/// of calls of the *same* kind wait forever. So the stage completes if and only
/// if graph extraction and summarization were both in flight at the same
/// moment.
struct RendezvousLlm {
    graph_dispatched: Gate,
    summary_dispatched: Gate,
    graph_calls: AtomicUsize,
    summary_calls: AtomicUsize,
}

impl RendezvousLlm {
    fn new() -> Self {
        Self {
            graph_dispatched: Gate::new(),
            summary_dispatched: Gate::new(),
            graph_calls: AtomicUsize::new(0),
            summary_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Llm for RendezvousLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Ok(GenerationResponse {
            content: r#"{"nodes":[],"edges":[]}"#.to_string(),
            model: "rendezvous-llm".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        if is_summary_schema(json_schema) {
            self.summary_calls.fetch_add(1, Ordering::SeqCst);
            self.summary_dispatched.open();
            self.graph_dispatched.wait().await;
            Ok(json!({ "summary": "s", "description": "d" }))
        } else {
            self.graph_calls.fetch_add(1, Ordering::SeqCst);
            self.graph_dispatched.open();
            self.summary_dispatched.wait().await;
            Ok(json!({ "nodes": [], "edges": [] }))
        }
    }

    fn model(&self) -> &str {
        "rendezvous-llm"
    }
}

// ─────────────────────────────── fixtures ───────────────────────────────────

/// One chunk, one document. The rendezvous needs only that both kinds of call
/// exist; more chunks would add fan-out without adding evidence.
fn one_chunk_input() -> ExtractedChunks {
    let document_id = Uuid::new_v4();
    let text = "Ada Lovelace wrote the first algorithm for the Analytical Engine.".to_string();
    let chunk = DocumentChunk::new(
        Uuid::new_v4(),
        text.clone(),
        text.split_whitespace().count(),
        0,
        "paragraph_end".to_string(),
        document_id,
    );

    ExtractedChunks {
        chunks: vec![chunk],
        // No `Document`s: nothing here is a DLT row, and web-page node creation
        // is switched off in `stage_config`, so the stage never looks for one.
        documents: vec![],
        dataset_id: Uuid::new_v4(),
        user_id: None,
        tenant_id: None,
        failures: Default::default(),
    }
}

/// Summarization on (it is the stage under test) and web-page nodes off (they
/// would add LLM-free graph work that only obscures the trace).
fn stage_config() -> CognifyConfig {
    let config = CognifyConfig::default().with_web_page_nodes(false);
    assert!(
        config.enable_summarization,
        "the fused stage only has two halves to overlap when summarization is on"
    );
    config
}

/// A `TaskContext` with mock backends, plus the cancellation handle the caller
/// must keep alive: dropping it is what cancels the run.
async fn task_ctx() -> (CancellationHandle, Arc<TaskContext>) {
    let db = connect("sqlite::memory:").await.expect("in-memory sqlite");
    initialize(&db).await.expect("initialize schema");

    let thread_pool: Arc<dyn CpuPool> =
        Arc::new(RayonThreadPool::with_default_threads().expect("RayonThreadPool init"));

    let (handle, ctx) = TaskContextBuilder::new()
        .thread_pool(thread_pool)
        .database(Arc::new(db))
        .graph_db(Arc::new(MockGraphDB::new()))
        .vector_db(Arc::new(MockVectorDB::new()))
        .build()
        .expect("every required context field is set");

    (handle, Arc::new(ctx))
}

/// Call a single-value `TypedTask` the way the pipeline executor would.
async fn call_task<I, O>(task: TypedTask<I, O>, input: &I, ctx: Arc<TaskContext>) -> Box<O>
where
    I: cognee_core::Value,
    O: cognee_core::Value,
{
    match task {
        TypedTask::Async(f) => f(input, ctx).await.expect("task must succeed"),
        // `try_parallel` only accepts `Sync`/`Async` branches and always
        // produces `Async`, and both `make_*` factories here build with
        // `TypedTask::async_fn` — so any other variant is a change of shape
        // this test needs to be re-read against, not a runtime condition.
        _ => panic!("expected a single-value Async task"),
    }
}

// ───────────────────────────────── test 1 ───────────────────────────────────

/// The real cognify stage dispatches both halves before either can finish.
///
/// Asserted by rendezvous, not by a stopwatch: the graph call blocks until a
/// summary call has been dispatched and vice versa, so the stage can only
/// return if both were in flight at once. A regression to sequential execution
/// cannot satisfy that, and fails on [`RENDEZVOUS_TIMEOUT`] rather than by
/// missing a timing threshold — nothing here is sensitive to machine load.
#[tokio::test(flavor = "multi_thread")]
async fn fused_stage_runs_graph_and_summary_concurrently() {
    let llm = Arc::new(RendezvousLlm::new());
    let (_cancel, ctx) = task_ctx().await;
    let input = one_chunk_input();

    let task = make_extract_graph_and_summarize_task(
        Arc::clone(&llm) as Arc<dyn Llm>,
        Arc::new(MockGraphDB::new()) as Arc<dyn GraphDBTrait>,
        Arc::new(NoOpOntologyResolver::new()),
        Arc::clone(&ctx.database),
        stage_config(),
    );

    let output: Box<SummarizedData> = tokio::time::timeout(
        RENDEZVOUS_TIMEOUT,
        call_task(task, &input, Arc::clone(&ctx)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "DEADLOCK: the fused stage did not dispatch graph extraction and \
             summarization concurrently.\n\
             Each LLM call waits for the other kind to be dispatched, so this \
             times out exactly when the two halves run one after the other.\n\
             graph calls dispatched: {}, summary calls dispatched: {}.\n\
             A single dispatched kind means the second half never started while \
             the first was in flight — the fusion in \
             `make_extract_graph_and_summarize_task` has regressed to sequential \
             execution.",
            llm.graph_calls.load(Ordering::SeqCst),
            llm.summary_calls.load(Ordering::SeqCst),
        )
    });

    assert_eq!(
        llm.graph_calls.load(Ordering::SeqCst),
        1,
        "one chunk must produce exactly one graph-extraction call"
    );
    assert_eq!(
        llm.summary_calls.load(Ordering::SeqCst),
        1,
        "one chunk must produce exactly one summarization call"
    );
    assert_eq!(
        output.summaries.len(),
        1,
        "the merge must carry the summarization branch's output through, not \
         just the graph branch's"
    );
}

// ───────────────────────────────── test 2 ───────────────────────────────────

/// The negative control: the pre-fusion shape fails the same rendezvous.
///
/// Without this, test 1 is unfalsifiable — a rendezvous that a serial pipeline
/// also satisfies would prove nothing about the fused one. Here the *same two
/// branch tasks* run one after the other over the *same* `RendezvousLlm`,
/// which is exactly what `build_cognify_pipeline` did before `040385e2`. The
/// graph call opens its gate and then waits for a summary call that the
/// sequential shape cannot dispatch until extraction has already returned, so
/// it hangs — and that hang is the assertion.
#[tokio::test(flavor = "multi_thread")]
async fn sequential_composition_of_the_same_branches_deadlocks() {
    let llm = Arc::new(RendezvousLlm::new());
    let (_cancel, ctx) = task_ctx().await;
    let input = one_chunk_input();

    let extract = make_extract_graph_task(
        Arc::clone(&llm) as Arc<dyn Llm>,
        Arc::new(MockGraphDB::new()) as Arc<dyn GraphDBTrait>,
        Arc::new(NoOpOntologyResolver::new()),
        Arc::clone(&ctx.database),
        stage_config(),
    );
    let summarize = make_summarize_text_task(Arc::clone(&llm) as Arc<dyn Llm>, stage_config());

    let sequential = {
        let ctx = Arc::clone(&ctx);
        let input = input.clone();
        async move {
            // Stage boundary: the summarization future is not even *built*
            // until extraction has fully returned. That is the whole
            // difference from `try_parallel`.
            let _graph = call_task(extract, &input, Arc::clone(&ctx)).await;
            let _summaries = call_task(summarize, &input, ctx).await;
        }
    };

    let outcome = tokio::time::timeout(DEADLOCK_CONFIRM, sequential).await;

    assert!(
        outcome.is_err(),
        "the sequential composition completed, so the rendezvous does not \
         distinguish concurrent execution from serial execution and test 1 \
         proves nothing. Either a branch stopped calling the LLM, or the \
         schema routing in `is_summary_schema` no longer separates the two \
         halves."
    );
    assert_eq!(
        llm.graph_calls.load(Ordering::SeqCst),
        1,
        "extraction must have dispatched its call and then parked"
    );
    assert_eq!(
        llm.summary_calls.load(Ordering::SeqCst),
        0,
        "the point of the control: summarization never got to dispatch, \
         because the stage in front of it had not returned"
    );
}

// ───────────────────────────────── test 3 ───────────────────────────────────

/// One observed LLM call, shaped like a telemetry span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallKind {
    Graph,
    Summary,
}

#[derive(Clone, Copy, Debug)]
struct Span {
    kind: CallKind,
    start_ms: u128,
    end_ms: u128,
}

/// An `Llm` that answers immediately but records when each call started and
/// finished, so the test can inspect the same intervals a span exporter would.
///
/// Each call sleeps for [`Self::DWELL`] so the spans have width; with zero-width
/// spans "overlap" is not a well-defined question.
struct RecordingLlm {
    epoch: Instant,
    spans: Mutex<Vec<Span>>,
}

impl RecordingLlm {
    const DWELL: Duration = Duration::from_millis(60);

    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            spans: Mutex::new(Vec::new()),
        }
    }

    fn spans(&self) -> Vec<Span> {
        self.spans.lock().expect("span lock").clone() // lock poison is unrecoverable
    }
}

#[async_trait]
impl Llm for RecordingLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Ok(GenerationResponse {
            content: r#"{"nodes":[],"edges":[]}"#.to_string(),
            model: "recording-llm".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let kind = if is_summary_schema(json_schema) {
            CallKind::Summary
        } else {
            CallKind::Graph
        };
        let start_ms = self.epoch.elapsed().as_millis();

        tokio::time::sleep(Self::DWELL).await;

        let end_ms = self.epoch.elapsed().as_millis();
        self.spans.lock().expect("span lock").push(Span {
            kind,
            start_ms,
            end_ms,
        }); // lock poison is unrecoverable

        Ok(match kind {
            CallKind::Summary => json!({ "summary": "s", "description": "d" }),
            CallKind::Graph => json!({ "nodes": [], "edges": [] }),
        })
    }

    fn model(&self) -> &str {
        "recording-llm"
    }
}

/// Render the recorded spans as a timeline, so a failure shows the evidence
/// rather than asking the reader to take a boolean on trust.
fn render_timeline(spans: &[Span]) -> String {
    let width = 60_u128;
    let horizon = spans.iter().map(|s| s.end_ms).max().unwrap_or(1).max(1);
    let mut out = String::from("\n  kind     |ms     | timeline\n");
    let mut sorted = spans.to_vec();
    sorted.sort_by_key(|s| (s.start_ms, s.end_ms));
    for span in &sorted {
        let from = (span.start_ms * width / horizon) as usize;
        let to = (span.end_ms * width / horizon) as usize;
        let bar: String = (0..=width as usize)
            .map(|i| if i >= from && i <= to { '#' } else { '.' })
            .collect();
        out.push_str(&format!(
            "  {:<8} |{:>3}-{:<3}| {}\n",
            match span.kind {
                CallKind::Graph => "graph",
                CallKind::Summary => "summary",
            },
            span.start_ms,
            span.end_ms,
            bar
        ));
    }
    out
}

/// End to end through the public `cognify()`, asserting the two halves overlap
/// in wall-clock time.
///
/// This is the test that speaks to a telemetry-based doubt directly: it
/// collects exactly what a span exporter collects — one `(kind, start, end)`
/// interval per LLM call — and then asks whether any graph interval intersects
/// any summary interval. Unlike tests 1 and 2 it exercises the whole pipeline,
/// so it also covers the wiring in `build_cognify_pipeline` and the executor
/// that drives it, not just the stage in isolation.
///
/// Four documents rather than one so the fan-out inside each branch is live
/// too, and a regression that serialised the *branches* while keeping each
/// branch's internal concurrency would still be caught.
#[tokio::test(flavor = "multi_thread")]
async fn full_cognify_overlaps_graph_and_summary_llm_calls() {
    const DOCUMENTS: usize = 4;

    let llm = Arc::new(RecordingLlm::new());
    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
    let embedding_engine: Arc<dyn EmbeddingEngine> = Arc::new(MockEmbeddingEngine::new(8));

    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    let mut data_items = Vec::with_capacity(DOCUMENTS);
    for index in 0..DOCUMENTS {
        let text = format!(
            "Document {index}. Natural language processing helps computers \
             understand human language, and knowledge graphs make that \
             understanding queryable."
        );
        let data_id = Uuid::new_v4();
        let location = format!("fusion-doc-{index}-{data_id}");
        let stored_location = storage
            .store(text.as_bytes(), &location)
            .await
            .expect("MockStorage::store should not fail");

        data_items.push(
            Data::builder(
                data_id,
                format!("doc-{index}.txt"),
                stored_location,
                format!("doc-{index}.txt"),
                "txt",
                "text/plain",
                format!("test-hash-fusion-{index}"),
                owner_id,
            )
            .build(),
        );
    }

    let db: Arc<DatabaseConnection> = {
        let conn = connect("sqlite::memory:").await.expect("in-memory sqlite");
        initialize(&conn).await.expect("initialize schema");
        // Ownership rows carry an FK to `datasets`, so the row has to exist
        // even for a run with no user.
        cognee_database::ops::datasets::create_dataset(
            &conn,
            Dataset::new("fusion".into(), owner_id, None, dataset_id),
        )
        .await
        .expect("seed dataset");
        Arc::new(conn)
    };

    let thread_pool: Arc<dyn CpuPool> =
        Arc::new(RayonThreadPool::with_default_threads().expect("RayonThreadPool init"));

    let result = cognify(
        data_items,
        dataset_id,
        None,
        None,
        None,
        Arc::clone(&llm) as Arc<dyn Llm>,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        db,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        &stage_config(),
    )
    .await
    .expect("cognify should succeed against mock backends");

    assert!(
        !result.summaries.is_empty(),
        "no summaries were produced, so there is no summarization work to \
         overlap with — the rest of this test would be vacuous"
    );

    let spans = llm.spans();
    let graph: Vec<Span> = spans
        .iter()
        .copied()
        .filter(|s| s.kind == CallKind::Graph)
        .collect();
    let summary: Vec<Span> = spans
        .iter()
        .copied()
        .filter(|s| s.kind == CallKind::Summary)
        .collect();

    assert!(
        !graph.is_empty() && !summary.is_empty(),
        "expected both kinds of LLM call; got {} graph and {} summary. \
         Schema routing may have drifted.{}",
        graph.len(),
        summary.len(),
        render_timeline(&spans)
    );

    // Half-open intersection: two calls overlapped if each began before the
    // other ended.
    let overlapping = graph
        .iter()
        .flat_map(|g| summary.iter().map(move |s| (g, s)))
        .filter(|(g, s)| g.start_ms < s.end_ms && s.start_ms < g.end_ms)
        .count();

    assert!(
        overlapping > 0,
        "NOT FUSED: no graph-extraction call overlapped any summarization call \
         in wall-clock time.\n\
         Every graph call finished before the first summary call began (or the \
         reverse), which is the signature of two sequential stages — the shape \
         `040385e2` replaced.{}",
        render_timeline(&spans)
    );

    // The stage-level view the commit message frames its claim in: the two
    // halves should cost `max(graph, summary)`, not `graph + summary`. Compare
    // the union of both kinds' extents against the sum of each kind's extent.
    let extent = |calls: &[Span]| -> u128 {
        let first = calls.iter().map(|s| s.start_ms).min().unwrap_or(0);
        let last = calls.iter().map(|s| s.end_ms).max().unwrap_or(0);
        last.saturating_sub(first)
    };
    let union_start = spans.iter().map(|s| s.start_ms).min().unwrap_or(0);
    let union_end = spans.iter().map(|s| s.end_ms).max().unwrap_or(0);
    let union = union_end.saturating_sub(union_start);
    let sum_of_parts = extent(&graph) + extent(&summary);

    println!(
        "fused-stage timing: graph extent {}ms over {} calls, summary extent \
         {}ms over {} calls, union {}ms, sum-of-parts {}ms, {} overlapping \
         pairs{}",
        extent(&graph),
        graph.len(),
        extent(&summary),
        summary.len(),
        union,
        sum_of_parts,
        overlapping,
        render_timeline(&spans)
    );

    assert!(
        union < sum_of_parts,
        "the two halves together spanned {union}ms, which is not less than the \
         {sum_of_parts}ms they would cost end to end — they did not overlap in \
         any meaningful amount.{}",
        render_timeline(&spans)
    );
}
