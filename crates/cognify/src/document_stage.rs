//! Per-document assembly and one ordered writer (SDK-1694 O4b).
//!
//! The cognify pipeline's third stage. It extracts, summarizes and saves the
//! run's documents **one document at a time**, as Python runs one pipeline per
//! data item, while keeping Rust's one shared pool of LLM calls and one
//! database writer:
//!
//! ```text
//!   extraction calls ─┐                ┌─ document complete ─> graph built on
//!   (one stream)      ├─> assembler ───┤   the CPU pool (O1 per-block
//!   summary calls ────┘   (counts the  │   expansion, once per document)
//!   (one stream)          results per  └─> one writer: merges it into the run,
//!                         document)         checks existing edges, saves it
//!                                           (persist_group), in input order
//! ```
//!
//! - **Dispatch.** Every chunk's extraction call and summary call go through
//!   [`dispatch_calls`]: one bounded stream each, no batch barrier. Results
//!   reach the assembler as they arrive. A document's calls start only once
//!   the document is **admitted**: at most
//!   [`CognifyConfig::max_unsaved_documents`] documents are admitted and not
//!   yet saved at once, which bounds the LLM results waiting to be saved. The
//!   run's chunk text, its merged graph ([`RunGraph`]), the edge-status map
//!   and the `CognifyResult` still grow with the run.
//! - **Stop.** Extraction, summaries and the writer share one [`StopFlag`].
//!   Under `FailFast` a failed call stops both streams; a failed save stops
//!   everything and fails the stage, and dropping the stage aborts the calls
//!   in flight.
//! - **Assembler.** It counts each document's outstanding results. Once the
//!   last one is in, it builds the document's graph on the CPU pool (the
//!   expansion of its blocks, with fresh maps) and hands it to the writer, in
//!   input order. Under `FailFast` a document with a failed or skipped call is
//!   not saved: only complete documents are, as before.
//! - **Writer.** One writer, saving in input order (D5): it merges the
//!   document into the run's graph ([`RunGraph`], first block wins, types
//!   combined), looks up which of the document's edges an earlier run already
//!   wrote, and saves the document's group through the persistence layer with
//!   one per-document [`PersistState`] (rewrite on change), inside one
//!   bulk-load scope.
//!
//! Completion markers and the rollback decision still happen once, at the end
//! of the run, and a failed save still fails the run (SDK-1694 O5b moves them
//! per document). DLT foreign-key edges need every document of the run, so
//! they stay in the post-run DLT step.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cognee_database::RelationalDb;
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_models::{Document, DocumentChunk};
use cognee_ontology::OntologyResolver;
use cognee_vector::{BulkLoadGuard, VectorDB};
use futures::stream::{FuturesOrdered, StreamExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::chunk_dispatch::{ChunkCall, Dispatch, StopFlag, dispatch_calls};
use crate::config::CognifyConfig;
use crate::dynamic_graph::{DynamicGraph, DynamicGraphSchema};
use crate::error::CognifyError;
use crate::fact_extraction::{FactExtractor, KnowledgeGraph};
use crate::failure::{FailureReport, FailureStage, FailureStop, RollbackScope, StageFailure};
use crate::graph_backend::{ChunkGraphExtractor, ChunkRef, ExtractionContext, GraphBackendError};
use crate::graph_extraction::{EdgeLabels, ensure_default_edge_text_for_pairs};
use crate::graph_integration::expansion::{ResolutionLog, RunGraph, UnitExpander};
use crate::graph_integration::{
    GraphEdgePair, GraphNodePair, extraction_units, retrieve_existing_edges,
};
use crate::persist::{PersistContext, PersistGroup, PersistState};
use crate::pipeline::CognifyResult;
use crate::summarization::{SummaryExtractor, TextSummary};
use crate::tasks::{
    AttributedDynamicGraph, COGNIFY_PIPELINE_STAMP_NAME, EXTRACT_GRAPH_AND_SUMMARIZE_TASK_NAME,
    EXTRACT_GRAPH_TASK_NAME, ExtractedChunks, LedgerIdentity,
};

/// The stores and services the stage reads and writes.
pub(crate) struct StageStores {
    pub(crate) llm: Arc<dyn Llm>,
    pub(crate) graph_db: Arc<dyn GraphDBTrait>,
    pub(crate) vector_db: Arc<dyn VectorDB>,
    pub(crate) embedding_engine: Arc<dyn EmbeddingEngine>,
    pub(crate) db: Arc<dyn RelationalDb>,
    pub(crate) ontology: Arc<dyn OntologyResolver>,
}

/// Who runs the stage, and the stamps it writes.
pub(crate) struct StageStamp {
    /// The provenance user label (the pipeline's `user_label()`).
    pub(crate) user_label: Option<String>,
    /// The run every ownership row is attributed to.
    pub(crate) pipeline_run_id: Option<Uuid>,
    /// `topological_rank` of the extracted entities, types and summaries.
    pub(crate) extract_rank: i32,
}

/// What the stage observed about itself, for tests.
#[derive(Debug, Default)]
pub(crate) struct StageStats {
    /// The most documents admitted and not yet saved at once.
    pub(crate) peak_unsaved: usize,
}

/// Extract, summarize and save every document of `input`, one document at a
/// time (see the module docs).
pub(crate) async fn run_document_stage(
    input: &ExtractedChunks,
    stores: &StageStores,
    config: &CognifyConfig,
    stamp: &StageStamp,
) -> Result<(CognifyResult, StageStats), CognifyError> {
    let plan = Plan::new(input);
    let policy = config.failure_policy();
    let fail_fast = policy.stop == FailureStop::FailFast;
    let stop = StopFlag::new();
    let admission = Admission::new(
        plan.documents.len(),
        config.max_unsaved_documents.max(1),
        stop.clone(),
    );

    let backend = config.graph_backend.as_ref().map(|handle| &handle.0);
    let backend_summarizes =
        config.enable_summarization && backend.is_some_and(|backend| backend.summarizes_chunks());
    let llm_summaries = config.enable_summarization && !backend_summarizes;
    if !plan.calls.is_empty() {
        if !config.enable_summarization {
            info!("Summarization disabled in config");
        } else if backend_summarizes {
            info!(
                chunks = plan.calls.len(),
                "Summarization delegated to the graph backend; skipping the LLM summarizer"
            );
        }
    }
    let dlt_chunks = input.chunks.len() - plan.calls.len();
    if dlt_chunks > 0 {
        info!(
            "Skipping {dlt_chunks} DLT chunks from LLM extraction and summarization ({} \
             non-DLT chunks remain)",
            plan.calls.len()
        );
    }

    let (arrivals, arrivals_rx) = mpsc::unbounded_channel::<Arrival>();
    let (ready, ready_rx) = mpsc::unbounded_channel::<ReadyDocument>();

    let extraction = dispatch_extraction(
        input,
        &plan,
        stores,
        config,
        &admission,
        &stop,
        fail_fast,
        arrivals.clone(),
    );
    let summary_arrivals = arrivals.clone();
    let (plan_ref, admission_ref, stop_ref) = (&plan, &admission, &stop);
    let summaries = async move {
        if llm_summaries {
            dispatch_summaries(
                input,
                plan_ref,
                stores,
                config,
                admission_ref,
                stop_ref,
                fail_fast,
                summary_arrivals,
            )
            .await;
        }
        Ok::<(), CognifyError>(())
    };
    // The streams hold the only senders from here on: the assembler sees the
    // channel close once both are done.
    drop(arrivals);

    let expander =
        (config.graph_schema.is_none()).then(|| unit_expander(input, stores, config, stamp));
    let assembler = assemble(
        input,
        &plan,
        config,
        llm_summaries,
        fail_fast,
        expander,
        &admission,
        arrivals_rx,
        ready,
    );
    let writer = write_documents(
        input, stores, config, stamp, &policy, &admission, &stop, ready_rx,
    );

    let (_, _, records, written) = futures::try_join!(extraction, summaries, assembler, writer)
        .inspect_err(|_| stop.raise())?;

    let failures = failure_report(input, &plan, config, fail_fast, records);
    if stop.is_raised() && policy.scope != RollbackScope::FailedItems {
        // A `FailFast` stop under a scope that keeps nothing: the run fails as
        // a whole, as an extraction abort always did. Whatever was saved
        // before the stop is the rollback's to sweep.
        return Err(CognifyError::RunFailed {
            report: Box::new(failures),
        });
    }

    let outputs = written.state.into_outputs();
    let (entities, edges) = written.graph.into_parts();
    let edges = edges
        .into_iter()
        .filter(|edge| {
            matches!(
                written.edge_status.get(&edge.dedup_key()),
                Some(EdgeStatus::New)
            )
        })
        .collect();
    written.resolution.emit();
    let result = CognifyResult {
        chunks: written.chunks,
        entities,
        edges,
        summaries: written.summaries,
        edge_types: outputs.edge_types,
        embeddings: outputs.embeddings,
        embedding_count: outputs.embedding_count,
        indexed_fields: outputs.indexed_fields,
        documents_for_dlt: written.documents,
        already_completed: false,
        prior_pipeline_run_id: None,
        pipeline_run_id: stamp.pipeline_run_id,
        failures,
    };
    Ok((
        result,
        StageStats {
            peak_unsaved: admission.peak.load(Ordering::Acquire),
        },
    ))
}

// ── The plan ────────────────────────────────────────────────────────────────

/// The run's documents, in input order, and its LLM calls, in document order.
struct Plan {
    documents: Vec<PlannedDocument>,
    /// The chunk (index into `input.chunks`) of each call: every non-DLT
    /// chunk, document by document.
    calls: Vec<usize>,
    /// The document (index into `documents`) of each call.
    call_document: Vec<usize>,
}

struct PlannedDocument {
    /// `None` for chunks whose document is not in the input (a hand-built
    /// input): they are saved together, after every document.
    document: Option<Document>,
    /// Its chunks (indices into `input.chunks`), in input order.
    chunks: Vec<usize>,
    /// How many of its chunks are sent to the LLM.
    calls: usize,
}

impl Plan {
    fn new(input: &ExtractedChunks) -> Self {
        let mut documents: Vec<PlannedDocument> = input
            .documents
            .iter()
            .map(|document| PlannedDocument {
                document: Some(document.clone()),
                chunks: Vec::new(),
                calls: 0,
            })
            .collect();
        let position: HashMap<Uuid, usize> = input
            .documents
            .iter()
            .enumerate()
            .map(|(index, document)| (document.base.id, index))
            .collect();
        let mut orphans: Vec<usize> = Vec::new();
        for (index, chunk) in input.chunks.iter().enumerate() {
            match position.get(&chunk.document_id) {
                Some(document) => documents[*document].chunks.push(index),
                None => orphans.push(index),
            }
        }
        if !orphans.is_empty() {
            documents.push(PlannedDocument {
                document: None,
                chunks: orphans,
                calls: 0,
            });
        }
        let mut calls = Vec::new();
        let mut call_document = Vec::new();
        for (index, document) in documents.iter_mut().enumerate() {
            for chunk in &document.chunks {
                if !input.chunks[*chunk].is_dlt_row() {
                    calls.push(*chunk);
                    call_document.push(index);
                    document.calls += 1;
                }
            }
        }
        Self {
            documents,
            calls,
            call_document,
        }
    }
}

// ── Admission: the cap on unsaved documents ────────────────────────────────

/// Which documents may have calls in flight: at most `cap` documents are
/// admitted and not yet saved (or dropped) at once. Documents are admitted in
/// input order, whichever stream asks first, so a stream waiting to admit a
/// document never holds a permit a document before it needs.
///
/// One caller at a time admits the next document (it holds `next` while it
/// waits for a permit); every other caller waits for the admitted count to
/// pass its document, so a call of an already admitted document is never
/// stuck behind the caller waiting for a later one.
struct Admission {
    semaphore: Arc<Semaphore>,
    /// The next document to admit; held by the one caller admitting it.
    next: tokio::sync::Mutex<usize>,
    /// How many documents have been admitted, for the callers waiting.
    admitted_count: tokio::sync::watch::Sender<usize>,
    admitted: Vec<AtomicBool>,
    permits: Vec<Mutex<PermitSlot>>,
    stop: StopFlag,
    unsaved: AtomicUsize,
    peak: AtomicUsize,
}

impl Admission {
    fn new(documents: usize, cap: usize, stop: StopFlag) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(cap)),
            next: tokio::sync::Mutex::new(0),
            admitted_count: tokio::sync::watch::Sender::new(0),
            admitted: (0..documents).map(|_| AtomicBool::new(false)).collect(),
            permits: (0..documents)
                .map(|_| Mutex::new(PermitSlot::default()))
                .collect(),
            stop,
            unsaved: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    fn is_admitted(&self, document: usize) -> bool {
        self.admitted[document].load(Ordering::Acquire)
    }

    /// Wait until `document` (and every document before it) is admitted. A
    /// stage that has stopped admits nothing more: its calls are skipped.
    async fn admit(&self, document: usize) {
        let mut admitted = self.admitted_count.subscribe();
        loop {
            if *admitted.borrow_and_update() > document || self.stop.is_raised() {
                return;
            }
            if let Ok(mut next) = self.next.try_lock() {
                if *next <= document {
                    // The semaphore is never closed, so this cannot fail.
                    let permit = Arc::clone(&self.semaphore).acquire_owned().await.ok();
                    self.hold(*next, permit);
                    *next += 1;
                    self.admitted_count.send_replace(*next);
                }
                continue;
            }
            // Another caller is admitting: every admission it makes is
            // announced, and it lets go of `next` only after announcing.
            if admitted.changed().await.is_err() {
                return;
            }
        }
    }

    /// Admit `document` (and every document before it) if that needs no
    /// wait; `false` when a permit is not free right now, or the stage has
    /// stopped (which admits nothing).
    fn try_admit(&self, document: usize) -> bool {
        if self.is_admitted(document) {
            return true;
        }
        if self.stop.is_raised() {
            return false;
        }
        let Ok(mut next) = self.next.try_lock() else {
            return false;
        };
        while *next <= document {
            let Ok(permit) = Arc::clone(&self.semaphore).try_acquire_owned() else {
                return false;
            };
            self.hold(*next, Some(permit));
            *next += 1;
            self.admitted_count.send_replace(*next);
        }
        true
    }

    fn hold(&self, document: usize, permit: Option<OwnedSemaphorePermit>) {
        #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
        let mut slot = self.permits[document]
            .lock()
            .expect("lock poison is unrecoverable");
        self.admitted[document].store(true, Ordering::Release);
        if slot.released {
            // Saved or dropped before it was admitted (a document with no
            // calls, reached by a later document's admission): its permit
            // goes straight back.
            return;
        }
        slot.permit = permit;
        let unsaved = self.unsaved.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(unsaved, Ordering::AcqRel);
    }

    /// `document` was saved or dropped: its place in the window is free, and
    /// stays free — a later admission of it holds no permit.
    fn release(&self, document: usize) {
        if document >= self.permits.len() {
            return;
        }
        #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
        let mut slot = self.permits[document]
            .lock()
            .expect("lock poison is unrecoverable");
        slot.released = true;
        if slot.permit.take().is_some() {
            self.unsaved.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// One document's place in the [`Admission`] window.
#[derive(Default)]
struct PermitSlot {
    permit: Option<OwnedSemaphorePermit>,
    /// Saved or dropped: a permit handed to it later is returned at once.
    released: bool,
}

// ── Dispatch ────────────────────────────────────────────────────────────────

/// What one chunk's extraction call produced.
enum Extracted {
    Graph(KnowledgeGraph),
    Dynamic(DynamicGraph),
    /// A graph backend's graph, and its summary when the backend summarizes.
    Backend(KnowledgeGraph, Option<Box<TextSummary>>),
}

/// A failed extraction call: its message as the report lists it, and the
/// error the report classifies (`None` for a backend failure, which is
/// recorded by message only, as the backend path always recorded it).
struct ExtractionFailure {
    message: String,
    error: Option<CognifyError>,
}

/// One call's outcome, as it reaches the assembler.
enum Arrival {
    Extraction(usize, Result<Extracted, ExtractionFailure>),
    ExtractionSkipped(usize),
    Summary(usize, ChunkCall<Box<TextSummary>>),
}

fn extraction_arrival(call: usize, outcome: ChunkCall<Extracted>) -> Arrival {
    match outcome {
        ChunkCall::Done(Ok(extracted)) => Arrival::Extraction(call, Ok(extracted)),
        ChunkCall::Done(Err(error)) => Arrival::Extraction(
            call,
            Err(ExtractionFailure {
                message: error.to_string(),
                error: Some(error),
            }),
        ),
        ChunkCall::Skipped => Arrival::ExtractionSkipped(call),
    }
}

/// Send every chunk's extraction call — to the LLM, the custom-schema LLM
/// call, or the graph backend.
#[allow(clippy::too_many_arguments)]
async fn dispatch_extraction(
    input: &ExtractedChunks,
    plan: &Plan,
    stores: &StageStores,
    config: &CognifyConfig,
    admission: &Admission,
    stop: &StopFlag,
    fail_fast: bool,
    arrivals: mpsc::UnboundedSender<Arrival>,
) -> Result<(), CognifyError> {
    if let Some(backend) = config.graph_backend.as_ref() {
        return dispatch_backend(
            backend.0.as_ref(),
            input,
            plan,
            stores.ontology.as_ref(),
            config,
            admission,
            stop,
            fail_fast,
            arrivals,
        )
        .await;
    }
    let dispatch = Dispatch {
        max_parallel: config.max_parallel_extractions.max(1),
        stop: stop.clone(),
        honour_stop: fail_fast,
        raise_on_failure: fail_fast,
        what: "graph extraction",
    };
    let admit = |call: usize| admission.admit(plan.call_document[call]);
    let sink = |call: usize, outcome: ChunkCall<Extracted>| {
        let _ = arrivals.send(extraction_arrival(call, outcome));
    };
    let chunk_of = |call: usize| &input.chunks[plan.calls[call]];

    if let Some(schema_value) = config.graph_schema.as_ref() {
        let schema = Arc::new(
            DynamicGraphSchema::compile(schema_value)
                .map_err(|error| CognifyError::ConfigError(error.to_string()))?,
        );
        // Date hints for the default prompt, per document, as on the whole-run
        // path; a custom prompt is sent verbatim with none.
        let mut hints = if config.custom_extraction_prompt.is_none() {
            crate::tasks::chunk_temporal_hints(plan.calls.iter().map(|c| &input.chunks[*c]))
        } else {
            HashMap::new()
        };
        let custom_prompt = config.custom_extraction_prompt.clone();
        dispatch_calls(
            plan.calls.len(),
            &dispatch,
            admit,
            |call| {
                let chunk = chunk_of(call);
                let chunk_hints = hints.remove(&chunk.base.id).unwrap_or_default();
                let graph = crate::tasks::dynamic_graph_call(
                    &stores.llm,
                    &schema,
                    custom_prompt.as_deref(),
                    input.dataset_id,
                    &chunk.text,
                    &chunk_hints,
                );
                async move { graph.await.map(Extracted::Dynamic) }
            },
            crate::tasks::dynamic_task_error,
            sink,
        )
        .await;
        return Ok(());
    }

    let mut hints = if crate::tasks::sends_temporal_hints(config) {
        crate::tasks::chunk_temporal_hints(plan.calls.iter().map(|c| &input.chunks[*c]))
    } else {
        HashMap::new()
    };
    let extractor = FactExtractor::new(Arc::clone(&stores.llm));
    dispatch_calls(
        plan.calls.len(),
        &dispatch,
        admit,
        |call| {
            let chunk = chunk_of(call);
            let chunk_hints = hints.remove(&chunk.base.id).unwrap_or_default();
            let graph =
                crate::tasks::graph_extraction_call(&extractor, config, &chunk.text, chunk_hints);
            async move { graph.await.map(Extracted::Graph) }
        },
        CognifyError::FactExtractionError,
        sink,
    )
    .await;
    Ok(())
}

/// Drive a graph backend over every chunk, in blocks of at most
/// `chunks_per_batch` chunks, one `extract_graphs` call at a time, sending
/// each chunk's result to the assembler as its block returns.
///
/// A block takes the chunks of admitted documents only: when the next
/// document cannot be admitted without a wait and the block already has
/// chunks, the block is sent as it is, so a block never waits on the
/// documents its own results would free.
#[allow(clippy::too_many_arguments)]
async fn dispatch_backend(
    backend: &dyn ChunkGraphExtractor,
    input: &ExtractedChunks,
    plan: &Plan,
    ontology: &dyn OntologyResolver,
    config: &CognifyConfig,
    admission: &Admission,
    stop: &StopFlag,
    fail_fast: bool,
    arrivals: mpsc::UnboundedSender<Arrival>,
) -> Result<(), CognifyError> {
    let ctx = ExtractionContext::new(&input.documents, ontology, input.dataset_id);
    let summarizes = config.enable_summarization && backend.summarizes_chunks();
    let block_size = config.chunks_per_batch.max(1);
    let mut next = 0usize;
    let mut blocks = 0usize;
    while next < plan.calls.len() {
        if fail_fast && stop.is_raised() {
            for call in next..plan.calls.len() {
                let _ = arrivals.send(Arrival::ExtractionSkipped(call));
            }
            break;
        }
        let mut block: Vec<usize> = Vec::new();
        while next < plan.calls.len() && block.len() < block_size {
            // A stop raised while this block was being filled (by a summary,
            // or by the wait for admission) ends dispatch: the loop's head
            // sends the rest as skipped.
            if fail_fast && stop.is_raised() {
                break;
            }
            let document = plan.call_document[next];
            if !admission.try_admit(document) {
                if !block.is_empty() {
                    break;
                }
                admission.admit(document).await;
                if fail_fast && stop.is_raised() {
                    break;
                }
            }
            block.push(next);
            next += 1;
        }
        if block.is_empty() {
            continue;
        }
        let chunks: Vec<&DocumentChunk> = block
            .iter()
            .map(|call| &input.chunks[plan.calls[*call]])
            .collect();
        let refs: Vec<ChunkRef<'_>> = chunks
            .iter()
            .map(|chunk| ChunkRef::new(chunk.base.id, chunk.document_id, &chunk.text))
            .collect();
        let mut block_failed = false;
        match backend.extract_graphs(&refs, &ctx).await {
            Err(error @ GraphBackendError::ArityMismatch { .. }) => return Err(error.into()),
            Err(error) => {
                // Whole-block failure: every chunk in it is charged.
                for (call, chunk) in block.iter().zip(&chunks) {
                    warn!(
                        data_id = %chunk.document_id,
                        chunk_id = %chunk.base.id,
                        "graph backend failed for chunk: {error}"
                    );
                    let _ = arrivals.send(Arrival::Extraction(
                        *call,
                        Err(ExtractionFailure {
                            message: error.to_string(),
                            error: None,
                        }),
                    ));
                }
                block_failed = true;
            }
            Ok(results) => {
                if results.len() != refs.len() {
                    return Err(GraphBackendError::ArityMismatch {
                        backend: backend.name().to_string(),
                        expected: refs.len(),
                        got: results.len(),
                    }
                    .into());
                }
                for ((call, chunk), result) in block.iter().zip(&chunks).zip(results) {
                    let graph = match result {
                        Ok(graph) => graph,
                        Err(error) => {
                            warn!(
                                data_id = %chunk.document_id,
                                chunk_id = %chunk.base.id,
                                "graph backend failed for chunk: {error}"
                            );
                            let _ = arrivals.send(Arrival::Extraction(
                                *call,
                                Err(ExtractionFailure {
                                    message: error.to_string(),
                                    error: None,
                                }),
                            ));
                            block_failed = true;
                            continue;
                        }
                    };
                    let summary = if summarizes {
                        backend_summary(backend, chunk, &graph)
                    } else {
                        None
                    };
                    let _ = arrivals.send(Arrival::Extraction(
                        *call,
                        Ok(Extracted::Backend(graph, summary.map(Box::new))),
                    ));
                }
                blocks += 1;
                info!(
                    "Processed graph extraction block {blocks} ({} chunks, {next} of {} \
                     dispatched) via backend '{}'",
                    block.len(),
                    plan.calls.len(),
                    backend.name()
                );
            }
        }
        if block_failed && fail_fast {
            stop.raise();
        }
    }
    Ok(())
}

/// A backend's summary of one chunk, scoped like the chunk; `None` when the
/// backend declined.
fn backend_summary(
    backend: &dyn ChunkGraphExtractor,
    chunk: &DocumentChunk,
    graph: &KnowledgeGraph,
) -> Option<TextSummary> {
    let chunk_ref = ChunkRef::new(chunk.base.id, chunk.document_id, &chunk.text);
    let text = backend.summarize_chunk(&chunk_ref, graph);
    if text.trim().is_empty() {
        debug!(
            chunk_id = %chunk.base.id,
            "graph backend '{}' produced no summary for this chunk",
            backend.name()
        );
        return None;
    }
    // `TextSummary::new` derives uuid5(chunk_id, b"TextSummary"), as the LLM
    // path does; the chunk's NodeSets and weight carry over (summarize_text.py:79,81).
    let mut summary = TextSummary::new(chunk.base.id, text, None, backend.name().to_string());
    summary.base.importance_weight = chunk.base.importance_weight;
    summary.base.belongs_to_set = chunk.base.belongs_to_set.clone();
    Some(summary)
}

/// Send every chunk's summary call.
#[allow(clippy::too_many_arguments)]
async fn dispatch_summaries(
    input: &ExtractedChunks,
    plan: &Plan,
    stores: &StageStores,
    config: &CognifyConfig,
    admission: &Admission,
    stop: &StopFlag,
    fail_fast: bool,
    arrivals: mpsc::UnboundedSender<Arrival>,
) {
    let extractor =
        SummaryExtractor::new_with_schema(Arc::clone(&stores.llm), config.summary_schema.clone());
    // A failed summary stops the stage only when it fails its item: with
    // `tolerate_summarization_failures` it fails nothing. Either way the
    // stream stops once the stage has stopped.
    let dispatch = Dispatch {
        max_parallel: config.max_parallel_extractions.max(1),
        stop: stop.clone(),
        honour_stop: fail_fast,
        raise_on_failure: fail_fast && !config.tolerate_summarization_failures,
        what: "summarization",
    };
    dispatch_calls(
        plan.calls.len(),
        &dispatch,
        |call| admission.admit(plan.call_document[call]),
        |call| extractor.summary_call(&input.chunks[plan.calls[call]], None),
        CognifyError::LlmError,
        |call, outcome| {
            let outcome = match outcome {
                ChunkCall::Done(result) => ChunkCall::Done(result.map(Box::new)),
                ChunkCall::Skipped => ChunkCall::Skipped,
            };
            let _ = arrivals.send(Arrival::Summary(call, outcome));
        },
    )
    .await;
}

// ── The assembler ───────────────────────────────────────────────────────────

/// Every failure and skip, by call, for the run's report.
#[derive(Default)]
struct Records {
    extraction: Vec<(usize, ExtractionFailure)>,
    extraction_skipped: Vec<usize>,
    summary: Vec<(usize, CognifyError)>,
    summary_skipped: Vec<usize>,
}

/// A document waiting for its results.
#[derive(Default)]
struct Pending {
    outstanding: usize,
    graphs: Vec<(usize, Extracted)>,
    summaries: Vec<(usize, TextSummary)>,
    /// A call of it failed its item, or was never sent.
    incomplete: bool,
}

/// A document ready for the writer.
struct ReadyDocument {
    index: usize,
    document: Option<Document>,
    chunks: Vec<DocumentChunk>,
    /// The expansion of each block, in block order, with its sample of
    /// unresolved endpoints.
    expansions: Vec<(crate::graph_integration::Expansion, Vec<String>)>,
    dynamic: Vec<AttributedDynamicGraph>,
    summaries: Vec<TextSummary>,
}

/// The expander the run's documents share: per-chunk lookups built once, one
/// ontology cache, the run's CPU pool.
fn unit_expander(
    input: &ExtractedChunks,
    stores: &StageStores,
    config: &CognifyConfig,
    stamp: &StageStamp,
) -> UnitExpander {
    // Each extracted entity inherits its chunk's NodeSets and importance
    // weight (Python `expand_with_nodes_and_edges.py:66,79,163,227,229`).
    let extracted = input.chunks.iter().filter(|chunk| !chunk.is_dlt_row());
    let node_sets: HashMap<Uuid, Vec<serde_json::Value>> = extracted
        .clone()
        .filter_map(|chunk| {
            chunk
                .base
                .belongs_to_set
                .as_ref()
                .map(|sets| (chunk.base.id, sets.clone()))
        })
        .collect();
    let weights: HashMap<Uuid, f64> = extracted
        .map(|chunk| (chunk.base.id, chunk.base.importance_weight.unwrap_or(0.5)))
        .collect();
    let user_label = stamp
        .user_label
        .clone()
        .or_else(|| input.user_id.map(|id| id.to_string()));
    UnitExpander::new(
        input.dataset_id,
        node_sets,
        weights,
        Arc::clone(&stores.ontology),
        user_label.as_deref(),
        (stamp.extract_rank > 0).then_some(stamp.extract_rank),
        config.endpoint_scope,
        config.cpu_pool.as_ref().map(|pool| Arc::clone(&pool.0)),
    )
}

/// Count each document's results as they arrive, and hand every finished
/// document to the writer in input order — its graph built first, on the
/// CPU pool. Returns every failure and skip, for the report.
#[allow(clippy::too_many_arguments)]
async fn assemble(
    input: &ExtractedChunks,
    plan: &Plan,
    config: &CognifyConfig,
    llm_summaries: bool,
    fail_fast: bool,
    expander: Option<UnitExpander>,
    admission: &Admission,
    mut arrivals: mpsc::UnboundedReceiver<Arrival>,
    ready: mpsc::UnboundedSender<ReadyDocument>,
) -> Result<Records, CognifyError> {
    let per_call = if llm_summaries { 2 } else { 1 };
    let mut pending: Vec<Pending> = plan
        .documents
        .iter()
        .map(|document| Pending {
            outstanding: document.calls * per_call,
            ..Pending::default()
        })
        .collect();
    let mut records = Records::default();
    let mut next = 0usize;
    let mut builds = FuturesOrdered::new();
    let mut arrivals_open = true;

    loop {
        // Hand over every document that is complete and next in line.
        while next < plan.documents.len() && pending[next].outstanding == 0 {
            let done = std::mem::take(&mut pending[next]);
            if fail_fast && done.incomplete {
                // Only complete documents are saved under `FailFast`, as the
                // abort-time partition always kept them.
                admission.release(next);
            } else {
                builds.push_back(build_document(
                    input,
                    plan,
                    config,
                    expander.clone(),
                    next,
                    done,
                ));
            }
            next += 1;
        }
        if !arrivals_open && builds.is_empty() {
            break;
        }
        tokio::select! {
            built = builds.next(), if !builds.is_empty() => {
                if let Some(built) = built {
                    let _ = ready.send(built?);
                }
            }
            arrival = arrivals.recv(), if arrivals_open => {
                let Some(arrival) = arrival else {
                    arrivals_open = false;
                    continue;
                };
                let call = match &arrival {
                    Arrival::Extraction(call, _)
                    | Arrival::ExtractionSkipped(call)
                    | Arrival::Summary(call, _) => *call,
                };
                let chunk = plan.calls[call];
                let document = &mut pending[plan.call_document[call]];
                document.outstanding = document.outstanding.saturating_sub(1);
                match arrival {
                    Arrival::Extraction(_, Ok(extracted)) => document.graphs.push((chunk, extracted)),
                    Arrival::Extraction(_, Err(failure)) => {
                        document.incomplete = true;
                        records.extraction.push((call, failure));
                    }
                    Arrival::ExtractionSkipped(_) => {
                        document.incomplete = true;
                        records.extraction_skipped.push(call);
                    }
                    Arrival::Summary(_, ChunkCall::Done(Ok(summary))) => {
                        document.summaries.push((chunk, *summary));
                    }
                    Arrival::Summary(_, ChunkCall::Done(Err(error))) => {
                        document.incomplete |= !config.tolerate_summarization_failures;
                        records.summary.push((call, error));
                    }
                    Arrival::Summary(_, ChunkCall::Skipped) => {
                        document.incomplete = true;
                        records.summary_skipped.push(call);
                    }
                }
            }
        }
    }
    if next < plan.documents.len() {
        return Err(CognifyError::Execute(format!(
            "cognify: {} documents never received all their results",
            plan.documents.len() - next
        )));
    }
    Ok(records)
}

/// Build one finished document: its graph, block by block, on the CPU pool.
async fn build_document(
    input: &ExtractedChunks,
    plan: &Plan,
    config: &CognifyConfig,
    expander: Option<UnitExpander>,
    index: usize,
    mut done: Pending,
) -> Result<ReadyDocument, CognifyError> {
    let planned = &plan.documents[index];
    let chunks: Vec<DocumentChunk> = planned
        .chunks
        .iter()
        .map(|chunk| input.chunks[*chunk].clone())
        .collect();
    // Chunk order, as the whole-run merge sees them: within a block the first
    // declaration of an entity decides its description and type.
    done.graphs.sort_by_key(|(chunk, _)| *chunk);
    done.summaries.sort_by_key(|(chunk, _)| *chunk);
    // (document, chunk, graph): the chunk's own document, so chunks of several
    // documents without a `Document` record still form separate units.
    let mut graphs: Vec<(Uuid, Uuid, KnowledgeGraph)> = Vec::new();
    let mut dynamic: Vec<AttributedDynamicGraph> = Vec::new();
    let mut summaries: Vec<TextSummary> = done.summaries.into_iter().map(|(_, s)| s).collect();
    for (chunk, extracted) in done.graphs {
        let chunk = &input.chunks[chunk];
        match extracted {
            Extracted::Graph(graph) => graphs.push((chunk.document_id, chunk.base.id, graph)),
            Extracted::Backend(graph, summary) => {
                graphs.push((chunk.document_id, chunk.base.id, graph));
                summaries.extend(summary.map(|summary| *summary));
            }
            Extracted::Dynamic(graph) => dynamic.push(AttributedDynamicGraph {
                graph,
                chunk_id: chunk.base.id,
                data_id: chunk.document_id,
            }),
        }
    }
    let expansions = match expander {
        Some(expander) if !graphs.is_empty() => {
            let units = extraction_units(
                &chunks,
                config.chunks_per_batch,
                graphs.into_iter().map(|(document_id, chunk_id, graph)| {
                    (document_id, chunk_id, (chunk_id, graph))
                }),
            );
            expander.expand(units).await.map_err(|error| {
                CognifyError::Execute(format!("building the extracted graph: {error}"))
            })?
        }
        _ => Vec::new(),
    };
    Ok(ReadyDocument {
        index,
        document: planned.document.clone(),
        chunks,
        expansions,
        dynamic,
        summaries,
    })
}

// ── The writer ──────────────────────────────────────────────────────────────

/// Whether an edge of the run is one the graph held before the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeStatus {
    /// Not in the graph before the run: written by the first document that
    /// produced it.
    New,
    /// In the graph before the run: claimed, not written.
    Existing,
}

/// What the writer saved, for the result.
struct Written {
    state: PersistState,
    graph: RunGraph,
    edge_status: HashMap<(Uuid, Uuid, String), EdgeStatus>,
    resolution: ResolutionLog,
    /// Every chunk of the run → its document, for the ownership fallbacks.
    chunk_documents: HashMap<Uuid, Uuid>,
    chunks: Vec<DocumentChunk>,
    documents: Vec<Document>,
    summaries: Vec<TextSummary>,
}

/// Save each document as it arrives — they arrive in input order — through
/// the persistence layer, with one per-document state, inside one bulk-load
/// scope.
#[allow(clippy::too_many_arguments)]
async fn write_documents(
    input: &ExtractedChunks,
    stores: &StageStores,
    config: &CognifyConfig,
    stamp: &StageStamp,
    policy: &crate::failure::FailurePolicy,
    admission: &Admission,
    stop: &StopFlag,
    mut ready: mpsc::UnboundedReceiver<ReadyDocument>,
) -> Result<Written, CognifyError> {
    let mut written = Written {
        state: PersistState::per_document(),
        graph: RunGraph::default(),
        edge_status: HashMap::new(),
        resolution: ResolutionLog::default(),
        chunk_documents: input
            .chunks
            .iter()
            .map(|chunk| (chunk.base.id, chunk.document_id))
            .collect(),
        chunks: Vec::new(),
        documents: Vec::new(),
        summaries: Vec::new(),
    };
    // Inside `cognify` this nests in the run's own scope (an inner scope's
    // end does no maintenance while the outer one is open); in a pipeline of
    // the caller's own it is the run's scope.
    let bulk = match BulkLoadGuard::begin(stores.vector_db.as_ref()).await {
        Ok(guard) => Some(guard),
        Err(error) => {
            warn!(%error, "cognify: vector store rejected the bulk-load scope");
            None
        }
    };
    let mut outcome = Ok(());
    while let Some(document) = ready.recv().await {
        let index = document.index;
        // Under any scope but `failed_items` a stop fails the run (`is_fatal`
        // is fatal for `whole_run` and `nothing` alike): saving more would
        // only add to what is swept, or kept from a failed run.
        if stop.is_raised() && policy.scope != RollbackScope::FailedItems {
            admission.release(index);
            continue;
        }
        let saved = write_document(input, stores, config, stamp, &mut written, document).await;
        admission.release(index);
        if let Err(error) = saved {
            stop.raise();
            outcome = Err(error);
            break;
        }
    }
    if let Some(bulk) = bulk
        && let Err(error) = bulk.finish().await
    {
        warn!(%error, "cognify: vector store failed to finish its bulk load");
    }
    outcome.map(|()| written)
}

/// Merge one document into the run's graph and save its group.
async fn write_document(
    input: &ExtractedChunks,
    stores: &StageStores,
    config: &CognifyConfig,
    stamp: &StageStamp,
    written: &mut Written,
    document: ReadyDocument,
) -> Result<(), CognifyError> {
    let ReadyDocument {
        document: doc,
        mut chunks,
        expansions,
        mut dynamic,
        mut summaries,
        ..
    } = document;
    let user_label = stamp.user_label.as_deref();
    let rank = stamp.extract_rank;

    // ── The document's graph, merged into the run's ─────────────────────────
    let mut units = Vec::with_capacity(expansions.len());
    for (expansion, sample) in expansions {
        written.resolution.add(&expansion.stats, sample);
        units.push(expansion);
    }
    let view = written.graph.add_document(units, |pair| {
        crate::tasks::stamp_entity_provenance(
            std::slice::from_mut(pair),
            EXTRACT_GRAPH_TASK_NAME,
            user_label,
            rank,
        );
    });

    // Which of its edges the graph held before the run, looked up on their
    // final ids (Python's `find_existing_edge_identities`) after every earlier
    // document was saved. An edge an earlier document of this run wrote is
    // this run's: it stays an edge of this document, written once.
    let unseen: Vec<&GraphEdgePair> = view
        .edges
        .iter()
        .filter(|edge| !written.edge_status.contains_key(&edge.dedup_key()))
        .collect();
    let existing =
        retrieve_existing_edges(stores.graph_db.as_ref(), unseen.iter().copied()).await?;
    let mut new_edges: Vec<GraphEdgePair> = Vec::new();
    for edge in unseen {
        let key = edge.dedup_key();
        if existing.contains(&key) {
            written.edge_status.insert(key, EdgeStatus::Existing);
        } else {
            written.edge_status.insert(key, EdgeStatus::New);
            new_edges.push(edge.clone());
        }
    }
    // Python fills a blank `edge_text` in `add_data_points`; the new edges are
    // stamped once, on the run's copy, so every later document gets it too.
    stamp_edge_text(&mut new_edges, &view.nodes, &view.timestamps);
    let stamped: HashMap<(Uuid, Uuid, String), GraphEdgePair> = new_edges
        .into_iter()
        .map(|edge| (edge.dedup_key(), edge))
        .collect();
    for (key, edge) in &stamped {
        if let Some(run_copy) = written.graph.edge_mut(key) {
            *run_copy = edge.clone();
        }
    }
    let mut edges = Vec::new();
    let mut claimed = Vec::new();
    for edge in view.edges {
        let key = edge.dedup_key();
        match written.edge_status.get(&key) {
            Some(EdgeStatus::New) => edges.push(stamped.get(&key).cloned().unwrap_or(edge)),
            // Claimed by every document that produced it, by extraction or
            // through its ontology lookup (SDK-1694 O5a).
            Some(EdgeStatus::Existing) if view.producers.produces_edge(&key) => claimed.push(edge),
            _ => {}
        }
    }

    // ── The chunks' links, and the stamps ───────────────────────────────────
    let links = crate::tasks::chunk_entity_links(&view.nodes, &view.timestamps, &view.producers);
    let attachment = config
        .chunk_attachment
        .unwrap_or(crate::config::ChunkAttachment::Direct);
    for chunk in &mut chunks {
        if let Some(entity_ids) = links.get(&chunk.base.id) {
            chunk.contains = entity_ids.clone();
        }
        stamp_point(&mut chunk.base, EXTRACT_GRAPH_TASK_NAME, user_label, rank);
    }
    for attributed in &mut dynamic {
        let targets = match attachment {
            crate::config::ChunkAttachment::Direct => attributed.graph.direct_attachment_targets(),
            crate::config::ChunkAttachment::All => attributed.graph.all_attachment_targets(),
        };
        if let Some(chunk) = chunks
            .iter_mut()
            .find(|chunk| chunk.base.id == attributed.chunk_id)
        {
            chunk.contains = targets
                .into_iter()
                .map(|target| serde_json::json!(target.to_string()))
                .collect();
        }
        for node in &mut attributed.graph.nodes {
            stamp_point(&mut node.base, EXTRACT_GRAPH_TASK_NAME, user_label, rank);
        }
    }
    for summary in &mut summaries {
        stamp_point(
            &mut summary.base,
            EXTRACT_GRAPH_AND_SUMMARIZE_TASK_NAME,
            user_label,
            rank,
        );
    }
    let mut documents: Vec<Document> = doc.into_iter().collect();
    for document in &mut documents {
        stamp_point(
            &mut document.base,
            EXTRACT_GRAPH_TASK_NAME,
            user_label,
            rank,
        );
    }
    let web_pages = if config.create_web_page_nodes {
        crate::tasks::web_page_artifacts(&documents, &chunks)
    } else {
        Vec::new()
    };

    // ── Save ────────────────────────────────────────────────────────────────
    let ledger = LedgerIdentity::new(
        input.tenant_id,
        input.user_id,
        input.dataset_id,
        stamp.pipeline_run_id,
    );
    let evidence = LedgerIdentity {
        pipeline_run_id: config.evidence_pipeline_run_id.or(ledger.pipeline_run_id),
        ..ledger
    };
    let context = PersistContext {
        graph_db: &stores.graph_db,
        vector_db: &stores.vector_db,
        embedding_engine: &stores.embedding_engine,
        db: stores.db.as_ref(),
        ledger,
        evidence: (config.edge_evidence_enabled && input.user_id.is_some()).then_some(evidence),
        user_id: input.user_id,
        config,
        run_chunk_documents: Some(&written.chunk_documents),
    };
    let group = PersistGroup {
        chunks: &chunks,
        documents: &documents,
        summaries: &summaries,
        entities: &view.nodes,
        timestamps: &view.timestamps,
        edges: &edges,
        claimed_existing_edges: &claimed,
        producers: &view.producers,
        dynamic_graphs: &dynamic,
        web_pages: &web_pages,
        dlt: None,
    };
    crate::persist::persist_group(group, &mut written.state, &context).await?;

    written.chunks.extend(chunks);
    written.documents.extend(documents);
    written.summaries.extend(summaries);
    Ok(())
}

fn stamp_point(base: &mut cognee_models::DataPoint, task: &str, user: Option<&str>, rank: i32) {
    crate::tasks::stamp_provenance(base, COGNIFY_PIPELINE_STAMP_NAME, task, user, rank);
}

/// Stamp the default `edge_text` on edges that have none (Python
/// `ensure_default_edge_properties`), labelling the endpoints with the
/// entities', their types' and the Timestamps' names.
fn stamp_edge_text(
    edges: &mut [GraphEdgePair],
    nodes: &[GraphNodePair],
    timestamps: &[crate::graph_integration::TimestampNode],
) {
    if edges.is_empty() {
        return;
    }
    let mut labels = EdgeLabels::default();
    for node in timestamps {
        labels.insert(node.timestamp.base.id, &node.timestamp.name);
    }
    for pair in nodes {
        labels.insert(pair.entity.base.id, &pair.entity.name);
        for entity_type in pair.entity_types() {
            labels.insert(entity_type.base.id, &entity_type.name);
        }
    }
    let unlabelled = ensure_default_edge_text_for_pairs(edges, &labels);
    if unlabelled > 0 {
        debug!(
            unlabelled,
            "Extracted edges left without edge_text: an endpoint is not in this document"
        );
    }
}

// ── The report ──────────────────────────────────────────────────────────────

/// The run's failure report, built as the whole-run stages built it: the
/// upstream report, then extraction's failures in chunk order and the
/// documents it never reached, then summarization's, which keep only the
/// chunks of documents extraction did not exclude.
fn failure_report(
    input: &ExtractedChunks,
    plan: &Plan,
    config: &CognifyConfig,
    fail_fast: bool,
    mut records: Records,
) -> FailureReport {
    let policy = config.failure_policy();
    let chunk_of = |call: usize| &input.chunks[plan.calls[call]];
    let mut failures = input.failures.clone();
    let mut aborted = false;
    records.extraction.sort_by_key(|(call, _)| *call);
    let failed_extractions = records.extraction.len();
    for (call, failure) in records.extraction {
        let chunk = chunk_of(call);
        if failure.error.is_some() {
            warn!(
                data_id = %chunk.document_id,
                chunk_id = %chunk.base.id,
                "graph extraction failed for chunk: {}",
                failure.message
            );
        }
        let stage_failure = StageFailure {
            stage: FailureStage::GraphExtraction,
            data_id: chunk.document_id,
            chunk_id: Some(chunk.base.id),
            error: failure.message,
            fails_item: true,
        };
        match &failure.error {
            Some(error) => failures.record_error(stage_failure, error),
            None => failures.record(stage_failure),
        }
        aborted |= fail_fast;
    }
    records.extraction_skipped.sort_unstable();
    for call in &records.extraction_skipped {
        failures.mark_unreached(chunk_of(*call).document_id);
    }
    if !plan.calls.is_empty() {
        info!(
            "Extracted graphs from {} of {} chunks ({} never dispatched)",
            plan.calls.len() - failed_extractions - records.extraction_skipped.len(),
            plan.calls.len(),
            records.extraction_skipped.len()
        );
    }

    let mut summary_failures = FailureReport::with_policy(&policy);
    records.summary.sort_by_key(|(call, _)| *call);
    for (call, error) in &records.summary {
        let chunk = chunk_of(*call);
        warn!(
            data_id = %chunk.document_id,
            chunk_id = %chunk.base.id,
            "summarization failed for chunk: {error}"
        );
        summary_failures.record_error(
            StageFailure {
                stage: FailureStage::Summarization,
                data_id: chunk.document_id,
                chunk_id: Some(chunk.base.id),
                error: error.to_string(),
                fails_item: !config.tolerate_summarization_failures,
            },
            error,
        );
    }
    records.summary_skipped.sort_unstable();
    for call in &records.summary_skipped {
        summary_failures.mark_unreached(chunk_of(*call).document_id);
    }
    if !records.summary_skipped.is_empty() {
        warn!(
            skipped_chunks = records.summary_skipped.len(),
            "summarization stopped early under FailFast; the remaining chunks were never \
             dispatched"
        );
    }
    // A summary of a file extraction excluded is not charged to the run's
    // chunk ratio: that file was given up on already.
    if aborted {
        let excluded: BTreeSet<Uuid> = failures
            .failed_items()
            .iter()
            .chain(failures.unreached_items().iter())
            .copied()
            .collect();
        let surviving: BTreeSet<Uuid> = input
            .chunks
            .iter()
            .filter(|chunk| !excluded.contains(&chunk.document_id))
            .map(|chunk| chunk.base.id)
            .collect();
        summary_failures.retain_failed_chunks(&surviving);
    }
    failures.absorb(&summary_failures);
    failures
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    //! The per-document stage (SDK-1694 O4b): order independence (ticket
    //! test 3), run-split independence (ticket test 4), the cap on unsaved
    //! documents, rewrite on change, stop and cancellation, and the
    //! whole-run equivalence of per-document groups.

    use super::*;
    use std::collections::BTreeSet;
    use std::time::Duration;

    use async_trait::async_trait;
    use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
    use cognee_llm::{LlmError, LlmResult, MessageRole};
    use cognee_models::DataPoint;
    use cognee_ontology::NoOpOntologyResolver;
    use serde_json::{Value, json};

    use crate::failure::FailureReport;
    use crate::persist::tests::{DATASET, Stores, USER};

    /// Answers each chunk by its text, `Name:Type Name:Type … [SLOW<ms>]
    /// [FAIL]`: one node per `Name:Type`, an edge between consecutive ones,
    /// and a summary. `SLOW<ms>` delays the extraction call; `FAIL` fails it.
    /// Counts the calls that started and finished.
    #[derive(Default)]
    struct TextLlm {
        started: AtomicUsize,
        finished: AtomicUsize,
        summaries_started: AtomicUsize,
        /// Extra delay for every summary call.
        summary_delay_ms: u64,
        /// Reverse the delays: later chunks first.
        reverse: bool,
        /// Fail the summary of a chunk whose text holds `FAILSUM`.
        fail_summaries: bool,
    }

    fn user_text(messages: &[Message]) -> String {
        messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .map(|message| message.content.as_str())
            .collect()
    }

    fn graph_from(text: &str) -> Value {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut previous: Option<String> = None;
        for token in text.split_whitespace() {
            let Some((name, kind)) = token.split_once(':') else {
                continue;
            };
            let id = name.to_lowercase();
            // `+` stands for a space, which the format cannot hold.
            let name = name.replace('+', " ");
            nodes.push(json!({"id": id, "name": name, "type": kind,
                              "description": format!("{name} as a {kind}.")}));
            if let Some(source) = previous.replace(id.clone()) {
                edges.push(json!({"source_node_id": source, "target_node_id": id,
                                  "relationship_name": "knows"}));
            }
        }
        json!({"nodes": nodes, "edges": edges})
    }

    fn delay_of(text: &str) -> u64 {
        text.split_whitespace()
            .find_map(|token| token.strip_prefix("SLOW"))
            .and_then(|ms| ms.parse().ok())
            .unwrap_or(0)
    }

    #[async_trait]
    impl Llm for TextLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            Err(LlmError::FeatureNotSupported("structured only".to_string()))
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            messages: Vec<Message>,
            json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            let text = user_text(&messages);
            let properties = json_schema.get("properties");
            if properties.and_then(|p| p.get("summary")).is_some() {
                self.summaries_started.fetch_add(1, Ordering::SeqCst);
                if self.fail_summaries && text.contains("FAILSUM") {
                    return Err(LlmError::ApiError("the summary fails".to_string()));
                }
                tokio::time::sleep(Duration::from_millis(self.summary_delay_ms)).await;
                return Ok(json!({"summary": format!("About {text}"), "description": "d"}));
            }
            self.started.fetch_add(1, Ordering::SeqCst);
            let mut delay = delay_of(&text);
            if self.reverse {
                delay = 60u64.saturating_sub(delay);
            }
            tokio::time::sleep(Duration::from_millis(delay)).await;
            if text.contains("FAIL") {
                return Err(LlmError::ApiError("the chunk fails".to_string()));
            }
            self.finished.fetch_add(1, Ordering::SeqCst);
            Ok(graph_from(&text))
        }

        fn model(&self) -> &str {
            "text"
        }
    }

    fn document(id: u128) -> Document {
        let mut base = DataPoint::new("TextDocument", None);
        base.id = Uuid::from_u128(id);
        Document {
            base,
            document_type: "text".to_string(),
            name: format!("doc-{id}"),
            raw_data_location: format!("file:///tmp/{id}.txt"),
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: Uuid::from_u128(id),
            external_metadata: None,
        }
    }

    fn chunk(document: u128, index: usize, text: &str, node_set: Option<&str>) -> DocumentChunk {
        let document_id = Uuid::from_u128(document);
        let mut chunk = DocumentChunk::new(
            Uuid::new_v5(&document_id, format!("chunk-{index}").as_bytes()),
            text.to_string(),
            text.split_whitespace().count(),
            index,
            "paragraph_end".to_string(),
            document_id,
        );
        if let Some(node_set) = node_set {
            chunk.base.belongs_to_set = Some(vec![json!(node_set)]);
        }
        chunk
    }

    /// `documents`: `(id, [(chunk text, node set)])`.
    /// A document: its id and its chunks' `(text, node set)`.
    type DocumentSpec<'a> = (u128, Vec<(&'a str, Option<&'a str>)>);

    fn input(documents: &[DocumentSpec<'_>]) -> ExtractedChunks {
        let mut chunks = Vec::new();
        for (id, texts) in documents {
            for (index, (text, node_set)) in texts.iter().enumerate() {
                chunks.push(chunk(*id, index, text, *node_set));
            }
        }
        ExtractedChunks {
            chunks,
            documents: documents.iter().map(|(id, _)| document(*id)).collect(),
            dataset_id: DATASET,
            user_id: Some(USER),
            tenant_id: None,
            failures: FailureReport::default(),
        }
    }

    fn stores_of(test: &Stores, llm: Arc<dyn Llm>) -> StageStores {
        StageStores {
            llm,
            graph_db: test.graph.clone(),
            vector_db: test.vector.clone(),
            embedding_engine: test.engine.clone(),
            db: Arc::new(test.db.clone()),
            ontology: Arc::new(NoOpOntologyResolver::new()),
        }
    }

    fn stamp() -> StageStamp {
        StageStamp {
            user_label: None,
            pipeline_run_id: Some(Uuid::from_u128(7)),
            extract_rank: 3,
        }
    }

    async fn run(
        test: &Stores,
        llm: Arc<dyn Llm>,
        input: &ExtractedChunks,
        config: &CognifyConfig,
    ) -> Result<(CognifyResult, StageStats), CognifyError> {
        run_document_stage(input, &stores_of(test, llm), config, &stamp()).await
    }

    /// The `(entity, type)` pairs the graph holds: its `is_a` edges, by name.
    async fn entity_types(test: &Stores) -> BTreeSet<(String, String)> {
        let (nodes, edges) = test.graph.get_graph_data().await.unwrap();
        let names: HashMap<String, String> = nodes
            .into_iter()
            .map(|(id, props)| {
                let name = props
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                (id, name)
            })
            .collect();
        edges
            .into_iter()
            .filter(|(_, _, relationship, _)| relationship == "is_a")
            .map(|(source, target, _, _)| (names[&source].clone(), names[&target].clone()))
            .collect()
    }

    fn three_documents() -> ExtractedChunks {
        input(&three_documents_spec())
    }

    fn three_documents_spec() -> Vec<DocumentSpec<'static>> {
        vec![
            (
                0xa,
                vec![
                    ("Alice:Person Acme:Company SLOW40", None),
                    ("Apple:Company Alice:Engineer SLOW5", None),
                ],
            ),
            (0xb, vec![("Apple:Fruit Bob:Person SLOW20", None)]),
            (
                0xc,
                vec![
                    ("Bob:Engineer Carol:Person SLOW1", None),
                    ("Carol:Person Dan:Person", None),
                ],
            ),
        ]
    }

    /// Ticket test 3: whatever order the calls complete in, the stage stores
    /// the same nodes, edges, `is_a` set and producers (the ledger rows).
    #[tokio::test]
    async fn completion_order_does_not_change_what_is_stored() {
        // One block per document: within it the first chunk's type wins, so
        // the chunks must reach graph building in chunk order.
        let config = CognifyConfig::default();
        let mut snapshots = Vec::new();
        let mut type_sets = Vec::new();
        for reverse in [false, true] {
            let test = Stores::new().await;
            let llm = Arc::new(TextLlm {
                reverse,
                ..TextLlm::default()
            });
            run(&test, llm, &three_documents(), &config).await.unwrap();
            type_sets.push(entity_types(&test).await);
            snapshots.push(test.snapshot().await);
        }
        assert_eq!(type_sets[0], type_sets[1]);
        assert!(type_sets[0].contains(&("apple".into(), "company".into())));
        assert!(type_sets[0].contains(&("apple".into(), "fruit".into())));
        assert!(type_sets[0].contains(&("alice".into(), "person".into())));
        assert!(!type_sets[0].contains(&("alice".into(), "engineer".into())));
        assert_eq!(snapshots[0], snapshots[1]);
    }

    /// Ticket test 4: two documents cognified together or one run each give
    /// the same `(entity, type)` pairs.
    #[tokio::test]
    async fn one_run_or_one_per_document_gives_the_same_types() {
        let config = CognifyConfig::default();
        let documents = [
            (0xa_u128, vec![("Apple:Company Alice:Person", None)]),
            (0xb_u128, vec![("Apple:Fruit Alice:Engineer", None)]),
        ];
        let together = Stores::new().await;
        run(
            &together,
            Arc::new(TextLlm::default()),
            &input(&documents),
            &config,
        )
        .await
        .unwrap();
        let apart = Stores::new().await;
        for document in &documents {
            run(
                &apart,
                Arc::new(TextLlm::default()),
                &input(std::slice::from_ref(document)),
                &config,
            )
            .await
            .unwrap();
        }
        let pairs = entity_types(&together).await;
        assert_eq!(pairs, entity_types(&apart).await);
        assert!(pairs.contains(&("apple".into(), "fruit".into())));
        assert!(pairs.contains(&("alice".into(), "engineer".into())));
    }

    /// The number of documents admitted and not yet saved never exceeds the
    /// cap, and the cap binds.
    #[tokio::test]
    async fn unsaved_documents_never_exceed_the_cap() {
        let documents: Vec<DocumentSpec<'_>> = (1..=8)
            .map(|id| {
                (
                    id as u128,
                    vec![
                        ("Alice:Person Bob:Person SLOW5", None),
                        ("Bob:Person Carol:Person", None),
                    ],
                )
            })
            .collect();
        for cap in [1, 2, 3] {
            let test = Stores::new().await;
            let config = CognifyConfig::default().with_max_unsaved_documents(cap);
            let (result, stats) = run(
                &test,
                Arc::new(TextLlm::default()),
                &input(&documents),
                &config,
            )
            .await
            .unwrap();
            assert!(
                stats.peak_unsaved <= cap,
                "cap {cap}: peak {}",
                stats.peak_unsaved
            );
            assert_eq!(stats.peak_unsaved, cap, "the cap binds");
            assert_eq!(result.documents_for_dlt.len(), 8, "every document saved");
        }
    }

    /// An entity two documents declare with different types and NodeSets:
    /// its graph node and its vector payload end up with the union, its name
    /// is embedded once, and the first document's description stays.
    #[tokio::test]
    async fn a_shared_entity_gains_the_later_documents_types_and_node_sets() {
        let test = Stores::new().await;
        let documents = [
            (
                0xa_u128,
                vec![("Alice:Person Acme:Company", Some("team-x"))],
            ),
            (
                0xb_u128,
                vec![("Alice:Engineer Bob:Person", Some("team-y"))],
            ),
        ];
        run(
            &test,
            Arc::new(TextLlm::default()),
            &input(&documents),
            &CognifyConfig::default(),
        )
        .await
        .unwrap();

        let alice = cognee_models::Entity::id_for("Alice");
        let node = test
            .graph
            .get_node(&alice.to_string())
            .await
            .unwrap()
            .expect("Alice is stored");
        let sets = node
            .get("belongs_to_set")
            .cloned()
            .unwrap_or_default()
            .to_string();
        assert!(
            sets.contains("team-x") && sets.contains("team-y"),
            "graph node: {sets}"
        );
        assert_eq!(
            node.get("extra_types")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1),
            "the second document's type is on the node"
        );
        assert_eq!(
            node.get("description").and_then(Value::as_str),
            Some("Alice as a Person."),
            "the first document's description stays"
        );

        let point = test.vector.all_points()["Entity_name"]
            .iter()
            .find(|point| point.id == alice)
            .cloned()
            .expect("Alice's vector point");
        let payload_sets = point.metadata["belongs_to_set"].to_string();
        assert!(
            payload_sets.contains("team-x") && payload_sets.contains("team-y"),
            "vector payload: {payload_sets}"
        );
        // Alice, Acme, Bob once each, plus the chunk, summary, type, document
        // and edge-type texts; Alice's name only once.
        let entity_points = test.vector.all_points()["Entity_name"].len();
        assert_eq!(entity_points, 3);
        // Both documents' ownership rows of Alice carry the merged entity.
        let alice_text = alice.to_string();
        let rows: Vec<String> = test.snapshot().await["nodes"]
            .iter()
            .filter(|row| row.contains(&format!("\"id\":\"{alice_text}\"")))
            .cloned()
            .collect();
        assert_eq!(rows.len(), 2, "one row per document: {rows:#?}");
        for row in rows {
            assert!(row.contains("team-y"), "a row keeps the old entity: {row}");
        }
    }

    /// A failed extraction under `FailFast` stops the summaries too: no
    /// summary call starts once it is in.
    #[tokio::test]
    async fn an_extraction_failure_stops_the_summaries() {
        let test = Stores::new().await;
        let llm = Arc::new(TextLlm {
            summary_delay_ms: 30,
            ..TextLlm::default()
        });
        let documents: Vec<DocumentSpec<'_>> =
            std::iter::once((1u128, vec![("Alice:Person FAIL", None)]))
                .chain((2..=6).map(|id| (id as u128, vec![("Bob:Person SLOW40", None)])))
                .collect();
        let config = CognifyConfig::default()
            .with_max_parallel_extractions(1)
            .with_rollback_scope(RollbackScope::FailedItems);
        let (result, _) = run(&test, llm.clone(), &input(&documents), &config)
            .await
            .unwrap();
        assert!(
            llm.summaries_started.load(Ordering::SeqCst) < documents.len(),
            "summaries stopped: {} of {}",
            llm.summaries_started.load(Ordering::SeqCst),
            documents.len()
        );
        assert!(result.failures.failed_items().contains(&Uuid::from_u128(1)));
        assert!(!result.failures.unreached_items().is_empty());
        assert!(
            result
                .documents_for_dlt
                .iter()
                .all(|document| document.base.id != Uuid::from_u128(1)),
            "the failed document is not saved"
        );
    }

    /// A failed save fails the stage and stops dispatch: the calls of the
    /// documents behind the window never start.
    #[tokio::test]
    async fn a_failed_save_stops_dispatch() {
        let test = Stores::new().await;
        test.graph.set_add_nodes_error("graph is down");
        let llm = Arc::new(TextLlm::default());
        let documents: Vec<DocumentSpec<'_>> = (1..=10)
            .map(|id| (id as u128, vec![("Alice:Person Bob:Person", None)]))
            .collect();
        let config = CognifyConfig::default().with_max_unsaved_documents(1);
        let error = run(&test, llm.clone(), &input(&documents), &config)
            .await
            .expect_err("the save fails");
        assert!(error.to_string().contains("graph is down"), "{error}");
        assert!(
            llm.started.load(Ordering::SeqCst) < documents.len(),
            "dispatch stopped: {} calls",
            llm.started.load(Ordering::SeqCst)
        );
    }

    /// Dropping the stage aborts the calls in flight: they never finish.
    #[tokio::test]
    async fn dropping_the_stage_aborts_calls_in_flight() {
        let test = Stores::new().await;
        let llm = Arc::new(TextLlm::default());
        let documents = [(
            1u128,
            vec![
                ("Alice:Person SLOW2000", None),
                ("Bob:Person SLOW2000", None),
            ],
        )];
        let input = input(&documents);
        let stores = stores_of(&test, llm.clone());
        let stage_stamp = stamp();
        let config = CognifyConfig::default();
        let stage = run_document_stage(&input, &stores, &config, &stage_stamp);
        let _ = tokio::time::timeout(Duration::from_millis(100), stage).await;
        assert_eq!(
            llm.started.load(Ordering::SeqCst),
            2,
            "both calls were in flight"
        );
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(
            llm.finished.load(Ordering::SeqCst),
            0,
            "no aborted call finished"
        );
    }

    /// Per-document groups, each built from its own document's expansion
    /// with its own producers, store exactly what the whole-run path stores
    /// as one group — and write every vector point once, embedding no text
    /// twice.
    #[tokio::test]
    async fn per_document_groups_store_what_one_group_stores() {
        let config = CognifyConfig::default().with_chunks_per_batch(1);
        // Stamped upstream, as classify and chunk stamp them in a pipeline.
        let mut documents = three_documents();
        for document in &mut documents.documents {
            stamp_point(&mut document.base, "classify_documents", None, 1);
        }
        for chunk in &mut documents.chunks {
            stamp_point(&mut chunk.base, "extract_chunks_from_documents", None, 2);
        }

        let whole = Stores::new().await;
        let llm: Arc<dyn Llm> = Arc::new(TextLlm::default());
        let graph = crate::tasks::build_extracted_graph(
            &documents,
            Arc::clone(&llm),
            whole.graph.clone(),
            Arc::new(NoOpOntologyResolver::new()),
            &config,
            None,
            Some(3),
        )
        .await
        .unwrap();
        let summaries = crate::tasks::summarize_text(&documents, llm, &config)
            .await
            .unwrap();
        let mut data = crate::tasks::SummarizedData {
            chunks: graph.chunks,
            documents: graph.documents,
            entities: graph.entities,
            timestamps: graph.timestamps,
            edges: graph.edges,
            claimed_existing_edges: graph.claimed_existing_edges,
            handoff: graph.handoff,
            producers: graph.producers,
            dynamic_graphs: graph.dynamic_graphs,
            summaries: summaries.summaries,
            dataset_id: DATASET,
            user_id: Some(USER),
            tenant_id: None,
            failures: FailureReport::default(),
        };
        crate::tasks::stamp_entity_provenance(&mut data.entities, EXTRACT_GRAPH_TASK_NAME, None, 3);
        for summary in &mut data.summaries {
            stamp_point(
                &mut summary.base,
                EXTRACT_GRAPH_AND_SUMMARIZE_TASK_NAME,
                None,
                3,
            );
        }
        let whole_result = crate::tasks::add_data_points(
            &data,
            whole.graph.clone(),
            whole.vector.clone(),
            whole.engine.clone(),
            &whole.db,
            Some(Uuid::from_u128(7)),
            &config,
        )
        .await
        .unwrap();

        let split = Stores::new().await;
        let (split_result, _) = run(&split, Arc::new(TextLlm::default()), &documents, &config)
            .await
            .unwrap();

        let (want, got) = (whole.snapshot().await, split.snapshot().await);
        assert_eq!(
            want.keys().collect::<Vec<_>>(),
            got.keys().collect::<Vec<_>>()
        );
        for (section, want) in &want {
            let got = &got[section];
            let missing: Vec<_> = want.difference(got).collect();
            let extra: Vec<_> = got.difference(want).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{section} differs per document:\nmissing {missing:#?}\nextra {extra:#?}"
            );
        }
        assert_eq!(
            split.engine.embedded_text_count(),
            whole.engine.embedded_text_count(),
            "no text embedded twice"
        );
        assert_eq!(split_result.embedding_count, whole_result.embedding_count);
        assert_eq!(
            split_result.indexed_fields.field_counts,
            whole_result.indexed_fields.field_counts
        );
        let edge_types = |result: &CognifyResult| -> BTreeSet<(Uuid, String, i32)> {
            result
                .edge_types
                .iter()
                .map(|e| (e.base.id, e.relationship_name.clone(), e.number_of_edges))
                .collect()
        };
        assert_eq!(edge_types(&split_result), edge_types(&whole_result));
    }

    /// Admission: documents are admitted in order, at most `cap` at once, and
    /// a released one frees its place.
    #[tokio::test]
    async fn admission_caps_unsaved_documents() {
        let admission = Admission::new(4, 2, StopFlag::new());
        admission.admit(1).await;
        assert!(admission.is_admitted(0) && admission.is_admitted(1));
        assert!(!admission.try_admit(2), "the window is full");
        admission.release(0);
        assert!(admission.try_admit(2));
        assert_eq!(admission.peak.load(Ordering::Acquire), 2);
        admission.release(1);
        admission.release(2);
        admission.admit(3).await;
        assert_eq!(admission.unsaved.load(Ordering::Acquire), 1);
    }

    // ── Per-document groups vs one group, with an ontology (review fixes) ──

    /// Alice works for Acme and for X_Y; Acme and X_Y are organisations.
    const ONTOLOGY: &str = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix : <http://test.cognee.ai/ontology#> .

:Person a owl:Class ; rdfs:label "Person" .
:Organisation a owl:Class ; rdfs:label "Organisation" .
:worksFor a owl:ObjectProperty .
:Alice a :Person ; rdfs:label "Alice" ; :worksFor :Acme ; :worksFor :X_Y .
:Acme a :Organisation ; rdfs:label "Acme" .
:X_Y a :Organisation ; rdfs:label "X_Y" .
"#;

    fn ontology() -> Arc<dyn OntologyResolver> {
        let reader: Box<dyn std::io::Read> = Box::new(ONTOLOGY.as_bytes());
        Arc::new(
            cognee_ontology::RdfLibOntologyResolver::new(
                cognee_ontology::OntologyFileInput::Reader(reader),
            )
            .unwrap(),
        )
    }

    /// Stamped as classify and chunk stamp them in a pipeline.
    fn stamped(mut documents: ExtractedChunks) -> ExtractedChunks {
        for document in &mut documents.documents {
            stamp_point(&mut document.base, "classify_documents", None, 1);
        }
        for chunk in &mut documents.chunks {
            stamp_point(&mut chunk.base, "extract_chunks_from_documents", None, 2);
        }
        documents
    }

    /// Save `documents` once through the whole-run path (one group) and once
    /// through the stage (one group per document); return both stores and
    /// every row only one of them holds, as `section ONLY-WHOLE|ONLY-SPLIT row`.
    async fn whole_vs_split(
        documents: &ExtractedChunks,
        ontology: Arc<dyn OntologyResolver>,
        config: &CognifyConfig,
    ) -> (Stores, Stores, Vec<String>) {
        let whole = Stores::new().await;
        let llm: Arc<dyn Llm> = Arc::new(TextLlm::default());
        let graph = crate::tasks::build_extracted_graph(
            documents,
            Arc::clone(&llm),
            whole.graph.clone(),
            Arc::clone(&ontology),
            config,
            None,
            Some(3),
        )
        .await
        .unwrap();
        let summaries = crate::tasks::summarize_text(documents, llm, config)
            .await
            .unwrap();
        let mut data = crate::tasks::SummarizedData {
            chunks: graph.chunks,
            documents: graph.documents,
            entities: graph.entities,
            timestamps: graph.timestamps,
            edges: graph.edges,
            claimed_existing_edges: graph.claimed_existing_edges,
            handoff: graph.handoff,
            producers: graph.producers,
            dynamic_graphs: graph.dynamic_graphs,
            summaries: summaries.summaries,
            dataset_id: DATASET,
            user_id: Some(USER),
            tenant_id: None,
            failures: FailureReport::default(),
        };
        crate::tasks::stamp_entity_provenance(&mut data.entities, EXTRACT_GRAPH_TASK_NAME, None, 3);
        for summary in &mut data.summaries {
            stamp_point(
                &mut summary.base,
                EXTRACT_GRAPH_AND_SUMMARIZE_TASK_NAME,
                None,
                3,
            );
        }
        crate::tasks::add_data_points(
            &data,
            whole.graph.clone(),
            whole.vector.clone(),
            whole.engine.clone(),
            &whole.db,
            Some(Uuid::from_u128(7)),
            config,
        )
        .await
        .unwrap();

        let split = Stores::new().await;
        let stores = StageStores {
            ontology,
            ..stores_of(&split, Arc::new(TextLlm::default()))
        };
        run_document_stage(documents, &stores, config, &stamp())
            .await
            .unwrap();

        let (want, got) = (whole.snapshot().await, split.snapshot().await);
        let empty = BTreeSet::new();
        let mut differences = Vec::new();
        for section in want.keys().chain(got.keys()).collect::<BTreeSet<_>>() {
            let (w, g) = (
                want.get(section).unwrap_or(&empty),
                got.get(section).unwrap_or(&empty),
            );
            differences.extend(
                w.difference(g)
                    .map(|row| format!("{section} ONLY-WHOLE {row}")),
            );
            differences.extend(
                g.difference(w)
                    .map(|row| format!("{section} ONLY-SPLIT {row}")),
            );
        }
        (whole, split, differences)
    }

    /// An ontology edge between an entity of a later document and one an
    /// earlier document's chunk is stamped on gets the ownership row one group
    /// gives it: the stamp is looked up run-wide.
    #[tokio::test]
    async fn an_ontology_edge_to_another_documents_entity_keeps_its_ownership_row() {
        let documents = stamped(input(&[
            (0xa, vec![("Acme:Company", None)]),
            (0xb, vec![("Alice:Human", None)]),
        ]));
        let (_, split, differences) =
            whole_vs_split(&documents, ontology(), &CognifyConfig::default()).await;
        assert!(differences.is_empty(), "{differences:#?}");
        let (alice, acme) = (
            cognee_models::Entity::id_for("Alice").to_string(),
            cognee_models::Entity::id_for("Acme").to_string(),
        );
        let snapshot = split.snapshot().await;
        let rows = snapshot["edges"]
            .iter()
            .filter(|row| row.contains(&alice) && row.contains(&acme))
            .count();
        assert!(rows > 0, "the alice → acme edge has an ownership row");
    }

    /// An entity only an earlier document's ontology lookup brought in, which
    /// a later document then extracts: per-document groups store exactly what
    /// one group stores (SDK-1694 O5a; O4b left the split with an extra nil
    /// row). The entity has a row for the extracting document, one for the
    /// document whose lookup brought it in, and the nil row an ontology-first
    /// entity keeps as the delete guard on both paths.
    #[tokio::test]
    async fn an_ontology_only_entity_extracted_later_matches_one_group() {
        let documents = stamped(input(&[
            (0xa, vec![("Alice:Human", None)]),
            (0xb, vec![("Acme:Company", None)]),
        ]));
        let (whole, split, differences) =
            whole_vs_split(&documents, ontology(), &CognifyConfig::default()).await;
        assert!(differences.is_empty(), "{differences:#?}");
        let acme = cognee_models::Entity::id_for("Acme");
        for stores in [&whole, &split] {
            assert_eq!(
                node_owners(stores, acme).await,
                BTreeSet::from([Uuid::nil(), Uuid::from_u128(0xa), Uuid::from_u128(0xb)])
            );
        }
    }

    /// An entity one block's lookup brings in and a later block of the same
    /// document extracts has only that document's row, as on `main`: the nil
    /// guard is decided per document, not per unit, so deleting the document
    /// still removes the entity (review F1).
    #[tokio::test]
    async fn an_entity_looked_up_and_extracted_by_one_document_gets_no_nil_row() {
        let documents = stamped(input(&[(
            0xa,
            vec![("Alice:Human", None), ("Acme:Company", None)],
        )]));
        let config = CognifyConfig::default().with_chunks_per_batch(1);
        let (whole, split, differences) = whole_vs_split(&documents, ontology(), &config).await;
        assert!(differences.is_empty(), "{differences:#?}");
        let acme = cognee_models::Entity::id_for("Acme");
        for stores in [&whole, &split] {
            assert_eq!(
                node_owners(stores, acme).await,
                BTreeSet::from([Uuid::from_u128(0xa)])
            );
        }
    }

    /// The `data_id`s of the ledger rows of the node `slug`.
    async fn node_owners(stores: &Stores, slug: Uuid) -> BTreeSet<Uuid> {
        cognee_database::ops::graph_storage::get_nodes_by_dataset(&stores.db, DATASET)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.slug == slug)
            .map(|row| row.data_id)
            .collect()
    }

    /// The `data_id`s of the ledger rows of the edge `(source, target,
    /// relationship)`, `relationship` being the edge's name (a `contains`
    /// row is keyed on its edge text and labelled `contains`).
    async fn edge_owners(
        stores: &Stores,
        source: Uuid,
        target: Uuid,
        relationship: &str,
    ) -> BTreeSet<Uuid> {
        cognee_database::ops::graph_storage::get_edges_by_dataset(&stores.db, DATASET)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| {
                row.source_node_id == source
                    && row.destination_node_id == target
                    && row.label.as_deref().unwrap_or(&row.relationship_name) == relationship
            })
            .map(|row| row.data_id)
            .collect()
    }

    // ── Per-document type and structural rows (SDK-1694 O5a) ─────────────

    /// The documents of `three_documents`, by letter.
    fn doc(letter: char) -> Uuid {
        Uuid::from_u128(u128::from(letter.to_digit(16).unwrap()))
    }

    /// `[nil, documents…]`, the owners an O5a row set has: the nil row every
    /// type and structural edge always had (the delete guard) and one row per
    /// producing document.
    fn owners(letters: &str) -> BTreeSet<Uuid> {
        std::iter::once(Uuid::nil())
            .chain(letters.chars().map(doc))
            .collect()
    }

    /// Ticket item: the ledger snapshot of a multi-document run. Every
    /// EntityType and `is_a` edge has rows for exactly the documents that
    /// declared it, a type several documents share has one per document, and
    /// every `contains` / `is_part_of` / `made_from` edge has a row for its
    /// chunk's document — each beside the nil row it always had.
    #[tokio::test]
    async fn every_type_and_structural_edge_names_the_documents_that_produced_it() {
        use cognee_models::{Entity, EntityType};
        let test = Stores::new().await;
        let input = three_documents();
        run(
            &test,
            Arc::new(TextLlm::default()),
            &input,
            &CognifyConfig::default(),
        )
        .await
        .unwrap();

        // Types: a shared type gets one row per declaring document. Alice is
        // a Person in doc A (its first declaration in the block wins over
        // Engineer), so `engineer` is doc C's alone (Bob).
        for (name, letters) in [
            ("person", "abc"),
            ("company", "a"),
            ("fruit", "b"),
            ("engineer", "c"),
        ] {
            assert_eq!(
                node_owners(&test, EntityType::id_for(name)).await,
                owners(letters),
                "EntityType {name}"
            );
        }

        // `is_a`: per (entity, type), the documents that declared that pair.
        for (entity, kind, letters) in [
            ("Alice", "person", "a"),
            ("Acme", "company", "a"),
            ("Apple", "company", "a"),
            ("Apple", "fruit", "b"),
            ("Bob", "person", "b"),
            ("Bob", "engineer", "c"),
            ("Carol", "person", "c"),
            ("Dan", "person", "c"),
        ] {
            assert_eq!(
                edge_owners(
                    &test,
                    Entity::id_for(entity),
                    EntityType::id_for(kind),
                    "is_a"
                )
                .await,
                owners(letters),
                "{entity} is_a {kind}"
            );
        }

        // Every structural edge of a chunk or summary: its document.
        let rows = cognee_database::ops::graph_storage::get_edges_by_dataset(&test.db, DATASET)
            .await
            .unwrap();
        let chunk_documents: HashMap<Uuid, Uuid> = input
            .chunks
            .iter()
            .map(|chunk| (chunk.base.id, chunk.document_id))
            .collect();
        let summary_documents: HashMap<Uuid, Uuid> = test
            .graph
            .get_graph_data()
            .await
            .unwrap()
            .1
            .into_iter()
            .filter(|(_, _, relationship, _)| relationship == "made_from")
            .map(|(summary, chunk, _, _)| {
                (
                    Uuid::parse_str(&summary).unwrap(),
                    chunk_documents[&Uuid::parse_str(&chunk).unwrap()],
                )
            })
            .collect();
        let mut checked = HashMap::<&str, usize>::new();
        for relationship in ["contains", "is_part_of", "made_from"] {
            let edges: BTreeSet<(Uuid, Uuid)> = rows
                .iter()
                .filter(|row| {
                    row.label.as_deref().unwrap_or(&row.relationship_name) == relationship
                })
                .map(|row| (row.source_node_id, row.destination_node_id))
                .collect();
            for (source, target) in edges {
                let document = chunk_documents
                    .get(&source)
                    .or_else(|| summary_documents.get(&source))
                    .copied()
                    .unwrap();
                assert_eq!(
                    edge_owners(&test, source, target, relationship).await,
                    BTreeSet::from([Uuid::nil(), document]),
                    "{relationship} {source} → {target}"
                );
                *checked.entry(relationship).or_default() += 1;
            }
        }
        assert_eq!(checked["is_part_of"], 5, "one per chunk");
        assert_eq!(checked["made_from"], 5, "one per summary");
        assert!(checked["contains"] >= 9, "{checked:?}");
    }

    /// Ticket item 5: an ontology edge the graph already holds, which a later
    /// run's document brings in through its own lookup, is claimed by that
    /// document (SDK-1694 O5a). Before, it was dropped and only the first
    /// run's rows named it.
    #[tokio::test]
    async fn an_existing_ontology_edge_is_claimed_by_each_document_that_looks_it_up() {
        use cognee_models::Entity;
        let test = Stores::new().await;
        let stores = StageStores {
            ontology: ontology(),
            ..stores_of(&test, Arc::new(TextLlm::default()))
        };
        for letter in ['a', 'b'] {
            let id = u128::from(letter.to_digit(16).unwrap());
            run_document_stage(
                &stamped(input(&[(id, vec![("Alice:Human", None)])])),
                &stores,
                &CognifyConfig::default(),
                &stamp(),
            )
            .await
            .unwrap();
        }
        let (alice, acme) = (Entity::id_for("Alice"), Entity::id_for("Acme"));
        let works_for: Vec<String> = test
            .graph
            .get_graph_data()
            .await
            .unwrap()
            .1
            .into_iter()
            .filter(|(source, target, _, _)| {
                *source == alice.to_string() && *target == acme.to_string()
            })
            .map(|(_, _, relationship, _)| relationship)
            .collect();
        assert_eq!(
            works_for.len(),
            1,
            "the ontology edge, written once: {works_for:?}"
        );
        let owners = edge_owners(&test, alice, acme, &works_for[0]).await;
        assert!(
            owners.contains(&doc('a')),
            "the first run's row: {owners:?}"
        );
        assert!(
            owners.contains(&doc('b')),
            "the second run's document claims the existing edge: {owners:?}"
        );
    }

    /// A whole-run sweep after a failed run removes every row and artifact the
    /// run wrote, the per-document type and structural rows (SDK-1694 O5a)
    /// and the nil rows alike.
    #[tokio::test]
    async fn a_whole_run_sweep_after_a_failed_run_leaves_nothing() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
        use cognee_delete::{RunSweeper, SweepScope};
        let test = Stores::new().await;
        let input = input(&[
            (0xa, vec![("Alice:Person Apple:Company", None)]),
            (0xb, vec![("Apple:Fruit Bob:Person", None)]),
            (0xc, vec![("Carol:Person FAIL SLOW30", None)]),
        ]);
        let config = CognifyConfig::default().with_rollback_scope(RollbackScope::FailedItems);
        let outcome = run(&test, Arc::new(TextLlm::default()), &input, &config)
            .await
            .map(|_| ());
        // The failed document was never saved; the others were.
        let nodes = get_nodes_by_dataset(&test.db, DATASET).await.unwrap();
        let typed = |letter: char| {
            nodes
                .iter()
                .any(|row| row.node_type == "EntityType" && row.data_id == doc(letter))
        };
        assert!(
            typed('a') && typed('b'),
            "per-document type rows before the sweep ({outcome:?})"
        );

        let sweeper = RunSweeper::new(
            Arc::new(test.db.clone()),
            test.graph.clone(),
            test.vector.clone(),
        );
        sweeper
            .sweep(&SweepScope::whole_run(Uuid::from_u128(7), DATASET))
            .await
            .unwrap();
        assert!(
            get_nodes_by_dataset(&test.db, DATASET)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            get_edges_by_dataset(&test.db, DATASET)
                .await
                .unwrap()
                .is_empty()
        );
        let (graph_nodes, graph_edges) = test.graph.get_graph_data().await.unwrap();
        assert!(graph_nodes.is_empty(), "{graph_nodes:?}");
        assert!(graph_edges.is_empty(), "{graph_edges:?}");
        let points: usize = test.vector.all_points().values().map(Vec::len).sum();
        assert_eq!(points, 0, "{:?}", test.vector.all_points().keys());
    }

    /// The item-scoped sweep (`SweepScope::for_data`, O5b's) of one document
    /// removes that document's rows — its type and structural rows included,
    /// which it can now select by `data_id` — and leaves every other row.
    #[tokio::test]
    async fn an_item_scoped_sweep_removes_only_that_documents_rows() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
        use cognee_delete::{RunSweeper, SweepScope};
        let test = Stores::new().await;
        run(
            &test,
            Arc::new(TextLlm::default()),
            &three_documents(),
            &CognifyConfig::default(),
        )
        .await
        .unwrap();
        let rows = || async {
            let nodes = get_nodes_by_dataset(&test.db, DATASET).await.unwrap();
            let edges = get_edges_by_dataset(&test.db, DATASET).await.unwrap();
            (nodes, edges)
        };
        let (nodes, edges) = rows().await;
        assert!(
            nodes
                .iter()
                .any(|row| row.node_type == "EntityType" && row.data_id == doc('a'))
                && edges
                    .iter()
                    .any(|row| row.relationship_name == "is_a" && row.data_id == doc('a')),
            "doc A has type and is_a rows to sweep"
        );
        let kept = |nodes: &[cognee_database::GraphNode], edges: &[cognee_database::GraphEdge]| {
            (
                nodes
                    .iter()
                    .filter(|row| row.data_id != doc('a'))
                    .map(|row| row.id)
                    .collect::<BTreeSet<_>>(),
                edges
                    .iter()
                    .filter(|row| row.data_id != doc('a'))
                    .map(|row| row.id)
                    .collect::<BTreeSet<_>>(),
            )
        };
        let before = kept(&nodes, &edges);

        RunSweeper::new(
            Arc::new(test.db.clone()),
            test.graph.clone(),
            test.vector.clone(),
        )
        .sweep(&SweepScope::for_data(
            Uuid::from_u128(7),
            DATASET,
            vec![doc('a')],
        ))
        .await
        .unwrap();

        let (nodes, edges) = rows().await;
        assert!(nodes.iter().all(|row| row.data_id != doc('a')));
        assert!(edges.iter().all(|row| row.data_id != doc('a')));
        assert_eq!(kept(&nodes, &edges), before, "every other row is kept");
    }

    /// A second dataset sharing the stores, so claims across datasets
    /// (`ClaimantScope::AnyDataset`, a dataset delete) are exercised.
    const OTHER_DATASET: Uuid = Uuid::from_u128(0x0a4a_5003);

    /// One pipeline run of the stage: its run id, dataset and documents.
    struct Step {
        run: u128,
        dataset: Uuid,
        documents: Vec<DocumentSpec<'static>>,
    }

    /// Run `steps` in order on `test`, each with its own run id.
    async fn run_steps(test: &Stores, steps: &[Step], ontology: &Arc<dyn OntologyResolver>) {
        cognee_database::ops::datasets::create_dataset(
            &test.db,
            cognee_models::Dataset::new("other".into(), USER, None, OTHER_DATASET),
        )
        .await
        .unwrap();
        for step in steps {
            let mut documents = stamped(input(&step.documents));
            documents.dataset_id = step.dataset;
            let stores = StageStores {
                ontology: Arc::clone(ontology),
                ..stores_of(test, Arc::new(TextLlm::default()))
            };
            let stamp = StageStamp {
                pipeline_run_id: Some(Uuid::from_u128(step.run)),
                ..stamp()
            };
            run_document_stage(&documents, &stores, &CognifyConfig::default(), &stamp)
                .await
                .unwrap();
        }
    }

    /// Every ledger row of both datasets.
    async fn all_rows(
        db: &cognee_database::DatabaseConnection,
    ) -> (
        Vec<cognee_database::GraphNode>,
        Vec<cognee_database::GraphEdge>,
    ) {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for dataset in [DATASET, OTHER_DATASET] {
            nodes.extend(get_nodes_by_dataset(db, dataset).await.unwrap());
            edges.extend(get_edges_by_dataset(db, dataset).await.unwrap());
        }
        (nodes, edges)
    }

    /// The ids of the rows O5a added to `test`'s ledger — everything the run
    /// would not have written before it:
    /// - a per-document EntityType row;
    /// - a per-document row of a structural edge (keyed, like its nil row, on
    ///   Python's `generate_edge_id` of its text);
    /// - an entity row for a document that neither extracted the entity (no
    ///   chunk of it `contains` the entity) nor is its `chunk_id` stamp's
    ///   document: a document that reached it by ontology lookup;
    /// - an ontology edge row (`ontology_valid` "true") for a document other
    ///   than the edge's fallback owner (both endpoints' stamp documents when
    ///   they agree, else nil): a lookup document's row or claim.
    async fn rows_added_by_o5a(test: &Stores) -> (Vec<Uuid>, Vec<Uuid>) {
        let (graph_nodes, graph_edges) = test.graph.get_graph_data().await.unwrap();
        drop(graph_nodes);
        let parse = |id: &str| Uuid::parse_str(id).unwrap();
        let chunk_documents: HashMap<Uuid, Uuid> = graph_edges
            .iter()
            .filter(|(_, _, relationship, _)| relationship == "is_part_of")
            .map(|(chunk, document, _, _)| (parse(chunk), parse(document)))
            .collect();
        let extracted: std::collections::HashSet<(Uuid, Uuid)> = graph_edges
            .iter()
            .filter(|(_, _, relationship, _)| relationship == "contains")
            .filter_map(|(chunk, entity, _, _)| {
                Some((*chunk_documents.get(&parse(chunk))?, parse(entity)))
            })
            .collect();
        let (nodes, edges) = all_rows(&test.db).await;
        // An entity row's `chunk_id` stamp, as its run saw it: each run
        // merges the entity anew, so the stamp is per (entity, run).
        let stamp_of = |row: &cognee_database::GraphNode| -> Option<Uuid> {
            let chunk = row
                .attributes
                .as_ref()?
                .pointer("/metadata/chunk_id")?
                .as_str()?;
            chunk_documents.get(&parse(chunk)).copied()
        };
        let stamps: HashMap<(Uuid, Option<Uuid>), Uuid> = nodes
            .iter()
            .filter(|row| row.node_type == "Entity")
            .filter_map(|row| Some(((row.slug, row.pipeline_run_id), stamp_of(row)?)))
            .collect();
        let node_ids = nodes
            .iter()
            .filter(|row| {
                !row.data_id.is_nil()
                    && (row.node_type == "EntityType"
                        || (row.node_type == "Entity"
                            && !extracted.contains(&(row.data_id, row.slug))
                            && stamp_of(row) != Some(row.data_id)))
            })
            .map(|row| row.id)
            .collect();
        let edge_ids = edges
            .iter()
            .filter(|row| {
                if row.data_id.is_nil() {
                    return false;
                }
                let text = row
                    .relationship_name
                    .to_lowercase()
                    .replace(' ', "_")
                    .replace('\'', "");
                let structural = row.slug == Uuid::new_v5(&Uuid::NAMESPACE_OID, text.as_bytes());
                let ontology = row
                    .attributes
                    .as_ref()
                    .and_then(|attributes| attributes.get("ontology_valid"))
                    .and_then(Value::as_str)
                    == Some("true");
                let fallback = match (
                    stamps.get(&(row.source_node_id, row.pipeline_run_id)),
                    stamps.get(&(row.destination_node_id, row.pipeline_run_id)),
                ) {
                    (Some(source), Some(target)) if source == target => *source,
                    _ => Uuid::nil(),
                };
                structural || (ontology && row.data_id != fallback)
            })
            .map(|row| row.id)
            .collect();
        (node_ids, edge_ids)
    }

    /// What every delete and sweep reads from the ledger, as artifact sets:
    /// for each document and claimant scope the data-scoped delete's unique
    /// nodes, unique edges, structural edges and surviving `EdgeType` texts;
    /// each dataset delete's unique nodes and edges; each run's whole-run and
    /// per-document sweeps' unique nodes, edges and surviving texts.
    async fn delete_selections(
        db: &cognee_database::DatabaseConnection,
        steps: &[Step],
    ) -> Vec<String> {
        use cognee_database::ClaimantScope;
        use cognee_database::ops::graph_storage::{self as ledger, RunScope};
        let (_, edges) = all_rows(db).await;
        let candidates: Vec<String> = edges
            .iter()
            .map(|row| {
                row.attributes
                    .as_ref()
                    .and_then(|attributes| attributes.get("edge_text"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .unwrap_or(row.relationship_name.trim())
                    .to_string()
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let slugs = |rows: Vec<cognee_database::GraphNode>| -> BTreeSet<Uuid> {
            rows.into_iter().map(|row| row.slug).collect()
        };
        let triples = |rows: Vec<cognee_database::GraphEdge>| -> BTreeSet<(Uuid, String, Uuid)> {
            rows.into_iter()
                .map(|row| {
                    (
                        row.source_node_id,
                        row.relationship_name,
                        row.destination_node_id,
                    )
                })
                .collect()
        };
        let mut out = Vec::new();
        for step in steps {
            let (run, dataset) = (Uuid::from_u128(step.run), step.dataset);
            for (id, _) in &step.documents {
                let data = Uuid::from_u128(*id);
                for scope in [ClaimantScope::SameDataset, ClaimantScope::AnyDataset] {
                    let at = format!("{id:x} {scope:?}");
                    out.push(format!(
                        "{at} unique nodes {:?}",
                        slugs(
                            ledger::get_unique_nodes_for_data(db, data, dataset, scope)
                                .await
                                .unwrap()
                        )
                    ));
                    out.push(format!(
                        "{at} unique edges {:?}",
                        triples(
                            ledger::get_unique_edges_for_data(db, data, dataset, scope)
                                .await
                                .unwrap()
                        )
                    ));
                    out.push(format!(
                        "{at} structural edges {:?}",
                        triples(
                            ledger::get_structural_edges_for_data(db, data, dataset, scope)
                                .await
                                .unwrap()
                        )
                    ));
                    out.push(format!(
                        "{at} texts kept {:?}",
                        ledger::get_edge_retrieval_texts_claimed_outside_data(
                            db,
                            data,
                            dataset,
                            scope,
                            &candidates
                        )
                        .await
                        .unwrap()
                    ));
                }
                let items = [data];
                let item_scope = RunScope::for_data(run, dataset, &items);
                out.push(format!(
                    "{id:x} run unique nodes {:?}",
                    slugs(
                        ledger::get_unique_nodes_for_run(db, &item_scope)
                            .await
                            .unwrap()
                    )
                ));
                out.push(format!(
                    "{id:x} run unique edges {:?}",
                    triples(
                        ledger::get_unique_edges_for_run(db, &item_scope)
                            .await
                            .unwrap()
                    )
                ));
                out.push(format!(
                    "{id:x} run texts kept {:?}",
                    ledger::get_edge_retrieval_texts_claimed_outside_run(db, &item_scope)
                        .await
                        .unwrap()
                ));
            }
            let whole = RunScope::whole_run(run, dataset);
            out.push(format!(
                "run {} unique nodes {:?}",
                step.run,
                slugs(ledger::get_unique_nodes_for_run(db, &whole).await.unwrap())
            ));
            out.push(format!(
                "run {} unique edges {:?}",
                step.run,
                triples(ledger::get_unique_edges_for_run(db, &whole).await.unwrap())
            ));
            out.push(format!(
                "run {} texts kept {:?}",
                step.run,
                ledger::get_edge_retrieval_texts_claimed_outside_run(db, &whole)
                    .await
                    .unwrap()
            ));
        }
        for dataset in [DATASET, OTHER_DATASET] {
            out.push(format!(
                "dataset {dataset} unique nodes {:?}",
                slugs(
                    ledger::get_unique_nodes_for_datasets(db, &[dataset])
                        .await
                        .unwrap()
                )
            ));
            out.push(format!(
                "dataset {dataset} unique edges {:?}",
                triples(
                    ledger::get_unique_edges_for_datasets(db, &[dataset])
                        .await
                        .unwrap()
                )
            ));
            out.push(format!(
                "dataset {dataset} texts kept {:?}",
                ledger::get_edge_retrieval_texts_claimed_outside_datasets(
                    db,
                    &[dataset],
                    &candidates
                )
                .await
                .unwrap()
            ));
        }
        out
    }

    /// Run `steps` into two stores, strip the O5a rows from one, and compare
    /// what every delete and sweep selects in each. Returns the selections
    /// and how many node and edge rows were stripped.
    async fn selections_with_and_without_o5a_rows(
        steps: &[Step],
        ontology: Arc<dyn OntologyResolver>,
    ) -> (Vec<String>, usize, usize) {
        let with_rows = Stores::new().await;
        let stripped = Stores::new().await;
        for stores in [&with_rows, &stripped] {
            run_steps(stores, steps, &ontology).await;
        }
        let (node_ids, edge_ids) = rows_added_by_o5a(&stripped).await;
        let counts = (node_ids.len(), edge_ids.len());
        for (table, ids) in [("nodes", node_ids), ("edges", edge_ids)] {
            for id in ids {
                use sea_orm::ConnectionTrait;
                stripped
                    .db
                    .execute_unprepared(&format!(
                        "DELETE FROM {table} WHERE id = '{}'",
                        id.simple()
                    ))
                    .await
                    .unwrap();
            }
        }
        let (want, got) = (
            delete_selections(&stripped.db, steps).await,
            delete_selections(&with_rows.db, steps).await,
        );
        assert_eq!(want.len(), got.len());
        let differences: Vec<String> = want
            .iter()
            .zip(&got)
            .filter(|(want, got)| want != got)
            .map(|(want, got)| format!("without O5a rows: {want}\nwith them:        {got}"))
            .collect();
        assert!(differences.is_empty(), "{}", differences.join("\n\n"));
        (want, counts.0, counts.1)
    }

    /// Whether some selection starting with `prefix` selected something.
    fn selects(selections: &[String], prefix: &str) -> bool {
        selections
            .iter()
            .any(|line| line.starts_with(prefix) && !line.ends_with("{}") && !line.ends_with("[]"))
    }

    /// Deletion is unchanged by the per-document rows (SDK-1694 O5a): every
    /// selection a data-scoped delete (both claimant scopes), a dataset
    /// delete, the whole-run sweep and the item-scoped sweep make reads the
    /// same artifacts from the ledger with them as with the ledger stripped
    /// back to what the runs wrote before O5a — over three runs, two of them
    /// in one dataset and one in a second dataset sharing the stores. The nil
    /// row each type and structural edge keeps claims its slug for every
    /// document, so a per-document row is never the last claim.
    ///
    /// The later run in the first dataset shares entities but no type with
    /// the earlier one: a type both declare is the one intended difference,
    /// pinned by `a_later_runs_document_keeps_a_shared_type_from_an_earlier_runs_sweep`.
    #[tokio::test]
    async fn the_per_document_rows_change_no_delete_selection() {
        let steps = [
            Step {
                run: 7,
                dataset: DATASET,
                documents: three_documents_spec(),
            },
            Step {
                run: 8,
                dataset: DATASET,
                documents: vec![(0xd, vec![("Apple:Gadget Erin:Robot", None)])],
            },
            Step {
                run: 9,
                dataset: OTHER_DATASET,
                documents: vec![(0xe, vec![("Alice:Person Apple:Fruit", None)])],
            },
        ];
        let (selections, nodes, edges) =
            selections_with_and_without_o5a_rows(&steps, Arc::new(NoOpOntologyResolver::new()))
                .await;
        assert!(
            nodes >= 6 && edges >= 20,
            "the runs wrote per-document rows: {nodes} nodes, {edges} edges"
        );
        // Not vacuous: deletes select artifacts, and claims across documents,
        // runs and datasets decide what they select.
        for prefix in [
            "a SameDataset unique nodes",
            "a SameDataset structural edges",
            "a SameDataset texts kept",
            "e AnyDataset unique nodes",
            "d run unique nodes",
            "run 8 unique nodes",
            "run 8 texts kept",
            &format!("dataset {OTHER_DATASET} unique nodes"),
        ] {
            assert!(selects(&selections, prefix), "{prefix}: {selections:#?}");
        }
    }

    /// The same with an ontology (review F2): documents that reach entities
    /// and edges only by lookup get rows only beside a nil row, and only for
    /// an edge whose endpoints they own, so no delete or sweep selects
    /// differently. Doc `a` extracts Alice and Acme, whose lookup brings in
    /// `alice works_for acme` (both endpoints stamped with doc `a`: a non-nil
    /// fallback row), `acme is_a organisation` and `alice works_for x_y`
    /// (X_Y is ontology-first: nil fallback rows). Doc `b` reaches Acme, X_Y
    /// and those edges by lookup alone (Acme was extracted by `a` first: no
    /// nil row, so `b` owns neither Acme nor its edges). In the second
    /// variant doc `e` extracts Alice and Acme too, so it owns both endpoints
    /// of `alice works_for acme`, whose fallback row stays `a`'s alone. A
    /// second run and a second dataset share Bob with doc `a` and look nothing
    /// up: an existing ontology edge a later run claims is the one intended
    /// difference, pinned by
    /// `an_existing_ontology_edge_is_claimed_by_each_document_that_looks_it_up`.
    #[tokio::test]
    async fn the_ontology_rows_change_no_delete_selection() {
        for co_extractor in [false, true] {
            let mut first: Vec<DocumentSpec<'static>> = vec![
                (0xa, vec![("Alice:Human Acme:Company Bob:Engineer", None)]),
                (0xb, vec![("Alice:Human", None)]),
            ];
            if co_extractor {
                first.push((0xe, vec![("Acme:Company Alice:Human", None)]));
            }
            let steps = [
                Step {
                    run: 7,
                    dataset: DATASET,
                    documents: first,
                },
                Step {
                    run: 8,
                    dataset: DATASET,
                    documents: vec![(0xc, vec![("Bob:Engineer Dave:Engineer", None)])],
                },
                Step {
                    run: 9,
                    dataset: OTHER_DATASET,
                    documents: vec![(0xd, vec![("Bob:Engineer Carol:Pilot", None)])],
                },
            ];
            let (selections, nodes, edges) =
                selections_with_and_without_o5a_rows(&steps, ontology()).await;
            assert!(
                nodes >= 2 && edges >= 2,
                "ontology rows stripped: {nodes} nodes, {edges} edges"
            );
            for prefix in [
                "a SameDataset unique nodes",
                "a SameDataset unique edges",
                "a AnyDataset texts kept",
                "run 7 unique nodes",
            ] {
                assert!(selects(&selections, prefix), "{prefix}: {selections:#?}");
            }
        }
    }

    /// A type (and an `is_a` edge) a later run's document also declares is
    /// no longer removed by a whole-run sweep of the earlier run: that
    /// document's row claims it. Before O5a both runs wrote the same nil row,
    /// which kept the earlier run's id, so the sweep deleted a type the later
    /// run still uses. The one sweep that selects differently, on purpose.
    #[tokio::test]
    async fn a_later_runs_document_keeps_a_shared_type_from_an_earlier_runs_sweep() {
        use cognee_delete::{RunSweeper, SweepScope};
        use cognee_models::EntityType;
        let test = Stores::new().await;
        let steps = [
            Step {
                run: 7,
                dataset: DATASET,
                documents: vec![(0xa, vec![("Apple:Company", None)])],
            },
            Step {
                run: 8,
                dataset: DATASET,
                documents: vec![(0xb, vec![("Apple:Company Bob:Engineer", None)])],
            },
        ];
        run_steps(&test, &steps, &(Arc::new(NoOpOntologyResolver::new()) as _)).await;
        RunSweeper::new(
            Arc::new(test.db.clone()),
            test.graph.clone(),
            test.vector.clone(),
        )
        .sweep(&SweepScope::whole_run(Uuid::from_u128(7), DATASET))
        .await
        .unwrap();
        let company = EntityType::id_for("company").to_string();
        let (nodes, _) = test.graph.get_graph_data().await.unwrap();
        assert!(
            nodes.iter().any(|(id, _)| *id == company),
            "doc b's company type survives the sweep of run 7"
        );
    }

    /// Chunks of two documents without a `Document` record are still built as
    /// two units: Apple keeps both documents' types.
    #[tokio::test]
    async fn chunks_without_a_document_record_keep_their_own_units() {
        let mut documents = stamped(input(&[
            (0xa, vec![("Apple:Company", None)]),
            (0xb, vec![("Apple:Fruit Bob:Person", None)]),
        ]));
        documents.documents.clear();
        let (whole, split, differences) = whole_vs_split(
            &documents,
            Arc::new(NoOpOntologyResolver::new()),
            &CognifyConfig::default(),
        )
        .await;
        assert!(differences.is_empty(), "{differences:#?}");
        let types = entity_types(&split).await;
        assert_eq!(types, entity_types(&whole).await);
        assert!(types.contains(&("apple".into(), "fruit".into())));
    }

    // ── Entity vectors on rewrite (review fixes) ──────────────────────────

    /// Texts embedded for one run of two documents that both declare Alice,
    /// with the given node sets, and the payload of Alice's point.
    async fn shared_alice(retain: bool, second_set: &str) -> (usize, String, Stores) {
        let test = Stores::new().await;
        let documents = input(&[
            (0xa, vec![("Alice:Person Acme:Company", Some("team-x"))]),
            (0xb, vec![("Alice:Engineer Bob:Person", Some(second_set))]),
        ]);
        let config = CognifyConfig::default().with_retained_embeddings(retain);
        run(&test, Arc::new(TextLlm::default()), &documents, &config)
            .await
            .unwrap();
        let alice = cognee_models::Entity::id_for("Alice");
        let payload = test.vector.all_points()["Entity_name"]
            .iter()
            .find(|point| point.id == alice)
            .map(|point| point.metadata["belongs_to_set"].to_string())
            .unwrap();
        (test.engine.embedded_text_count(), payload, test)
    }

    /// With `retain_embeddings` a changed payload is rewritten with the
    /// cached vector: nothing is embedded again.
    #[tokio::test]
    async fn a_rewritten_payload_reuses_the_retained_vector() {
        let (unchanged, _, _) = shared_alice(true, "team-x").await;
        let (changed, payload, _) = shared_alice(true, "team-y").await;
        assert!(
            payload.contains("team-x") && payload.contains("team-y"),
            "{payload}"
        );
        assert_eq!(changed, unchanged, "no text embedded again");
    }

    /// Without `retain_embeddings` nothing is cached: a changed payload is
    /// embedded once more by the index pass, once per actual change.
    #[tokio::test]
    async fn without_retention_a_rewritten_payload_is_embedded_once_more() {
        let (unchanged, _, _) = shared_alice(false, "team-x").await;
        let (changed, payload, _) = shared_alice(false, "team-y").await;
        assert!(
            payload.contains("team-x") && payload.contains("team-y"),
            "{payload}"
        );
        assert_eq!(changed, unchanged + 1, "Alice's name embedded once more");
    }

    /// An ontology placeholder (`x_y`, brought in by the first document's
    /// lookup) that a later document extracts as `x y`: the stored vector is
    /// the new name's, retained or not, and a retained vector is replaced.
    #[tokio::test]
    async fn a_renamed_entity_is_embedded_with_its_new_name() {
        use cognee_embedding::engine::EmbeddingEngine as _;
        for retain in [true, false] {
            let test = Stores::new().await;
            let documents = stamped(input(&[
                (0xa, vec![("Alice:Human", None)]),
                (0xb, vec![("X+Y:Company", None)]),
            ]));
            let config = CognifyConfig::default().with_retained_embeddings(retain);
            let stores = StageStores {
                ontology: ontology(),
                ..stores_of(&test, Arc::new(TextLlm::default()))
            };
            let (result, _) = run_document_stage(&documents, &stores, &config, &stamp())
                .await
                .unwrap();
            let id = cognee_models::Entity::id_for("X Y");
            let expected = test.engine.embed(&["x y"]).await.unwrap().remove(0);
            let point = test.vector.all_points()["Entity_name"]
                .iter()
                .find(|point| point.id == id)
                .cloned()
                .expect("the entity's point");
            assert_eq!(point.metadata["text"], json!("x y"), "retain {retain}");
            assert_eq!(
                point.vector, expected,
                "retain {retain}: the vector embeds the new name"
            );
            if retain {
                let retained: Vec<&Vec<f32>> = result
                    .embeddings
                    .iter()
                    .filter(|e| e.data_point_id == id && e.data_type == "Entity")
                    .map(|e| &e.vector)
                    .collect();
                assert_eq!(retained, vec![&expected], "the retained vector is replaced");
            }
        }
    }

    // ── Admission and the backend under a stop (review fixes) ────────────

    /// A document released before it was admitted (no calls of its own, saved
    /// while an earlier one still held the window) holds no permit when a
    /// later admission reaches it, so the later document is admitted.
    #[tokio::test]
    async fn a_document_released_before_admission_holds_no_permit() {
        let admission = Admission::new(3, 1, StopFlag::new());
        admission.admit(0).await;
        let waiting = admission.admit(2);
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        admission.release(0);
        admission.release(1);
        tokio::time::timeout(Duration::from_secs(5), &mut waiting)
            .await
            .expect("document 2 is admitted");
        assert_eq!(admission.unsaved.load(Ordering::Acquire), 1);
        assert!(admission.is_admitted(1) && admission.is_admitted(2));
    }

    /// Under `FailFast`, a failed summary stops the graph backend too: once
    /// it is in, no further block is sent to the backend.
    #[tokio::test]
    async fn a_stop_ends_the_backend_dispatch() {
        let test = Stores::new().await;
        let backend = Arc::new(crate::graph_backend::MockChunkGraphExtractor::new());
        let mut specs: Vec<DocumentSpec<'_>> = vec![(1, vec![("Alice:Person FAILSUM", None)])];
        specs.extend((2..=10).map(|id| (id as u128, vec![("Bob:Person", None)])));
        let mut config = CognifyConfig::default()
            .with_max_unsaved_documents(1)
            .with_rollback_scope(RollbackScope::FailedItems);
        config.graph_backend = Some(crate::config::GraphBackendHandle(backend.clone()));
        let llm = Arc::new(TextLlm {
            fail_summaries: true,
            ..TextLlm::default()
        });
        let outcome = tokio::time::timeout(
            Duration::from_secs(20),
            run(&test, llm, &input(&specs), &config),
        )
        .await
        .expect("the stage finishes");
        let (result, _) = outcome.unwrap();
        assert!(
            backend.chunks_seen() < 10,
            "the backend saw {} chunks after the stop",
            backend.chunks_seen()
        );
        assert!(!result.failures.unreached_items().is_empty());
    }

    /// Two documents fetched from one URL share a WebPage node: the last one
    /// in input order wins, as when the run is saved as one group.
    #[tokio::test]
    async fn two_documents_of_one_url_leave_the_last_ones_page() {
        let test = Stores::new().await;
        let mut documents = input(&[
            (0xa, vec![("Alice:Person FIRSTPAGE", None)]),
            (0xb, vec![("Bob:Person SECONDPAGE", None)]),
        ]);
        let url = json!({"_cognee": {"source_uri": "https://example.com/page"}}).to_string();
        for document in &mut documents.documents {
            document.external_metadata = Some(url.clone());
        }
        run(
            &test,
            Arc::new(TextLlm::default()),
            &documents,
            &CognifyConfig::default(),
        )
        .await
        .unwrap();
        let (nodes, _) = test.graph.get_graph_data().await.unwrap();
        let pages: Vec<String> = nodes
            .iter()
            .filter(|(_, props)| props.get("type") == Some(&json!("WebPage")))
            .map(|(_, props)| {
                props
                    .get("content")
                    .map(Value::to_string)
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(pages.len(), 1, "one page node");
        assert!(
            pages[0].contains("SECONDPAGE"),
            "the last document wins: {}",
            pages[0]
        );
    }
}
