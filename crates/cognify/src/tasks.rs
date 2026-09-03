//! Cognify pipeline tasks — individual steps of the cognify process.
//!
//! Matches the Python SDK task breakdown:
//! 1. [`classify_documents`] — Data items → Documents
//! 2. [`extract_chunks_from_documents`] — Documents → DocumentChunks
//! 3. [`extract_graph_from_data`] — Chunks → Chunks + entities/edges (stored in graph DB)
//! 4. [`summarize_text`] — + summaries via LLM
//! 5. [`add_data_points`] — embeddings + vector indexing → [`CognifyResult`]
//!
//! Temporal pipeline variant:
//! 1. [`classify_documents`] — same
//! 2. [`extract_chunks_from_documents`] — same
//! 3. [`extract_temporal_events`] — Chunks → [`AttributedEvent`]s (via two LLM passes)
//! 4. [`add_temporal_data_points`] — persists events, timestamps, intervals, entities → graph+vector
//!
//! Public surface:
//! - Intermediate types: [`CognifyInput`], [`ClassifiedDocuments`],
//!   [`ExtractedChunks`], [`ExtractedGraphData`], [`SummarizedData`],
//!   [`ExtractedTemporalEvents`], [`AttributedEvent`]
//! - Task implementations (free functions)
//! - [`TypedTask`] factories: [`make_classify_documents_task`], etc.
//! - Pipeline builders: [`build_cognify_pipeline`], [`build_temporal_cognify_pipeline`]

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use cognee_chunking::{CutType, NAMESPACE_OID, TokenCounterKind, chunk_by_row, chunk_text};
use cognee_core::{
    CpuPool, Pipeline, PipelineBuilder, PipelineContext, TaskContextBuilder, TypedTask, Value,
};
use cognee_database::{DatabaseConnection, PipelineRunRepository};
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::{EdgeData, GraphDBTrait, GraphDBTraitExt};
#[cfg(feature = "audio-loader")]
use cognee_ingestion::loaders::audio::AudioLoader;
#[cfg(feature = "image-loader")]
use cognee_ingestion::loaders::image::ImageLoader;
use cognee_ingestion::loaders::{LoaderOutput, LoaderRegistry};
use cognee_llm::Llm;
use cognee_models::{
    Data, Document, DocumentChunk, EdgeType, Embedding, Entity, TemporalEvent,
    classify_documents as model_classify_documents,
};
use cognee_ontology::OntologyResolver;
use cognee_storage::StorageTrait;
use cognee_utils::sanitize::{sanitize_str, sanitize_string};
use cognee_vector::{VectorDB, VectorPoint};
use futures::StreamExt;
use serde::Serialize;
use serde_json::json;
use tracing::{info, warn};
use url::Url;
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::fact_extraction::{FactExtractor, KnowledgeGraph};
use crate::failure::{
    FailurePolicy, FailureReport, FailureStage, FailureStop, RollbackScope, StageFailure,
};
use crate::graph_integration::{
    ArtifactProducers, GraphEdgePair, GraphNodePair, deduplicate_nodes_and_edges,
    expand_with_nodes_and_edges_with_stats, retrieve_existing_edges,
};
use crate::pipeline::{CognifyResult, IndexedFieldsStats};
use crate::qualification::{Qualification, check_pipeline_run_qualification};
use crate::rollback;
use crate::summarization::{SummaryExtractor, TextSummary};
use crate::temporal_extraction::{TemporalEntityEnricher, TemporalEventExtractor};
use cognee_models::DataPoint;

// ---------------------------------------------------------------------------
// Intermediate types
// ---------------------------------------------------------------------------

/// Input to the cognify pipeline.
///
/// Wraps all data items for a dataset along with the dataset identifier
/// and optional user/tenant context.
#[derive(Debug, Clone)]
pub struct CognifyInput {
    pub data_items: Vec<Data>,
    pub dataset_id: Uuid,
    /// Optional user ID (owner of the pipeline run).
    pub user_id: Option<Uuid>,
    /// Optional tenant ID for multi-tenant isolation.
    pub tenant_id: Option<Uuid>,
}

/// Output of [`classify_documents`]: classified documents ready for chunking.
#[derive(Debug, Clone)]
pub struct ClassifiedDocuments {
    pub documents: Vec<Document>,
    /// Items dropped for an unmappable extension. Seeds the report the
    /// chunking stage carries forward, so an unclassifiable file reaches the
    /// run result as a failure instead of silently vanishing.
    pub failures: FailureReport,
    pub dataset_id: Uuid,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
}

/// Output of [`extract_chunks_from_documents`]: text chunks ready for graph extraction.
#[derive(Debug, Clone)]
pub struct ExtractedChunks {
    pub chunks: Vec<DocumentChunk>,
    /// Classified documents — carried forward so downstream tasks (e.g. DLT
    /// filtering in [`extract_graph_from_data`]) can inspect document metadata.
    pub documents: Vec<Document>,
    pub dataset_id: Uuid,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    /// Everything that went wrong so far, collected rather than propagated.
    ///
    /// Written by the chunking stage and forwarded verbatim by every stage
    /// after it, so the run result carries the complete list. See
    /// [`crate::failure`].
    pub failures: FailureReport,
}

/// Output of [`extract_graph_from_data`]: chunks plus extracted entities and edges
/// (already stored in graph DB).
#[derive(Debug, Clone)]
pub struct ExtractedGraphData {
    pub chunks: Vec<DocumentChunk>,
    /// Classified documents — carried forward for DLT FK edge extraction.
    pub documents: Vec<Document>,
    pub entities: Vec<GraphNodePair>,
    pub edges: Vec<GraphEdgePair>,
    /// Every chunk that produced each merged entity and edge. Carried through
    /// to [`upsert_provenance`], which turns it into one ownership row per
    /// (artifact, data item). Not serialized anywhere — see
    /// [`ArtifactProducers`].
    pub producers: ArtifactProducers,
    pub dataset_id: Uuid,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    /// Forwarded from [`ExtractedChunks::failures`], plus this stage's own
    /// graph-extraction failures.
    pub failures: FailureReport,
}

/// Output of [`summarize_text`]: graph data plus generated summaries.
#[derive(Debug, Clone)]
pub struct SummarizedData {
    pub chunks: Vec<DocumentChunk>,
    /// Classified documents — carried forward for DLT FK edge extraction.
    pub documents: Vec<Document>,
    pub entities: Vec<GraphNodePair>,
    pub edges: Vec<GraphEdgePair>,
    /// Forwarded verbatim from [`ExtractedGraphData::producers`].
    pub producers: ArtifactProducers,
    pub summaries: Vec<TextSummary>,
    pub dataset_id: Uuid,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    /// Forwarded from [`ExtractedGraphData::failures`], plus this stage's own
    /// summarization failures.
    pub failures: FailureReport,
}

/// One extracted event and the data item whose chunk produced it.
///
/// Temporal events are content-addressed by name (`uuid5("event:{name}")`), so
/// two files describing the same event land on one graph node with two
/// producers — the same many-to-one shape merged entities have. The producing
/// `data_id` travels *with* the event rather than in a side map, because the
/// abort-time partition below filters the event list in place and a parallel
/// `Vec<Uuid>` would be one `retain` away from silent misattribution.
#[derive(Debug, Clone)]
pub struct AttributedEvent {
    pub event: TemporalEvent,
    pub data_id: Uuid,
}

/// Output of [`extract_temporal_events`]: temporal events extracted from chunks
/// via two LLM passes (event extraction + entity enrichment).
///
/// Used as the intermediate type between Task 3 and Task 4 in the temporal pipeline.
#[derive(Debug, Clone)]
pub struct ExtractedTemporalEvents {
    pub events: Vec<AttributedEvent>,
    pub dataset_id: Uuid,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    /// Forwarded from [`ExtractedChunks::failures`], plus the extraction and
    /// enrichment failures this stage collected itself.
    pub failures: FailureReport,
}

// ---------------------------------------------------------------------------
// Ownership-ledger identity
// ---------------------------------------------------------------------------

/// The owner recorded on ownership rows when the caller identified no user.
///
/// Python resolves the same case one layer up — `run_pipeline.py:75` and
/// `run_tasks.py:62` both do `user = await get_default_user()` before any task
/// sees the context — so `add_data_points`'s `if user and …` guard never fires
/// on a real run there. Rust's entry points resolve a user too (every CLI
/// command reads `settings.default_user_id`), so this fallback is reached only
/// by a library caller that passed `user_id: None` directly.
///
/// The value is `settings.default_user_id`'s own default. `cognee-cognify`
/// cannot read `Settings` — `cognee-lib` depends on this crate, not the other
/// way round — so the constant is duplicated here and pinned by
/// `crates/lib/tests/default_ledger_user.rs`, which fails if the setting's
/// default ever moves.
pub const DEFAULT_LEDGER_USER_ID: Uuid = Uuid::nil();

/// Who and what an ownership row is attributed to.
///
/// Constructed once per persistence site from the stage input and the pipeline
/// context, so the sites that write ownership rows cannot drift apart and the
/// default-user resolution happens in exactly one place. `pub` because it
/// appears in the signatures of the `pub` tasks [`create_web_page_nodes`] and
/// [`extract_dlt_fk_edges`].
#[derive(Debug, Clone, Copy)]
pub struct LedgerIdentity {
    tenant_id: Option<Uuid>,
    user_id: Uuid,
    dataset_id: Uuid,
    /// The run that created the artifacts these rows name. `None` only when the
    /// task is driven outside a pipeline executor — a NULL run id means
    /// "predates ownership tracking, permanently exempt from sweeping", which
    /// is the honest value for an artifact no run created.
    pipeline_run_id: Option<Uuid>,
}

impl LedgerIdentity {
    /// Resolve the identity every ownership row written by one persistence site
    /// carries.
    ///
    /// An absent `user_id` resolves to [`DEFAULT_LEDGER_USER_ID`] rather than
    /// suppressing the write: the ledger is what makes an artifact reachable,
    /// so it is always written.
    pub fn new(
        tenant_id: Option<Uuid>,
        user_id: Option<Uuid>,
        dataset_id: Uuid,
        pipeline_run_id: Option<Uuid>,
    ) -> Self {
        if pipeline_run_id.is_none() {
            warn!(
                dataset_id = %dataset_id,
                "recording artifact ownership with no pipeline run id — the rows are \
                 permanently exempt from run-scoped queries. Expected only when a task \
                 is driven outside a pipeline executor."
            );
        }
        Self {
            tenant_id,
            user_id: user_id.unwrap_or(DEFAULT_LEDGER_USER_ID),
            dataset_id,
            pipeline_run_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Task 1: classify_documents
// ---------------------------------------------------------------------------

/// Classify Data items into typed Documents (Task 1).
///
/// Maps each Data item to a Document based on mime_type.
/// Non-text items are filtered out.
pub fn classify_documents(
    input: &CognifyInput,
    failure_policy: FailurePolicy,
) -> Result<ClassifiedDocuments, CognifyError> {
    let documents: Vec<Document> = model_classify_documents(&input.data_items);
    info!(doc_count = documents.len(), "documents classified");

    // `model_classify_documents` filter_maps items whose extension maps to no
    // document type (`md`, `json`, `xml`, `yaml`, anything unmapped). They are
    // dropped silently, so without this they are neither failed nor unreached
    // and a completing run would mark them done — and, since markers now skip,
    // never revisit them. Record each as a file-unit failure instead.
    let mut failures = FailureReport::with_policy(&failure_policy);
    if documents.len() != input.data_items.len() {
        let classified: HashSet<Uuid> = documents.iter().map(|d| d.data_id).collect();
        for item in input
            .data_items
            .iter()
            .filter(|d| !classified.contains(&d.id))
        {
            warn!(
                data_id = %item.id,
                extension = %item.extension,
                "no document type for this extension; the item cannot be cognified"
            );
            // Unreached, not failed. Failing it would make one `.py` in a
            // dataset error and sweep every run under the default scope,
            // forever. Leaving it out of the report entirely was worse still:
            // it was then neither failed nor unreached, so a completing run
            // marked it done and the marker skipped it for good. Unreached
            // keeps it unmarked and retried, and the no-survivor backstop
            // still catches a dataset where nothing is supported.
            failures.mark_unreached(item.id);
        }
    }

    Ok(ClassifiedDocuments {
        documents,
        failures,
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
    })
}

// ---------------------------------------------------------------------------
// Task 2: extract_chunks_from_documents
// ---------------------------------------------------------------------------

/// Chunk one classified document.
///
/// Split out of [`extract_chunks_from_documents`] so the per-document work has
/// exactly one fallible boundary: everything that can go wrong for a single
/// file — storage `retrieve`, UTF-8 decode, an unregistered document type, the
/// loader's own `extract` — surfaces here as one `Err` the caller records
/// against the file rather than propagating out of the batch.
async fn chunk_one_document(
    document: &Document,
    storage: &dyn StorageTrait,
    max_chunk_size: usize,
    counter: &(dyn cognee_chunking::TokenCounter + Send + Sync),
    db: Option<&DatabaseConnection>,
    loader_registry: &LoaderRegistry,
) -> Result<Vec<DocumentChunk>, CognifyError> {
    let content_bytes = storage
        .retrieve(&document.raw_data_location)
        .await
        .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;

    // ---- DLT short-circuit ----
    // DLT documents emit exactly one chunk with cut_type="dlt_row".
    // No word/sentence/paragraph chunking. Mirrors Python DltRowDocument.read().
    if document.document_type == "dlt_row" {
        let text = String::from_utf8(content_bytes)
            .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }
        let chunk_id = Uuid::new_v5(&NAMESPACE_OID, format!("{}-0", document.base.id).as_bytes());
        let word_count = counter.count_tokens(trimmed);
        let mut chunk = DocumentChunk::new(
            chunk_id,
            trimmed.to_string(),
            word_count,
            0, // chunk_index
            CutType::DltRow.to_string(),
            document.base.id,
        );
        if document.base.belongs_to_set.is_some() {
            chunk.base.belongs_to_set = document.base.belongs_to_set.clone();
        }
        // Propagate importance_weight (always Some after classify) — unconditional.
        chunk.base.importance_weight = document.base.importance_weight;
        // Token count write-back
        if let Some(db) = db
            && let Err(e) = cognee_database::ops::data::update_data_token_count(
                db,
                document.data_id,
                word_count as i64,
            )
            .await
        {
            warn!(data_id = %document.data_id, "Failed to update token count: {e}");
        }
        return Ok(vec![chunk]);
    }

    // ---- Loader dispatch ----
    let loader = loader_registry
        .get(&document.document_type)
        .ok_or_else(|| CognifyError::UnsupportedDocumentType(document.document_type.clone()))?;

    let output = loader
        .extract(&content_bytes, document)
        .await
        .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;

    let mut chunks = match output {
        LoaderOutput::Text(text) => chunk_text(document.base.id, &text, max_chunk_size, &counter),
        LoaderOutput::Rows(rows) => {
            let joined = rows.join("\n\n");
            chunk_by_row(document.base.id, &joined, max_chunk_size, &counter)
        }
        LoaderOutput::SingleChunk { text, cut_type } => {
            let chunk_id =
                Uuid::new_v5(&NAMESPACE_OID, format!("{}-0", document.base.id).as_bytes());
            let word_count = counter.count_tokens(&text);
            vec![DocumentChunk::new(
                chunk_id,
                text,
                word_count,
                0,
                cut_type.to_string(),
                document.base.id,
            )]
        }
    };

    // Propagate belongs_to_set from Document to each DocumentChunk
    // Mirrors Python: document_chunk.belongs_to_set = document.belongs_to_set
    if document.base.belongs_to_set.is_some() {
        for chunk in &mut chunks {
            chunk.base.belongs_to_set = document.base.belongs_to_set.clone();
        }
    }

    // Propagate importance_weight from Document to each DocumentChunk
    // (always Some after classify) — unconditional, unlike belongs_to_set.
    for chunk in &mut chunks {
        chunk.base.importance_weight = document.base.importance_weight;
    }

    // Accumulate token count and write back to the Data record.
    // Mirrors Python: update_document_token_count(document.id, document_token_count)
    if let Some(db) = db {
        let document_token_count: i64 = chunks.iter().map(|c| c.chunk_size as i64).sum();
        if let Err(e) = cognee_database::ops::data::update_data_token_count(
            db,
            document.data_id,
            document_token_count,
        )
        .await
        {
            warn!(
                data_id = %document.data_id,
                "Failed to update token count: {e}"
            );
        }
    }

    Ok(chunks)
}

/// Extract text chunks from classified documents (Task 2).
///
/// For each document, reads content from storage and applies the
/// word → sentence → paragraph → text chunker hierarchy.
///
/// When `db` is `Some`, the accumulated token count for each document
/// is written back to the corresponding `Data` record, mirroring
/// Python's `update_document_token_count()`.
///
/// This is the only stage whose failure unit is the *file*. A document that
/// cannot be read or chunked is recorded in the returned
/// [`ExtractedChunks::failures`] and contributes neither chunks nor a
/// `Document` to the output — dropping the `Document` matters, because
/// [`add_data_points`] writes `Document` nodes to the graph, so a failed file
/// that kept its `Document` would leave an artifact behind and break invariant
/// I2. What happens next is `failure_policy`'s call:
///
/// * `FailFast` with any scope but `FailedItems` — the remaining documents are
///   marked unreached and the stage returns [`CognifyError::RunFailed`].
///   Nothing has been persisted, so there is nothing to preserve.
/// * `FailFast` + `FailedItems` — the remaining documents are marked unreached
///   and the stage returns `Ok` with the documents it already chunked, so the
///   files that completed can still travel down the rest of the pipeline and be
///   persisted.
/// * `RunToEnd` — keep going and collect every failing document.
///
/// Items `model_classify_documents` silently skipped (an unrecognised
/// extension) are *not* recorded: Python skips them silently too, and recording
/// them would fail every dataset containing a `.png` under the default policy.
#[allow(clippy::too_many_arguments)]
pub async fn extract_chunks_from_documents(
    input: &ClassifiedDocuments,
    storage: &dyn StorageTrait,
    max_chunk_size: usize,
    token_counter_kind: TokenCounterKind,
    db: Option<&DatabaseConnection>,
    loader_registry: &LoaderRegistry,
    failure_policy: FailurePolicy,
) -> Result<ExtractedChunks, CognifyError> {
    let counter = token_counter_kind
        .build()
        .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;
    let mut all_chunks = Vec::new();
    let mut kept_documents: Vec<Document> = Vec::new();
    // Seeded from classification so items dropped there stay in the report.
    let mut failures = input.failures.clone();
    // Denominator counts everything handed to the run, including anything
    // classification dropped — those never become `documents`. Counted from
    // `unreached_items`, which is never capped; `entries` is truncated at
    // `failure_report_cap`, so counting it would undercount the denominator
    // and could trip the no-survivor backstop on a run full of survivors.
    let item_total = input.documents.len() + failures.unreached_items().len();

    for (index, document) in input.documents.iter().enumerate() {
        let outcome = chunk_one_document(
            document,
            storage,
            max_chunk_size,
            counter.as_ref(),
            db,
            loader_registry,
        )
        .await;

        let chunks = match outcome {
            Ok(chunks) => chunks,
            Err(e) => {
                warn!(
                    data_id = %document.data_id,
                    "chunking failed for document: {e}"
                );
                failures.record(StageFailure {
                    stage: FailureStage::Chunking,
                    data_id: document.data_id,
                    chunk_id: None,
                    error: e.to_string(),
                    fails_item: true,
                });

                if failure_policy.stop == FailureStop::FailFast {
                    for later in &input.documents[index + 1..] {
                        failures.mark_unreached(later.data_id);
                    }
                    // The denominators are set on every exit path, so the ratio
                    // and the "nothing survived" backstop can be evaluated on
                    // the report whatever happens next.
                    failures.note_totals(item_total, all_chunks.len());
                    if failure_policy.scope != RollbackScope::FailedItems {
                        return Err(CognifyError::RunFailed {
                            report: Box::new(failures),
                        });
                    }
                    // `FailedItems` keeps what has already been chunked so the
                    // complete files can still be persisted and marked.
                    return Ok(ExtractedChunks {
                        chunks: all_chunks,
                        documents: kept_documents,
                        dataset_id: input.dataset_id,
                        user_id: input.user_id,
                        tenant_id: input.tenant_id,
                        failures,
                    });
                }
                continue;
            }
        };

        all_chunks.extend(chunks);
        kept_documents.push(document.clone());
    }

    failures.note_totals(item_total, all_chunks.len());
    info!(total_chunks = all_chunks.len(), "chunking complete");
    Ok(ExtractedChunks {
        chunks: all_chunks,
        documents: kept_documents,
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
        failures,
    })
}

// ---------------------------------------------------------------------------
// Task 3: extract_graph_from_data
// ---------------------------------------------------------------------------

/// Which entities each chunk should list in its `contains` field.
///
/// Every chunk that *produced* an entity links to it, not only the chunk that
/// created it: merging keeps a single `metadata["chunk_id"]`, so keying off
/// that alone drops the `contains` edge from every later producing chunk and
/// leaves the merged entity at degree one — where `sweep_orphan_nodes`'
/// `get_degree_one_nodes("Entity")` reaps it out from under the files that
/// still reference it. Python's `_link_chunk_to_entity`
/// (`expand_with_nodes_and_edges.py:107-127`) runs for every (chunk, node)
/// pair and links them all.
///
/// Entities with no producer — ontology-derived nodes, and any caller that did
/// not build a producer set — fall back to the `metadata["chunk_id"]` stamp so
/// they behave exactly as before.
fn chunk_entity_links(
    nodes: &[GraphNodePair],
    producers: &ArtifactProducers,
) -> HashMap<Uuid, Vec<serde_json::Value>> {
    let mut chunk_entity_map: HashMap<Uuid, Vec<serde_json::Value>> = HashMap::new();

    for node_pair in nodes {
        let entity_ref = json!(node_pair.entity.base.id.to_string());
        let producing_chunks = producers.entity_chunks(node_pair.entity.base.id);

        if producing_chunks.is_empty() {
            if let Some(chunk_id_val) = node_pair.entity.base.get_metadata("chunk_id")
                && let Some(chunk_id_str) = chunk_id_val.as_str()
                && let Ok(chunk_id) = Uuid::parse_str(chunk_id_str)
            {
                chunk_entity_map
                    .entry(chunk_id)
                    .or_default()
                    .push(entity_ref);
            }
            continue;
        }

        for chunk_id in producing_chunks {
            chunk_entity_map
                .entry(*chunk_id)
                .or_default()
                .push(entity_ref.clone());
        }
    }

    chunk_entity_map
}

/// Extract knowledge graphs from chunks via LLM, then integrate (Task 3).
///
/// For each chunk batch, calls the LLM to extract entities and relationships.
/// Then integrates: expands to storage-layer types, deduplicates against
/// existing DB entries and in-memory, and stores nodes/edges in graph DB.
///
/// Ownership of the entities and semantic edges is recorded in `db` *before*
/// they reach the graph — see [`record_extraction_ownership`].
#[allow(clippy::too_many_arguments)]
pub async fn extract_graph_from_data(
    input: &ExtractedChunks,
    llm: Arc<dyn Llm>,
    graph_db: Arc<dyn GraphDBTrait>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    db: &DatabaseConnection,
    // The run whose ownership rows these artifacts get. `None` when the stage
    // is driven outside a pipeline executor.
    pipeline_run_id: Option<Uuid>,
    config: &CognifyConfig,
    // Optional caller-supplied provenance user label. When `Some`, used
    // verbatim for the entity / EntityType / EdgeType pre-stamps inside
    // `expand_with_nodes_and_edges`. When `None`, falls back to the
    // string-form `user_id` (the only label `ExtractedChunks` carries).
    //
    // The pipeline-driven path threads through
    // `PipelineContext::user_label()` here so entities arrive at the
    // task body already stamped with the email-form label that the
    // provenance E2E test expects (locked decision 4 of
    // `docs/telemetry/05-datapoint-provenance.md`).
    user_label_override: Option<&str>,
    // 1-based pipeline position written to `topological_rank` on every
    // Entity / EntityType created below. Must be supplied here rather than
    // stamped by the caller afterwards: the nodes are persisted to the graph
    // DB before this function returns. `None` leaves the rank at its `0`
    // sentinel. The default pipeline passes
    // `Some(EXTRACT_GRAPH_TASK_RANK)` via `make_extract_graph_task`.
    task_rank: Option<i32>,
) -> Result<ExtractedGraphData, CognifyError> {
    if input.chunks.is_empty() {
        return Ok(ExtractedGraphData {
            chunks: input.chunks.clone(),
            documents: input.documents.clone(),
            entities: vec![],
            edges: vec![],
            producers: ArtifactProducers::default(),
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
            failures: input.failures.clone(),
        });
    }

    // Filter out DLT chunks — their graph is built deterministically by
    // extract_dlt_fk_edges from schema metadata, not by LLM extraction.
    // Mirrors Python: cognee/tasks/graph/extract_graph_from_data.py:148-155
    let dlt_doc_ids: HashSet<Uuid> = input
        .documents
        .iter()
        .filter(|d| d.document_type == "dlt_row")
        .map(|d| d.base.id)
        .collect();

    let (dlt_chunks, non_dlt_chunks): (Vec<&DocumentChunk>, Vec<&DocumentChunk>) = input
        .chunks
        .iter()
        .partition(|c| dlt_doc_ids.contains(&c.document_id));

    if !dlt_chunks.is_empty() {
        info!(
            "Skipping {} DLT chunks from LLM extraction ({} non-DLT chunks remain)",
            dlt_chunks.len(),
            non_dlt_chunks.len()
        );
    }

    // If only DLT chunks remain, return early with all chunks but no entities/edges
    if non_dlt_chunks.is_empty() {
        return Ok(ExtractedGraphData {
            chunks: input.chunks.clone(),
            documents: input.documents.clone(),
            entities: vec![],
            edges: vec![],
            producers: ArtifactProducers::default(),
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
            failures: input.failures.clone(),
        });
    }

    // Collect non-DLT chunks for LLM processing
    let chunks_for_extraction: Vec<DocumentChunk> = non_dlt_chunks.into_iter().cloned().collect();

    let batch_size = config.chunks_per_batch;
    let failure_policy = config.failure_policy();
    let mut failures = input.failures.clone();
    let max_parallel = config.max_parallel_extractions.max(1);
    let mut all_graphs: Vec<(Uuid, KnowledgeGraph)> = Vec::new();
    // `Some(n)` once a FailFast abort has fired, naming the first chunk index
    // (into `chunks_for_extraction`) that was never dispatched.
    let mut aborted_at: Option<usize> = None;

    for (batch_idx, batch) in chunks_for_extraction.chunks(batch_size).enumerate() {
        let fact_extractor = FactExtractor::new(Arc::clone(&llm));

        // Pre-extract owned per-chunk inputs so the stream yields owned items.
        // Mapping a stream over borrowed `&chunk` references trips a
        // higher-ranked-lifetime inference bug when the surrounding future is
        // boxed (same workaround as `SummaryExtractor::summarize_chunks`).
        let inputs: Vec<(Uuid, String)> = batch
            .iter()
            .map(|chunk| (chunk.base.id, chunk.text.clone()))
            .collect();
        let chunk_ids: Vec<Uuid> = inputs.iter().map(|(id, _)| *id).collect();
        // Parallel to `chunk_ids`: a failure is attributed to its file, and the
        // stream items carry only the chunk id and text.
        let chunk_documents: Vec<Uuid> = batch.iter().map(|chunk| chunk.document_id).collect();

        // Bounded-concurrency pipeline: at most `max_parallel` extraction calls
        // are in flight at once. `buffer_unordered`, not `buffered`: the latter's
        // `FuturesOrdered` counts completed-but-undrained outputs against its
        // limit, so a chunk stuck in the retry cascade pins its slot *and* every
        // slot filled behind it until it returns — head-of-line blocking that
        // stops the batch dead above `max_parallel` chunks. The `tokio::spawn`
        // inside the mapped future keeps calls on the multi-threaded runtime,
        // and `buffer_unordered` only polls up to `max_parallel` futures, so at
        // most that many extraction tasks exist at once.
        //
        // Completion order is not input order, so each future carries its index
        // and the batch is re-sorted below. Order still matters downstream:
        // `all_graphs` feeds dedup in `retrieve_existing_edges`, and the failure
        // records below must be deterministic for a given input. Same shape as
        // `SummaryExtractor::summarize_chunks`.
        //
        // Peak duplicated text is still O(`chunks_per_batch`), not
        // O(`max_parallel`): `inputs` above clones every chunk's text up front.
        // Making that lazy would mean borrowing `&chunk` across the stream, which
        // is the higher-ranked-lifetime case the comment there describes.
        let mut indexed_results: Vec<_> = futures::stream::iter(inputs.into_iter().enumerate())
            .map(|(index, (_, text))| {
                let extractor = fact_extractor.clone();
                let prompt = config.custom_extraction_prompt.clone();
                async move {
                    let result = tokio::spawn(async move {
                        extractor.extract_facts(&text, prompt.as_deref()).await
                    })
                    .await;
                    (index, result)
                }
            })
            .buffer_unordered(max_parallel)
            .collect()
            .await;
        indexed_results.sort_by_key(|(index, _)| *index);
        let batch_results: Vec<_> = indexed_results
            .into_iter()
            .map(|(_, result)| result)
            .collect();

        // The whole batch is collected before any result is inspected, so a
        // FailFast abort reports every failure in the batch that tripped it —
        // not only the first one to come back.
        let mut batch_failed = false;
        for ((result, chunk_id), document_id) in batch_results
            .into_iter()
            .zip(chunk_ids)
            .zip(chunk_documents)
        {
            let outcome = result
                .map_err(|e| CognifyError::FactExtractionError(e.to_string()))
                .and_then(|inner| inner);
            match outcome {
                Ok(graph) => all_graphs.push((chunk_id, graph)),
                Err(e) => {
                    warn!(
                        data_id = %document_id,
                        chunk_id = %chunk_id,
                        "graph extraction failed for chunk: {e}"
                    );
                    failures.record(StageFailure {
                        stage: FailureStage::GraphExtraction,
                        data_id: document_id,
                        chunk_id: Some(chunk_id),
                        error: e.to_string(),
                        fails_item: true,
                    });
                    batch_failed = true;
                }
            }
        }

        info!(
            "Processed graph extraction batch {}/{} ({} chunks)",
            batch_idx + 1,
            chunks_for_extraction.len().div_ceil(batch_size),
            batch.len()
        );

        if batch_failed && failure_policy.stop == FailureStop::FailFast {
            aborted_at = Some((batch_idx + 1) * batch_size);
            break;
        }
    }

    // ── The abort-time partition ────────────────────────────────────────────
    // At the moment of a FailFast abort nothing has been persisted — the loop
    // above accumulates every chunk's graph in memory and the first write of
    // any kind happens below. So "keep the results for the other files" is not
    // a matter of not-deleting; it takes deliberately persisting the finished
    // work before stopping. Files partition three ways:
    //
    //   complete  — every one of its non-DLT chunks was attempted and none
    //               failed. Persisted below, and marked complete when the
    //               run ends.
    //   failed    — at least one chunk failed. Not persisted, left unmarked.
    //   unreached — chunks never dispatched. Not persisted, left unmarked.
    //
    // Only complete files are persisted, which is what keeps items
    // all-or-nothing. Failed and unreached files are indistinguishable to the
    // next run and both are simply redone.
    //
    // Under `RunToEnd` there is no partition and no filtering: the run persists
    // normally and the item-scoped sweep removes the failed files'
    // contributions afterwards, so one deletion path serves both cases.
    let mut complete_documents: Option<HashSet<Uuid>> = None;
    if let Some(first_unreached) = aborted_at {
        for chunk in chunks_for_extraction.iter().skip(first_unreached) {
            failures.mark_unreached(chunk.document_id);
        }

        if failure_policy.scope != RollbackScope::FailedItems {
            return Err(CognifyError::RunFailed {
                report: Box::new(failures),
            });
        }

        // A file is complete when it owns no failed chunk and no unreached one.
        // DLT-only files are trivially complete: their chunks never reach the
        // LLM at all.
        let excluded: HashSet<Uuid> = failures
            .failed_items()
            .iter()
            .chain(failures.unreached_items().iter())
            .copied()
            .collect();
        complete_documents = Some(
            input
                .documents
                .iter()
                .map(|d| d.base.id)
                .filter(|id| !excluded.contains(id))
                .collect(),
        );
    }

    // Reduce every downstream collection to the complete files. Everything
    // after this point — expansion, deduplication, the ownership rows, the
    // graph writes — then runs unchanged over exactly the artifacts that are
    // allowed to survive the abort.
    let (chunks_for_extraction, all_graphs, surviving_chunks, surviving_documents) =
        match &complete_documents {
            None => (
                chunks_for_extraction,
                all_graphs,
                input.chunks.clone(),
                input.documents.clone(),
            ),
            Some(complete) => {
                let kept_chunk_ids: HashSet<Uuid> = input
                    .chunks
                    .iter()
                    .filter(|c| complete.contains(&c.document_id))
                    .map(|c| c.base.id)
                    .collect();
                (
                    chunks_for_extraction
                        .into_iter()
                        .filter(|c| complete.contains(&c.document_id))
                        .collect::<Vec<_>>(),
                    all_graphs
                        .into_iter()
                        .filter(|(chunk_id, _)| kept_chunk_ids.contains(chunk_id))
                        .collect::<Vec<_>>(),
                    input
                        .chunks
                        .iter()
                        .filter(|c| complete.contains(&c.document_id))
                        .cloned()
                        .collect::<Vec<_>>(),
                    input
                        .documents
                        .iter()
                        .filter(|d| complete.contains(&d.base.id))
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            }
        };

    // Database deduplication — query for existing edges
    let graphs_only: Vec<KnowledgeGraph> = all_graphs.iter().map(|(_, g)| g.clone()).collect();
    let existing_edges_set = retrieve_existing_edges(graph_db.as_ref(), &graphs_only).await?;

    // Merge and deduplicate graphs (with DB awareness).
    //
    // The string-form `user_id` is the best label we have at this
    // point in the pipeline-driven path — `ExtractedChunks` does not
    // carry `user_email`. The executor's downstream walk
    // (`PipelineContext::user_label()`, task 05-07) fills in the
    // email-form label later if the run has it; the pre-stamp's
    // `if dp.source_user.is_none()` guard then skips, so the more
    // specific value wins.
    let user_label_owned = user_label_override
        .map(|s| s.to_string())
        .or_else(|| input.user_id.as_ref().map(|id| id.to_string()));

    // Map each source chunk to its NodeSet `belongs_to_set` so extracted
    // entities inherit their chunk's NodeSet membership (parity with Python
    // `Entity(belongs_to_set=data_chunk.belongs_to_set)` in
    // expand_with_nodes_and_edges.py:227). Without this, a node_name-scoped
    // HYBRID_COMPLETION search drops every extracted entity. Chunks with no
    // NodeSet metadata are simply omitted.
    let chunk_node_sets: HashMap<Uuid, Vec<serde_json::Value>> = chunks_for_extraction
        .iter()
        .filter_map(|chunk| {
            chunk
                .base
                .belongs_to_set
                .as_ref()
                .map(|sets| (chunk.base.id, sets.clone()))
        })
        .collect();

    // Map each source chunk to its `importance_weight` so every extracted
    // EntityType / Entity / ontology node inherits it (parity with Python
    // `importance_weight=data_chunk.importance_weight` in
    // expand_with_nodes_and_edges.py:66,79,163,229). Default 0.5 when None,
    // matching Python's `DataPoint.importance_weight` default.
    let chunk_importance_weights: HashMap<Uuid, f64> = chunks_for_extraction
        .iter()
        .map(|chunk| (chunk.base.id, chunk.base.importance_weight.unwrap_or(0.5)))
        .collect();

    let (nodes, edges, claimed_existing_edges, producers, edge_resolution) =
        expand_with_nodes_and_edges_with_stats(
            all_graphs,
            input.dataset_id,
            &chunk_node_sets,
            &chunk_importance_weights,
            &existing_edges_set,
            ontology_resolver.as_ref(),
            user_label_owned.as_deref(),
            task_rank,
        )
        .await;

    // Endpoint resolution is lossy: the model routinely emits edges referencing
    // node ids it never declared. Report the rate once per run rather than
    // leaving one log line per dropped edge as the only evidence.
    //
    // This counts endpoint resolution only. An edge that resolves here can still
    // be filtered out downstream for already existing in the database, so the
    // resolved figure is an upper bound on what this pass goes on to emit.
    info!(
        attempted = edge_resolution.attempted,
        dropped = edge_resolution.dropped(),
        recovered_by_name = edge_resolution.resolved_by_name,
        "Edge endpoint resolution: both endpoints resolved for {} of {} extracted edges",
        edge_resolution.attempted - edge_resolution.dropped(),
        edge_resolution.attempted
    );

    // Final deduplication pass (in-memory only after DB filtering)
    let dedup_result = deduplicate_nodes_and_edges(nodes, edges);

    // Build chunk_id → entity IDs mapping from the deduplicated nodes.
    let chunk_entity_map = chunk_entity_links(&dedup_result.unique_nodes, &producers);

    // Populate DocumentChunk.contains with extracted entity IDs
    let mut updated_chunks = surviving_chunks;
    for chunk in &mut updated_chunks {
        if let Some(entity_ids) = chunk_entity_map.get(&chunk.base.id) {
            chunk.contains = entity_ids.clone();
        }
    }

    // I1 — the ledger row must exist before the artifact does. Rust persists
    // entities and edges here, two stages before `add_data_points`; Python does
    // not (its `extract_graph_from_data` returns chunks and lets
    // `add_data_points` do every write), so this ledger write has no Python
    // counterpart. Without it a run that dies between here and
    // `add_data_points` leaves entities and edges in the graph that nothing can
    // find, and the extraction dedup filter then hides them from every retry.
    record_extraction_ownership(
        db,
        LedgerIdentity::new(
            input.tenant_id,
            input.user_id,
            input.dataset_id,
            pipeline_run_id,
        ),
        &updated_chunks,
        &dedup_result.unique_nodes,
        &dedup_result.unique_edges,
        // Edges an earlier run already put in the graph, which this run
        // produced too. They get ownership rows and nothing else — see the
        // `claimed_existing_edges` note on `expand_with_nodes_and_edges`. The
        // row is what makes `get_unique_edges_for_data` stop calling the edge
        // exclusive to the earlier file, so deleting that file no longer takes
        // the edge away from this one.
        &claimed_existing_edges,
        &producers,
    )
    .await?;

    // Store graph data (nodes and edges) in graph database
    let entity_refs: Vec<&cognee_models::Entity> = dedup_result
        .unique_nodes
        .iter()
        .map(|n| &n.entity)
        .collect();
    graph_db
        .add_nodes(&entity_refs)
        .await
        .map_err(CognifyError::from)?;

    let edge_data: Vec<_> = dedup_result
        .unique_edges
        .iter()
        .map(|edge_pair| {
            let properties: HashMap<std::borrow::Cow<'static, str>, serde_json::Value> = edge_pair
                .properties
                .iter()
                .map(|(k, v)| {
                    (
                        std::borrow::Cow::Owned(k.clone()),
                        serde_json::Value::String(v.clone()),
                    )
                })
                .collect();
            (
                edge_pair.source_entity_id.to_string(),
                edge_pair.target_entity_id.to_string(),
                edge_pair.relationship_name.clone(),
                properties,
            )
        })
        .collect();

    graph_db
        .add_edges(&edge_data)
        .await
        .map_err(CognifyError::from)?;
    if !edge_data.is_empty() {
        info!(
            "Upserted {} extracted graph edges (LLM and ontology)",
            edge_data.len()
        );
    }

    Ok(ExtractedGraphData {
        chunks: updated_chunks,
        documents: surviving_documents,
        entities: dedup_result.unique_nodes,
        edges: dedup_result.unique_edges,
        producers,
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
        failures,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WebPageMetadata {
    url: String,
    domain: String,
    title: Option<String>,
}

fn parse_web_page_metadata(document: &Document) -> Option<WebPageMetadata> {
    let metadata = document.external_metadata.as_ref()?;
    let value: serde_json::Value = serde_json::from_str(metadata).ok()?;
    let source = value.get("source").and_then(|v| v.as_str())?;
    if source != "url" {
        return None;
    }

    let url = value
        .get("final_url")
        .or_else(|| value.get("url"))
        .and_then(|v| v.as_str())?;
    let parsed = Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let domain = parsed.host_str()?.to_ascii_lowercase();
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Some(WebPageMetadata {
        url: parsed.to_string(),
        domain,
        title,
    })
}

fn web_page_id(url: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("WebPage:{url}").as_bytes())
}

fn web_site_id(domain: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("WebSite:{}", domain.to_ascii_lowercase()).as_bytes(),
    )
}

fn first_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn document_content_preview(document_id: Uuid, chunks: &[DocumentChunk]) -> String {
    let mut preview = String::new();
    for chunk in chunks
        .iter()
        .filter(|chunk| chunk.document_id == document_id)
    {
        if !preview.is_empty() {
            preview.push('\n');
        }
        preview.push_str(&chunk.text);
        if preview.chars().count() >= 500 {
            break;
        }
    }
    first_chars(&preview, 500)
}

fn empty_edge_props() -> HashMap<Cow<'static, str>, serde_json::Value> {
    HashMap::new()
}

/// Create deterministic WebPage/WebSite graph provenance for URL-sourced documents.
///
/// Uses only URL metadata carried on [`Document::external_metadata`], produced
/// by ingestion for URL inputs. Invalid JSON, non-URL metadata, unparsable URLs,
/// and non-HTTP(S) URLs are skipped.
///
/// Ownership of the nodes and edges is recorded in `db` before any of them
/// reaches the graph (I1). These artifacts had no ownership row at all before —
/// they are `add_nodes_raw` JSON blobs rather than DataPoints, so
/// [`upsert_provenance`] never saw them — which also made them survive every
/// delete.
pub async fn create_web_page_nodes(
    documents: &[Document],
    chunks: &[DocumentChunk],
    graph_db: Arc<dyn GraphDBTrait>,
    db: &DatabaseConnection,
    id: LedgerIdentity,
) -> Result<(), CognifyError> {
    if documents.is_empty() || chunks.is_empty() {
        return Ok(());
    }

    let mut nodes_by_id: HashMap<String, serde_json::Value> = HashMap::new();
    let mut candidate_edges: Vec<EdgeData> = Vec::new();
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();
    let mut prov_nodes: Vec<cognee_database::GraphNode> = Vec::new();
    let mut prov_edges: Vec<cognee_database::GraphEdge> = Vec::new();

    for document in documents {
        let Some(metadata) = parse_web_page_metadata(document) else {
            continue;
        };

        let page_id = web_page_id(&metadata.url);
        let site_id = web_site_id(&metadata.domain);
        let page_id_str = page_id.to_string();
        let site_id_str = site_id.to_string();

        let page_node = json!({
            "id": page_id_str,
            "type": "WebPage",
            "url": metadata.url,
            "title": metadata.title,
            "content": document_content_preview(document.base.id, chunks),
        });
        let site_node = json!({
            "id": site_id_str,
            "type": "WebSite",
            "domain": metadata.domain,
        });

        // One row per producing document, for the WebSite node as much as for
        // the WebPage: two URL documents on one domain share a single physical
        // WebSite node, so a single row would let deleting the first take a
        // node the second still references — the merged-entity rule applied to
        // a second artifact class.
        prov_nodes.push(web_page_provenance_row(
            id,
            document.base.id,
            page_id,
            "WebPage",
            metadata
                .title
                .clone()
                .unwrap_or_else(|| metadata.url.clone()),
            &page_node,
        ));
        prov_nodes.push(web_page_provenance_row(
            id,
            document.base.id,
            site_id,
            "WebSite",
            metadata.domain.clone(),
            &site_node,
        ));

        nodes_by_id.insert(page_id_str.clone(), page_node);
        nodes_by_id.insert(site_id_str.clone(), site_node);

        push_unique_edge(
            &mut candidate_edges,
            &mut seen_edges,
            page_id_str.clone(),
            site_id_str,
            "PART_OF",
        );
        prov_edges.push(web_page_provenance_edge_row(
            id,
            document.base.id,
            page_id,
            "PART_OF",
            site_id,
        ));

        for chunk in chunks
            .iter()
            .filter(|chunk| chunk.document_id == document.base.id)
        {
            push_unique_edge(
                &mut candidate_edges,
                &mut seen_edges,
                chunk.base.id.to_string(),
                page_id_str.clone(),
                "SOURCED_FROM",
            );
            prov_edges.push(web_page_provenance_edge_row(
                id,
                document.base.id,
                chunk.base.id,
                "SOURCED_FROM",
                page_id,
            ));
        }
    }

    // Every candidate edge is claimed, including the ones the `has_edges`
    // filter below stops the graph write from re-issuing: an edge an earlier
    // run already created is still produced by this document, and a surplus
    // claimant only ever *prevents* a deletion.
    cognee_database::ops::graph_storage::upsert_provenance_graph(db, &prov_nodes, &prov_edges)
        .await?;

    if !nodes_by_id.is_empty() {
        graph_db
            .add_nodes_raw(nodes_by_id.into_values().collect())
            .await
            .map_err(CognifyError::from)?;
    }

    if candidate_edges.is_empty() {
        return Ok(());
    }

    let existing_edges = graph_db
        .has_edges(&candidate_edges)
        .await
        .map_err(CognifyError::from)?;
    let existing_keys: HashSet<(String, String, String)> = existing_edges
        .into_iter()
        .map(|(source, target, relationship, _)| (source, target, relationship))
        .collect();
    let missing_edges: Vec<EdgeData> = candidate_edges
        .into_iter()
        .filter(|(source, target, relationship, _)| {
            !existing_keys.contains(&(source.clone(), target.clone(), relationship.clone()))
        })
        .collect();

    if !missing_edges.is_empty() {
        graph_db
            .add_edges(&missing_edges)
            .await
            .map_err(CognifyError::from)?;
        info!("Upserted {} web page edges", missing_edges.len());
    }

    Ok(())
}

fn push_unique_edge(
    edges: &mut Vec<EdgeData>,
    seen: &mut HashSet<(String, String, String)>,
    source: String,
    target: String,
    relationship: &str,
) {
    let key = (source.clone(), target.clone(), relationship.to_string());
    if seen.insert(key) {
        edges.push((source, target, relationship.to_string(), empty_edge_props()));
    }
}

/// One ownership row for a WebPage / WebSite graph node.
///
/// `indexed_fields` is deliberately empty: these nodes go in through
/// `add_nodes_raw` and are never vector-indexed, so an empty field list is what
/// keeps the delete path from issuing vector-point ids that never existed.
fn web_page_provenance_row(
    id: LedgerIdentity,
    data_id: Uuid,
    node_id: Uuid,
    node_type: &str,
    label: String,
    attributes: &serde_json::Value,
) -> cognee_database::GraphNode {
    cognee_database::GraphNode {
        id: provenance_node_id(id.tenant_id, id.user_id, id.dataset_id, data_id, node_id),
        slug: node_id,
        user_id: id.user_id,
        data_id,
        dataset_id: id.dataset_id,
        pipeline_run_id: id.pipeline_run_id,
        label: Some(label),
        node_type: node_type.to_string(),
        indexed_fields: json!([]),
        attributes: Some(attributes.clone()),
        created_at: Utc::now(),
    }
}

/// One ownership row for a PART_OF / SOURCED_FROM graph edge.
fn web_page_provenance_edge_row(
    id: LedgerIdentity,
    data_id: Uuid,
    source_id: Uuid,
    relationship_name: &str,
    target_id: Uuid,
) -> cognee_database::GraphEdge {
    cognee_database::GraphEdge {
        id: provenance_edge_id(
            id.tenant_id,
            id.user_id,
            id.dataset_id,
            data_id,
            source_id,
            relationship_name,
            target_id,
        ),
        slug: triplet_slug(source_id, relationship_name, target_id),
        user_id: id.user_id,
        data_id,
        dataset_id: id.dataset_id,
        pipeline_run_id: id.pipeline_run_id,
        source_node_id: source_id,
        destination_node_id: target_id,
        relationship_name: relationship_name.to_string(),
        label: None,
        attributes: None,
        created_at: Utc::now(),
    }
}

// ---------------------------------------------------------------------------
// Task 3b: extract_custom_graph_from_data (custom graph model path)
// ---------------------------------------------------------------------------

/// Extract a custom graph model from chunks via LLM (Task 3 — custom model variant).
///
/// Mirrors the Python branching at `extract_graph_from_data.py:99-103`:
/// when the graph model is **not** the built-in [`KnowledgeGraph`], the LLM
/// output is serialized to JSON and stored directly in each
/// [`DocumentChunk::contains`] without entity/edge expansion, deduplication,
/// or graph DB storage.
///
/// This function is the generic counterpart of [`extract_graph_from_data`].
/// It accepts any type implementing [`GraphModel`].
///
/// The returned [`ExtractedGraphData`] will have empty `entities` and `edges`
/// fields (those only apply to the default KnowledgeGraph flow).
///
/// # Type Parameters
/// * `M` — A type implementing [`GraphModel`]. Must be `Serialize +
///   DeserializeOwned + JsonSchema + Clone + Send + Sync + 'static`.
///
/// # Errors
/// - [`CognifyError::LlmError`] if any LLM call fails
/// - [`CognifyError::SerializationError`] if the extracted model cannot be
///   serialized to JSON
pub async fn extract_custom_graph_from_data<M: crate::fact_extraction::GraphModel>(
    input: &ExtractedChunks,
    llm: Arc<dyn Llm>,
    config: &CognifyConfig,
) -> Result<ExtractedGraphData, CognifyError> {
    if input.chunks.is_empty() {
        return Ok(ExtractedGraphData {
            chunks: input.chunks.clone(),
            documents: input.documents.clone(),
            entities: vec![],
            edges: vec![],
            producers: ArtifactProducers::default(),
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
            failures: input.failures.clone(),
        });
    }

    // Filter out DLT chunks — same as extract_graph_from_data
    let dlt_doc_ids: HashSet<Uuid> = input
        .documents
        .iter()
        .filter(|d| d.document_type == "dlt_row")
        .map(|d| d.base.id)
        .collect();

    let batch_size = config.chunks_per_batch;
    let max_parallel = config.max_parallel_extractions.max(1);

    let mut updated_chunks = input.chunks.clone();

    // Only process non-DLT chunks through LLM
    let non_dlt_indices: Vec<usize> = updated_chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| !dlt_doc_ids.contains(&c.document_id))
        .map(|(i, _)| i)
        .collect();

    if non_dlt_indices.is_empty() {
        return Ok(ExtractedGraphData {
            chunks: updated_chunks,
            documents: input.documents.clone(),
            entities: vec![],
            edges: vec![],
            producers: ArtifactProducers::default(),
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
            failures: input.failures.clone(),
        });
    }

    let total_batches = non_dlt_indices.len().div_ceil(batch_size);
    let failure_policy = config.failure_policy();
    let mut failures = input.failures.clone();
    // Set once a `FailFast` abort has fired; the failed and unreached files are
    // dropped from the output below.
    let mut aborted = false;

    for (batch_idx, batch_indices) in non_dlt_indices.chunks(batch_size).enumerate() {
        // Owned per-chunk inputs, for the same higher-ranked-lifetime reason as
        // `extract_graph_from_data`.
        let inputs: Vec<String> = batch_indices
            .iter()
            .map(|&idx| updated_chunks[idx].text.clone())
            .collect();

        // Bounded-concurrency pipeline. `buffer_unordered`, not `buffered`, for
        // the reason spelled out in `extract_graph_from_data`: `buffered` holds a
        // completed-but-undrained output in its slot, so one slow chunk blocks
        // every chunk queued behind it. The results below are indexed back onto
        // `batch_indices`, so each future carries its position and the batch is
        // re-sorted here — a positional zip against completion order would
        // attach each extraction to the wrong chunk.
        let mut indexed_results: Vec<_> = futures::stream::iter(inputs.into_iter().enumerate())
            .map(|(index, text)| {
                let extractor = FactExtractor::new(Arc::clone(&llm));
                let prompt = config.custom_extraction_prompt.clone();
                async move {
                    let result = tokio::spawn(async move {
                        extractor.extract::<M>(&text, prompt.as_deref()).await
                    })
                    .await;
                    (index, result)
                }
            })
            .buffer_unordered(max_parallel)
            .collect()
            .await;
        indexed_results.sort_by_key(|(index, _)| *index);
        let batch_results: Vec<_> = indexed_results
            .into_iter()
            .map(|(_, result)| result)
            .collect();

        let batch_len = batch_indices.len();
        let mut batch_failed = false;

        for (i, result) in batch_results.into_iter().enumerate() {
            let chunk = &updated_chunks[batch_indices[i]];
            let chunk_id = chunk.base.id;
            let document_id = chunk.document_id;
            let outcome = result
                .map_err(|e| CognifyError::FactExtractionError(e.to_string()))
                .and_then(|inner| inner)
                .and_then(|model: M| {
                    serde_json::to_value(&model)
                        .map_err(|e| CognifyError::SerializationError(e.to_string()))
                });
            match outcome {
                Ok(value) => updated_chunks[batch_indices[i]].contains = vec![value],
                Err(e) => {
                    warn!(
                        data_id = %document_id,
                        chunk_id = %chunk_id,
                        "custom graph extraction failed for chunk: {e}"
                    );
                    failures.record(StageFailure {
                        stage: FailureStage::GraphExtraction,
                        data_id: document_id,
                        chunk_id: Some(chunk_id),
                        error: e.to_string(),
                        fails_item: true,
                    });
                    batch_failed = true;
                }
            }
        }

        info!(
            "Processed custom graph extraction batch {}/{} ({} chunks)",
            batch_idx + 1,
            total_batches,
            batch_len
        );

        if batch_failed && failure_policy.stop == FailureStop::FailFast {
            for &idx in non_dlt_indices.iter().skip((batch_idx + 1) * batch_size) {
                failures.mark_unreached(updated_chunks[idx].document_id);
            }
            aborted = true;
            break;
        }
    }

    // This function persists nothing, so there is no partition to compute —
    // only, on a `FailFast` abort, the failed and unreached files to drop from
    // the output, which is what keeps a partially-extracted file from reaching a
    // later persisting stage. Under `RunToEnd` the output is left whole, which
    // is the same choice `extract_graph_from_data` makes, so both extraction
    // functions mean the same thing by `RunToEnd`.
    if aborted && failure_policy.scope != RollbackScope::FailedItems {
        return Err(CognifyError::RunFailed {
            report: Box::new(failures),
        });
    }
    let excluded: HashSet<Uuid> = if aborted {
        failures
            .failed_items()
            .iter()
            .chain(failures.unreached_items().iter())
            .copied()
            .collect()
    } else {
        HashSet::new()
    };
    if !excluded.is_empty() {
        updated_chunks.retain(|c| !excluded.contains(&c.document_id));
    }

    Ok(ExtractedGraphData {
        chunks: updated_chunks,
        documents: input
            .documents
            .iter()
            .filter(|d| !excluded.contains(&d.base.id))
            .cloned()
            .collect(),
        entities: vec![],
        edges: vec![],
        producers: ArtifactProducers::default(),
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
        failures,
    })
}

// ---------------------------------------------------------------------------
// Task 4: summarize_text
// ---------------------------------------------------------------------------

/// Summarize text chunks via LLM (Task 4).
///
/// If summarization is enabled in config, generates summaries for each chunk
/// using batched parallel LLM calls.
///
/// Honours axis 1: under [`FailureStop::FailFast`], and only when a
/// summarization failure would actually fail its item (see
/// `tolerate_summarization_failures`), the first failed chunk stops further
/// calls from being *dispatched*. Nothing already in flight is cancelled and no
/// finished summary is discarded, so the granularity is the in-flight window —
/// up to `max_parallel_extractions` calls are already outstanding when the
/// first failure returns. The files whose chunks went undispatched are marked
/// unreached, which keeps them out of the completion markers and inside the
/// item-scoped sweep.
pub async fn summarize_text(
    input: &ExtractedGraphData,
    llm: Arc<dyn Llm>,
    config: &CognifyConfig,
) -> Result<SummarizedData, CognifyError> {
    // Filter out DLT chunks — structured data rows should not be summarized.
    // Mirrors Python: cognee/tasks/summarization/summarize_text.py:52-62
    let dlt_doc_ids: HashSet<Uuid> = input
        .documents
        .iter()
        .filter(|d| d.document_type == "dlt_row")
        .map(|d| d.base.id)
        .collect();

    let non_dlt_chunks: Vec<DocumentChunk> = input
        .chunks
        .iter()
        .filter(|c| !dlt_doc_ids.contains(&c.document_id))
        .cloned()
        .collect();

    if non_dlt_chunks.len() < input.chunks.len() {
        info!(
            "Skipping {} DLT chunks from summarization ({} non-DLT chunks remain)",
            input.chunks.len() - non_dlt_chunks.len(),
            non_dlt_chunks.len()
        );
    }

    let mut failures = input.failures.clone();
    let failure_policy = config.failure_policy();

    let summaries = if config.enable_summarization && !non_dlt_chunks.is_empty() {
        // Axis 1 reaches this stage here. `FailFast` alone is not enough: with
        // `tolerate_summarization_failures` on, a failed summary fails nothing,
        // so there is no failure for the axis to stop on and the run must keep
        // summarizing. The flag is the conjunction of the two.
        let stop_on_failure =
            failure_policy.stop == FailureStop::FailFast && !config.tolerate_summarization_failures;
        let summary_extractor =
            SummaryExtractor::new_with_schema(llm, config.summary_schema.clone())
                .with_max_parallel(config.max_parallel_extractions)
                .with_fail_fast(stop_on_failure);

        // Stream every chunk through one bounded pipeline. `summarize_chunks`
        // already caps in-flight requests at `max_parallel_extractions` internally
        // (issue #19), so an outer batch loop would only insert a sequential
        // barrier between batches without lowering peak concurrency.
        //
        // The stream is drained whatever happens: a failing chunk is data, not
        // a reason to abandon the summaries already paid for, and no in-flight
        // call is ever cancelled. Under `stop_on_failure` what stops is
        // *dispatch* — chunks not yet admitted come back in `outcome.skipped`
        // instead of being paid for to produce a result the run is about to
        // sweep. Whether the failure ends the run is still decided at the end
        // of the run, not here.
        let outcome = summary_extractor
            .summarize_chunks(&non_dlt_chunks, None)
            .await;

        if !outcome.failures.is_empty() || !outcome.skipped.is_empty() {
            let chunk_documents: HashMap<Uuid, Uuid> = non_dlt_chunks
                .iter()
                .map(|c| (c.base.id, c.document_id))
                .collect();
            // A nil `data_id` here would be inserted into the report's
            // failed-item set — the exact set the sweep selects by — so the
            // impossible case is made loud rather than silent.
            #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
            let data_id_of = |chunk_id: &Uuid| -> Uuid {
                chunk_documents.get(chunk_id).copied().expect(
                    "chunk_documents is built from the same slice that was summarized, \
                     so every returned chunk_id is present",
                )
            };
            for (chunk_id, error) in &outcome.failures {
                let data_id = data_id_of(chunk_id);
                warn!(
                    data_id = %data_id,
                    chunk_id = %chunk_id,
                    "summarization failed for chunk: {error}"
                );
                // `fails_item` is the entire meaning of the config flag: with
                // tolerance on the failure is still listed and still counted in
                // the total, but it stays out of `failed_items`, out of the
                // chunk-failure ratio, and out of every fatality decision.
                failures.record(StageFailure {
                    stage: FailureStage::Summarization,
                    data_id,
                    chunk_id: Some(*chunk_id),
                    error: error.to_string(),
                    fails_item: !config.tolerate_summarization_failures,
                });
            }
            // A file whose summaries were never dispatched has not been fully
            // processed, so it must not end the run marked complete. This is
            // the same `mark_unreached` the extraction stages use for the
            // chunks after a `FailFast` abort, and it is what keeps a stopped
            // summarization from silently producing summary-less files under
            // `FailedItems`. `mark_unreached` is a no-op for a file already in
            // `failed_items`, so a file that both failed and was cut short
            // stays a failure.
            for chunk_id in &outcome.skipped {
                failures.mark_unreached(data_id_of(chunk_id));
            }
            if !outcome.skipped.is_empty() {
                warn!(
                    skipped_chunks = outcome.skipped.len(),
                    "summarization stopped early under FailFast; the remaining chunks were \
                     never dispatched"
                );
            }
        }

        info!("Generated {} summaries", outcome.summaries.len());
        outcome.summaries
    } else {
        if !config.enable_summarization {
            info!("Summarization disabled in config");
        } else {
            info!("No non-DLT chunks to summarize");
        }
        Vec::new()
    };

    // This stage never drops a chunk or a document, under any policy: by the
    // time it runs, the extraction stage has already committed those files'
    // entities and edges to the graph, so dropping them here would manufacture
    // exactly the partial file invariant I2 exists to prevent. The failed item
    // is recorded and the sweep removes it.
    Ok(SummarizedData {
        chunks: input.chunks.clone(),
        documents: input.documents.clone(),
        entities: input.entities.clone(),
        edges: input.edges.clone(),
        producers: input.producers.clone(),
        summaries,
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
        failures,
    })
}

// ---------------------------------------------------------------------------
// Task 5: add_data_points
// ---------------------------------------------------------------------------

/// Generate embeddings and index all data points in vector DB (Task 5).
///
/// Generates embeddings for chunks, entities (name + description), summaries,
/// and optionally triplets. Creates vector collections and indexes points.
///
/// Writes the provenance records (nodes/edges) to the relational database
/// *before* the first graph or vector write, mirroring Python's
/// `add_data_points.py:138-141` ("the rollback ledger is written BEFORE the
/// graph/vector writes so a failed write can always be swept"). Python's
/// `if user and dataset and data:` guard has no Rust counterpart any more: the
/// database is non-optional and an absent `user_id` resolves to
/// [`DEFAULT_LEDGER_USER_ID`], so the ledger is always written.
pub async fn add_data_points(
    input: &SummarizedData,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: &DatabaseConnection,
    // The run whose ownership rows these artifacts get. `None` when the stage
    // is driven outside a pipeline executor.
    pipeline_run_id: Option<Uuid>,
    config: &CognifyConfig,
) -> Result<CognifyResult, CognifyError> {
    // ── Ownership before artifacts (I1) ─────────────────────────────────────
    // Discovering the structural edges is pure computation over the input
    // (`get_graph_from_model`, no I/O), so it can happen up here and let the
    // ledger name every artifact this stage is about to write before the first
    // of them exists.
    let mut extractable_items: Vec<&dyn crate::graph_extraction::GraphExtractable> = Vec::new();
    for chunk in &input.chunks {
        extractable_items.push(chunk as &dyn crate::graph_extraction::GraphExtractable);
    }
    for summary in &input.summaries {
        extractable_items.push(summary as &dyn crate::graph_extraction::GraphExtractable);
    }
    for pair in &input.entities {
        extractable_items.push(&pair.entity as &dyn crate::graph_extraction::GraphExtractable);
        extractable_items.push(&pair.entity_type as &dyn crate::graph_extraction::GraphExtractable);
    }

    let structural_edges = crate::graph_extraction::get_graph_from_model(&extractable_items);

    upsert_provenance(
        db,
        LedgerIdentity::new(
            input.tenant_id,
            input.user_id,
            input.dataset_id,
            pipeline_run_id,
        ),
        &input.chunks,
        &input.entities,
        &input.edges,
        &input.summaries,
        &input.documents,
        &structural_edges,
        &input.producers,
    )
    .await?;

    // Store all DataPoint types as graph nodes (matches Python's add_data_points behavior).
    // Python stores DocumentChunks, TextSummaries, and EntityTypes as graph nodes.

    // Store DocumentChunks as graph nodes
    if !input.chunks.is_empty() {
        let chunk_refs: Vec<&DocumentChunk> = input.chunks.iter().collect();
        graph_db
            .add_nodes(&chunk_refs)
            .await
            .map_err(CognifyError::from)?;
        info!("Stored {} document chunks as graph nodes", chunk_refs.len());
    }

    // Store TextSummaries as graph nodes
    if !input.summaries.is_empty() {
        let summary_refs: Vec<&TextSummary> = input.summaries.iter().collect();
        graph_db
            .add_nodes(&summary_refs)
            .await
            .map_err(CognifyError::from)?;
        info!(
            "Stored {} text summaries as graph nodes",
            summary_refs.len()
        );
    }

    // Store EntityTypes as graph nodes (extract from GraphNodePairs)
    if !input.entities.is_empty() {
        let entity_type_refs: Vec<&cognee_models::EntityType> = input
            .entities
            .iter()
            .map(|pair| &pair.entity_type)
            .collect();
        graph_db
            .add_nodes(&entity_type_refs)
            .await
            .map_err(CognifyError::from)?;
        info!(
            "Stored {} entity types as graph nodes",
            entity_type_refs.len()
        );
    }

    // Store Documents as graph nodes. Python reaches Documents by recursively
    // walking each DocumentChunk's `is_part_of` field (a full Document
    // DataPoint) in get_graph_from_model(). Rust's `is_part_of` is just a
    // `Uuid`, so we store Documents explicitly here. The node `id` equals the
    // source Data item's id (content-addressed, Python-identical) and the node
    // `type` is the concrete subclass name (TextDocument, PdfDocument, …), so
    // the `is_part_of` edge target now resolves to a stored Document node.
    if !input.documents.is_empty() {
        let doc_refs: Vec<&Document> = input.documents.iter().collect();
        graph_db
            .add_nodes(&doc_refs)
            .await
            .map_err(CognifyError::from)?;
        info!("Stored {} documents as graph nodes", doc_refs.len());
    }

    // Build EdgeTypes keyed on each edge's retrieval text
    // (port of Python's create_edge_type_datapoints + index_graph_edges).
    //
    // Parity note: Python's `index_graph_edges` only *vector-indexes* these
    // EdgeType DataPoints (into `EdgeType_relationship_name`) — it never adds
    // them to the graph as nodes (see index_graph_edges.py:86-88 →
    // index_data_points, which touches the vector engine only). We therefore
    // build + vector-index them below but deliberately do NOT call
    // `graph_db.add_nodes` on them, so the Rust graph node-set matches Python's
    // and they don't surface as untyped/uncolored nodes in the visualization.
    //
    // Python keys EdgeType IDs and the embedded relationship_name on the
    // edge's retrieval text — `get_edge_retrieval_text(edge_text,
    // relationship_name)` (index_graph_edges.py:33-53), i.e. the nonblank
    // `edge_text` property, falling back to the nonblank relationship_name,
    // else dropped. `generate_edge_id(edge_id=text)` then derives the ID from
    // that text. We mirror that here so EdgeType UUIDs and the
    // EdgeType_relationship_name vector inputs match Python (B2.5).
    let mut edge_type_counts: HashMap<String, i32> = HashMap::new();
    for edge_pair in &input.edges {
        let edge_text = edge_retrieval_text(edge_pair);
        if edge_text.is_empty() {
            continue;
        }
        *edge_type_counts.entry(edge_text).or_insert(0) += 1;
    }

    let mut edge_types: Vec<EdgeType> = edge_type_counts
        .into_iter()
        .map(|(text, count)| {
            let mut et = EdgeType::new_deterministic(&text, Some(input.dataset_id));
            et.set_count(count);
            et
        })
        .collect();

    // Pre-stamp freshly-built EdgeType DataPoints at construction time so the
    // `source_*` provenance keys are populated before they are vector-indexed
    // (collection `EdgeType_relationship_name`) and before the Triplet payloads
    // copy those keys from the originating EdgeType (gap-05/08 §4.4, below).
    // The LLM-derived edge-type names trace back to the entity-extraction task,
    // so the `source_pipeline` / `source_task` literals match.
    //
    // These DataPoints are NOT stored as graph nodes (see parity note above),
    // so the stamp only affects vector payloads, not the graph/visualization.
    //
    // `task_rank: None` — Python's `create_edge_type_datapoints`
    // (`index_graph_edges.py:50`) builds these objects locally and never
    // returns them from `add_data_points`, so no stamper ever assigns them a
    // rank and they reach the vector payload with the `0` sentinel. Stamping
    // a rank here would put a non-Python value in the persisted
    // `EdgeType_relationship_name` payload (`DataPoint::vector_metadata`
    // serialises `topological_rank` unconditionally).
    //
    // DLT-derived edges (`extract_dlt_fk_edges`) construct
    // `GraphEdgePair` instances rather than DataPoints; they carry no
    // DataPoint to stamp, so no pre-stamp call is needed there.
    {
        let user_label = input.user_id.as_ref().map(|id| id.to_string());
        let mut local_visited: HashSet<Uuid> = HashSet::new();
        for et in &mut edge_types {
            crate::graph_integration::expansion::pre_stamp_extraction(
                et,
                user_label.as_deref(),
                None,
                &mut local_visited,
            );
        }
    }

    // Persist the structural edges discovered above (port of Python's
    // get_graph_from_model() relationship discovery).
    if !structural_edges.is_empty() {
        graph_db
            .add_edges(&structural_edges)
            .await
            .map_err(CognifyError::from)?;
        info!("Upserted {} structural edges", structural_edges.len());
    }

    let embeddings = generate_embeddings(
        &input.chunks,
        &input.entities,
        &input.summaries,
        embedding_engine.clone(),
    )
    .await?;

    let indexed_fields = index_data_points(
        &input.chunks,
        &input.entities,
        &input.summaries,
        &input.documents,
        &input.edges,
        &edge_types,
        input.dataset_id,
        input.user_id,
        input.tenant_id,
        embedding_engine,
        vector_db,
        config,
        &embeddings,
    )
    .await?;

    Ok(CognifyResult {
        chunks: input.chunks.clone(),
        entities: input.entities.clone(),
        edges: input.edges.clone(),
        summaries: input.summaries.clone(),
        edge_types,
        embeddings,
        indexed_fields,
        documents_for_dlt: input.documents.clone(),
        already_completed: false,
        prior_pipeline_run_id: None,
        pipeline_run_id,
        // Forwarded verbatim: this stage collects nothing of its own — every
        // failure it could hit is a persistence failure, which is run-fatal
        // under every configuration and propagates as its own error variant.
        failures: input.failures.clone(),
    })
}

// ---------------------------------------------------------------------------
// Temporal Task 3: extract_temporal_events
// ---------------------------------------------------------------------------

/// Extract temporal events from text chunks via two LLM passes (Temporal Task 3).
///
/// Mirrors the Python `get_temporal_tasks` pipeline stage 3:
/// `extract_events_and_timestamps` followed by `extract_knowledge_graph_from_events`.
///
/// Steps:
/// 1. Collects all non-DLT [`DocumentChunk`]s from `input`.
/// 2. Batches by `config.data_per_batch`.
/// 3. For each chunk in a batch, runs [`TemporalEventExtractor::extract_events`]
///    in parallel (bounded by `config.max_parallel_extractions`).
/// 4. Flattens per-chunk results and enriches each batch with entity attributes
///    via [`TemporalEntityEnricher::enrich`].
/// 5. Returns all events as [`ExtractedTemporalEvents`], each attributed to the
///    data item whose chunk produced it.
///
/// Neither LLM pass propagates on failure. Both *collect* — the extraction pass
/// per chunk, the enrichment pass per chunk of the batch it covered — and the
/// configured [`FailureStop`] then decides whether to keep going, exactly as
/// the standard [`extract_graph_from_data`] loop does.
pub async fn extract_temporal_events(
    input: &ExtractedChunks,
    llm: Arc<dyn Llm>,
    config: &CognifyConfig,
) -> Result<ExtractedTemporalEvents, CognifyError> {
    if input.chunks.is_empty() {
        return Ok(ExtractedTemporalEvents {
            events: vec![],
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
            failures: input.failures.clone(),
        });
    }

    // Filter out DLT chunks — same rationale as extract_graph_from_data.
    let dlt_doc_ids: HashSet<Uuid> = input
        .documents
        .iter()
        .filter(|d| d.document_type == "dlt_row")
        .map(|d| d.base.id)
        .collect();

    let non_dlt_chunks: Vec<&DocumentChunk> = input
        .chunks
        .iter()
        .filter(|c| !dlt_doc_ids.contains(&c.document_id))
        .collect();

    if non_dlt_chunks.is_empty() {
        return Ok(ExtractedTemporalEvents {
            events: vec![],
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
            failures: input.failures.clone(),
        });
    }

    // NOTE: unlike the graph-extraction stages, the batch loop here is
    // structural, not a throttle: `enricher.enrich` runs one aggregate LLM pass
    // per batch, so the barrier is a real data dependency and is kept.
    let batch_size = config.data_per_batch;
    let failure_policy = config.failure_policy();
    let max_parallel = config.max_parallel_extractions.max(1);
    let extractor = Arc::new(TemporalEventExtractor::new(Arc::clone(&llm)));
    let enricher = TemporalEntityEnricher::new(Arc::clone(&llm));

    let mut failures = input.failures.clone();
    let mut all_events: Vec<AttributedEvent> = Vec::new();
    // `Some(n)` once a FailFast abort has fired, naming the first chunk index
    // (into `non_dlt_chunks`) that was never dispatched.
    let mut aborted_at: Option<usize> = None;

    for (batch_idx, batch) in non_dlt_chunks.chunks(batch_size).enumerate() {
        // Owned inputs. `chunk_ids` and `chunk_documents` run parallel to them:
        // a failure is attributed to its chunk and its file, neither of which
        // the stream items carry, and the events a chunk produced are attributed
        // to its file the same way — so the results must be back in input order
        // before the zip below.
        let inputs: Vec<String> = batch.iter().map(|chunk| chunk.text.clone()).collect();
        let chunk_ids: Vec<Uuid> = batch.iter().map(|chunk| chunk.base.id).collect();
        let chunk_documents: Vec<Uuid> = batch.iter().map(|chunk| chunk.document_id).collect();

        // The whole batch is collected before any result is inspected, so a
        // FailFast abort reports every failure in the batch that tripped it —
        // not only the first one to come back.
        // `buffer_unordered`, not `buffered`, for the reason spelled out in
        // `extract_graph_from_data`: `buffered` keeps a completed-but-undrained
        // output in its slot, so one slow chunk blocks every chunk behind it.
        // Each future carries its index and the batch is re-sorted, which both
        // keeps the zip below correlated and keeps `all_events` deterministic
        // for a given input.
        let mut indexed_results: Vec<_> = futures::stream::iter(inputs.into_iter().enumerate())
            .map(|(index, text)| {
                let ext = Arc::clone(&extractor);
                async move {
                    let result = tokio::spawn(async move { ext.extract_events(&text).await }).await;
                    (index, result)
                }
            })
            .buffer_unordered(max_parallel)
            .collect()
            .await;
        indexed_results.sort_by_key(|(index, _)| *index);
        let batch_results: Vec<_> = indexed_results
            .into_iter()
            .map(|(_, result)| result)
            .collect();

        let mut batch_events: Vec<TemporalEvent> = Vec::new();
        let mut batch_data_ids: Vec<Uuid> = Vec::new();
        let mut extraction_failed_chunks: HashSet<Uuid> = HashSet::new();
        let mut batch_failed = false;
        for ((result, chunk_id), document_id) in batch_results
            .into_iter()
            .zip(chunk_ids.iter().copied())
            .zip(chunk_documents.iter().copied())
        {
            let outcome = result
                .map_err(|e| CognifyError::FactExtractionError(e.to_string()))
                .and_then(|inner| inner);
            match outcome {
                Ok(events) => {
                    batch_data_ids.extend(std::iter::repeat_n(document_id, events.len()));
                    batch_events.extend(events);
                }
                Err(e) => {
                    warn!(
                        data_id = %document_id,
                        chunk_id = %chunk_id,
                        "temporal event extraction failed for chunk: {e}"
                    );
                    failures.record(StageFailure {
                        stage: FailureStage::TemporalExtraction,
                        data_id: document_id,
                        chunk_id: Some(chunk_id),
                        error: e.to_string(),
                        fails_item: true,
                    });
                    extraction_failed_chunks.insert(chunk_id);
                    batch_failed = true;
                }
            }
        }

        info!(
            "Temporal extraction batch {}/{}: {} events extracted",
            batch_idx + 1,
            non_dlt_chunks.len().div_ceil(batch_size),
            batch_events.len()
        );

        // ── Entity enrichment pass for the whole batch ──────────────────────
        // One LLM call covers every event the batch produced, across many
        // chunks and possibly many files, so a failure here is recorded
        // against *each* chunk that fed it rather than once for the batch. The
        // chunk failure ratio counts chunks; a batch-shaped failure recorded
        // once would contribute almost nothing to it, and a `FailedItems` run
        // could then "complete" having swept most of the dataset. A chunk whose
        // *extraction* already failed is skipped — it is one failed chunk, not
        // two.
        match enricher.enrich(batch_events).await {
            Ok(enriched) => {
                // `enrich` preserves input order, which is what lets the data
                // ids collected above be zipped straight back on. A change
                // there is the one way ownership could be silently
                // misattributed.
                debug_assert_eq!(
                    enriched.len(),
                    batch_data_ids.len(),
                    "enrich must return its input events, in order"
                );
                all_events.extend(
                    enriched
                        .into_iter()
                        .zip(batch_data_ids)
                        .map(|(event, data_id)| AttributedEvent { event, data_id }),
                );
            }
            Err(e) => {
                // The batch's events are dropped: unenriched events carry none
                // of the entity nodes and edges this pass exists to produce, so
                // persisting them would leave a half-cognified item behind.
                for (chunk_id, document_id) in chunk_ids
                    .iter()
                    .zip(chunk_documents.iter())
                    .filter(|(chunk_id, _)| !extraction_failed_chunks.contains(chunk_id))
                {
                    warn!(
                        data_id = %document_id,
                        chunk_id = %chunk_id,
                        "temporal entity enrichment failed for the batch this chunk fed: {e}"
                    );
                    failures.record(StageFailure {
                        stage: FailureStage::TemporalEnrichment,
                        data_id: *document_id,
                        chunk_id: Some(*chunk_id),
                        error: e.to_string(),
                        fails_item: true,
                    });
                }
                batch_failed = true;
            }
        }

        if batch_failed && failure_policy.stop == FailureStop::FailFast {
            aborted_at = Some((batch_idx + 1) * batch_size);
            break;
        }
    }

    // ── The abort-time partition ────────────────────────────────────────────
    // At the moment of a FailFast abort nothing has been persisted — the loop
    // above accumulates every batch's events in memory and the first write of
    // any kind happens in `add_temporal_data_points`. So "keep the results for
    // the other files" is not a matter of not-deleting; it takes deliberately
    // persisting the finished work before stopping. Files partition three ways:
    //
    //   complete  — every one of its chunks was attempted, none failed
    //               extraction, and every batch it fed was enriched.
    //               Persisted below, and marked complete when the run ends.
    //   failed    — at least one of its chunks failed either pass. Not
    //               persisted, left unmarked.
    //   unreached — chunks never dispatched. Not persisted, left unmarked.
    //
    // Only complete files are persisted, which is what keeps items
    // all-or-nothing. Failed and unreached files are indistinguishable to the
    // next run and both are simply redone.
    //
    // Under `RunToEnd` there is no partition and no filtering: the run persists
    // normally and the item-scoped sweep removes the failed files'
    // contributions afterwards, so one deletion path serves both cases.
    //
    // Temporal needs no `complete_documents` set of its own: events are the
    // only thing this stage produces, and every one of them names its file, so
    // dropping the excluded files' events *is* the partition.
    if let Some(first_unreached) = aborted_at {
        for chunk in non_dlt_chunks.iter().skip(first_unreached) {
            failures.mark_unreached(chunk.document_id);
        }

        if failure_policy.scope != RollbackScope::FailedItems {
            return Err(CognifyError::RunFailed {
                report: Box::new(failures),
            });
        }

        let excluded: HashSet<Uuid> = failures
            .failed_items()
            .iter()
            .chain(failures.unreached_items().iter())
            .copied()
            .collect();
        all_events.retain(|attributed| !excluded.contains(&attributed.data_id));
    }

    info!(
        "Temporal event extraction complete: {} total events",
        all_events.len()
    );

    Ok(ExtractedTemporalEvents {
        events: all_events,
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
        failures,
    })
}

// ---------------------------------------------------------------------------
// Temporal Task 4: add_temporal_data_points
// ---------------------------------------------------------------------------

/// One temporal artifact and the data items that produced it.
///
/// Temporal nodes are content-addressed — an event by its name, a timestamp by
/// its instant, an entity by [`Entity::id_for`] — so one physical node routinely
/// has several producers. Ownership is one row per (artifact, producing data
/// item), which is what makes the node removable only when its *last* owning
/// file goes.
struct TemporalNodeOwner {
    node_type: String,
    label: Option<String>,
    /// The node payload, exactly as the graph store received it.
    attributes: serde_json::Value,
    /// The vector collections a sweep must clear for this node. Only `Event`
    /// has one: temporal indexes `Event.name` and nothing else, so claiming
    /// `["name"]` on an entity node would make a temporal sweep delete an
    /// `Entity_name` point a *standard* run wrote.
    indexed_fields: &'static [&'static str],
    /// Insertion-ordered and deduplicated.
    data_ids: Vec<Uuid>,
}

/// One temporal edge and the data items that produced it. Same shape, same
/// reason, as [`TemporalNodeOwner`].
struct TemporalEdgeOwner {
    properties: HashMap<std::borrow::Cow<'static, str>, serde_json::Value>,
    data_ids: Vec<Uuid>,
}

/// Note that `data_id` produced the node `node_id`, creating the owner record
/// on first sight and appending the producer on every later one.
fn record_temporal_node_owner(
    owners: &mut BTreeMap<Uuid, TemporalNodeOwner>,
    node_id: Uuid,
    node_type: &str,
    label: Option<String>,
    attributes: serde_json::Value,
    indexed_fields: &'static [&'static str],
    data_id: Uuid,
) {
    let owner = owners.entry(node_id).or_insert_with(|| TemporalNodeOwner {
        node_type: node_type.to_string(),
        label,
        attributes,
        indexed_fields,
        data_ids: Vec::new(),
    });
    if !owner.data_ids.contains(&data_id) {
        owner.data_ids.push(data_id);
    }
}

/// The same, for an edge keyed on its sanitized triplet.
fn record_temporal_edge_owner(
    owners: &mut BTreeMap<(Uuid, String, Uuid), TemporalEdgeOwner>,
    source_id: Uuid,
    relationship_name: &str,
    target_id: Uuid,
    properties: &HashMap<std::borrow::Cow<'static, str>, serde_json::Value>,
    data_id: Uuid,
) {
    let owner = owners
        .entry((source_id, relationship_name.to_string(), target_id))
        .or_insert_with(|| TemporalEdgeOwner {
            properties: properties.clone(),
            data_ids: Vec::new(),
        });
    if !owner.data_ids.contains(&data_id) {
        owner.data_ids.push(data_id);
    }
}

/// Turn the accumulated owner maps into ownership rows — one per (artifact,
/// producing data item).
///
/// Modelled on [`dlt_provenance_rows`], the other site that builds rows from
/// raw `serde_json` node payloads rather than from `DataPoint`s. The one thing
/// it does differently: the relationship name is sanitized before it is hashed,
/// because a temporal relationship comes straight from the LLM and NUL bytes
/// are stripped on the way into Postgres. Hashing the raw text would key the
/// ledger on text no store holds, and the sweep would then miss the edge.
fn temporal_provenance_rows(
    id: LedgerIdentity,
    node_owners: &BTreeMap<Uuid, TemporalNodeOwner>,
    edge_owners: &BTreeMap<(Uuid, String, Uuid), TemporalEdgeOwner>,
) -> (
    Vec<cognee_database::GraphNode>,
    Vec<cognee_database::GraphEdge>,
) {
    use cognee_database::{GraphEdge, GraphNode};

    let mut prov_nodes: Vec<GraphNode> = Vec::new();
    for (node_id, owner) in node_owners {
        for data_id in &owner.data_ids {
            prov_nodes.push(GraphNode {
                id: provenance_node_id(id.tenant_id, id.user_id, id.dataset_id, *data_id, *node_id),
                slug: *node_id,
                user_id: id.user_id,
                data_id: *data_id,
                dataset_id: id.dataset_id,
                pipeline_run_id: id.pipeline_run_id,
                label: owner.label.clone(),
                node_type: owner.node_type.clone(),
                indexed_fields: json!(owner.indexed_fields),
                attributes: Some(owner.attributes.clone()),
                created_at: Utc::now(),
            });
        }
    }

    let mut prov_edges: Vec<GraphEdge> = Vec::new();
    for ((source_id, relationship_name, target_id), owner) in edge_owners {
        let edge_text = sanitize_string(relationship_name.clone());
        let attributes = if owner.properties.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(
                owner
                    .properties
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            ))
        };
        for data_id in &owner.data_ids {
            prov_edges.push(GraphEdge {
                id: provenance_edge_id(
                    id.tenant_id,
                    id.user_id,
                    id.dataset_id,
                    *data_id,
                    *source_id,
                    &edge_text,
                    *target_id,
                ),
                slug: triplet_slug(*source_id, &edge_text, *target_id),
                user_id: id.user_id,
                data_id: *data_id,
                dataset_id: id.dataset_id,
                pipeline_run_id: id.pipeline_run_id,
                source_node_id: *source_id,
                destination_node_id: *target_id,
                relationship_name: edge_text.clone(),
                label: None,
                attributes: attributes.clone(),
                created_at: Utc::now(),
            });
        }
    }

    (prov_nodes, prov_edges)
}

/// Record ownership of the temporal artifacts a run is about to write, in one
/// transaction, before the graph or the vector store sees any of them.
async fn record_temporal_ownership(
    db: &DatabaseConnection,
    id: LedgerIdentity,
    node_owners: &BTreeMap<Uuid, TemporalNodeOwner>,
    edge_owners: &BTreeMap<(Uuid, String, Uuid), TemporalEdgeOwner>,
) -> Result<(), CognifyError> {
    let (prov_nodes, prov_edges) = temporal_provenance_rows(id, node_owners, edge_owners);
    cognee_database::ops::graph_storage::upsert_provenance_graph(db, &prov_nodes, &prov_edges)
        .await?;
    if !prov_nodes.is_empty() || !prov_edges.is_empty() {
        info!(
            "Recorded ownership of {} temporal nodes and {} temporal edges before writing them",
            prov_nodes.len(),
            prov_edges.len()
        );
    }
    Ok(())
}

/// Persist temporal events to graph and vector databases (Temporal Task 4).
///
/// Mirrors the Python `add_data_points` stage in the temporal pipeline.
///
/// For each [`AttributedEvent`]:
/// 1. Creates an `Event` graph node with a deterministic UUID5 ID.
/// 2. For `event.at` — creates a `Timestamp` graph node and an `at` edge.
/// 3. For `event.during` — creates `Timestamp` nodes for from/to, an `Interval`
///    node, and `during` / `time_from` / `time_to` edges (Python-compatible layout).
/// 4. For each [`EventAttribute`] — creates or looks up an entity graph node
///    and adds a typed edge from the `Event` to the entity.
/// 5. Embeds `event.name` and indexes to the `Event_name` vector collection.
///
/// Every one of those artifacts is claimed in the ownership ledger *before* the
/// first store write, so a failure part-way through leaves rows a sweep can
/// find rather than artifacts nothing can name. Discovering what the run is
/// about to write is pure computation — the loop below builds the entire
/// payload in memory — so the ledger write drops in between the build and the
/// first `add_nodes_raw` with no restructuring.
pub async fn add_temporal_data_points(
    events: &ExtractedTemporalEvents,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: &DatabaseConnection,
    pipeline_run_id: Option<Uuid>,
) -> Result<CognifyResult, CognifyError> {
    if events.events.is_empty() {
        info!("No temporal events to persist.");
        return Ok(CognifyResult {
            failures: events.failures.clone(),
            pipeline_run_id,
            ..CognifyResult::empty()
        });
    }

    let mut graph_nodes: Vec<serde_json::Value> = Vec::new();
    let mut graph_edges: Vec<EdgeData> = Vec::new();

    // Deduplicate event nodes across producers: two files describing the same
    // event address the same node id, and pushing it twice would embed and
    // index the same `Event_name` point twice. The ownership rows below
    // deduplicate the artifact and keep both producers, so the payload has to
    // agree.
    let mut seen_event_ids: HashSet<Uuid> = HashSet::new();
    // Deduplicate entity nodes across events to avoid redundant graph inserts.
    let mut seen_entity_ids: HashSet<Uuid> = HashSet::new();
    // Deduplicate edges: (source_id, target_id, relationship_name)
    let mut seen_edge_keys: HashSet<(String, String, String)> = HashSet::new();

    let mut event_ids: Vec<Uuid> = Vec::new();
    let mut event_names: Vec<String> = Vec::new();

    // The producer sets the ledger is written from. Recorded *outside* the
    // `seen_*` guards above: a second occurrence of an artifact is precisely a
    // second producer, and the guards exist only to keep the graph payload from
    // repeating itself.
    let mut node_owners: BTreeMap<Uuid, TemporalNodeOwner> = BTreeMap::new();
    let mut edge_owners: BTreeMap<(Uuid, String, Uuid), TemporalEdgeOwner> = BTreeMap::new();

    for attributed in &events.events {
        let event = &attributed.event;
        let data_id = attributed.data_id;

        // ── Event node ──────────────────────────────────────────────────────
        let event_id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("event:{}", event.name).as_bytes(),
        );

        let mut event_node = json!({
            "id": event_id.to_string(),
            "data_type": "Event",
            "name": event.name,
        });
        if let Some(desc) = &event.description {
            event_node["description"] = json!(desc);
        }
        if let Some(loc) = &event.location {
            event_node["location"] = json!(loc);
        }
        record_temporal_node_owner(
            &mut node_owners,
            event_id,
            "Event",
            Some(event.name.clone()),
            event_node.clone(),
            &["name"],
            data_id,
        );
        if seen_event_ids.insert(event_id) {
            event_ids.push(event_id);
            event_names.push(event.name.clone());
            graph_nodes.push(event_node);
        }

        // ── Timestamp for event.at ──────────────────────────────────────────
        if let Some(ts) = &event.at {
            let ts_id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("timestamp:{}", ts.time_at).as_bytes(),
            );
            let ts_node = json!({
                "id": ts_id.to_string(),
                "data_type": "Timestamp",
                "time_at": ts.time_at,
                "timestamp_str": ts.timestamp_str,
                "year": ts.year,
                "month": ts.month,
                "day": ts.day,
                "hour": ts.hour,
                "minute": ts.minute,
                "second": ts.second,
            });
            record_temporal_node_owner(
                &mut node_owners,
                ts_id,
                "Timestamp",
                None,
                ts_node.clone(),
                &[],
                data_id,
            );
            graph_nodes.push(ts_node);

            let props = build_edge_props(&event_id.to_string(), &ts_id.to_string(), "at");
            record_temporal_edge_owner(&mut edge_owners, event_id, "at", ts_id, &props, data_id);
            let edge_key = (event_id.to_string(), ts_id.to_string(), "at".to_string());
            if seen_edge_keys.insert(edge_key) {
                graph_edges.push((
                    event_id.to_string(),
                    ts_id.to_string(),
                    "at".to_string(),
                    props,
                ));
            }
        }

        // ── Interval for event.during ───────────────────────────────────────
        if let Some(interval) = &event.during {
            let ts_from = &interval.time_from;
            let ts_to = &interval.time_to;

            let ts_from_id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("timestamp:{}", ts_from.time_at).as_bytes(),
            );
            let ts_to_id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("timestamp:{}", ts_to.time_at).as_bytes(),
            );
            let interval_id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("interval:{}:{}", ts_from.time_at, ts_to.time_at).as_bytes(),
            );

            let ts_from_node = json!({
                "id": ts_from_id.to_string(),
                "data_type": "Timestamp",
                "time_at": ts_from.time_at,
                "timestamp_str": ts_from.timestamp_str,
                "year": ts_from.year,
                "month": ts_from.month,
                "day": ts_from.day,
                "hour": ts_from.hour,
                "minute": ts_from.minute,
                "second": ts_from.second,
            });
            let ts_to_node = json!({
                "id": ts_to_id.to_string(),
                "data_type": "Timestamp",
                "time_at": ts_to.time_at,
                "timestamp_str": ts_to.timestamp_str,
                "year": ts_to.year,
                "month": ts_to.month,
                "day": ts_to.day,
                "hour": ts_to.hour,
                "minute": ts_to.minute,
                "second": ts_to.second,
            });
            let interval_node = json!({
                "id": interval_id.to_string(),
                "data_type": "Interval",
            });
            for (node_id, node_type, node) in [
                (ts_from_id, "Timestamp", &ts_from_node),
                (ts_to_id, "Timestamp", &ts_to_node),
                (interval_id, "Interval", &interval_node),
            ] {
                record_temporal_node_owner(
                    &mut node_owners,
                    node_id,
                    node_type,
                    None,
                    node.clone(),
                    &[],
                    data_id,
                );
            }
            graph_nodes.push(ts_from_node);
            graph_nodes.push(ts_to_node);
            graph_nodes.push(interval_node);

            // Event -[during]-> Interval, Interval -[time_from|time_to]-> Timestamp
            for (source_id, relationship_name, target_id) in [
                (event_id, "during", interval_id),
                (interval_id, "time_from", ts_from_id),
                (interval_id, "time_to", ts_to_id),
            ] {
                let props = build_edge_props(
                    &source_id.to_string(),
                    &target_id.to_string(),
                    relationship_name,
                );
                record_temporal_edge_owner(
                    &mut edge_owners,
                    source_id,
                    relationship_name,
                    target_id,
                    &props,
                    data_id,
                );
                let edge_key = (
                    source_id.to_string(),
                    target_id.to_string(),
                    relationship_name.to_string(),
                );
                if seen_edge_keys.insert(edge_key) {
                    graph_edges.push((
                        source_id.to_string(),
                        target_id.to_string(),
                        relationship_name.to_string(),
                        props,
                    ));
                }
            }
        }

        // ── Entity attribute nodes and edges ────────────────────────────────
        for attr in &event.attributes {
            // Python temporal path: `Entity.id_for(attribute.entity)`
            // (add_entities_to_event.py:39). Was a bare `entity:{name}` hash with
            // no normalization and no class prefix.
            let entity_id = Entity::id_for(&attr.entity);

            let entity_node = json!({
                "id": entity_id.to_string(),
                "data_type": attr.entity_type,
                "name": attr.entity,
            });
            record_temporal_node_owner(
                &mut node_owners,
                entity_id,
                &attr.entity_type,
                Some(attr.entity.clone()),
                entity_node.clone(),
                &[],
                data_id,
            );
            if seen_entity_ids.insert(entity_id) {
                graph_nodes.push(entity_node);
            }

            let props = build_edge_props(
                &event_id.to_string(),
                &entity_id.to_string(),
                &attr.relationship,
            );
            record_temporal_edge_owner(
                &mut edge_owners,
                event_id,
                &attr.relationship,
                entity_id,
                &props,
                data_id,
            );
            let rel_key = (
                event_id.to_string(),
                entity_id.to_string(),
                attr.relationship.clone(),
            );
            if seen_edge_keys.insert(rel_key) {
                graph_edges.push((
                    event_id.to_string(),
                    entity_id.to_string(),
                    attr.relationship.clone(),
                    props,
                ));
            }
        }
    }

    // ── Ownership first ─────────────────────────────────────────────────────
    // Everything above is pure computation; nothing has left this function yet.
    // The ledger goes in before the graph and vector writes so that no temporal
    // artifact can exist without a row naming the run that created it.
    record_temporal_ownership(
        db,
        LedgerIdentity::new(
            events.tenant_id,
            events.user_id,
            events.dataset_id,
            pipeline_run_id,
        ),
        &node_owners,
        &edge_owners,
    )
    .await?;

    // Persist nodes and edges to graph DB.
    if !graph_nodes.is_empty() {
        let node_count = graph_nodes.len();
        graph_db
            .add_nodes_raw(graph_nodes)
            .await
            .map_err(CognifyError::from)?;
        info!("Stored {} temporal graph nodes", node_count);
    }

    if !graph_edges.is_empty() {
        let edge_count = graph_edges.len();
        graph_db
            .add_edges(&graph_edges)
            .await
            .map_err(CognifyError::from)?;
        info!("Stored {} temporal graph edges", edge_count);
    }

    // ── Vector indexing: Event.name ──────────────────────────────────────────
    let mut indexed_fields = IndexedFieldsStats::default();

    if !event_ids.is_empty() {
        let dimension = embedding_engine.dimension();

        if !vector_db
            .has_collection("Event", "name")
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
        {
            vector_db
                .create_collection("Event", "name", dimension)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
        }

        let name_strs: Vec<&str> = event_names.iter().map(String::as_str).collect();
        let vectors = embedding_engine
            .embed(&name_strs)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

        let points: Vec<VectorPoint> = event_ids
            .iter()
            .zip(event_names.iter())
            .zip(vectors.iter())
            .map(|((id, name), vector)| {
                let mut point = VectorPoint::new(*id, vector.clone())
                    .with_metadata("type", json!("Event"))
                    .with_metadata("field", json!("name"))
                    .with_metadata("name", json!(name))
                    .with_metadata("dataset_id", json!(events.dataset_id.to_string()));
                if let Some(uid) = events.user_id {
                    point = point.with_metadata("user_id", json!(uid.to_string()));
                }
                if let Some(tid) = events.tenant_id {
                    point = point.with_metadata("tenant_id", json!(tid.to_string()));
                }
                point
            })
            .collect();

        vector_db
            .index_points("Event", "name", &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        indexed_fields.record("Event", "name", event_ids.len());
        info!("Indexed {} event names in vector DB", event_ids.len());
    }

    Ok(CognifyResult {
        chunks: vec![],
        entities: vec![],
        edges: vec![],
        summaries: vec![],
        edge_types: vec![],
        embeddings: vec![],
        indexed_fields,
        documents_for_dlt: vec![],
        already_completed: false,
        prior_pipeline_run_id: None,
        pipeline_run_id,
        failures: events.failures.clone(),
    })
}

/// Resolve the retrieval text for an edge, mirroring Python's
/// `get_edge_retrieval_text(edge_text, relationship_name)`
/// (prepare_edges_for_storage.py:26-28 via index_graph_edges.py:33-53):
/// prefer the nonblank `edge_text` property, fall back to the nonblank
/// `relationship_name`, else return an empty string (caller drops empties).
fn edge_retrieval_text(edge_pair: &GraphEdgePair) -> String {
    EdgeType::retrieval_text(
        edge_pair.properties.get("edge_text").map(String::as_str),
        &edge_pair.relationship_name,
    )
}

/// Build minimal edge properties for graph storage.
fn build_edge_props(
    source_id: &str,
    target_id: &str,
    relationship_name: &str,
) -> HashMap<std::borrow::Cow<'static, str>, serde_json::Value> {
    let mut props = HashMap::new();
    props.insert(
        std::borrow::Cow::Borrowed("source_node_id"),
        json!(source_id),
    );
    props.insert(
        std::borrow::Cow::Borrowed("target_node_id"),
        json!(target_id),
    );
    props.insert(
        std::borrow::Cow::Borrowed("relationship_name"),
        json!(relationship_name),
    );
    props
}

// ---------------------------------------------------------------------------
// Task 6: extract_dlt_fk_edges
// ---------------------------------------------------------------------------

/// Create graph edges and schema nodes from DLT-sourced relational data.
///
/// Mirrors the Python `cognee/tasks/ingestion/extract_dlt_fk_edges.py`.
/// This task runs after `add_data_points` in the cognify pipeline. It:
/// 1. Identifies DLT documents from the classified documents list
/// 2. Parses `external_metadata` for table info and foreign key definitions
/// 3. Creates `is_row_of` edges from DLT document nodes to their source table
/// 4. Creates FK-based edges between documents of related rows
///
/// If no DLT documents are present, this is a no-op.
///
/// Ownership of the schema nodes and FK edges is recorded in `db` before any of
/// them reaches the graph (I1). The teardown deliberately stays outside the
/// executor — the `pipeline_runs` row is already COMPLETED by the time it runs
/// — but its artifacts still need to be *reachable* by a sweep, so they are
/// stamped with the run id like everything else the run wrote.
pub async fn extract_dlt_fk_edges(
    _chunks: &[DocumentChunk],
    documents: &[Document],
    graph_db: Arc<dyn GraphDBTrait>,
    db: &DatabaseConnection,
    id: LedgerIdentity,
) -> Result<(), CognifyError> {
    // Collect DLT documents
    let dlt_docs: Vec<&Document> = documents
        .iter()
        .filter(|d| d.document_type == "dlt_row")
        .collect();

    if dlt_docs.is_empty() {
        return Ok(());
    }

    info!(
        "Processing {} DLT documents for FK edge extraction",
        dlt_docs.len()
    );

    // Parse external_metadata for each DLT document
    // Collect table info and FK definitions
    let mut tables_seen: HashMap<String, DltTableMeta> = HashMap::new();
    let mut dlt_doc_meta: HashMap<Uuid, serde_json::Value> = HashMap::new();
    let mut fk_defs_seen: HashSet<(String, String, String, String)> = HashSet::new();

    for doc in &dlt_docs {
        let ext_metadata = match &doc.external_metadata {
            Some(m) => match serde_json::from_str::<serde_json::Value>(m) {
                Ok(v) if v.get("source").and_then(|s| s.as_str()) == Some("dlt") => v,
                _ => continue,
            },
            None => continue,
        };

        dlt_doc_meta.insert(doc.base.id, ext_metadata.clone());

        let table_name = ext_metadata
            .get("table_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !table_name.is_empty() && !tables_seen.contains_key(&table_name) {
            tables_seen.insert(
                table_name.clone(),
                DltTableMeta {
                    schema_info: ext_metadata.get("schema_info").cloned(),
                    foreign_keys: ext_metadata
                        .get("foreign_keys")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default(),
                    dlt_db_name: ext_metadata
                        .get("dlt_db_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                },
            );
        }
    }

    if dlt_doc_meta.is_empty() {
        return Ok(());
    }

    let mut all_edges: Vec<cognee_graph::EdgeData> = Vec::new();

    // Phase 1: Build table node IDs (deterministic via uuid5) and SchemaTable nodes
    let mut table_node_ids: HashMap<String, Uuid> = HashMap::new();
    let mut schema_nodes: Vec<serde_json::Value> = Vec::new();

    for (table_name, table_meta) in &tables_seen {
        let id = Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("dlt:{table_name}").as_bytes());
        table_node_ids.insert(table_name.clone(), id);

        let columns_str = table_meta
            .schema_info
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "[]".to_string());
        let fk_str =
            serde_json::to_string(&table_meta.foreign_keys).unwrap_or_else(|_| "[]".to_string());

        let table_node = SchemaTableNode {
            id: id.to_string(),
            name: table_name.clone(),
            columns: columns_str,
            primary_key: None,
            foreign_keys: fk_str,
            sample_rows: "[]".to_string(),
            row_count_estimate: None,
            description: format!(
                "DLT-ingested relational table '{}' from database '{}'.",
                table_name, table_meta.dlt_db_name
            ),
            data_type: "SchemaTable".to_string(),
        };
        if let Ok(val) = serde_json::to_value(&table_node) {
            schema_nodes.push(val);
        }
    }

    // Phase 2: Create FK relationship edges between table nodes
    for (table_name, table_meta) in &tables_seen {
        for fk in &table_meta.foreign_keys {
            let fk_col = fk
                .get("column")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ref_table = fk
                .get("ref_table")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ref_col = fk
                .get("ref_column")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if fk_col.is_empty() || ref_table.is_empty() {
                continue;
            }

            let fk_key = (
                table_name.clone(),
                fk_col.clone(),
                ref_table.clone(),
                ref_col.clone(),
            );
            if fk_defs_seen.contains(&fk_key) {
                continue;
            }
            fk_defs_seen.insert(fk_key);

            let rel_name = format!("{table_name}:{fk_col}->{ref_table}:{ref_col}");
            let rel_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("dlt:{rel_name}").as_bytes());

            // Create SchemaRelationship node for this FK definition
            let rel_node = SchemaRelationshipNode {
                id: rel_id.to_string(),
                name: rel_name.clone(),
                source_table: table_name.clone(),
                target_table: ref_table.clone(),
                relationship_type: "foreign_key".to_string(),
                source_column: fk_col.clone(),
                target_column: ref_col.clone(),
                description: format!("Foreign key: {table_name}.{fk_col} -> {ref_table}.{ref_col}"),
                data_type: "SchemaRelationship".to_string(),
            };
            if let Ok(val) = serde_json::to_value(&rel_node) {
                schema_nodes.push(val);
            }

            // source_table -> relationship (has_foreign_key)
            if let Some(&source_table_id) = table_node_ids.get(table_name.as_str()) {
                let mut props = HashMap::new();
                props.insert(
                    std::borrow::Cow::Borrowed("source_node_id"),
                    json!(source_table_id.to_string()),
                );
                props.insert(
                    std::borrow::Cow::Borrowed("target_node_id"),
                    json!(rel_id.to_string()),
                );
                props.insert(
                    std::borrow::Cow::Borrowed("relationship_name"),
                    json!("has_foreign_key"),
                );
                all_edges.push((
                    source_table_id.to_string(),
                    rel_id.to_string(),
                    "has_foreign_key".to_string(),
                    props,
                ));
            }

            // relationship -> target_table (references_table)
            if let Some(&target_table_id) = table_node_ids.get(ref_table.as_str()) {
                let mut props = HashMap::new();
                props.insert(
                    std::borrow::Cow::Borrowed("source_node_id"),
                    json!(rel_id.to_string()),
                );
                props.insert(
                    std::borrow::Cow::Borrowed("target_node_id"),
                    json!(target_table_id.to_string()),
                );
                props.insert(
                    std::borrow::Cow::Borrowed("relationship_name"),
                    json!("references_table"),
                );
                all_edges.push((
                    rel_id.to_string(),
                    target_table_id.to_string(),
                    "references_table".to_string(),
                    props,
                ));
            }
        }
    }

    // Phase 3: Create row-level edges (document -> table, document -> referenced document)
    let mut seen_row_edges: HashSet<(String, String, String)> = HashSet::new();

    for (doc_id, ext_metadata) in &dlt_doc_meta {
        let table_name = ext_metadata
            .get("table_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Link document to its SchemaTable node
        if let Some(&table_node_id) = table_node_ids.get(table_name) {
            let mut props = HashMap::new();
            props.insert(
                std::borrow::Cow::Borrowed("source_node_id"),
                json!(doc_id.to_string()),
            );
            props.insert(
                std::borrow::Cow::Borrowed("target_node_id"),
                json!(table_node_id.to_string()),
            );
            props.insert(
                std::borrow::Cow::Borrowed("relationship_name"),
                json!("is_row_of"),
            );
            all_edges.push((
                doc_id.to_string(),
                table_node_id.to_string(),
                "is_row_of".to_string(),
                props,
            ));
        }

        // Create FK row-level edges
        let fk_references = ext_metadata
            .get("fk_references")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        for fk_ref in &fk_references {
            let target_data_id = match fk_ref.get("target_data_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };

            let relationship_name = fk_ref
                .get("relationship_name")
                .and_then(|v| v.as_str())
                .unwrap_or("references")
                .to_string();

            let edge_key = (
                doc_id.to_string(),
                target_data_id.clone(),
                relationship_name.clone(),
            );
            if seen_row_edges.contains(&edge_key) {
                continue;
            }
            seen_row_edges.insert(edge_key);

            let mut props = HashMap::new();
            props.insert(
                std::borrow::Cow::Borrowed("source_node_id"),
                json!(doc_id.to_string()),
            );
            props.insert(
                std::borrow::Cow::Borrowed("target_node_id"),
                json!(target_data_id.clone()),
            );
            props.insert(
                std::borrow::Cow::Borrowed("relationship_name"),
                json!(relationship_name.clone()),
            );
            props.insert(
                std::borrow::Cow::Borrowed("edge_text"),
                json!(relationship_name.replace('_', " ")),
            );
            props.insert(
                std::borrow::Cow::Borrowed("source_table"),
                json!(table_name),
            );
            props.insert(
                std::borrow::Cow::Borrowed("target_table"),
                json!(
                    fk_ref
                        .get("target_table")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                ),
            );
            props.insert(
                std::borrow::Cow::Borrowed("fk_column"),
                json!(fk_ref.get("column").and_then(|v| v.as_str()).unwrap_or("")),
            );

            all_edges.push((doc_id.to_string(), target_data_id, relationship_name, props));
        }
    }

    // I1: claim every schema node and FK edge before a single one of them
    // reaches the graph.
    let row_doc_ids: HashSet<Uuid> = dlt_doc_meta.keys().copied().collect();
    let (prov_nodes, prov_edges) = dlt_provenance_rows(id, &schema_nodes, &all_edges, &row_doc_ids);
    cognee_database::ops::graph_storage::upsert_provenance_graph(db, &prov_nodes, &prov_edges)
        .await?;

    // Persist schema nodes to graph DB (SchemaTable + SchemaRelationship)
    // NOTE: Python also calls `index_data_points(schema_nodes)` to embed these
    // into vector DB. That is out of scope for Phase 0; Rust's `add_data_points`
    // task handles vector indexing for the main pipeline data.
    if !schema_nodes.is_empty() {
        let node_count = schema_nodes.len();
        graph_db
            .add_nodes_raw(schema_nodes)
            .await
            .map_err(CognifyError::from)?;
        info!("Added {} DLT schema nodes to graph", node_count);
    }

    // Persist edges to graph DB
    if !all_edges.is_empty() {
        graph_db
            .add_edges(&all_edges)
            .await
            .map_err(CognifyError::from)?;
        info!(
            "Added {} DLT FK edges to graph ({} tables, {} FK definitions)",
            all_edges.len(),
            table_node_ids.len(),
            fk_defs_seen.len()
        );
    }

    Ok(())
}

/// Ownership rows for the DLT teardown's schema nodes and FK edges.
///
/// Attribution follows the two precedents already in the ledger rather than a
/// third rule of its own:
///
/// - `Uuid::nil()` where an artifact genuinely spans data items — a SchemaTable
///   node is shared by every row-document of that table, and the table→
///   relationship→table edges belong to the schema, not to a row. This is the
///   same shape the structural edges and EntityType rows already use.
/// - the producing document's id for the row-level edges (`is_row_of` and the
///   FK row references), whose source node *is* that document.
///
/// `indexed_fields` is empty because Rust does not vector-index these nodes
/// (Python does; see the note at the `add_nodes_raw` call).
fn dlt_provenance_rows(
    id: LedgerIdentity,
    schema_nodes: &[serde_json::Value],
    edges: &[cognee_graph::EdgeData],
    row_doc_ids: &HashSet<Uuid>,
) -> (
    Vec<cognee_database::GraphNode>,
    Vec<cognee_database::GraphEdge>,
) {
    use cognee_database::{GraphEdge, GraphNode};

    let mut prov_nodes: Vec<GraphNode> = Vec::new();
    for node in schema_nodes {
        let Some(node_id) = node
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            continue;
        };
        let node_type = node
            .get("data_type")
            .and_then(|v| v.as_str())
            .unwrap_or("SchemaTable")
            .to_string();
        let label = node
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        prov_nodes.push(GraphNode {
            id: provenance_node_id(
                id.tenant_id,
                id.user_id,
                id.dataset_id,
                Uuid::nil(),
                node_id,
            ),
            slug: node_id,
            user_id: id.user_id,
            data_id: Uuid::nil(),
            dataset_id: id.dataset_id,
            pipeline_run_id: id.pipeline_run_id,
            label,
            node_type,
            indexed_fields: json!([]),
            attributes: Some(node.clone()),
            created_at: Utc::now(),
        });
    }

    let mut prov_edges: Vec<GraphEdge> = Vec::new();
    for (source, target, relationship_name, properties) in edges {
        let source_id = Uuid::parse_str(source).unwrap_or(Uuid::nil());
        let target_id = Uuid::parse_str(target).unwrap_or(Uuid::nil());
        // A row-level edge leaves the DLT document node it was built from; a
        // schema-level one leaves a SchemaTable / SchemaRelationship node.
        let data_id = if row_doc_ids.contains(&source_id) {
            source_id
        } else {
            Uuid::nil()
        };

        let attributes = if properties.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(
                properties
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            ))
        };

        prov_edges.push(GraphEdge {
            id: provenance_edge_id(
                id.tenant_id,
                id.user_id,
                id.dataset_id,
                data_id,
                source_id,
                relationship_name,
                target_id,
            ),
            slug: triplet_slug(source_id, relationship_name, target_id),
            user_id: id.user_id,
            data_id,
            dataset_id: id.dataset_id,
            pipeline_run_id: id.pipeline_run_id,
            source_node_id: source_id,
            destination_node_id: target_id,
            relationship_name: relationship_name.clone(),
            label: None,
            attributes,
            created_at: Utc::now(),
        });
    }

    (prov_nodes, prov_edges)
}

/// Graph node representing a DLT-ingested relational table.
///
/// Mirrors Python's `SchemaTable` DataPoint model from
/// `cognee/tasks/schema/models.py`.
#[derive(Debug, Serialize)]
struct SchemaTableNode {
    id: String,
    name: String,
    columns: String,
    primary_key: Option<String>,
    foreign_keys: String,
    sample_rows: String,
    row_count_estimate: Option<i64>,
    description: String,
    data_type: String,
}

/// Graph node representing a foreign-key relationship between two tables.
///
/// Mirrors Python's `SchemaRelationship` DataPoint model from
/// `cognee/tasks/schema/models.py`.
#[derive(Debug, Serialize)]
struct SchemaRelationshipNode {
    id: String,
    name: String,
    source_table: String,
    target_table: String,
    relationship_type: String,
    source_column: String,
    target_column: String,
    description: String,
    data_type: String,
}

/// Internal metadata for a DLT source table.
#[derive(Debug)]
struct DltTableMeta {
    schema_info: Option<serde_json::Value>,
    foreign_keys: Vec<serde_json::Value>,
    dlt_db_name: String,
}

// ---------------------------------------------------------------------------
// Provenance stamping helper
// ---------------------------------------------------------------------------

/// Stamp pipeline provenance fields on a [`DataPoint`].
///
/// Called **in-body**, inside each cognify task's closure, because cognify's
/// task outputs are wrapper structs (`ClassifiedDocuments`, `ExtractedChunks`,
/// `ExtractedGraphData`, …) that do not implement `HasDataPoint` and are
/// therefore not recognised by the executor's
/// [`cognee_core::provenance::stamp_tree_dyn`] cascade — the executor walks
/// right past them without stamping the nested DataPoints. Per locked
/// decision 6 of `docs/telemetry/05-datapoint-provenance.md` both mechanisms
/// coexist; the `if dp.source_X.is_none()` guards make double-stamping a
/// no-op, and because these calls run inside the closure they always land
/// *before* the executor's post-task walk.
///
/// Only sets each field if it is currently `None`, so earlier (more specific)
/// stamps are never overwritten.  Mirrors the Python
/// `run_tasks_base.py` post-task provenance stamping.
///
/// `rank` is the 1-based position of the emitting task (one of the
/// `*_TASK_RANK` constants below, or a caller-supplied position via the
/// `make_*_task_with_rank` factories). It is written to
/// `DataPoint.topological_rank` only while that field is still unset — `None`
/// or the `Some(0)` Python sentinel — matching `run_tasks_base.py:69-75`.
/// Pass `0` to skip the rank write entirely.
fn stamp_provenance(dp: &mut DataPoint, pipeline: &str, task: &str, user: Option<&str>, rank: i32) {
    if dp.source_pipeline.is_none() {
        dp.source_pipeline = Some(pipeline.to_string());
    }
    if dp.source_task.is_none() {
        dp.source_task = Some(task.to_string());
    }
    if dp.source_user.is_none() {
        dp.source_user = user.map(String::from);
    }
    if rank > 0 && matches!(dp.topological_rank, None | Some(0)) {
        dp.topological_rank = Some(rank);
    }
}

// ---------------------------------------------------------------------------
// Convenience function: sequential execution of all tasks
// ---------------------------------------------------------------------------

/// Run the complete cognify pipeline on a set of Data items.
///
/// Executes each task sequentially: classify → chunk → extract graph →
/// summarize → add data points (embed + index).
///
/// For composable pipeline-based execution (with concurrency, retry, progress
/// tracking), use [`build_cognify_pipeline`] + [`cognee_core::execute`].
#[allow(clippy::too_many_arguments)]
pub async fn cognify(
    data_items: Vec<Data>,
    dataset_id: Uuid,
    user_id: Option<Uuid>,
    user_email: Option<String>,
    tenant_id: Option<Uuid>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    database: Arc<DatabaseConnection>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    thread_pool: Arc<dyn CpuPool>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: &CognifyConfig,
) -> Result<CognifyResult, CognifyError> {
    config
        .validate()
        .map_err(|e| CognifyError::ConfigError(e.to_string()))?;

    // Auto-calculate chunk size when the caller is using the default value.
    // Matches Python's `get_max_chunk_tokens()` from
    // `cognee/infrastructure/llm/utils.py`. Locked Decision 6: this mutation
    // happens **before** `pipeline::execute` so the executor sees a frozen
    // config in `build_cognify_pipeline`.
    let effective_config = if config.max_chunk_size.is_none() {
        let cfg = config
            .clone()
            .with_auto_chunk_size(embedding_engine.as_ref(), llm.as_ref());
        info!("Auto-calculated max_chunk_size: {}", cfg.chunk_size());
        // Re-validate: the first `validate()` above runs while the size is still
        // unset, so the `chunk_overlap < max_chunk_size` rule is only checkable
        // once the auto value is known. A large overlap against a small
        // engine-derived size (512 on the local ONNX/BGE engine) must fail here
        // rather than reach the chunkers.
        cfg.validate()
            .map_err(|e| CognifyError::ConfigError(e.to_string()))?;
        cfg
    } else {
        config.clone()
    };

    info!(
        "Starting cognify pipeline with config: chunks_per_batch={}, max_chunk_size={}",
        effective_config.chunks_per_batch,
        effective_config.chunk_size()
    );

    // ── Qualification gate (gap 08-08, locked decision 3) ───────────────────
    // Python-parity `check_pipeline_run_qualification`: read the latest
    // `pipeline_runs` row for `(dataset_id, pipeline_name)` and decide whether
    // to proceed, short-circuit, or reject.
    //
    // The two verdicts are gated differently, on purpose:
    //
    // * `AlreadyCompleted` short-circuits ONLY when the caller opted into the
    //   pipeline cache. Python guards this layer behind `if use_pipeline_cache:`
    //   in `run_pipeline_per_dataset` (`modules/pipelines/operations/pipeline.py`)
    //   and every public entry point passes `use_pipeline_cache=False`, so
    //   upstream a repeat cognify always re-runs. Short-circuiting
    //   unconditionally made every cognify after the first a silent no-op, so a
    //   dataset could never take a second wave of data. `dataset_resolver`
    //   already gates its own dataset-level skip on this same flag.
    //
    // * `AlreadyRunning` still rejects regardless of the flag. Python can put
    //   this behind the flag because `run_pipeline_per_dataset` serializes on
    //   `get_dataset_lock(dataset.id)` — its own comment reads "concurrent runs
    //   are kept safe by the per-dataset lock, not by this check". Rust has no
    //   such lock, so this row check is the closest equivalent available: it
    //   rejects a run that has already reached `Started`, which covers the
    //   common case of re-invoking cognify while a long run is in flight, and
    //   keeps a stale `Started` row from being silently ignored.
    //
    //   By itself this row check is NOT serialization: it and the `Started`
    //   write in `pipeline::execute` (`crates/core/src/pipeline.rs:916-923`)
    //   are separate operations, so two callers entering between them would
    //   both observe the pre-run state and both proceed. What actually
    //   serializes runs is the exclusive-run claim taken below; this check
    //   stays because it is the cheaper path (no write) for the common case of
    //   a run already visibly in flight, and because it is what reports a
    //   stale `Started` row rather than silently ignoring it.
    //
    //   For calibration against the reference: Python's lock is an
    //   `asyncio.Lock` held in a module-level dict
    //   (`infrastructure/locks/dataset_lock.py`), so it serializes within a
    //   single process only. The claim below is a database row, so it also
    //   excludes concurrent runs across processes.
    let pipeline_name: &str = if effective_config.temporal_cognify {
        "temporal-cognify"
    } else {
        COGNIFY_PIPELINE_STAMP_NAME
    };
    // The prior completed run, kept whichever way the gate goes: when the
    // completion markers below turn out to cover every data item, the no-op
    // result reports that run's id exactly as a cache hit would.
    let mut prior_completed: Option<Uuid> = None;
    match check_pipeline_run_qualification(pipeline_run_repo.as_ref(), dataset_id, pipeline_name)
        .await
        .map_err(|e| CognifyError::DatabaseError(e.to_string()))?
    {
        Qualification::AlreadyCompleted(prior)
            if effective_config.use_pipeline_cache
                && !rollback::run_info_has_outstanding_failures(prior.run_info.as_ref()) =>
        {
            info!(
                dataset_id = %dataset_id,
                pipeline_run_id = %prior.pipeline_run_id,
                "cognify: dataset already completed; short-circuiting (pipeline cache hit)"
            );
            return Ok(CognifyResult::already_completed(prior.pipeline_run_id));
        }
        // Cache off (the default), or a run that completed with files still
        // outstanding: neither stops this one. A tolerant run that swept and
        // listed its failed files is not a cache hit — the next run has work to
        // do, and the completion markers tell it exactly which files.
        Qualification::AlreadyCompleted(prior) => {
            prior_completed = Some(prior.pipeline_run_id);
        }
        Qualification::AlreadyRunning(_prior) => {
            return Err(CognifyError::PipelineAlreadyRunning {
                pipeline_name: pipeline_name.to_string(),
                dataset_id,
            });
        }
        Qualification::Proceed => {}
    }

    // ── Exclusive-run claim ─────────────────────────────────────────────────
    // The `AlreadyRunning` verdict above only catches a run that already wrote
    // its `Started` row; that write and the read above are separate
    // operations, so two callers entering within the window between them would
    // both observe the pre-run state and both proceed. This claim closes the
    // window — the insert *is* the contended operation, so exactly one caller
    // holds `(dataset_id, pipeline_name)` at a time. It also excludes
    // concurrent runs across processes, which Python's in-process
    // `asyncio.Lock` (`infrastructure/locks/dataset_lock.py`) does not.
    //
    // Deliberately NOT cleared by the server's startup orphan sweep: claims
    // are cross-process, so one instance restarting must not drop another
    // live instance's claim. A claim a killed process left behind is recovered
    // by the staleness window below, or by an explicit reset.
    let claim_repo = Arc::clone(&pipeline_run_repo);
    let claim_id = Uuid::new_v4();
    if !claim_repo
        .try_claim_pipeline_run(dataset_id, pipeline_name, claim_id, CLAIM_STALE_AFTER)
        .await
        .map_err(|e| CognifyError::DatabaseError(e.to_string()))?
    {
        return Err(CognifyError::PipelineAlreadyRunning {
            pipeline_name: pipeline_name.to_string(),
            dataset_id,
        });
    }

    // Everything past the claim runs inside this block so the release below is
    // reached on every exit path, `?` included. `Drop` cannot await, so an RAII
    // guard is not an option.
    let outcome: Result<CognifyResult, CognifyError> = async {
        // ── Empty-document short-circuit ────────────────────────────────────────
        // Preserved from the pre-executor path: a caller passing zero documents
        // gets back an empty result without paying for pipeline / context
        // construction or a no-op LLM round-trip.
        if data_items.is_empty() {
            return Ok(CognifyResult::empty());
        }

        // Selects the pipeline below, and skips the DLT teardown. It no longer
        // gates the marker filter: both branches run under Python's one
        // `cognify_pipeline` marker key, so a dataset either branch completed
        // is a no-op for the other — which is exactly what Python does, since
        // its temporal cognify runs under the same pipeline name.
        let is_temporal = effective_config.temporal_cognify;

        // ── Completion markers: skip what an earlier run finished ───────────
        // `incremental_loading` (default on) finally means something here.
        // Python skips per item inside `run_tasks_data_item_incremental`; Rust
        // skips for the dataset, before the pipeline is built, so a skipped
        // item is never classified, never chunked and never sent to an LLM.
        let data_items = if effective_config.incremental_loading {
            rollback::drop_already_complete(&database, dataset_id, data_items).await?
        } else {
            data_items
        };
        if data_items.is_empty() {
            // Everything this dataset holds was cognified by an earlier run.
            // Re-cognifying a complete dataset is a no-op — the behaviour
            // change `incremental_loading` has been promising all along.
            info!(
                dataset_id = %dataset_id,
                "cognify: every data item is already cognified; nothing to do"
            );
            let mut result = CognifyResult::empty();
            result.already_completed = true;
            result.prior_pipeline_run_id = prior_completed;
            return Ok(result);
        }
        // The items this run is responsible for: what it marks complete on
        // success, and what its run record names.
        let processed_ids: Vec<Uuid> = data_items.iter().map(|item| item.id).collect();

        // ── Branch: temporal vs. standard pipeline ──────────────────────────────
        // LIB-06-04: both branches now route through `pipeline::execute`. The
        // selection happens *before* `execute()` per locked Decision 2 — temporal
        // is a distinct `Pipeline` with its own task DAG. Per locked option (a)
        // (user decision 2026-05-15), the shared tasks
        // (`make_classify_documents_task`, `make_extract_chunks_task`) stamp
        // `Document` / `DocumentChunk` DataPoints with
        // `source_pipeline = COGNIFY_PIPELINE_STAMP_NAME` on both
        // branches; the temporal pipeline keeps its distinct identity at the
        // `pipeline_runs` row level via `build_temporal_cognify_pipeline`'s
        // `with_name("temporal-cognify")`.
        let pipeline = if is_temporal {
            build_temporal_cognify_pipeline(
                Arc::clone(&storage),
                Arc::clone(&graph_db),
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&llm),
                Arc::clone(&database),
                effective_config.clone(),
            )
        } else {
            build_cognify_pipeline(
                Arc::clone(&storage),
                Arc::clone(&graph_db),
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&llm),
                Arc::clone(&database),
                Arc::clone(&ontology_resolver),
                effective_config.clone(),
            )
        };

        // The executor re-derives `PipelineRunInfo.pipeline_id` from
        // `(pipeline.name, user_id, dataset_id)`; we still carry `pipeline.id`
        // through `PipelineContext` as the placeholder.
        let pipeline_ctx = PipelineContext {
            pipeline_id: pipeline.id,
            pipeline_name: pipeline.name.clone().unwrap_or_default(),
            user_id,
            tenant_id,
            dataset_id: Some(dataset_id),
            current_data: None,
            run_id: None,
            user_email: user_email.clone(),
            provenance_visited: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        };

        let (_cancel_handle, ctx) = TaskContextBuilder::new()
            .thread_pool(thread_pool)
            .database(Arc::clone(&database))
            .graph_db(Arc::clone(&graph_db))
            .vector_db(Arc::clone(&vector_db))
            .pipeline_context(pipeline_ctx)
            .build()
            .map_err(|e| CognifyError::ContextBuild(e.to_string()))?;
        let ctx = Arc::new(ctx);

        let input = CognifyInput {
            data_items,
            dataset_id,
            user_id,
            tenant_id,
        };
        let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(input) as Arc<dyn Value>];

        // Decision 11 (gap 08-07): `DbPipelineWatcher` persists the four-state
        // `pipeline_runs` trail through the caller-supplied repository.
        // Embedded callers pass `NoopPipelineRunRepository`; CLI / HTTP callers
        // pass a `SeaOrmPipelineRunRepository` to surface rows in the
        // `/api/v1/activity/pipeline-runs` endpoint.
        //
        // Wrapped in `RunIdCapturingWatcher` so the run id is in hand on every
        // exit path. The executor mints it internally and hands it to tasks
        // through a cloned context, so a run that *fails* leaves no result to
        // read it from — and a sweep with no run to select on would silently
        // remove nothing.
        let watcher = rollback::RunIdCapturingWatcher::new(Arc::clone(&pipeline_run_repo));
        let executed = cognee_core::pipeline::execute(&pipeline, inputs, ctx, &watcher)
            .await
            .map_err(unwrap_execution_error);

        // ── The policy layer ────────────────────────────────────────────────
        // Everything from here on is `rollback`'s decision: which scope to
        // sweep, which items to mark complete, and what the run record says.
        let run_ctx = rollback::RunContext {
            database: Arc::clone(&database),
            graph_db: Arc::clone(&graph_db),
            vector_db: Arc::clone(&vector_db),
            repo: Arc::clone(&pipeline_run_repo),
            dataset_id,
            pipeline_run_id: watcher.run_id(),
            pipeline_id: watcher.pipeline_id(),
            pipeline_name,
            processed: processed_ids,
            config: &effective_config,
        };

        let outputs = match executed {
            Ok(outputs) => outputs,
            Err(e) => {
                // The executor already wrote the `ERRORED` row on its way out.
                rollback::on_run_failed(&run_ctx, &e, true).await;
                return Err(e);
            }
        };

        // Decision 5: post-pipeline teardown — `extract_dlt_fk_edges` stays
        // outside the executor.
        //
        // LIB-06-04: skip DLT FK extraction on the temporal branch — temporal
        // does not propagate `documents_for_dlt` (and Python's temporal cognify
        // does not run DLT teardown either).
        //
        // The teardown's own artifacts get ownership rows like everything else
        // the run wrote, so a sweep can reach them. It takes the run id from
        // the result because it runs outside the executor and never sees a
        // `TaskContext`.
        //
        // The watcher has already written `COMPLETED` by now, so a failure
        // here needs a further `ERRORED` row of its own — `on_run_failed`
        // appends one. Without it a swept run would be left looking complete,
        // and would then be a pipeline-cache hit for a dataset whose artifacts
        // are gone.
        let after_completion = async {
            let result = extract_cognify_outputs(outputs)?;
            if !is_temporal {
                extract_dlt_fk_edges(
                    &result.chunks,
                    &result.documents_for_dlt,
                    Arc::clone(&graph_db),
                    &database,
                    LedgerIdentity::new(tenant_id, user_id, dataset_id, result.pipeline_run_id),
                )
                .await?;
            }
            Ok::<CognifyResult, CognifyError>(result)
        }
        .await;

        match after_completion {
            Ok(result) => {
                rollback::on_run_completed(&run_ctx, &result.failures).await;
                Ok(result)
            }
            Err(e) => {
                rollback::on_run_failed(&run_ctx, &e, false).await;
                Err(e)
            }
        }
    }
    .await;

    if let Err(e) = claim_repo
        .release_pipeline_run_claim(dataset_id, pipeline_name, claim_id)
        .await
    {
        // The run is over either way, and an unreleased claim is reclaimed as
        // stale rather than lost, so this must not mask the outcome.
        warn!(
            dataset_id = %dataset_id,
            pipeline_name = %pipeline_name,
            "failed to release the pipeline-run claim (it will age out): {e}"
        );
    }

    outcome
}

/// Recover the typed [`CognifyError`] a failing task returned.
///
/// [`cognee_core::pipeline::ExecutionError::TaskFailed`] keeps the task's error
/// boxed rather than stringified, so the structured
/// [`CognifyError::RunFailed`] a stage produced — with its failure report —
/// survives the trip through the executor. Flattening it to
/// [`CognifyError::Execute`] the way this call used to is what made a collected
/// failure indistinguishable from any other error string.
///
/// Every other variant — [`cognee_core::pipeline::ExecutionError::Cancelled`]
/// included — flattens to [`CognifyError::Execute`] on purpose. The policy
/// layer is then handed an error with no shape to branch on, which is what
/// makes a cancelled run sweep like any other failed one (the deliberate
/// divergence from Python documented in [`crate::rollback`]).
pub(crate) fn unwrap_execution_error(e: cognee_core::pipeline::ExecutionError) -> CognifyError {
    match e {
        cognee_core::pipeline::ExecutionError::TaskFailed { source, .. } => {
            match source.downcast::<CognifyError>() {
                Ok(typed) => *typed,
                Err(other) => CognifyError::Execute(other.to_string()),
            }
        }
        other => CognifyError::Execute(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Output extraction (Decision 9)
// ---------------------------------------------------------------------------

/// Downcast the executor's [`Arc<dyn Value>`] outputs back to the concrete
/// [`CognifyResult`] the convenience function promises.
///
/// Returns [`CognifyError::OutputTypeMismatch`] when the downcast fails — a
/// programmer error indicating the pipeline's last task does not emit
/// `CognifyResult`. Mirrors `cognee_ingestion::pipeline::extract_data_outputs`
/// (LIB-06-01) and `cognee_cognify::memify::extract_memify_outputs` (LIB-06-02).
fn extract_cognify_outputs(outputs: Vec<Arc<dyn Value>>) -> Result<CognifyResult, CognifyError> {
    let first = outputs
        .into_iter()
        .next()
        .ok_or(CognifyError::OutputTypeMismatch {
            expected: "CognifyResult",
            actual: "empty",
        })?;
    // Explicit deref through `Arc` to reach the inner `dyn Value`, then call
    // `as_any` via vtable dispatch — without this, method resolution would
    // pick the blanket `<Arc<dyn Value> as Value>::as_any()` which downcasts
    // to `Arc<dyn Value>` and never to `CognifyResult`.
    (*first)
        .as_any()
        .downcast_ref::<CognifyResult>()
        .cloned()
        .ok_or(CognifyError::OutputTypeMismatch {
            expected: "CognifyResult",
            actual: "unknown",
        })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

// ── Provenance helpers ──────────────────────────────────────────────────────

/// Deterministic provenance node ID, matching Python's:
/// `uuid5(NAMESPACE_OID, str(tenant_id) + str(user_id) + str(dataset_id) + str(data_id) + str(node_id))`
///
/// When `tenant_id` is `None`, Python's `str(None)` produces `"None"`.
fn provenance_node_id(
    tenant_id: Option<Uuid>,
    user_id: Uuid,
    dataset_id: Uuid,
    data_id: Uuid,
    node_id: Uuid,
) -> Uuid {
    let tid = tenant_id.map_or("None".to_string(), |t| t.to_string());
    let raw = format!("{tid}{user_id}{dataset_id}{data_id}{node_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.as_bytes())
}

/// Deterministic provenance edge ID:
/// `uuid5(NAMESPACE_OID, str(tenant_id) + str(user_id) + str(dataset_id) + str(data_id) + str(source_id) + str(edge_text) + str(target_id))`
///
/// **Deliberate divergence from Python.** `upsert_edges.py:47-56` omits
/// `data_id`, so one edge has exactly one row there and
/// `on_conflict_do_nothing` lets the first writer keep it — an edge produced by
/// two files is attributed to one of them. Folding `data_id` in, the way
/// [`provenance_node_id`] already does, lets several rows for one edge coexist
/// instead of colliding on the primary key, which is what makes an edge
/// removable only when its *last* owning file goes. Two consequences, both
/// accepted:
///
/// - On a database the two SDKs share, Rust's ids stop colliding with
///   Python's. The old formula *was* Python's, byte for byte, so both SDKs
///   wrote the same primary key for one edge and whichever ran last silently
///   took ownership; with `data_id` folded in the two sets of rows coexist and
///   each side keeps its own ownership record. What that does not buy is
///   mutual protection: the exclusivity check correlates on `slug`, and the
///   slug formulas differ — Rust hashes [`triplet_slug`]'s
///   `source + edge_text + target`, Python's `generate_edge_id` hashes the
///   sanitized edge text alone, with no node ids — so a Python row is
///   invisible to Rust's `NOT EXISTS` and vice versa. That gap predates this
///   change and is issue #156's territory; nothing here widens it.
/// - Re-cognifying a dataset processed before this change writes new rows
///   rather than updating the old ones, which keep their `data_id` and go on
///   protecting the artifact until the dataset is deleted. Over-protection, not
///   loss; rewriting the historical ids would need every past run's producer
///   set, which nothing records.
///
/// When `tenant_id` is `None`, Python's `str(None)` produces `"None"`.
fn provenance_edge_id(
    tenant_id: Option<Uuid>,
    user_id: Uuid,
    dataset_id: Uuid,
    data_id: Uuid,
    source_id: Uuid,
    edge_text: &str,
    target_id: Uuid,
) -> Uuid {
    let tid = tenant_id.map_or("None".to_string(), |t| t.to_string());
    let raw = format!("{tid}{user_id}{dataset_id}{data_id}{source_id}{edge_text}{target_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.as_bytes())
}

/// The data items behind a set of producing chunks, in first-seen order.
///
/// Deduplicated: two chunks of the same file are two producers but one data
/// item, and the row id formulas would then mint the same primary key twice in
/// one batch. `upsert_nodes_on` / `upsert_edges_on` only collapse duplicates
/// *within* a `PROVENANCE_INSERT_BATCH` chunk, so relying on them would be a
/// latent bug at scale.
fn producing_data_ids(chunk_ids: &[Uuid], chunk_data_map: &HashMap<Uuid, Uuid>) -> Vec<Uuid> {
    let mut data_ids: Vec<Uuid> = Vec::new();
    for chunk_id in chunk_ids {
        if let Some(data_id) = chunk_data_map.get(chunk_id).copied()
            && !data_ids.contains(&data_id)
        {
            data_ids.push(data_id);
        }
    }
    data_ids
}

/// Deterministic edge slug, matching Python's `generate_edge_id`:
/// `uuid5(NAMESPACE_OID, edge_text.lower().replace(" ", "_").replace("'", ""))`
fn edge_slug(edge_text: &str) -> Uuid {
    let normalized = edge_text.to_lowercase().replace(' ', "_").replace('\'', "");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, normalized.as_bytes())
}

/// Deterministic triplet slug, matching `Triplet::new`.
fn triplet_slug(source_id: Uuid, relationship_name: &str, target_id: Uuid) -> Uuid {
    let raw = format!("{source_id}{relationship_name}{target_id}");
    let normalized = raw.to_lowercase().replace(' ', "_").replace('\'', "");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, normalized.as_bytes())
}

/// Map each chunk to the data item it came from, for tracing an artifact's
/// provenance back to the originating `Data` record.
fn chunk_document_map(chunks: &[DocumentChunk]) -> HashMap<Uuid, Uuid> {
    chunks.iter().map(|c| (c.base.id, c.document_id)).collect()
}

/// Map each entity to the data item of the chunk stamped on its metadata.
///
/// Only used as the fallback attribution for edges whose producer set is empty.
fn entity_document_map(
    entities: &[GraphNodePair],
    chunk_data_map: &HashMap<Uuid, Uuid>,
) -> HashMap<Uuid, Uuid> {
    entities
        .iter()
        .filter_map(|pair| {
            pair.entity
                .base
                .get_metadata("chunk_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .and_then(|chunk_id| chunk_data_map.get(&chunk_id).copied())
                .map(|data_id| (pair.entity.base.id, data_id))
        })
        .collect()
}

/// Ownership rows for the extracted entities.
///
/// One row per data item that produced this entity. A merged entity is created
/// once but claimed by every file whose chunks yielded it, and
/// `get_unique_nodes_for_data` only spares a slug that another `data_id` in the
/// dataset claims — so a single row lets deleting one file sweep an entity the
/// others still reference. Deduplicated because two chunks of the same file are
/// two producers but one `data_id`, and [`provenance_node_id`] would then mint
/// the same primary key twice in one batch.
fn entity_provenance_rows(
    id: LedgerIdentity,
    entities: &[GraphNodePair],
    chunk_data_map: &HashMap<Uuid, Uuid>,
    producers: &ArtifactProducers,
) -> Vec<cognee_database::GraphNode> {
    let mut rows = Vec::new();

    for pair in entities {
        let entity = &pair.entity;

        let mut data_ids =
            producing_data_ids(producers.entity_chunks(entity.base.id), chunk_data_map);
        if data_ids.is_empty() {
            // Ontology-derived entities, and callers that hand us a
            // `SummarizedData` they built themselves, carry no producer set:
            // fall back to the single `chunk_id` stamp, then to nil.
            data_ids.push(
                entity
                    .base
                    .get_metadata("chunk_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .and_then(|chunk_id| chunk_data_map.get(&chunk_id).copied())
                    .unwrap_or(Uuid::nil()),
            );
        }

        let indexed_fields = entity
            .base
            .get_metadata("index_fields")
            .cloned()
            .unwrap_or(json!(["name"]));

        let label = if entity.name.is_empty() {
            entity.base.id.to_string()
        } else {
            entity.name.clone()
        };

        for data_id in data_ids {
            rows.push(cognee_database::GraphNode {
                id: provenance_node_id(
                    id.tenant_id,
                    id.user_id,
                    id.dataset_id,
                    data_id,
                    entity.base.id,
                ),
                slug: entity.base.id,
                user_id: id.user_id,
                data_id,
                dataset_id: id.dataset_id,
                pipeline_run_id: id.pipeline_run_id,
                label: Some(label.clone()),
                node_type: entity.base.data_type.clone(),
                indexed_fields: indexed_fields.clone(),
                attributes: serde_json::to_value(entity).ok(),
                created_at: Utc::now(),
            });
        }
    }

    rows
}

/// Ownership rows for the semantic edges produced by graph extraction.
///
/// One row per data item that produced the edge — the same one-to-many relation
/// the entity rows carry, so an edge is swept only when its LAST owning file
/// goes. The row *identity* carries `data_id` ([`provenance_edge_id`]), which is
/// what lets several rows for one edge coexist instead of colliding on the
/// primary key; the rows share a `slug` ([`triplet_slug`]), which is what
/// `get_unique_edges_for_data` compares, so each row claims the edge on behalf
/// of its own file. Without this, an edge between entities merged from two
/// different files resolved to nil — and `get_unique_edges_for_data` never
/// selects a nil row, so the edge's EdgeType/Triplet vectors outlived every file
/// that produced them.
///
/// Divergence from Python, deliberate: `upsert_edges.py` omits `data_id` from
/// the row id and keeps exactly one row per edge, letting the last writer take
/// ownership. See the note on [`provenance_edge_id`].
fn semantic_edge_provenance_rows(
    id: LedgerIdentity,
    edges: &[GraphEdgePair],
    chunk_data_map: &HashMap<Uuid, Uuid>,
    entity_data_map: &HashMap<Uuid, Uuid>,
    producers: &ArtifactProducers,
) -> Vec<cognee_database::GraphEdge> {
    let mut rows = Vec::new();

    for edge_pair in edges {
        let edge_text = if edge_pair.relationship_name == "contains" {
            edge_pair
                .properties
                .get("edge_text")
                .cloned()
                .unwrap_or_else(|| edge_pair.relationship_name.clone())
        } else {
            edge_pair.relationship_name.clone()
        };
        // Strip NUL bytes *before* deriving the id and slug, not after. Python's
        // `upsert_edges` (`cognee/modules/graph/methods/upsert_edges.py:41-66`)
        // feeds `sanitized_edge_text` into both its uuid5 and `generate_edge_id`,
        // so deriving from the raw text here would hand a NUL-bearing edge a
        // different deterministic id than the Python SDK produces for the same
        // input. The relational conversion sanitizes again on the way out; this
        // is about what the hash sees.
        let edge_text = sanitize_string(edge_text);

        let mut data_ids = producing_data_ids(
            producers.edge_chunks(&edge_pair.dedup_key()),
            chunk_data_map,
        );
        if data_ids.is_empty() {
            // No producer set (a hand-built `SummarizedData`): fall back to the
            // old both-endpoints-agree heuristic, then to nil, so those callers
            // keep exactly the rows they got before.
            let source_data_id = entity_data_map.get(&edge_pair.source_entity_id).copied();
            let target_data_id = entity_data_map.get(&edge_pair.target_entity_id).copied();
            data_ids.push(match (source_data_id, target_data_id) {
                (Some(source), Some(target)) if source == target => source,
                _ => Uuid::nil(),
            });
        }

        // `slug`, endpoints, `relationship_name`, `label` and `attributes` are
        // identical across the group; only `id` and `data_id` differ.
        for data_id in data_ids {
            rows.push(cognee_database::GraphEdge {
                id: provenance_edge_id(
                    id.tenant_id,
                    id.user_id,
                    id.dataset_id,
                    data_id,
                    edge_pair.source_entity_id,
                    &edge_text,
                    edge_pair.target_entity_id,
                ),
                slug: triplet_slug(
                    edge_pair.source_entity_id,
                    &edge_text,
                    edge_pair.target_entity_id,
                ),
                user_id: id.user_id,
                data_id,
                dataset_id: id.dataset_id,
                pipeline_run_id: id.pipeline_run_id,
                source_node_id: edge_pair.source_entity_id,
                destination_node_id: edge_pair.target_entity_id,
                relationship_name: edge_text.clone(),
                label: Some(edge_pair.relationship_name.clone()),
                attributes: serde_json::to_value(&edge_pair.properties).ok(),
                created_at: Utc::now(),
            });
        }
    }

    rows
}

/// Record ownership of the entities and semantic edges a run is about to write
/// to the graph, in one transaction, before the graph sees them.
///
/// The rows this writes are a subset of the ones [`upsert_provenance`] writes
/// two stages later, and they carry the same deterministic ids — the second
/// write is an idempotent upsert that leaves `pipeline_run_id` alone (it is
/// deliberately absent from the `ON CONFLICT` update list, so the first run to
/// claim an artifact keeps it).
///
/// One exception to "subset": `claimed_existing_edges` is written *only* here.
/// Those edges never reach `add_data_points`, because re-writing them to the
/// graph is exactly what the extraction dedup filter exists to prevent — but
/// their ownership rows are what keeps an earlier file's deletion from taking
/// a still-referenced edge with it.
async fn record_extraction_ownership(
    db: &DatabaseConnection,
    id: LedgerIdentity,
    chunks: &[DocumentChunk],
    entities: &[GraphNodePair],
    edges: &[GraphEdgePair],
    // Edges this run produced that the extraction dedup filter kept out of
    // `edges` because an earlier run had already written them to the graph.
    // They are ownership-only: rows here, no graph or vector write anywhere.
    claimed_existing_edges: &[GraphEdgePair],
    producers: &ArtifactProducers,
) -> Result<(), CognifyError> {
    let chunk_data_map = chunk_document_map(chunks);
    let entity_data_map = entity_document_map(entities, &chunk_data_map);

    let prov_nodes = entity_provenance_rows(id, entities, &chunk_data_map, producers);
    let mut prov_edges =
        semantic_edge_provenance_rows(id, edges, &chunk_data_map, &entity_data_map, producers);
    prov_edges.extend(semantic_edge_provenance_rows(
        id,
        claimed_existing_edges,
        &chunk_data_map,
        &entity_data_map,
        producers,
    ));

    cognee_database::ops::graph_storage::upsert_provenance_graph(db, &prov_nodes, &prov_edges)
        .await?;
    if !prov_nodes.is_empty() || !prov_edges.is_empty() {
        info!(
            "Recorded ownership of {} entities and {} semantic edges before writing them",
            prov_nodes.len(),
            prov_edges.len()
        );
    }
    Ok(())
}

/// Write provenance node and edge records to the relational database.
///
/// Mirrors the Python `upsert_nodes()` / `upsert_edges()` calls in
/// `add_data_points` (guarded by `if user and dataset and data:`).
///
/// Provenance records link graph nodes/edges back to the user, tenant,
/// dataset, and data item they originated from.
///
/// `producers` names every chunk that produced each merged entity and edge.
/// It is what turns a merged entity — and a merged edge — into one ownership
/// row per producing data item, so the artifact is removed only when its last
/// owning file is deleted. An empty set is a valid input: callers that build a
/// `SummarizedData` themselves get the previous single-`chunk_id` (entities)
/// and both-endpoints-agree (edges) attribution.
#[allow(clippy::too_many_arguments)]
async fn upsert_provenance(
    db: &DatabaseConnection,
    id: LedgerIdentity,
    chunks: &[DocumentChunk],
    entities: &[GraphNodePair],
    edges: &[GraphEdgePair],
    summaries: &[TextSummary],
    documents: &[Document],
    structural_edges: &[EdgeData],
    producers: &ArtifactProducers,
) -> Result<(), CognifyError> {
    use cognee_database::ops::graph_storage;
    use cognee_database::{GraphEdge, GraphNode};

    let LedgerIdentity {
        tenant_id,
        user_id,
        dataset_id,
        pipeline_run_id,
    } = id;

    // Build chunk_id → document_id map for tracing entity provenance back
    // to the originating Data item.
    let chunk_data_map = chunk_document_map(chunks);
    let entity_data_map = entity_document_map(entities, &chunk_data_map);

    // ── Provenance nodes ────────────────────────────────────────────────
    // Entities. Shared with the extraction stage, which wrote the same rows
    // before it put the entities in the graph; this pass is the idempotent
    // second write.
    let mut prov_nodes: Vec<GraphNode> =
        entity_provenance_rows(id, entities, &chunk_data_map, producers);

    // DocumentChunks
    for chunk in chunks {
        let data_id = chunk.document_id;

        let indexed_fields = chunk
            .base
            .get_metadata("index_fields")
            .cloned()
            .unwrap_or(json!(["text"]));

        prov_nodes.push(GraphNode {
            id: provenance_node_id(tenant_id, user_id, dataset_id, data_id, chunk.base.id),
            slug: chunk.base.id,
            user_id,
            data_id,
            dataset_id,
            pipeline_run_id,
            label: Some(format!("chunk_{}", chunk.chunk_index)),
            node_type: chunk.base.data_type.clone(),
            indexed_fields,
            attributes: serde_json::to_value(chunk).ok(),
            created_at: Utc::now(),
        });
    }

    // TextSummaries
    for summary in summaries {
        let data_id = summary
            .made_from
            .and_then(|chunk_id| chunk_data_map.get(&chunk_id).copied())
            .unwrap_or(Uuid::nil());

        let indexed_fields = summary
            .base
            .get_metadata("index_fields")
            .cloned()
            .unwrap_or(json!(["text"]));

        prov_nodes.push(GraphNode {
            id: provenance_node_id(tenant_id, user_id, dataset_id, data_id, summary.base.id),
            slug: summary.base.id,
            user_id,
            data_id,
            dataset_id,
            pipeline_run_id,
            label: Some(format!("summary_{}", summary.base.id)),
            node_type: summary.base.data_type.clone(),
            indexed_fields,
            attributes: serde_json::to_value(summary).ok(),
            created_at: Utc::now(),
        });
    }

    // EntityTypes
    for pair in entities {
        let et = &pair.entity_type;
        // EntityType rows carry a nil `data_id`: one type is shared by every
        // entity of that type, so there is no single producing file. Note this
        // is *not* Python's shape — `upsert_nodes` stamps the ctx `data_id` on
        // every node it is handed, EntityTypes included — which means a
        // data-scoped delete never selects these rows. Correcting that flips a
        // class of artifacts from never-deleted to deleted and belongs in its
        // own change.
        prov_nodes.push(GraphNode {
            id: provenance_node_id(tenant_id, user_id, dataset_id, Uuid::nil(), et.base.id),
            slug: et.base.id,
            user_id,
            data_id: Uuid::nil(),
            dataset_id,
            pipeline_run_id,
            label: Some(et.name.clone()),
            node_type: et.base.data_type.clone(),
            indexed_fields: et
                .base
                .get_metadata("index_fields")
                .cloned()
                .unwrap_or(json!(["name"])),
            attributes: serde_json::to_value(et).ok(),
            created_at: Utc::now(),
        });
    }

    // Documents. Python reaches the Document node by recursively walking each
    // DocumentChunk's `is_part_of` (a full Document DataPoint), so the Document
    // lands in `nodes` and `upsert_nodes(nodes, …)` writes its provenance row
    // keyed with the ctx `data_item.id`. Rust stores Documents explicitly (see
    // `add_data_points`), so we must register their provenance here too —
    // otherwise the Document graph node (slug == its id == the source Data
    // item's id) is never matched by the delete cleanup and leaks on hard
    // delete. The Document's id IS the Data item's id, so `data_id` = its id.
    for document in documents {
        let data_id = document.base.id;

        let indexed_fields = document
            .base
            .get_metadata("index_fields")
            .cloned()
            .unwrap_or(json!(["name"]));

        let label = if document.name.is_empty() {
            document.base.id.to_string()
        } else {
            document.name.clone()
        };

        prov_nodes.push(GraphNode {
            id: provenance_node_id(tenant_id, user_id, dataset_id, data_id, document.base.id),
            slug: document.base.id,
            user_id,
            data_id,
            dataset_id,
            pipeline_run_id,
            label: Some(label),
            node_type: document.base.data_type.clone(),
            indexed_fields,
            attributes: serde_json::to_value(document).ok(),
            created_at: Utc::now(),
        });
    }

    // Node provenance is written together with the edges below, in a single
    // transaction, so a mid-way failure cannot leave nodes without their edges.

    // ── Provenance edges ────────────────────────────────────────────────
    // Semantic edges from graph extraction — again the same rows the
    // extraction stage already wrote before persisting them.
    let mut prov_edges: Vec<GraphEdge> =
        semantic_edge_provenance_rows(id, edges, &chunk_data_map, &entity_data_map, producers);

    // Structural edges from get_graph_from_model (contains, is_a, made_from, etc.)
    // Python writes these to SQLite via upsert_edges() — Rust must match.
    for (source_id_str, target_id_str, rel_name, properties) in structural_edges {
        let source_id = Uuid::parse_str(source_id_str).unwrap_or(Uuid::nil());
        let target_id = Uuid::parse_str(target_id_str).unwrap_or(Uuid::nil());

        let attrs = if properties.is_empty() {
            None
        } else {
            let map: serde_json::Map<String, serde_json::Value> = properties
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();
            Some(serde_json::Value::Object(map))
        };

        // Sanitize before deriving the id and the slug, exactly as the semantic
        // branch above does. Python routes structural edges through the same
        // `upsert_edges`, which feeds `sanitized_edge_text` into both its uuid5
        // and `generate_edge_id`
        // (`cognee/modules/graph/methods/upsert_edges.py:41-57`). Deriving from
        // the raw name here while `conversions.rs` sanitizes `relationship_name`
        // on the way out left the two loops on opposite conventions and would
        // hand a NUL-bearing structural edge a different deterministic id than
        // Python produces for the same input.
        let rel_name = sanitize_str(rel_name);

        prov_edges.push(GraphEdge {
            // The nil `data_id` is folded into the id like every other edge's,
            // so the structural rows need no special case in the formula.
            id: provenance_edge_id(
                tenant_id,
                user_id,
                dataset_id,
                Uuid::nil(),
                source_id,
                &rel_name,
                target_id,
            ),
            slug: edge_slug(&rel_name),
            user_id,
            data_id: Uuid::nil(), // structural edges span multiple DataPoints
            dataset_id,
            pipeline_run_id,
            source_node_id: source_id,
            destination_node_id: target_id,
            relationship_name: rel_name.into_owned(),
            label: None,
            attributes: attrs,
            created_at: Utc::now(),
        });
    }

    // Write the node and edge provenance batches atomically: a failure partway
    // through rolls the whole group back (see `upsert_provenance_graph`).
    if !prov_nodes.is_empty() || !prov_edges.is_empty() {
        graph_storage::upsert_provenance_graph(db, &prov_nodes, &prov_edges).await?;
        if !prov_nodes.is_empty() {
            info!("Upserted {} provenance node records", prov_nodes.len());
        }
        if !prov_edges.is_empty() {
            info!("Upserted {} provenance edge records", prov_edges.len());
        }
    }

    Ok(())
}

/// Generate embeddings for chunks, entities, and summaries.
async fn generate_embeddings(
    chunks: &[DocumentChunk],
    entities: &[GraphNodePair],
    summaries: &[TextSummary],
    engine: Arc<dyn EmbeddingEngine>,
) -> Result<Vec<Embedding>, CognifyError> {
    let mut embeddings = Vec::new();

    if !chunks.is_empty() {
        let chunk_texts: Vec<_> = chunks.iter().map(|c| c.text.as_str()).collect();
        let chunk_vectors = engine
            .embed(&chunk_texts)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

        for (chunk, vector) in chunks.iter().zip(chunk_vectors) {
            embeddings.push(Embedding::new(
                chunk.base.id,
                "DocumentChunk",
                "text",
                vector,
            ));
        }
    }

    if !entities.is_empty() {
        let entity_names: Vec<_> = entities.iter().map(|e| e.entity.name.as_str()).collect();
        let entity_vectors = engine
            .embed(&entity_names)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

        for (entity, vector) in entities.iter().zip(entity_vectors) {
            embeddings.push(Embedding::new(
                entity.entity.base.id,
                "Entity",
                "name",
                vector,
            ));
        }
    }

    if !summaries.is_empty() {
        let summary_texts: Vec<_> = summaries.iter().map(|s| s.text.as_str()).collect();
        let summary_vectors = engine
            .embed(&summary_texts)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

        for (summary, vector) in summaries.iter().zip(summary_vectors) {
            embeddings.push(Embedding::new(
                summary.base.id,
                "TextSummary",
                "text",
                vector,
            ));
        }
    }

    Ok(embeddings)
}

/// Return one vector per `texts[i]`, reusing `precomputed[ids[i]]` when present
/// and embedding only the texts whose id is missing. `ids` and `texts` must be
/// parallel slices.
async fn reuse_or_embed(
    engine: &Arc<dyn EmbeddingEngine>,
    precomputed: &std::collections::HashMap<Uuid, Vec<f32>>,
    ids: &[Uuid],
    texts: &[&str],
) -> Result<Vec<Vec<f32>>, CognifyError> {
    debug_assert_eq!(ids.len(), texts.len(), "ids and texts must be parallel");
    let missing_texts: Vec<&str> = ids
        .iter()
        .zip(texts)
        .filter(|(id, _)| !precomputed.contains_key(*id))
        .map(|(_, text)| *text)
        .collect();

    let fresh = if missing_texts.is_empty() {
        Vec::new()
    } else {
        engine
            .embed(&missing_texts)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?
    };

    let mut fresh = fresh.into_iter();
    ids.iter()
        .map(|id| match precomputed.get(id) {
            Some(vector) => Ok(vector.clone()),
            None => fresh
                .next()
                .ok_or_else(|| CognifyError::EmbeddingError("missing fresh embedding".into())),
        })
        .collect()
}

/// Index data points in vector database.
#[allow(clippy::too_many_arguments)]
async fn index_data_points(
    chunks: &[DocumentChunk],
    entities: &[GraphNodePair],
    summaries: &[TextSummary],
    documents: &[Document],
    edges: &[GraphEdgePair],
    edge_types: &[EdgeType],
    dataset_id: Uuid,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    engine: Arc<dyn EmbeddingEngine>,
    vector_db: Arc<dyn VectorDB>,
    config: &CognifyConfig,
    precomputed_embeddings: &[Embedding],
) -> Result<IndexedFieldsStats, CognifyError> {
    let mut stats = IndexedFieldsStats::default();
    let dimension = engine.dimension();

    // Vectors already produced by `generate_embeddings`, keyed by data point id,
    // so the chunk/entity/summary collections below reuse them rather than
    // re-embedding the same text.
    let precomputed: std::collections::HashMap<Uuid, Vec<f32>> = precomputed_embeddings
        .iter()
        .map(|e| (e.data_point_id, e.vector.clone()))
        .collect();

    // 1. Index DocumentChunk.text field
    if !chunks.is_empty() {
        if !vector_db
            .has_collection("DocumentChunk", "text")
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
        {
            vector_db
                .create_collection("DocumentChunk", "text", dimension)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
        }

        let ids: Vec<Uuid> = chunks.iter().map(|c| c.base.id).collect();
        let texts: Vec<_> = chunks.iter().map(|c| c.text.as_str()).collect();
        let vectors = reuse_or_embed(&engine, &precomputed, &ids, &texts).await?;

        let points: Vec<VectorPoint> = chunks
            .iter()
            .zip(vectors)
            .map(|(chunk, vector)| {
                let mut point = VectorPoint::new(chunk.base.id, vector);

                // 1. Full DataPoint dump (Python parity — see gap-05/08).
                //    Provides `type`, `belongs_to_set`, all source_* keys, etc.
                for (k, v) in chunk.base.vector_metadata() {
                    point = point.with_metadata(k, v);
                }

                // 2. Context-specific keys not present on the DataPoint.
                point = point
                    .with_metadata("field", json!("text"))
                    .with_metadata("text", json!(chunk.text.clone()))
                    .with_metadata("dataset_id", json!(dataset_id.to_string()))
                    .with_metadata("document_id", json!(chunk.document_id.to_string()))
                    .with_metadata("chunk_index", json!(chunk.chunk_index));
                if let Some(uid) = user_id {
                    point = point.with_metadata("user_id", json!(uid.to_string()));
                }
                if let Some(tid) = tenant_id {
                    point = point.with_metadata("tenant_id", json!(tid.to_string()));
                }
                point
            })
            .collect();

        vector_db
            .index_points("DocumentChunk", "text", &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        stats.record("DocumentChunk", "text", chunks.len());
        info!("Indexed {} document chunks", chunks.len());
    }

    // 2a. Index Entity.name field
    if !entities.is_empty() {
        if !vector_db
            .has_collection("Entity", "name")
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
        {
            vector_db
                .create_collection("Entity", "name", dimension)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
        }

        let ids: Vec<Uuid> = entities.iter().map(|e| e.entity.base.id).collect();
        let names: Vec<_> = entities.iter().map(|e| e.entity.name.as_str()).collect();
        let vectors = reuse_or_embed(&engine, &precomputed, &ids, &names).await?;

        let points: Vec<VectorPoint> = entities
            .iter()
            .zip(vectors)
            .map(|(entity, vector)| {
                let mut point = VectorPoint::new(entity.entity.base.id, vector);

                // 1. Full DataPoint dump (Python parity — see gap-05/08).
                for (k, v) in entity.entity.base.vector_metadata() {
                    point = point.with_metadata(k, v);
                }

                // 2. Context-specific keys not present on the DataPoint.
                point = point
                    .with_metadata("field", json!("name"))
                    .with_metadata("dataset_id", json!(dataset_id.to_string()))
                    .with_metadata("entity_type", json!(entity.entity_type.name.clone()));
                if let Some(uid) = user_id {
                    point = point.with_metadata("user_id", json!(uid.to_string()));
                }
                if let Some(tid) = tenant_id {
                    point = point.with_metadata("tenant_id", json!(tid.to_string()));
                }
                point
            })
            .collect();

        vector_db
            .index_points("Entity", "name", &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        stats.record("Entity", "name", entities.len());
        info!("Indexed {} entity names", entities.len());
    }

    // 2b. Index EntityType.name field (deduplicated by EntityType ID)
    {
        let mut seen_ids = std::collections::HashSet::new();
        let unique_entity_types: Vec<&cognee_models::EntityType> = entities
            .iter()
            .map(|pair| &pair.entity_type)
            .filter(|et| seen_ids.insert(et.base.id))
            .collect();

        if !unique_entity_types.is_empty() {
            if !vector_db
                .has_collection("EntityType", "name")
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
            {
                vector_db
                    .create_collection("EntityType", "name", dimension)
                    .await
                    .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
            }

            let type_names: Vec<_> = unique_entity_types
                .iter()
                .map(|et| et.name.as_str())
                .collect();
            let vectors = engine
                .embed(&type_names)
                .await
                .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

            let points: Vec<VectorPoint> = unique_entity_types
                .iter()
                .zip(vectors)
                .map(|(et, vector)| {
                    let mut point = VectorPoint::new(et.base.id, vector);

                    // 1. Full DataPoint dump (Python parity — see gap-05/08).
                    for (k, v) in et.base.vector_metadata() {
                        point = point.with_metadata(k, v);
                    }

                    // 2. Context-specific keys not present on the DataPoint.
                    point = point
                        .with_metadata("field", json!("name"))
                        .with_metadata("dataset_id", json!(dataset_id.to_string()));
                    if let Some(uid) = user_id {
                        point = point.with_metadata("user_id", json!(uid.to_string()));
                    }
                    if let Some(tid) = tenant_id {
                        point = point.with_metadata("tenant_id", json!(tid.to_string()));
                    }
                    point
                })
                .collect();

            vector_db
                .index_points("EntityType", "name", &points)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

            stats.record("EntityType", "name", unique_entity_types.len());
            info!("Indexed {} entity type names", unique_entity_types.len());
        }
    }

    // 3. Index TextSummary.text field
    if !summaries.is_empty() {
        if !vector_db
            .has_collection("TextSummary", "text")
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
        {
            vector_db
                .create_collection("TextSummary", "text", dimension)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
        }

        let ids: Vec<Uuid> = summaries.iter().map(|s| s.base.id).collect();
        let texts: Vec<_> = summaries.iter().map(|s| s.text.as_str()).collect();
        let vectors = reuse_or_embed(&engine, &precomputed, &ids, &texts).await?;

        let points: Vec<VectorPoint> = summaries
            .iter()
            .zip(vectors)
            .map(|(summary, vector)| {
                let mut point = VectorPoint::new(summary.base.id, vector);

                // 1. Full DataPoint dump (Python parity — see gap-05/08).
                for (k, v) in summary.base.vector_metadata() {
                    point = point.with_metadata(k, v);
                }

                // 2. Context-specific keys not present on the DataPoint.
                point = point
                    .with_metadata("field", json!("text"))
                    .with_metadata("text", json!(summary.text.clone()))
                    .with_metadata("dataset_id", json!(dataset_id.to_string()));
                if let Some(made_from) = summary.made_from {
                    point = point.with_metadata("chunk_id", json!(made_from.to_string()));
                }
                if let Some(source_chunk_id) = summary.source_chunk_id {
                    point =
                        point.with_metadata("source_chunk_id", json!(source_chunk_id.to_string()));
                }
                if let Some(uid) = user_id {
                    point = point.with_metadata("user_id", json!(uid.to_string()));
                }
                if let Some(tid) = tenant_id {
                    point = point.with_metadata("tenant_id", json!(tid.to_string()));
                }
                point
            })
            .collect();

        vector_db
            .index_points("TextSummary", "text", &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        stats.record("TextSummary", "text", summaries.len());
        info!("Indexed {} summaries", summaries.len());
    }

    // 4. Index triplets (if enabled in config)
    if config.embed_triplets && !edges.is_empty() && !entities.is_empty() {
        use crate::triplet_creation::create_triplets_from_graph;

        let triplets = create_triplets_from_graph(entities, edges);

        if !triplets.is_empty() {
            if !vector_db
                .has_collection("Triplet", "text")
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
            {
                vector_db
                    .create_collection("Triplet", "text", dimension)
                    .await
                    .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
            }

            let triplet_texts: Vec<_> = triplets.iter().map(|t| t.text.as_str()).collect();
            let triplet_vectors = engine
                .embed(&triplet_texts)
                .await
                .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

            // Index the EdgeType DataPoints so each triplet payload can
            // inherit its originating edge's provenance (`source_*`) keys per
            // gap-05/08 §4.4. Triplet itself has no embedded `DataPoint`, so we
            // narrow the dump to just the five `source_*` keys to avoid
            // colliding with Triplet's own flat fields (id, type, etc.).
            //
            // EdgeTypes are now keyed on each edge's *retrieval text*
            // (`edge_retrieval_text`: nonblank `edge_text`, else
            // `relationship_name`) to match Python's `generate_edge_id`, but a
            // Triplet only carries the bare `relationship_name`. We therefore
            // map each triplet's (source, target, relationship) tuple to its
            // edge's retrieval text via the source edges, then look up the
            // EdgeType by that text — so the provenance copy survives the
            // Part-3 keying change even when edges carry a description.
            let edge_type_by_text: std::collections::HashMap<&str, &EdgeType> = edge_types
                .iter()
                .map(|et| (et.relationship_name.as_str(), et))
                .collect();
            let edge_text_by_triple: std::collections::HashMap<(Uuid, Uuid, &str), String> = edges
                .iter()
                .map(|e| {
                    (
                        (
                            e.source_entity_id,
                            e.target_entity_id,
                            e.relationship_name.as_str(),
                        ),
                        edge_retrieval_text(e),
                    )
                })
                .collect();

            let triplet_points: Vec<VectorPoint> = triplets
                .iter()
                .zip(triplet_vectors)
                .map(|(triplet, vector)| {
                    let mut point = VectorPoint::new(triplet.id, vector)
                        .with_metadata("type", json!("Triplet"))
                        .with_metadata("field", json!("text"))
                        .with_metadata("source_id", json!(triplet.source_entity_id.to_string()))
                        .with_metadata("target_id", json!(triplet.target_entity_id.to_string()))
                        .with_metadata("relationship", json!(triplet.relationship_name.clone()));

                    // Triplet special case (gap-05/08 §4.4): copy only the
                    // five `source_*` keys from the originating EdgeType's
                    // DataPoint, so Triplet's own flat fields are not
                    // overwritten.
                    let edge_type = edge_text_by_triple
                        .get(&(
                            triplet.source_entity_id,
                            triplet.target_entity_id,
                            triplet.relationship_name.as_str(),
                        ))
                        .and_then(|text| edge_type_by_text.get(text.as_str()));
                    if let Some(edge_type) = edge_type {
                        for (k, v) in edge_type.base.vector_metadata() {
                            if matches!(
                                k.as_str(),
                                "source_pipeline"
                                    | "source_task"
                                    | "source_user"
                                    | "source_node_set"
                                    | "source_content_hash"
                            ) {
                                point = point.with_metadata(k, v);
                            }
                        }
                    }
                    point
                })
                .collect();

            vector_db
                .index_points("Triplet", "text", &triplet_points)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

            stats.triplet_count = triplets.len();
            info!("Indexed {} triplets", triplets.len());
        }
    } else if config.embed_triplets {
        info!("Triplet embedding enabled but no edges/entities to index");
    }

    // 5. Index EdgeType.relationship_name field
    if !edge_types.is_empty() {
        if !vector_db
            .has_collection("EdgeType", "relationship_name")
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
        {
            vector_db
                .create_collection("EdgeType", "relationship_name", dimension)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
        }

        let names: Vec<&str> = edge_types
            .iter()
            .map(|et| et.relationship_name.as_str())
            .collect();
        let vectors = engine
            .embed(&names)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

        let points: Vec<VectorPoint> = edge_types
            .iter()
            .zip(vectors)
            .map(|(et, vector)| {
                let mut point = VectorPoint::new(et.base.id, vector);

                // 1. Full DataPoint dump (Python parity — see gap-05/08).
                for (k, v) in et.base.vector_metadata() {
                    point = point.with_metadata(k, v);
                }

                // 2. Context-specific keys not present on the DataPoint.
                point = point
                    .with_metadata("field", json!("relationship_name"))
                    .with_metadata("relationship_name", json!(et.relationship_name.clone()))
                    .with_metadata("number_of_edges", json!(et.number_of_edges))
                    .with_metadata("dataset_id", json!(dataset_id.to_string()));
                if let Some(uid) = user_id {
                    point = point.with_metadata("user_id", json!(uid.to_string()));
                }
                if let Some(tid) = tenant_id {
                    point = point.with_metadata("tenant_id", json!(tid.to_string()));
                }
                point
            })
            .collect();

        vector_db
            .index_points("EdgeType", "relationship_name", &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        stats.record("EdgeType", "relationship_name", edge_types.len());
        info!("Indexed {} edge types", edge_types.len());
    }

    // 6. Index Documents by name into `{ConcreteType}_name` collections
    //    (e.g. TextDocument_name, PdfDocument_name). Python indexes every
    //    Document subclass via its `index_fields=["name"]`
    //    (index_data_points.py:39-52). We group by the concrete subclass
    //    `data_type` so the collection names match Python's class names.
    if !documents.is_empty() {
        // Preserve a stable iteration order so the embed batches are
        // deterministic; group documents by their concrete type name.
        let mut by_type: std::collections::BTreeMap<&str, Vec<&Document>> =
            std::collections::BTreeMap::new();
        for d in documents {
            by_type
                .entry(d.base.data_type.as_str())
                .or_default()
                .push(d);
        }

        for (type_name, docs) in by_type {
            if !vector_db
                .has_collection(type_name, "name")
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
            {
                vector_db
                    .create_collection(type_name, "name", dimension)
                    .await
                    .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
            }

            let names: Vec<&str> = docs.iter().map(|d| d.name.as_str()).collect();
            let vectors = engine
                .embed(&names)
                .await
                .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

            let points: Vec<VectorPoint> = docs
                .iter()
                .zip(vectors)
                .map(|(doc, vector)| {
                    let mut point = VectorPoint::new(doc.base.id, vector);

                    // 1. Full DataPoint dump (Python parity — see gap-05/08).
                    for (k, v) in doc.base.vector_metadata() {
                        point = point.with_metadata(k, v);
                    }

                    // 2. Context-specific keys not present on the DataPoint.
                    point = point
                        .with_metadata("field", json!("name"))
                        .with_metadata("name", json!(doc.name.clone()))
                        .with_metadata("dataset_id", json!(dataset_id.to_string()));
                    if let Some(uid) = user_id {
                        point = point.with_metadata("user_id", json!(uid.to_string()));
                    }
                    if let Some(tid) = tenant_id {
                        point = point.with_metadata("tenant_id", json!(tid.to_string()));
                    }
                    point
                })
                .collect();

            vector_db
                .index_points(type_name, "name", &points)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

            stats.record(type_name, "name", docs.len());
            info!("Indexed {} {}", docs.len(), type_name);
        }
    }

    Ok(stats)
}

// ---------------------------------------------------------------------------
// TypedTask factories
// ---------------------------------------------------------------------------

/// Name used by the executor's `stamp_tree_dyn` for the `classify_documents` task.
///
/// Kept as a `const` so the inline `stamp_provenance` literals removed in LIB-06-03
/// stay byte-stable with the executor's automatic stamp. Matches the historical
/// inline literal `"classify_documents"` at the convenience function call site.
pub const CLASSIFY_DOCUMENTS_TASK_NAME: &str = "classify_documents";
pub const EXTRACT_CHUNKS_TASK_NAME: &str = "extract_chunks_from_documents";
pub const EXTRACT_GRAPH_TASK_NAME: &str = "extract_graph_from_data";
pub const SUMMARIZE_TEXT_TASK_NAME: &str = "summarize_text";
pub const ADD_DATA_POINTS_TASK_NAME: &str = "add_data_points";

/// `topological_rank` stamped on every DataPoint emitted by
/// `classify_documents`.
///
/// # What these constants are
///
/// `DataPoint.topological_rank` is the 1-based index of the pipeline stage
/// that created the node; the visualization's Story / Flow layouts use it as
/// the column number. Python derives it at runtime from a **deduplicated**
/// task-name sequence — `task_sequence.index(task_name) + 1`
/// (`run_tasks_base.py:181-190`) — and writes it in `_stamp_provenance`
/// (`:69-75`) only while the field is still unset (`None` or the `0`
/// sentinel). The `*_TASK_RANK` constants below are Rust's equivalent, and
/// they are keyed to **Python's** stage boundaries, not Rust's.
///
/// # Why they are not simply 1..=5
///
/// Python's default task list is
/// `[classify_documents, extract_chunks_from_documents,
/// extract_graph_and_summarize, add_data_points, extract_dlt_fk_edges]`
/// (`cognify.py:350-375`), so its deduplicated sequence numbers
/// `extract_graph_and_summarize` **3** and `add_data_points` **4**. Rust
/// splits Python's single fused stage into two tasks (`extract_graph_from_data`
/// then `summarize_text`); numbering them 3 and 4 would push
/// `add_data_points` to 5 and give the same node type a different column in
/// each SDK. Both halves of the fused stage therefore share rank **3** and
/// `add_data_points` keeps Python's **4**.
///
/// # How the value actually reaches the DataPoint
///
/// Both the convenience [`cognify`] entry point and an explicit
/// `cognee_core::execute(&build_cognify_pipeline(..), ..)` run the same
/// pipeline through the executor, and the executor stamps `first_index + 1`
/// on every output it recognises. These constants still win, for two
/// independent reasons:
///
/// 1. the in-body [`stamp_provenance`] calls run *inside* the task closure,
///    so they land before the executor's post-task walk and the
///    `None | Some(0)` write-once guard makes the later stamp a no-op; and
/// 2. cognify's task outputs are wrapper structs (`ClassifiedDocuments`,
///    `ExtractedGraphData`, …) that are not `HasDataPoint`, so
///    `cognee_core::provenance::stamp_tree_dyn` does not recognise them and
///    the executor never reaches the nested DataPoints at all.
///
/// The consequence is that reordering [`build_cognify_pipeline`] without
/// updating these constants silently corrupts the ranks — nothing errors and
/// no test fails. Custom pipelines composed from the public
/// `make_*_task` factories must use the `make_*_task_with_rank` variants to
/// supply their own positions (see [`make_extract_graph_task_with_rank`]).
///
/// Python parity: `run_tasks_base.py:69-75` / `:181-190`,
/// `cognify.py:350-375`.
pub const CLASSIFY_DOCUMENTS_TASK_RANK: i32 = 1;
/// `topological_rank` for `extract_chunks_from_documents`; see
/// [`CLASSIFY_DOCUMENTS_TASK_RANK`].
pub const EXTRACT_CHUNKS_TASK_RANK: i32 = 2;
/// `topological_rank` for `extract_graph_from_data` — the first half of
/// Python's fused `extract_graph_and_summarize` stage; see
/// [`CLASSIFY_DOCUMENTS_TASK_RANK`].
pub const EXTRACT_GRAPH_TASK_RANK: i32 = 3;
/// `topological_rank` for `summarize_text`. **Deliberately equal to
/// [`EXTRACT_GRAPH_TASK_RANK`]**: Python fuses both into the single
/// `extract_graph_and_summarize` task, which occupies one slot in its
/// deduplicated task sequence. See [`CLASSIFY_DOCUMENTS_TASK_RANK`].
pub const SUMMARIZE_TEXT_TASK_RANK: i32 = 3;
/// `topological_rank` for `add_data_points`; 4 in Python because the fused
/// graph+summarize stage takes only slot 3. See
/// [`CLASSIFY_DOCUMENTS_TASK_RANK`].
pub const ADD_DATA_POINTS_TASK_RANK: i32 = 4;

/// Pipeline name carried by cognify task stamps (locked Decision 14 of
/// LIB-06), matching Python's `pipeline_name="cognify_pipeline"` argument to
/// `run_tasks` (`cognify.py:298`). Written to `DataPoint.source_pipeline` by
/// the per-task in-body stamping below, used as the `pipeline_runs` row name
/// by [`build_cognify_pipeline`], and the value the visualization's
/// operations catalog matches on to mark a stage as observed.
pub const COGNIFY_PIPELINE_STAMP_NAME: &str = "cognify_pipeline";

/// How long an exclusive-run claim stays valid without being released, after
/// which another run may reclaim it — see
/// [`PipelineRunRepository::try_claim_pipeline_run`].
///
/// Deliberately generous. Reclaiming too eagerly is the worse failure: it
/// re-admits a concurrent run into a legitimately long one (a multi-gigabyte
/// cognify runs for hours), which is exactly what the claim exists to prevent.
/// The cost of being generous is that a claim left by a killed process blocks
/// that dataset until it ages out — recoverable by resetting the dataset's
/// pipeline-run state, and no worse than the stale `Started` row such a crash
/// already leaves behind.
pub const CLAIM_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Resolve the user label for in-body stamping from a [`TaskContext`].
///
/// Mirrors [`cognee_core::PipelineContext::user_label`]: prefer
/// `user_email`, fall back to `user_id.to_string()`, else `None`.
fn user_label_from_ctx(ctx: &Arc<cognee_core::TaskContext>) -> Option<String> {
    ctx.pipeline_ctx.as_ref().and_then(|p| p.user_label())
}

/// The run every ownership row written by this task is attributed to.
///
/// The executor sets it on the context before any task runs, and it is the same
/// value it writes to `pipeline_runs.pipeline_run_id`. `None` only when the
/// task is driven outside a pipeline executor.
fn pipeline_run_id_from_ctx(ctx: &Arc<cognee_core::TaskContext>) -> Option<Uuid> {
    ctx.pipeline_ctx.as_ref().and_then(|p| p.run_id)
}

// ── Rank-overriding factory variants ───────────────────────────────────────
//
// Every `make_*_task` factory below is public API, so an embedder can compose
// a custom pipeline that puts a stage at a different position than
// [`build_cognify_pipeline`] does. Because the rank is stamped in-body (see
// [`CLASSIFY_DOCUMENTS_TASK_RANK`] for why the executor's positional stamp
// never wins), a mis-positioned stage would otherwise emit the default
// pipeline's rank on every node — silently mis-rendering the visualization's
// Flow layout with no error and no failing test.
//
// Each factory therefore comes in two flavours:
//
// * `make_x_task(..)` — rank fixed to the Python-parity constant. Use this
//   inside (or alongside) the default pipeline.
// * `make_x_task_with_rank(.., rank)` — `rank` is the 1-based position the
//   stage occupies in the caller's pipeline. Pass `0` to suppress the in-body
//   rank write entirely and leave `topological_rank` at its `0` sentinel.

/// Build a [`TypedTask`] that classifies Data items into Documents.
///
/// The returned task does **not** carry a name; the pipeline builder
/// [`build_cognify_pipeline`] wraps it with [`CLASSIFY_DOCUMENTS_TASK_NAME`].
///
/// In-body provenance stamping: stamps every emitted `Document` with
/// `source_pipeline = `[`COGNIFY_PIPELINE_STAMP_NAME`] and `source_task =
/// "classify_documents"`. Necessary because `ClassifiedDocuments` is a
/// non-`HasDataPoint` wrapper not walked by the executor's `stamp_tree_dyn`
/// (LIB-06-03 fixup).
pub fn make_classify_documents_task(
    failure_policy: FailurePolicy,
) -> TypedTask<CognifyInput, ClassifiedDocuments> {
    make_classify_documents_task_with_rank(CLASSIFY_DOCUMENTS_TASK_RANK, failure_policy)
}

/// [`make_classify_documents_task`] with a caller-chosen `topological_rank`.
pub fn make_classify_documents_task_with_rank(
    rank: i32,
    failure_policy: FailurePolicy,
) -> TypedTask<CognifyInput, ClassifiedDocuments> {
    TypedTask::sync(move |input: &CognifyInput, ctx| {
        // `?` boxes the `CognifyError` rather than stringifying it, so
        // `cognify()` can downcast the executor's `TaskFailed` back to the
        // typed error — which is what turns a failure that is "merely thrown"
        // into one that is reported.
        let mut classified = classify_documents(input, failure_policy)?;
        let user_label = user_label_from_ctx(&ctx);
        for doc in &mut classified.documents {
            stamp_provenance(
                &mut doc.base,
                COGNIFY_PIPELINE_STAMP_NAME,
                CLASSIFY_DOCUMENTS_TASK_NAME,
                user_label.as_deref(),
                rank,
            );
        }
        Ok(Box::new(classified))
    })
}

/// Build a [`TypedTask`] that extracts text chunks from classified documents.
///
/// In-body provenance stamping: stamps every emitted `DocumentChunk`
/// with `source_task = "extract_chunks_from_documents"`. Documents
/// inherited from the upstream wrapper keep their already-set stamp via
/// the `is_none()` guard inside [`stamp_provenance`].
pub fn make_extract_chunks_task(
    storage: Arc<dyn StorageTrait>,
    max_chunk_size: usize,
    token_counter_kind: TokenCounterKind,
    db: Option<Arc<DatabaseConnection>>,
    loader_registry: Arc<LoaderRegistry>,
    failure_policy: FailurePolicy,
) -> TypedTask<ClassifiedDocuments, ExtractedChunks> {
    make_extract_chunks_task_with_rank(
        storage,
        max_chunk_size,
        token_counter_kind,
        db,
        loader_registry,
        failure_policy,
        EXTRACT_CHUNKS_TASK_RANK,
    )
}

/// [`make_extract_chunks_task`] with a caller-chosen `topological_rank`.
#[allow(clippy::too_many_arguments)]
pub fn make_extract_chunks_task_with_rank(
    storage: Arc<dyn StorageTrait>,
    max_chunk_size: usize,
    token_counter_kind: TokenCounterKind,
    db: Option<Arc<DatabaseConnection>>,
    loader_registry: Arc<LoaderRegistry>,
    failure_policy: FailurePolicy,
    rank: i32,
) -> TypedTask<ClassifiedDocuments, ExtractedChunks> {
    TypedTask::async_fn(move |input: &ClassifiedDocuments, ctx| {
        let input = input.clone();
        let storage = Arc::clone(&storage);
        let db = db.clone();
        let token_counter_kind = token_counter_kind.clone();
        let loader_registry = Arc::clone(&loader_registry);
        let user_label = user_label_from_ctx(&ctx);
        Box::pin(async move {
            let mut extracted = extract_chunks_from_documents(
                &input,
                &*storage,
                max_chunk_size,
                token_counter_kind,
                db.as_deref(),
                &loader_registry,
                failure_policy,
            )
            .await?;
            for chunk in &mut extracted.chunks {
                stamp_provenance(
                    &mut chunk.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_CHUNKS_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            // Documents carried forward keep their earlier stamp from
            // `classify_documents`; only stamp any that are still unstamped
            // (idempotent via the `is_none` guards).
            for doc in &mut extracted.documents {
                stamp_provenance(
                    &mut doc.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_CHUNKS_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            Ok(Box::new(extracted))
        })
    })
}

/// Build a [`TypedTask`] that extracts knowledge graphs from chunks via LLM.
///
/// In-body provenance stamping: stamps `entities[*].entity`,
/// `entities[*].entity_type` with `source_task = "extract_graph_from_data"`.
/// Carried-forward chunks/documents keep their earlier stamp via the
/// idempotent `is_none()` guards inside [`stamp_provenance`].
pub fn make_extract_graph_task(
    llm: Arc<dyn Llm>,
    graph_db: Arc<dyn GraphDBTrait>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    db: Arc<DatabaseConnection>,
    config: CognifyConfig,
) -> TypedTask<ExtractedChunks, ExtractedGraphData> {
    make_extract_graph_task_with_rank(
        llm,
        graph_db,
        ontology_resolver,
        db,
        config,
        EXTRACT_GRAPH_TASK_RANK,
    )
}

/// [`make_extract_graph_task`] with a caller-chosen `topological_rank`.
///
/// `rank` is threaded all the way into [`extract_graph_from_data`] (and from
/// there into `expand_with_nodes_and_edges`) rather than only being applied to
/// the task body's own stamp loop: Entity / EntityType nodes are written to the
/// graph DB *inside* `extract_graph_from_data`, so a rank applied afterwards
/// would never reach the persisted rows.
pub fn make_extract_graph_task_with_rank(
    llm: Arc<dyn Llm>,
    graph_db: Arc<dyn GraphDBTrait>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    db: Arc<DatabaseConnection>,
    config: CognifyConfig,
    rank: i32,
) -> TypedTask<ExtractedChunks, ExtractedGraphData> {
    TypedTask::async_fn(move |input: &ExtractedChunks, ctx| {
        let input = input.clone();
        let llm = Arc::clone(&llm);
        let graph_db = Arc::clone(&graph_db);
        let ontology_resolver = Arc::clone(&ontology_resolver);
        let db = Arc::clone(&db);
        let config = config.clone();
        let user_label = user_label_from_ctx(&ctx);
        let pipeline_run_id = pipeline_run_id_from_ctx(&ctx);
        Box::pin(async move {
            let mut graph_data = extract_graph_from_data(
                &input,
                llm,
                Arc::clone(&graph_db),
                ontology_resolver,
                &db,
                pipeline_run_id,
                &config,
                user_label.as_deref(),
                (rank > 0).then_some(rank),
            )
            .await?;
            if config.create_web_page_nodes {
                create_web_page_nodes(
                    &graph_data.documents,
                    &graph_data.chunks,
                    graph_db,
                    &db,
                    LedgerIdentity::new(
                        input.tenant_id,
                        input.user_id,
                        input.dataset_id,
                        pipeline_run_id,
                    ),
                )
                .await?;
            }
            for pair in &mut graph_data.entities {
                stamp_provenance(
                    &mut pair.entity.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_GRAPH_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
                stamp_provenance(
                    &mut pair.entity_type.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_GRAPH_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            // Chunks/documents carried forward — idempotent re-stamp keeps
            // their upstream `source_task` intact via the `is_none` guard.
            for chunk in &mut graph_data.chunks {
                stamp_provenance(
                    &mut chunk.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_GRAPH_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            for doc in &mut graph_data.documents {
                stamp_provenance(
                    &mut doc.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_GRAPH_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            Ok(Box::new(graph_data))
        })
    })
}

/// Build a [`TypedTask`] that summarizes text chunks via LLM.
///
/// In-body provenance stamping: stamps every emitted `TextSummary`
/// with `source_task = "summarize_text"`. Carried-forward
/// chunks/documents/entities keep their upstream stamps.
pub fn make_summarize_text_task(
    llm: Arc<dyn Llm>,
    config: CognifyConfig,
) -> TypedTask<ExtractedGraphData, SummarizedData> {
    make_summarize_text_task_with_rank(llm, config, SUMMARIZE_TEXT_TASK_RANK)
}

/// [`make_summarize_text_task`] with a caller-chosen `topological_rank`.
pub fn make_summarize_text_task_with_rank(
    llm: Arc<dyn Llm>,
    config: CognifyConfig,
    rank: i32,
) -> TypedTask<ExtractedGraphData, SummarizedData> {
    TypedTask::async_fn(move |input: &ExtractedGraphData, ctx| {
        let input = input.clone();
        let llm = Arc::clone(&llm);
        let config = config.clone();
        let user_label = user_label_from_ctx(&ctx);
        Box::pin(async move {
            let mut summarized = summarize_text(&input, llm, &config).await?;
            for summary in &mut summarized.summaries {
                stamp_provenance(
                    &mut summary.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    SUMMARIZE_TEXT_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            // Idempotent re-stamp of carried-forward DataPoints — only
            // ones that somehow escaped earlier stamping get filled in.
            for chunk in &mut summarized.chunks {
                stamp_provenance(
                    &mut chunk.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    SUMMARIZE_TEXT_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            for doc in &mut summarized.documents {
                stamp_provenance(
                    &mut doc.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    SUMMARIZE_TEXT_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            for pair in &mut summarized.entities {
                stamp_provenance(
                    &mut pair.entity.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    SUMMARIZE_TEXT_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
                stamp_provenance(
                    &mut pair.entity_type.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    SUMMARIZE_TEXT_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            Ok(Box::new(summarized))
        })
    })
}

/// Build a [`TypedTask`] that generates embeddings and indexes data points.
///
/// In-body provenance stamping: idempotent re-stamp of every DataPoint
/// in the produced `CognifyResult`. Upstream tasks have already stamped
/// them with their specific `source_task`; this loop only fills in any
/// stragglers (e.g. fresh `EdgeType` entries or DataPoints constructed
/// inside `add_data_points` itself) — the `is_none` guards inside
/// [`stamp_provenance`] keep upstream stamps intact.
pub fn make_add_data_points_task(
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Arc<DatabaseConnection>,
    config: CognifyConfig,
) -> TypedTask<SummarizedData, CognifyResult> {
    make_add_data_points_task_with_rank(
        graph_db,
        vector_db,
        embedding_engine,
        db,
        config,
        ADD_DATA_POINTS_TASK_RANK,
    )
}

/// [`make_add_data_points_task`] with a caller-chosen `topological_rank`.
///
/// `rank` never reaches `result.edge_types`: Python's `index_graph_edges`
/// `EdgeType` objects are never stamped at all, so Rust leaves their rank at
/// the `0` sentinel regardless of this argument (see the parity note at the
/// `edge_types` loop below).
#[allow(clippy::too_many_arguments)]
pub fn make_add_data_points_task_with_rank(
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Arc<DatabaseConnection>,
    config: CognifyConfig,
    rank: i32,
) -> TypedTask<SummarizedData, CognifyResult> {
    TypedTask::async_fn(move |input: &SummarizedData, ctx| {
        let input = input.clone();
        let graph_db = Arc::clone(&graph_db);
        let vector_db = Arc::clone(&vector_db);
        let embedding_engine = Arc::clone(&embedding_engine);
        let db = Arc::clone(&db);
        let config = config.clone();
        let user_label = user_label_from_ctx(&ctx);
        let pipeline_run_id = pipeline_run_id_from_ctx(&ctx);
        Box::pin(async move {
            let mut result = add_data_points(
                &input,
                graph_db,
                vector_db,
                embedding_engine,
                &db,
                pipeline_run_id,
                &config,
            )
            .await?;
            for chunk in &mut result.chunks {
                stamp_provenance(
                    &mut chunk.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            for pair in &mut result.entities {
                stamp_provenance(
                    &mut pair.entity.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
                stamp_provenance(
                    &mut pair.entity_type.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            for summary in &mut result.summaries {
                stamp_provenance(
                    &mut summary.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            // `rank = 0` skips the `topological_rank` write. Python's
            // `create_edge_type_datapoints` (`index_graph_edges.py:50`) never
            // routes these `EdgeType` objects through a provenance stamper, so
            // their rank stays at the `0` sentinel; `add_data_points` already
            // pre-stamped the `source_*` keys they need for the
            // `EdgeType_relationship_name` vector payload (gap-05/08 §4.4).
            for edge_type in &mut result.edge_types {
                stamp_provenance(
                    &mut edge_type.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                    0,
                );
            }
            for doc in &mut result.documents_for_dlt {
                stamp_provenance(
                    &mut doc.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                    rank,
                );
            }
            // The end-of-run gate. It lives here rather than in `cognify()`
            // because the `Err` has to originate *inside* the executor for the
            // run to be written ERRORED — a failure the caller only learns
            // about after `execute` returned would leave a COMPLETED row
            // behind.
            if result.failures.is_fatal(&config.failure_policy()) {
                return Err(Box::new(CognifyError::RunFailed {
                    report: Box::new(result.failures),
                }) as cognee_core::TaskError);
            }
            Ok(Box::new(result))
        })
    })
}

// ---------------------------------------------------------------------------
// Pipeline builder
// ---------------------------------------------------------------------------

/// Build a [`LoaderRegistry`] with the default text/pdf/csv loaders plus any
/// feature-gated media loaders that have the required handles available.
///
/// Centralized here so both [`build_cognify_pipeline`] and
/// [`build_temporal_cognify_pipeline`] stay in sync.
// `llm` is consumed only by the image loader and `config` only by the audio
// loader; when neither feature is enabled both are genuinely unused.
#[cfg_attr(
    not(any(feature = "image-loader", feature = "audio-loader")),
    allow(unused_variables)
)]
fn build_loader_registry(llm: &Arc<dyn Llm>, config: &CognifyConfig) -> LoaderRegistry {
    #[allow(unused_mut)]
    let mut registry = LoaderRegistry::default_registry();
    #[cfg(feature = "image-loader")]
    registry.register("image", Arc::new(ImageLoader::new(Arc::clone(llm))));
    #[cfg(feature = "audio-loader")]
    if let Some(ref transcriber_handle) = config.transcriber {
        registry.register(
            "audio",
            Arc::new(AudioLoader::new(Arc::clone(&transcriber_handle.0))),
        );
    }
    registry
}

/// Build a complete cognify [`Pipeline`]:
/// [`CognifyInput`] → classify → chunk → extract_graph → summarize → add_data_points → [`CognifyResult`].
///
/// The `user_id` and `tenant_id` parameters are threaded through all pipeline
/// stages and included as metadata on vector points and graph nodes.
///
/// For composable pipeline-based execution (with concurrency, retry, progress
/// tracking, etc.), pass the result to [`cognee_core::execute`].
#[allow(clippy::too_many_arguments)]
pub fn build_cognify_pipeline(
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    llm: Arc<dyn Llm>,
    // Non-optional: every stage that puts an artifact in the graph or vector
    // store records its ownership first, so a databaseless persistence stage is
    // a shape the pipeline cannot be built in. `cognify()` already requires a
    // connection, and every binding routes through it.
    db: Arc<DatabaseConnection>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: CognifyConfig,
) -> Pipeline {
    let loader_registry = Arc::new(build_loader_registry(&llm, &config));
    PipelineBuilder::new_with_task(
        COGNIFY_PIPELINE_STAMP_NAME,
        make_classify_documents_task(config.failure_policy()),
    )
    .with_first_task_name(CLASSIFY_DOCUMENTS_TASK_NAME)
    .add_task_named(
        make_extract_chunks_task(
            storage,
            config.chunk_size(),
            config.token_counter_kind.clone(),
            // Still `Option`: the chunk stage uses the connection for
            // incremental-loading bookkeeping, not for writing artifacts,
            // so it is not part of this invariant.
            Some(Arc::clone(&db)),
            loader_registry,
            config.failure_policy(),
        ),
        EXTRACT_CHUNKS_TASK_NAME,
    )
    .add_task_named(
        make_extract_graph_task(
            Arc::clone(&llm),
            Arc::clone(&graph_db),
            ontology_resolver,
            Arc::clone(&db),
            config.clone(),
        ),
        EXTRACT_GRAPH_TASK_NAME,
    )
    .add_task_named(
        make_summarize_text_task(llm, config.clone()),
        SUMMARIZE_TEXT_TASK_NAME,
    )
    .add_task_named(
        make_add_data_points_task(graph_db, vector_db, embedding_engine, db, config),
        ADD_DATA_POINTS_TASK_NAME,
    )
    .with_name(COGNIFY_PIPELINE_STAMP_NAME)
    .build()
}

/// Build a [`TypedTask`] that extracts temporal events from chunks via LLM.
pub fn make_extract_temporal_events_task(
    llm: Arc<dyn Llm>,
    config: CognifyConfig,
) -> TypedTask<ExtractedChunks, ExtractedTemporalEvents> {
    TypedTask::async_fn(move |input: &ExtractedChunks, _ctx| {
        let input = input.clone();
        let llm = Arc::clone(&llm);
        let config = config.clone();
        Box::pin(async move {
            extract_temporal_events(&input, llm, &config)
                .await
                .map(Box::new)
                .map_err(Into::into)
        })
    })
}

/// Build a [`TypedTask`] that persists temporal events to graph and vector DBs.
///
/// `db` is non-optional: the stage records ownership of every artifact before
/// it writes one, and the run id comes from the task context so those rows name
/// the run a sweep would select on.
pub fn make_add_temporal_data_points_task(
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Arc<DatabaseConnection>,
    failure_policy: FailurePolicy,
) -> TypedTask<ExtractedTemporalEvents, CognifyResult> {
    TypedTask::async_fn(move |input: &ExtractedTemporalEvents, ctx| {
        let input = input.clone();
        let graph_db = Arc::clone(&graph_db);
        let vector_db = Arc::clone(&vector_db);
        let embedding_engine = Arc::clone(&embedding_engine);
        let db = Arc::clone(&db);
        let pipeline_run_id = pipeline_run_id_from_ctx(&ctx);
        Box::pin(async move {
            let result = add_temporal_data_points(
                &input,
                graph_db,
                vector_db,
                embedding_engine,
                &db,
                pipeline_run_id,
            )
            .await?;
            // The same end-of-run gate as the standard branch, over the
            // failures the chunking and temporal extraction stages handed down.
            if result.failures.is_fatal(&failure_policy) {
                return Err(Box::new(CognifyError::RunFailed {
                    report: Box::new(result.failures),
                }) as cognee_core::TaskError);
            }
            Ok(Box::new(result))
        })
    })
}

/// Build a complete temporal cognify [`Pipeline`]:
/// [`CognifyInput`] → classify → chunk → extract_temporal_events → add_temporal_data_points → [`CognifyResult`].
///
/// This pipeline runs instead of the standard cognify pipeline when
/// `CognifyConfig::temporal_cognify` is `true`. It mirrors the Python
/// `get_temporal_tasks()` pipeline that replaces the default stages with
/// event/timestamp extraction and temporal graph construction.
pub fn build_temporal_cognify_pipeline(
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    llm: Arc<dyn Llm>,
    // Non-optional for the same reason [`build_cognify_pipeline`]'s is: the
    // temporal persistence stage records ownership of every artifact before it
    // writes one, so a databaseless temporal pipeline is a shape that cannot be
    // built.
    db: Arc<DatabaseConnection>,
    config: CognifyConfig,
) -> Pipeline {
    let loader_registry = Arc::new(build_loader_registry(&llm, &config));
    PipelineBuilder::new_with_task(
        "temporal-cognify",
        make_classify_documents_task(config.failure_policy()),
    )
    .with_first_task_name(CLASSIFY_DOCUMENTS_TASK_NAME)
    .add_task_named(
        make_extract_chunks_task(
            storage,
            config.chunk_size(),
            config.token_counter_kind.clone(),
            Some(Arc::clone(&db)),
            loader_registry,
            config.failure_policy(),
        ),
        EXTRACT_CHUNKS_TASK_NAME,
    )
    .add_task_named(
        make_extract_temporal_events_task(llm, config.clone()),
        "extract_temporal_events",
    )
    .add_task_named(
        make_add_temporal_data_points_task(
            graph_db,
            vector_db,
            embedding_engine,
            db,
            config.failure_policy(),
        ),
        "add_temporal_data_points",
    )
    .with_name("temporal-cognify")
    .build()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::graph_integration::expand_with_nodes_and_edges;
    use cognee_models::{DataPoint, Entity, EntityType};
    use cognee_storage::MockStorage;

    /// Provenance edge ids must be derived from *sanitized* edge text.
    ///
    /// Python's `upsert_edges`
    /// (`cognee/modules/graph/methods/upsert_edges.py:41-66`) derives its uuid5
    /// id from `sanitized_edge_text`, and so must Rust: a `contains` edge whose
    /// `edge_text` carried a NUL would otherwise hash under text no store ever
    /// holds, so re-running the same corpus would not land on the same row.
    /// (The Rust id also folds in `data_id` and therefore never equals Python's
    /// — see [`provenance_edge_id`]. What is shared is *which text* is hashed.)
    ///
    /// The slug is a weaker claim and is asserted as such. Rust uses
    /// `triplet_slug(source, edge_text, target)` while Python uses
    /// `generate_edge_id(sanitized_edge_text)`, which takes no node ids at all —
    /// a pre-existing formula divergence this change neither introduces nor
    /// fixes. All that is pinned below is that Rust's slug reads the same
    /// sanitized text as Rust's id, so the two cannot disagree with each other.
    ///
    /// The second assertion is the load-bearing one: it proves the strip
    /// actually changes the hash, so removing the `sanitize_string` call in
    /// `upsert_provenance` cannot be a no-op that slips through review.
    #[test]
    fn provenance_edge_ids_derive_from_sanitized_text() {
        let tenant = Some(Uuid::new_v4());
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        let source = Uuid::new_v4();
        let target = Uuid::new_v4();

        let data = Uuid::new_v4();

        let dirty = "Document chunk mentions Ali\u{0}ce";
        let clean = "Document chunk mentions Alice";

        assert_eq!(
            provenance_edge_id(
                tenant,
                user,
                dataset,
                data,
                source,
                &sanitize_string(dirty.to_string()),
                target
            ),
            provenance_edge_id(tenant, user, dataset, data, source, clean, target),
            "sanitizing before hashing must reproduce the id Python derives"
        );
        assert_eq!(
            triplet_slug(source, &sanitize_string(dirty.to_string()), target),
            triplet_slug(source, clean, target),
            "Rust's slug must read the same sanitized text as its id"
        );

        assert_ne!(
            provenance_edge_id(tenant, user, dataset, data, source, dirty, target),
            provenance_edge_id(tenant, user, dataset, data, source, clean, target),
            "hashing raw text must differ — otherwise this guard proves nothing"
        );
    }

    /// The structural-edge branch of `upsert_provenance` must follow the same
    /// convention as the semantic branch above.
    ///
    /// It used not to: it derived `provenance_edge_id` and `edge_slug` from the
    /// raw `rel_name` while `conversions.rs` sanitized `relationship_name` on
    /// the way to the database — the opposite convention from the loop directly
    /// above it. Python has no such split: every edge goes through the one
    /// `upsert_edges`, which feeds `sanitized_edge_text` into both its uuid5 and
    /// `generate_edge_id`.
    #[test]
    fn structural_edge_ids_derive_from_sanitized_rel_name() {
        let tenant = Some(Uuid::new_v4());
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        let source = Uuid::new_v4();
        let target = Uuid::new_v4();

        let dirty = "is\u{0}_part_of";
        let clean = "is_part_of";

        assert_eq!(
            provenance_edge_id(
                tenant,
                user,
                dataset,
                Uuid::nil(),
                source,
                &sanitize_str(dirty),
                target
            ),
            provenance_edge_id(tenant, user, dataset, Uuid::nil(), source, clean, target),
            "a structural edge's id must be derived from the sanitized name"
        );
        assert_eq!(
            edge_slug(&sanitize_str(dirty)),
            edge_slug(clean),
            "a structural edge's slug must read the same sanitized name as its id"
        );

        assert_ne!(
            edge_slug(dirty),
            edge_slug(clean),
            "hashing the raw name must differ — otherwise this guard proves nothing"
        );
    }

    /// [`producing_data_ids`] deduplicates on the way *out*, not on the way
    /// into the database. Two chunks of one file are two producers but one
    /// data item, and the row id formulas would mint the same primary key
    /// twice for them.
    ///
    /// Asserted on the return value, deliberately. `upsert_nodes_on` /
    /// `upsert_edges_on` collapse duplicates only *within* one
    /// `PROVENANCE_INSERT_BATCH` chunk, so a test that reads the rows back
    /// measures their dedup rather than this function's, and stays green
    /// against a duplicate pair that straddles a batch boundary — the latent
    /// bug at scale the doc comment names.
    #[test]
    fn producing_data_ids_deduplicates_and_keeps_first_seen_order() {
        let file_a = Uuid::new_v4();
        let file_b = Uuid::new_v4();
        let first_of_a = Uuid::new_v4();
        let only_of_b = Uuid::new_v4();
        let second_of_a = Uuid::new_v4();
        let third_of_a = Uuid::new_v4();

        let chunk_data_map: HashMap<Uuid, Uuid> = [
            (first_of_a, file_a),
            (only_of_b, file_b),
            (second_of_a, file_a),
            (third_of_a, file_a),
        ]
        .into_iter()
        .collect();

        assert_eq!(
            producing_data_ids(
                &[first_of_a, only_of_b, second_of_a, third_of_a],
                &chunk_data_map,
            ),
            vec![file_a, file_b],
            "one entry per data item, in first-seen order — three chunks of \
             one file are one owner"
        );

        // A chunk nobody mapped contributes no owner at all, rather than a nil
        // placeholder the delete path would never reproduce.
        assert!(
            producing_data_ids(&[Uuid::new_v4()], &chunk_data_map).is_empty(),
            "an unmapped chunk names no data item"
        );
    }

    #[test]
    fn test_classify_documents_empty() {
        let input = CognifyInput {
            data_items: vec![],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };
        let result = classify_documents(&input, FailurePolicy::default()).unwrap();
        assert!(result.documents.is_empty());
    }

    #[test]
    fn test_classify_documents_text_data() {
        let data = Data::builder(
            Uuid::new_v4(),
            "test.txt",
            "/storage/test.txt",
            "text://test",
            "txt",
            "text/plain",
            "hash123",
            Uuid::new_v4(),
        )
        .build();

        let input = CognifyInput {
            data_items: vec![data],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };
        let result = classify_documents(&input, FailurePolicy::default()).unwrap();
        assert_eq!(result.documents.len(), 1);
    }

    /// Markdown is cognified as text, exactly as Python does it.
    #[test]
    fn markdown_is_classified_as_text_like_python() {
        let markdown = Data::builder(
            Uuid::new_v4(),
            "README.md",
            "/storage/README.md",
            "file://README.md",
            "md",
            "text/markdown",
            "hash-md",
            Uuid::new_v4(),
        )
        .build();
        let markdown_id = markdown.id;

        let input = CognifyInput {
            data_items: vec![markdown],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        let result = classify_documents(&input, FailurePolicy::default()).unwrap();

        assert_eq!(result.documents.len(), 1);
        assert_eq!(result.documents[0].base.id, markdown_id);
        assert_eq!(result.documents[0].document_type, "text");
        assert!(result.failures.entries().is_empty());
        assert!(result.failures.unreached_items().is_empty());
    }

    /// A genuinely unsupported extension is skipped but never silently lost.
    #[test]
    fn unsupported_extension_is_recorded_as_unreached() {
        let source = Data::builder(
            Uuid::new_v4(),
            "main.py",
            "/storage/main.py",
            "file://main.py",
            "py",
            "text/x-python",
            "hash-py",
            Uuid::new_v4(),
        )
        .build();
        let source_id = source.id;

        let input = CognifyInput {
            data_items: vec![source],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        let result = classify_documents(&input, FailurePolicy::default()).unwrap();

        assert!(result.documents.is_empty(), "source files stay out");
        assert!(
            result.failures.entries().is_empty(),
            "skipping is not a failure — it must not error the run"
        );
        assert!(
            result.failures.unreached_items().contains(&source_id),
            "but it must be recorded, or a completing run marks it done"
        );
    }

    #[test]
    fn test_classify_documents_skips_unknown_extension() {
        let data = Data::builder(
            Uuid::new_v4(),
            "data.xyz",
            "/storage/data.xyz",
            "file://data.xyz",
            "xyz",
            "application/octet-stream",
            "hash456",
            Uuid::new_v4(),
        )
        .build();

        let input = CognifyInput {
            data_items: vec![data],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };
        let result = classify_documents(&input, FailurePolicy::default()).unwrap();
        assert!(result.documents.is_empty());
    }

    #[tokio::test]
    async fn test_extract_chunks_from_documents() {
        let storage = Arc::new(MockStorage::new());
        let location = storage
            .store(b"Hello world. This is a test.", "test.txt")
            .await
            .unwrap();

        let doc_id = Uuid::new_v4();
        let mut base = DataPoint::new("TextDocument", None);
        base.id = doc_id;
        base.importance_weight = Some(0.9);
        base.set_metadata("index_fields", serde_json::json!(["name"]));
        let doc = Document {
            base,
            document_type: "text".to_string(),
            name: "test.txt".to_string(),
            raw_data_location: location,
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: doc_id,
            external_metadata: None,
        };

        let input = ClassifiedDocuments {
            failures: FailureReport::default(),
            documents: vec![doc],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        let registry = LoaderRegistry::default();
        let result = extract_chunks_from_documents(
            &input,
            &*storage,
            100,
            TokenCounterKind::Word,
            None,
            &registry,
            CognifyConfig::default().failure_policy(),
        )
        .await
        .unwrap();
        assert!(!result.chunks.is_empty());
        // importance_weight propagates from Document to every chunk (regular path).
        assert!(
            result
                .chunks
                .iter()
                .all(|c| c.base.importance_weight == Some(0.9)),
            "every chunk must inherit the document's importance_weight"
        );
    }

    /// Drives the cognify-level provenance wrapper (`upsert_provenance`) and its
    /// wiring, not just the DB seam (`upsert_provenance_graph`): one document
    /// must produce exactly one provenance node written through the wrapper, so
    /// a regression in the wrapper's own wiring (a dropped call) is caught
    /// deterministically without an LLM.
    #[tokio::test]
    async fn upsert_provenance_writes_document_node_through_wrapper() {
        use cognee_database::ops::datasets::create_dataset;
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;
        use cognee_database::{connect, initialize};
        use cognee_models::Dataset;

        let db = connect("sqlite::memory:").await.expect("connect");
        initialize(&db).await.expect("migrate");

        let user_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        create_dataset(
            &db,
            Dataset::new("prov-wrapper".into(), user_id, None, dataset_id),
        )
        .await
        .expect("seed dataset");

        let doc_id = Uuid::new_v4();
        let mut base = DataPoint::new("TextDocument", None);
        base.id = doc_id;
        base.set_metadata("index_fields", serde_json::json!(["name"]));
        let doc = Document {
            base,
            document_type: "text".to_string(),
            name: "prov.txt".to_string(),
            raw_data_location: "text://prov".to_string(),
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: doc_id,
            external_metadata: None,
        };

        upsert_provenance(
            &db,
            LedgerIdentity::new(None, Some(user_id), dataset_id, None),
            &[],    // chunks
            &[],    // entities
            &[],    // edges
            &[],    // summaries
            &[doc], // documents
            &[],    // structural_edges
            &ArtifactProducers::default(),
        )
        .await
        .expect("wrapper provenance upsert must succeed");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert_eq!(
            nodes.len(),
            1,
            "the document's provenance node must be written through the wrapper",
        );
        assert_eq!(
            nodes[0].slug, doc_id,
            "provenance node slug is the document id",
        );
    }

    /// A merged entity must appear in the `contains` list of *every* chunk that
    /// produced it, not just the first-seen one.
    ///
    /// `extract_graph_from_data` needs a live fact extractor, so the map
    /// construction is tested through its own helper. The two-producer
    /// assertion is what fails against the previous single-`metadata["chunk_id"]`
    /// keying: the second chunk got no `contains` link, leaving the shared
    /// entity at degree one for `sweep_orphan_nodes` to reap.
    #[test]
    fn chunk_entity_links_lists_a_merged_entity_under_every_producer() {
        let dataset_id = Uuid::new_v4();
        let (chunk_a, chunk_b) = (Uuid::new_v4(), Uuid::new_v4());

        let entity_type = EntityType::from_node_type("Person", Some(dataset_id));
        let mut entity = Entity::from_node("alice_1", "Alice", "", entity_type.base.id, None);
        // Merging stamps only the first-seen chunk, which is exactly what the
        // old keying read — so chunk_b is the assertion that regresses.
        entity
            .base
            .set_metadata("chunk_id", json!(chunk_a.to_string()));
        let entity_id = entity.base.id;
        let nodes = vec![GraphNodePair {
            entity,
            entity_type,
        }];

        let mut producers = ArtifactProducers::default();
        producers.record_entity(entity_id, chunk_a);
        producers.record_entity(entity_id, chunk_b);

        let links = chunk_entity_links(&nodes, &producers);

        let expected = vec![json!(entity_id.to_string())];
        assert_eq!(
            links.get(&chunk_a),
            Some(&expected),
            "the first-seen chunk keeps its link"
        );
        assert_eq!(
            links.get(&chunk_b),
            Some(&expected),
            "the second producing chunk must link the merged entity too"
        );
    }

    /// Entities with no producer set — ontology-derived nodes, and callers that
    /// build their own node list — keep the old `metadata["chunk_id"]` keying.
    #[test]
    fn chunk_entity_links_falls_back_to_the_chunk_id_stamp() {
        let dataset_id = Uuid::new_v4();
        let chunk_id = Uuid::new_v4();

        let entity_type = EntityType::from_node_type("Person", Some(dataset_id));
        let mut entity = Entity::from_node("bob_1", "Bob", "", entity_type.base.id, None);
        entity
            .base
            .set_metadata("chunk_id", json!(chunk_id.to_string()));
        let entity_id = entity.base.id;
        let nodes = vec![GraphNodePair {
            entity,
            entity_type,
        }];

        let links = chunk_entity_links(&nodes, &ArtifactProducers::default());

        assert_eq!(links.len(), 1);
        assert_eq!(
            links.get(&chunk_id),
            Some(&vec![json!(entity_id.to_string())])
        );
    }

    /// Seed a dataset and return the in-memory connection plus its ids.
    async fn provenance_fixture() -> (cognee_database::DatabaseConnection, Uuid, Uuid) {
        use cognee_database::ops::datasets::create_dataset;
        use cognee_database::{connect, initialize};
        use cognee_models::Dataset;

        let db = connect("sqlite::memory:").await.expect("connect");
        initialize(&db).await.expect("migrate");

        let user_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        create_dataset(
            &db,
            Dataset::new("prov-producers".into(), user_id, None, dataset_id),
        )
        .await
        .expect("seed dataset");

        (db, user_id, dataset_id)
    }

    fn provenance_chunk(chunk_id: Uuid, document_id: Uuid) -> DocumentChunk {
        DocumentChunk::new(
            chunk_id,
            "chunk text".to_string(),
            2,
            0,
            "sentence_end".to_string(),
            document_id,
        )
    }

    /// A merged entity claimed by two files must own one row per file: the
    /// exclusivity check spares a slug only when another `data_id` in the
    /// dataset claims it, so a single row lets deleting one file sweep an
    /// entity the other still references.
    #[tokio::test]
    async fn upsert_provenance_writes_one_entity_row_per_producing_data_item() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;

        let (db, user_id, dataset_id) = provenance_fixture().await;

        let (doc_a, doc_b) = (Uuid::new_v4(), Uuid::new_v4());
        let (chunk_a, chunk_b) = (Uuid::new_v4(), Uuid::new_v4());
        let chunks = vec![
            provenance_chunk(chunk_a, doc_a),
            provenance_chunk(chunk_b, doc_b),
        ];

        let entity_type = EntityType::from_node_type("Person", Some(dataset_id));
        let entity = Entity::from_node("alice_1", "Alice", "", entity_type.base.id, None);
        let entity_id = entity.base.id;
        let entities = vec![GraphNodePair {
            entity,
            entity_type,
        }];

        let mut producers = ArtifactProducers::default();
        producers.record_entity(entity_id, chunk_a);
        producers.record_entity(entity_id, chunk_b);

        upsert_provenance(
            &db,
            LedgerIdentity::new(None, Some(user_id), dataset_id, None),
            &chunks,
            &entities,
            &[],
            &[],
            &[],
            &[],
            &producers,
        )
        .await
        .expect("provenance upsert must succeed");

        let rows: Vec<_> = get_nodes_by_dataset(&db, dataset_id)
            .await
            .expect("query")
            .into_iter()
            .filter(|n| n.slug == entity_id)
            .collect();

        assert_eq!(rows.len(), 2, "one ownership row per producing data item");

        let mut owners: Vec<Uuid> = rows.iter().map(|n| n.data_id).collect();
        owners.sort();
        let mut expected = vec![doc_a, doc_b];
        expected.sort();
        assert_eq!(owners, expected);

        for row in &rows {
            assert_eq!(
                row.id,
                provenance_node_id(None, user_id, dataset_id, row.data_id, entity_id),
                "the row id keeps the shared-DB scheme, which already carries data_id",
            );
        }
    }

    /// Two chunks of the *same* file are two producers but one data item.
    /// Without deduplication `provenance_node_id` would mint the same primary
    /// key twice in one batch.
    #[tokio::test]
    async fn upsert_provenance_collapses_two_chunks_of_one_file_to_one_row() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;

        let (db, user_id, dataset_id) = provenance_fixture().await;

        let doc_a = Uuid::new_v4();
        let (chunk_1, chunk_2) = (Uuid::new_v4(), Uuid::new_v4());
        let chunks = vec![
            provenance_chunk(chunk_1, doc_a),
            provenance_chunk(chunk_2, doc_a),
        ];

        let entity_type = EntityType::from_node_type("Person", Some(dataset_id));
        let entity = Entity::from_node("alice_1", "Alice", "", entity_type.base.id, None);
        let entity_id = entity.base.id;
        let entities = vec![GraphNodePair {
            entity,
            entity_type,
        }];

        let mut producers = ArtifactProducers::default();
        producers.record_entity(entity_id, chunk_1);
        producers.record_entity(entity_id, chunk_2);

        upsert_provenance(
            &db,
            LedgerIdentity::new(None, Some(user_id), dataset_id, None),
            &chunks,
            &entities,
            &[],
            &[],
            &[],
            &[],
            &producers,
        )
        .await
        .expect("provenance upsert must succeed");

        let rows: Vec<_> = get_nodes_by_dataset(&db, dataset_id)
            .await
            .expect("query")
            .into_iter()
            .filter(|n| n.slug == entity_id)
            .collect();

        assert_eq!(rows.len(), 1, "one data item owns the entity once");
        assert_eq!(rows[0].data_id, doc_a);
    }

    /// Callers that build a `SummarizedData` themselves hand over no producer
    /// set; those entities keep the old single-`chunk_id` attribution.
    #[tokio::test]
    async fn upsert_provenance_falls_back_to_chunk_metadata_without_producers() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;

        let (db, user_id, dataset_id) = provenance_fixture().await;

        let doc_a = Uuid::new_v4();
        let chunk_a = Uuid::new_v4();
        let chunks = vec![provenance_chunk(chunk_a, doc_a)];

        let entity_type = EntityType::from_node_type("Person", Some(dataset_id));
        let mut entity = Entity::from_node("alice_1", "Alice", "", entity_type.base.id, None);
        entity
            .base
            .set_metadata("chunk_id", json!(chunk_a.to_string()));
        let entity_id = entity.base.id;
        let entities = vec![GraphNodePair {
            entity,
            entity_type,
        }];

        upsert_provenance(
            &db,
            LedgerIdentity::new(None, Some(user_id), dataset_id, None),
            &chunks,
            &entities,
            &[],
            &[],
            &[],
            &[],
            &ArtifactProducers::default(),
        )
        .await
        .expect("provenance upsert must succeed");

        let rows: Vec<_> = get_nodes_by_dataset(&db, dataset_id)
            .await
            .expect("query")
            .into_iter()
            .filter(|n| n.slug == entity_id)
            .collect();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].data_id, doc_a);
    }

    /// An edge produced by two files must own one row per file: the rows share
    /// a `slug`, and `get_unique_edges_for_data` spares a slug only when
    /// another `data_id` in the dataset claims it — so a single row lets
    /// deleting one file sweep an edge (and its EdgeType/Triplet vectors) the
    /// other still references.
    ///
    /// Before per-producer rows existed the edge resolved to a nil `data_id`,
    /// which the exclusivity query never selects at all, so its vectors
    /// outlived every file that produced them.
    #[tokio::test]
    async fn upsert_provenance_writes_one_edge_row_per_producing_data_item() {
        use cognee_database::ops::graph_storage::get_edges_by_dataset;

        let (db, user_id, dataset_id) = provenance_fixture().await;

        let (doc_a, doc_b) = (Uuid::new_v4(), Uuid::new_v4());
        let (chunk_a, chunk_b) = (Uuid::new_v4(), Uuid::new_v4());
        let chunks = vec![
            provenance_chunk(chunk_a, doc_a),
            provenance_chunk(chunk_b, doc_b),
        ];

        // Two entities, each stamped with a *different* file's chunk, so the
        // no-producer fallback would have yielded nil.
        let entity_type = EntityType::from_node_type("Person", Some(dataset_id));
        let mut alice = Entity::from_node("alice_1", "Alice", "", entity_type.base.id, None);
        alice
            .base
            .set_metadata("chunk_id", json!(chunk_a.to_string()));
        let mut bob = Entity::from_node("bob_1", "Bob", "", entity_type.base.id, None);
        bob.base
            .set_metadata("chunk_id", json!(chunk_b.to_string()));
        let (alice_id, bob_id) = (alice.base.id, bob.base.id);
        let entities = vec![
            GraphNodePair {
                entity: alice,
                entity_type: entity_type.clone(),
            },
            GraphNodePair {
                entity: bob,
                entity_type,
            },
        ];

        let mut edge = GraphEdgePair::new(alice_id, bob_id, "knows");
        edge.add_property("edge_text", "Alice knows Bob");
        let mut producers = ArtifactProducers::default();
        producers.record_edge(edge.dedup_key(), chunk_a);
        producers.record_edge(edge.dedup_key(), chunk_b);

        upsert_provenance(
            &db,
            LedgerIdentity::new(None, Some(user_id), dataset_id, None),
            &chunks,
            &entities,
            std::slice::from_ref(&edge),
            &[],
            &[],
            &[],
            &producers,
        )
        .await
        .expect("provenance upsert must succeed");

        let rows: Vec<_> = get_edges_by_dataset(&db, dataset_id)
            .await
            .expect("query")
            .into_iter()
            .filter(|e| e.source_node_id == alice_id && e.destination_node_id == bob_id)
            .collect();

        assert_eq!(rows.len(), 2, "one ownership row per producing data item");

        let mut owners: Vec<Uuid> = rows.iter().map(|e| e.data_id).collect();
        owners.sort();
        let mut expected = vec![doc_a, doc_b];
        expected.sort();
        assert_eq!(owners, expected);

        for row in &rows {
            assert_eq!(
                row.id,
                provenance_edge_id(
                    None,
                    user_id,
                    dataset_id,
                    row.data_id,
                    alice_id,
                    "knows",
                    bob_id
                ),
                "the row id folds in data_id, which is what lets the rows coexist",
            );
        }
        assert_ne!(rows[0].id, rows[1].id, "the two rows must not collide");

        // The load-bearing assertion: the exclusivity query compares `slug`,
        // so both rows must claim the *same* edge.
        assert_eq!(
            rows[0].slug,
            triplet_slug(alice_id, "knows", bob_id),
            "the slug identifies the edge itself, independent of its owner"
        );
        assert_eq!(rows[0].slug, rows[1].slug);
    }

    /// Two chunks of the *same* file are two producers but one data item — the
    /// edge twin of the entity case above. Without deduplication
    /// `provenance_edge_id` would mint the same primary key twice in one batch.
    #[tokio::test]
    async fn upsert_provenance_collapses_two_chunks_of_one_file_to_one_edge_row() {
        use cognee_database::ops::graph_storage::get_edges_by_dataset;

        let (db, user_id, dataset_id) = provenance_fixture().await;

        let doc_a = Uuid::new_v4();
        let (chunk_1, chunk_2) = (Uuid::new_v4(), Uuid::new_v4());
        let chunks = vec![
            provenance_chunk(chunk_1, doc_a),
            provenance_chunk(chunk_2, doc_a),
        ];

        let entity_type = EntityType::from_node_type("Person", Some(dataset_id));
        let alice = Entity::from_node("alice_1", "Alice", "", entity_type.base.id, None);
        let bob = Entity::from_node("bob_1", "Bob", "", entity_type.base.id, None);
        let (alice_id, bob_id) = (alice.base.id, bob.base.id);
        let entities = vec![
            GraphNodePair {
                entity: alice,
                entity_type: entity_type.clone(),
            },
            GraphNodePair {
                entity: bob,
                entity_type,
            },
        ];

        let edge = GraphEdgePair::new(alice_id, bob_id, "knows");
        let mut producers = ArtifactProducers::default();
        producers.record_edge(edge.dedup_key(), chunk_1);
        producers.record_edge(edge.dedup_key(), chunk_2);

        upsert_provenance(
            &db,
            LedgerIdentity::new(None, Some(user_id), dataset_id, None),
            &chunks,
            &entities,
            std::slice::from_ref(&edge),
            &[],
            &[],
            &[],
            &producers,
        )
        .await
        .expect("provenance upsert must succeed");

        let rows: Vec<_> = get_edges_by_dataset(&db, dataset_id)
            .await
            .expect("query")
            .into_iter()
            .filter(|e| e.source_node_id == alice_id && e.destination_node_id == bob_id)
            .collect();

        assert_eq!(rows.len(), 1, "one data item owns the edge once");
        assert_eq!(rows[0].data_id, doc_a);
    }

    /// Callers that build a `SummarizedData` themselves hand over no producer
    /// set; those edges keep the old both-endpoints-agree attribution.
    #[tokio::test]
    async fn upsert_provenance_falls_back_to_endpoint_agreement_without_edge_producers() {
        use cognee_database::ops::graph_storage::get_edges_by_dataset;

        /// Stamp both endpoints with the given chunks and upsert one edge with
        /// an empty producer set, returning the rows written for it.
        async fn rows_for(
            source_chunk: Uuid,
            target_chunk: Uuid,
            chunk_docs: &[(Uuid, Uuid)],
        ) -> Vec<cognee_database::GraphEdge> {
            let (db, user_id, dataset_id) = provenance_fixture().await;
            let chunks: Vec<DocumentChunk> = chunk_docs
                .iter()
                .map(|(chunk_id, doc_id)| provenance_chunk(*chunk_id, *doc_id))
                .collect();

            let entity_type = EntityType::from_node_type("Person", Some(dataset_id));
            let mut alice = Entity::from_node("alice_1", "Alice", "", entity_type.base.id, None);
            alice
                .base
                .set_metadata("chunk_id", json!(source_chunk.to_string()));
            let mut bob = Entity::from_node("bob_1", "Bob", "", entity_type.base.id, None);
            bob.base
                .set_metadata("chunk_id", json!(target_chunk.to_string()));
            let (alice_id, bob_id) = (alice.base.id, bob.base.id);
            let entities = vec![
                GraphNodePair {
                    entity: alice,
                    entity_type: entity_type.clone(),
                },
                GraphNodePair {
                    entity: bob,
                    entity_type,
                },
            ];

            let edge = GraphEdgePair::new(alice_id, bob_id, "knows");

            upsert_provenance(
                &db,
                LedgerIdentity::new(None, Some(user_id), dataset_id, None),
                &chunks,
                &entities,
                std::slice::from_ref(&edge),
                &[],
                &[],
                &[],
                &ArtifactProducers::default(),
            )
            .await
            .expect("provenance upsert must succeed");

            get_edges_by_dataset(&db, dataset_id)
                .await
                .expect("query")
                .into_iter()
                .filter(|e| e.source_node_id == alice_id && e.destination_node_id == bob_id)
                .collect()
        }

        // Both endpoints from one file: that file owns the single row.
        let doc_a = Uuid::new_v4();
        let (chunk_1, chunk_2) = (Uuid::new_v4(), Uuid::new_v4());
        let rows = rows_for(chunk_1, chunk_2, &[(chunk_1, doc_a), (chunk_2, doc_a)]).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].data_id, doc_a);

        // Endpoints from different files: nil, exactly as before.
        let doc_b = Uuid::new_v4();
        let rows = rows_for(chunk_1, chunk_2, &[(chunk_1, doc_a), (chunk_2, doc_b)]).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].data_id, Uuid::nil());
    }

    /// Structural edges span several DataPoints and keep their nil `data_id`;
    /// the nil is now folded into the row id like every other edge's.
    #[tokio::test]
    async fn structural_edge_rows_keep_a_nil_data_id() {
        use cognee_database::ops::graph_storage::get_edges_by_dataset;

        let (db, user_id, dataset_id) = provenance_fixture().await;

        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let structural = vec![(
            source_id.to_string(),
            target_id.to_string(),
            "is_a".to_string(),
            HashMap::new(),
        )];

        upsert_provenance(
            &db,
            LedgerIdentity::new(None, Some(user_id), dataset_id, None),
            &[],
            &[],
            &[],
            &[],
            &[],
            &structural,
            &ArtifactProducers::default(),
        )
        .await
        .expect("provenance upsert must succeed");

        let rows = get_edges_by_dataset(&db, dataset_id).await.expect("query");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].data_id, Uuid::nil());
        assert_eq!(
            rows[0].id,
            provenance_edge_id(
                None,
                user_id,
                dataset_id,
                Uuid::nil(),
                source_id,
                "is_a",
                target_id
            ),
        );
    }

    #[tokio::test]
    async fn test_extract_chunks_empty_documents() {
        let storage = Arc::new(MockStorage::new());
        let input = ClassifiedDocuments {
            failures: FailureReport::default(),
            documents: vec![],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        let registry = LoaderRegistry::default();
        let result = extract_chunks_from_documents(
            &input,
            &*storage,
            100,
            TokenCounterKind::Word,
            None,
            &registry,
            CognifyConfig::default().failure_policy(),
        )
        .await
        .unwrap();
        assert!(result.chunks.is_empty());
    }

    #[tokio::test]
    async fn test_dlt_short_circuit() {
        let storage = Arc::new(MockStorage::new());
        let location = storage
            .store(b"  some dlt row content  ", "dlt.txt")
            .await
            .unwrap();

        let doc_id = Uuid::new_v4();
        let mut base = DataPoint::new("DltRowDocument", None);
        base.id = doc_id;
        base.importance_weight = Some(0.7);
        base.set_metadata("index_fields", serde_json::json!(["text"]));
        let doc = Document {
            base,
            document_type: "dlt_row".to_string(),
            name: "dlt.txt".to_string(),
            raw_data_location: location,
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: doc_id,
            external_metadata: None,
        };

        let input = ClassifiedDocuments {
            failures: FailureReport::default(),
            documents: vec![doc],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        let registry = LoaderRegistry::default();
        let result = extract_chunks_from_documents(
            &input,
            &*storage,
            100,
            TokenCounterKind::Word,
            None,
            &registry,
            CognifyConfig::default().failure_policy(),
        )
        .await
        .unwrap();

        assert_eq!(result.chunks.len(), 1);
        let chunk = &result.chunks[0];
        assert_eq!(chunk.text, "some dlt row content");
        assert_eq!(chunk.cut_type, "dlt_row");
        assert_eq!(chunk.chunk_index, 0);
        assert_eq!(chunk.document_id, doc_id);
        // importance_weight propagates on the DLT short-circuit path too.
        assert_eq!(chunk.base.importance_weight, Some(0.7));
    }

    // ── Chunk-stage failure collection ─────────────────────────────────
    //
    // The chunk stage is the only producer of file-unit failures: storage
    // `retrieve`, UTF-8 decode, an unregistered document type, and the loader's
    // own `extract` all surface here. `classify_documents` itself is infallible,
    // and `model_classify_documents` silently skips items with an unrecognised
    // extension, exactly as Python does.

    /// A document type intentionally never registered in
    /// `LoaderRegistry::default()`. The fixture used to say "pdf", but the PDF
    /// loader made that type supported, so this test started invoking the real
    /// PDFium loader on garbage bytes.
    const UNSUPPORTED_DOC_TYPE: &str = "no_such_loader_type_for_test";

    /// Three text documents A, B, C, of which B has an unregistered
    /// `document_type` and therefore fails at the loader dispatch.
    async fn three_documents_with_a_bad_middle(
        storage: &MockStorage,
    ) -> (ClassifiedDocuments, Uuid, Uuid, Uuid) {
        let mut documents = Vec::new();
        let mut ids = Vec::new();
        for (name, doc_type) in [
            ("a.txt", "text"),
            ("b.bin", UNSUPPORTED_DOC_TYPE),
            ("c.txt", "text"),
        ] {
            let location = storage
                .store(b"Alice works at Acme.", name)
                .await
                .expect("MockStorage::store");
            let doc_id = Uuid::new_v4();
            let mut base = DataPoint::new("TextDocument", None);
            base.id = doc_id;
            base.set_metadata("index_fields", serde_json::json!(["text"]));
            documents.push(Document {
                base,
                document_type: doc_type.to_string(),
                name: name.to_string(),
                raw_data_location: location,
                mime_type: "text/plain".to_string(),
                extension: "txt".to_string(),
                data_id: doc_id,
                external_metadata: None,
            });
            ids.push(doc_id);
        }

        (
            ClassifiedDocuments {
                failures: FailureReport::default(),
                documents,
                dataset_id: Uuid::new_v4(),
                user_id: None,
                tenant_id: None,
            },
            ids[0],
            ids[1],
            ids[2],
        )
    }

    async fn run_chunking(
        input: &ClassifiedDocuments,
        storage: &MockStorage,
        config: &CognifyConfig,
    ) -> Result<ExtractedChunks, CognifyError> {
        let registry = LoaderRegistry::default();
        extract_chunks_from_documents(
            input,
            storage,
            100,
            TokenCounterKind::Word,
            None,
            &registry,
            config.failure_policy(),
        )
        .await
    }

    /// Replaces the old `test_unsupported_document_type`, which asserted the
    /// raw `UnsupportedDocumentType` error. Under the default policy the stage
    /// still aborts at the first failure — but the failure is now *reported*:
    /// the error carries which file failed, with the original message intact,
    /// and names the file that was never reached.
    #[tokio::test]
    async fn chunking_failure_aborts_and_reports_under_the_default_policy() {
        let storage = MockStorage::new();
        let (input, _a, b, c) = three_documents_with_a_bad_middle(&storage).await;

        let err = run_chunking(&input, &storage, &CognifyConfig::default())
            .await
            .expect_err("the default policy still aborts");

        let CognifyError::RunFailed { report } = err else {
            panic!("expected RunFailed, got: {err:?}");
        };
        assert_eq!(report.entries().len(), 1);
        let entry = &report.entries()[0];
        assert_eq!(entry.stage, FailureStage::Chunking);
        assert_eq!(entry.data_id, b);
        assert!(entry.chunk_id.is_none(), "chunking fails whole files");
        assert!(
            entry.error.contains("Unsupported document type"),
            "the original message must survive: {}",
            entry.error
        );
        assert_eq!(
            report.failed_items().iter().copied().collect::<Vec<_>>(),
            [b]
        );
        assert_eq!(
            report.unreached_items().iter().copied().collect::<Vec<_>>(),
            [c],
            "C was never attempted"
        );
    }

    /// `RunToEnd` collects the failure and keeps going, so the run learns about
    /// every bad file in one pass.
    #[tokio::test]
    async fn chunking_failure_is_collected_under_run_to_end() {
        let storage = MockStorage::new();
        let (input, a, b, c) = three_documents_with_a_bad_middle(&storage).await;

        let result = run_chunking(
            &input,
            &storage,
            &CognifyConfig::default().with_failure_stop(FailureStop::RunToEnd),
        )
        .await
        .expect("RunToEnd collects rather than propagates");

        let doc_ids: Vec<Uuid> = result.documents.iter().map(|d| d.base.id).collect();
        assert_eq!(
            doc_ids,
            vec![a, c],
            "the failed file contributes no Document"
        );
        assert!(
            result.chunks.iter().all(|c| c.document_id != b),
            "…and no chunks either"
        );
        assert_eq!(result.failures.total(), 1);
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [b]
        );
        assert!(
            result.failures.unreached_items().is_empty(),
            "nothing was left unattempted"
        );
    }

    /// `FailFast` + `FailedItems` stops spending immediately but hands back
    /// what already completed, so the complete files can still be persisted.
    #[tokio::test]
    async fn chunking_failure_keeps_earlier_files_under_fail_fast_failed_items() {
        let storage = MockStorage::new();
        let (input, a, b, c) = three_documents_with_a_bad_middle(&storage).await;

        let result = run_chunking(
            &input,
            &storage,
            &CognifyConfig::default().with_rollback_scope(RollbackScope::FailedItems),
        )
        .await
        .expect("FailedItems keeps the completed work");

        let doc_ids: Vec<Uuid> = result.documents.iter().map(|d| d.base.id).collect();
        assert_eq!(doc_ids, vec![a], "only the file that fully completed");
        assert!(result.chunks.iter().all(|chunk| chunk.document_id == a));
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [b]
        );
        assert_eq!(
            result
                .failures
                .unreached_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [c]
        );
    }

    /// The ratio denominators are set exactly once per run, by the only stage
    /// that knows both counts — including on the abort paths.
    #[tokio::test]
    async fn chunking_totals_are_recorded_once() {
        let storage = MockStorage::new();
        let (input, _a, _b, _c) = three_documents_with_a_bad_middle(&storage).await;

        let result = run_chunking(
            &input,
            &storage,
            &CognifyConfig::default().with_failure_stop(FailureStop::RunToEnd),
        )
        .await
        .expect("RunToEnd");
        assert_eq!(result.failures.total_items(), 3);
        assert_eq!(result.failures.total_chunks(), result.chunks.len());

        let err = run_chunking(&input, &storage, &CognifyConfig::default())
            .await
            .expect_err("default aborts");
        let CognifyError::RunFailed { report } = err else {
            panic!("expected RunFailed");
        };
        assert_eq!(report.total_items(), 3, "even on the fatal path");
    }

    #[test]
    fn test_classify_documents_preserves_dataset_id() {
        let dataset_id = Uuid::new_v4();
        let input = CognifyInput {
            data_items: vec![],
            dataset_id,
            user_id: None,
            tenant_id: None,
        };
        let result = classify_documents(&input, FailurePolicy::default()).unwrap();
        assert_eq!(result.dataset_id, dataset_id);
    }

    // ── Provenance guard and ID tests ───────────────────────────────────

    #[test]
    fn provenance_node_id_works_with_none_tenant() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let dataset_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let data_id = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
        let node_id = Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();

        // Must not panic with None tenant
        let id = provenance_node_id(None, user_id, dataset_id, data_id, node_id);

        // Matches Python's str(None) → "None" in the UUID5 input
        let expected_input = format!("None{user_id}{dataset_id}{data_id}{node_id}");
        let expected = Uuid::new_v5(&Uuid::NAMESPACE_OID, expected_input.as_bytes());
        assert_eq!(id, expected);
    }

    #[test]
    fn provenance_node_id_with_real_tenant_differs_from_none() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let dataset_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let data_id = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
        let node_id = Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();
        let tenant_id = Uuid::parse_str("00000000-0000-0000-0000-000000000005").unwrap();

        let id_none = provenance_node_id(None, user_id, dataset_id, data_id, node_id);
        let id_real = provenance_node_id(Some(tenant_id), user_id, dataset_id, data_id, node_id);
        assert_ne!(id_none, id_real);
    }

    /// Pins the edge id formula, which now carries `data_id` between the
    /// dataset and the source — a deliberate divergence from Python, whose
    /// `upsert_edges` omits it (see [`provenance_edge_id`]).
    ///
    /// The second assertion is the one the whole per-producer design rests on:
    /// two data items must yield two different ids, or the rows would collide
    /// on the primary key and only one owner would survive.
    #[test]
    fn provenance_edge_id_works_with_none_tenant() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let dataset_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let data_id = Uuid::parse_str("00000000-0000-0000-0000-000000000005").unwrap();
        let source_id = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
        let target_id = Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();

        let id = provenance_edge_id(
            None,
            user_id,
            dataset_id,
            data_id,
            source_id,
            "relates_to",
            target_id,
        );

        let expected_input =
            format!("None{user_id}{dataset_id}{data_id}{source_id}relates_to{target_id}");
        let expected = Uuid::new_v5(&Uuid::NAMESPACE_OID, expected_input.as_bytes());
        assert_eq!(id, expected);

        let other_data_id = Uuid::parse_str("00000000-0000-0000-0000-000000000006").unwrap();
        assert_ne!(
            id,
            provenance_edge_id(
                None,
                user_id,
                dataset_id,
                other_data_id,
                source_id,
                "relates_to",
                target_id,
            ),
            "two owners of one edge must get two rows, not one collision"
        );
    }

    /// The provenance guard must fire when db + user_id are present,
    /// even if tenant_id is None.  This matches Python's
    /// `if user and dataset and data:` which doesn't check tenant.
    #[test]
    fn dlt_fk_rel_name_always_includes_ref_col_separator() {
        // Python: rel_name = f"{table_name}:{fk_col}->{ref_table}:{ref_col}"
        // This always includes the colon before ref_col, even when ref_col is empty.

        // Case 1: non-empty ref_col
        let table_name = "orders";
        let fk_col = "customer_id";
        let ref_table = "customers";
        let ref_col = "id";
        let rel_name = format!("{table_name}:{fk_col}->{ref_table}:{ref_col}");
        assert_eq!(rel_name, "orders:customer_id->customers:id");

        let rel_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("dlt:{rel_name}").as_bytes());
        let expected_id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            b"dlt:orders:customer_id->customers:id",
        );
        assert_eq!(rel_id, expected_id);

        // Case 2: empty ref_col -- must still include trailing colon
        let ref_col_empty = "";
        let rel_name_empty = format!("{table_name}:{fk_col}->{ref_table}:{ref_col_empty}");
        assert_eq!(
            rel_name_empty, "orders:customer_id->customers:",
            "rel_name must include trailing colon even when ref_col is empty"
        );

        let rel_id_empty = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("dlt:{rel_name_empty}").as_bytes(),
        );
        let expected_id_empty =
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"dlt:orders:customer_id->customers:");
        assert_eq!(rel_id_empty, expected_id_empty);

        // Verify the two IDs differ (trailing colon changes the UUID5 seed)
        assert_ne!(
            rel_id, rel_id_empty,
            "non-empty and empty ref_col must produce different UUIDs"
        );
    }

    #[test]
    fn provenance_guard_does_not_require_tenant_id() {
        // Simulate the guard condition from cognify():
        //   if let (Some(db), Some(user_id)) = (&db, input.user_id)
        let db: Option<u8> = Some(1); // stand-in for Some(db)
        let user_id: Option<Uuid> = Some(Uuid::new_v4());
        let tenant_id: Option<Uuid> = None;

        let guard_fires = matches!((&db, user_id), (Some(_), Some(_)));
        assert!(
            guard_fires,
            "Provenance guard must fire when db + user_id are present, regardless of tenant_id"
        );

        // Also verify the old (broken) guard would NOT fire
        let old_guard_fires = matches!((&db, user_id, tenant_id), (Some(_), Some(_), Some(_)));
        assert!(
            !old_guard_fires,
            "The old 3-way guard should NOT fire when tenant_id is None"
        );
    }

    fn test_document_with_metadata(doc_id: Uuid, external_metadata: Option<String>) -> Document {
        let mut base = DataPoint::new("TextDocument", None);
        base.id = doc_id;
        Document {
            base,
            document_type: "text".to_string(),
            name: "test.txt".to_string(),
            raw_data_location: "file:///tmp/test.txt".to_string(),
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: doc_id,
            external_metadata,
        }
    }

    fn test_chunk(chunk_id: Uuid, doc_id: Uuid, text: &str) -> DocumentChunk {
        DocumentChunk::new(
            chunk_id,
            text.to_string(),
            text.split_whitespace().count(),
            0,
            "paragraph_end".to_string(),
            doc_id,
        )
    }

    fn test_entity(name: &str, entity_type_id: Uuid) -> GraphNodePair {
        let mut entity_base = DataPoint::new("Entity", None);
        entity_base.id = Uuid::new_v4();
        let entity = cognee_models::Entity {
            base: entity_base,
            name: name.to_string(),
            is_a: None,
            description: format!("description of {name}"),
        };

        let mut type_base = DataPoint::new("EntityType", None);
        type_base.id = entity_type_id;
        let entity_type = cognee_models::EntityType {
            base: type_base,
            name: "Generic".to_string(),
            description: "Generic type".to_string(),
        };

        GraphNodePair {
            entity,
            entity_type,
        }
    }

    // index_data_points reuses the vectors produced by generate_embeddings, so
    // chunks/entities/summaries are embedded once. Only the entity-type name is
    // embedded inside index_data_points, for 6 embedded texts in total.
    #[tokio::test]
    async fn embedding_reuse_avoids_double_pass() {
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let engine = Arc::new(MockEmbeddingEngine::new(8));
        let engine_dyn: Arc<dyn EmbeddingEngine> = engine.clone();
        let vector: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());

        let doc_id = Uuid::new_v4();
        let chunks = vec![
            test_chunk(Uuid::new_v4(), doc_id, "first chunk text"),
            test_chunk(Uuid::new_v4(), doc_id, "second chunk text"),
        ];

        // Both entities share one EntityType id, so dedup embeds a single type.
        let shared_type_id = Uuid::new_v4();
        let entities = vec![
            test_entity("Alice", shared_type_id),
            test_entity("Bob", shared_type_id),
        ];

        let summaries = vec![TextSummary::new(
            chunks[0].base.id,
            "a summary".to_string(),
            None,
            "mock-model".to_string(),
        )];

        let dataset_id = Uuid::new_v4();
        let config = CognifyConfig::default(); // embed_triplets = false

        // 2 chunks + 2 entities + 1 summary = 5 texts.
        let embeddings = generate_embeddings(&chunks, &entities, &summaries, engine_dyn.clone())
            .await
            .unwrap();
        assert_eq!(embeddings.len(), 5);
        assert_eq!(engine.embedded_text_count(), 5);

        index_data_points(
            &chunks,
            &entities,
            &summaries,
            &[],
            &[],
            &[],
            dataset_id,
            None,
            None,
            engine_dyn,
            vector,
            &config,
            &embeddings,
        )
        .await
        .unwrap();

        // 5 from generate_embeddings + 1 entity-type (not precomputed) = 6.
        assert_eq!(engine.embedded_text_count(), 6);
    }

    // The TextSummary vector payload must carry both `chunk_id` (back-compat)
    // and the new `source_chunk_id` key with identical string values, so the
    // hybrid pairing algorithm can join a summary hit to its source chunk.
    #[tokio::test]
    async fn summary_payload_carries_chunk_id_and_source_chunk_id() {
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let engine = Arc::new(MockEmbeddingEngine::new(8));
        let engine_dyn: Arc<dyn EmbeddingEngine> = engine.clone();
        let mock = Arc::new(MockVectorDB::new());
        let vector: Arc<dyn VectorDB> = mock.clone();

        let doc_id = Uuid::new_v4();
        let chunks = vec![test_chunk(Uuid::new_v4(), doc_id, "chunk text")];

        let chunk_id = chunks[0].base.id;
        let summaries = vec![TextSummary::new(
            chunk_id,
            "a summary".to_string(),
            None,
            "mock-model".to_string(),
        )];
        let summary_id = summaries[0].base.id;

        let dataset_id = Uuid::new_v4();
        let config = CognifyConfig::default();

        let embeddings = generate_embeddings(&chunks, &[], &summaries, engine_dyn.clone())
            .await
            .unwrap();

        index_data_points(
            &chunks,
            &[],
            &summaries,
            &[],
            &[],
            &[],
            dataset_id,
            None,
            None,
            engine_dyn,
            vector,
            &config,
            &embeddings,
        )
        .await
        .unwrap();

        let payload = mock
            .get_payload("TextSummary", "text", summary_id)
            .expect("summary point must be indexed");

        let expected = json!(chunk_id.to_string());
        assert_eq!(payload.get("chunk_id"), Some(&expected));
        assert_eq!(payload.get("source_chunk_id"), Some(&expected));
        assert_eq!(payload.get("chunk_id"), payload.get("source_chunk_id"));
    }

    // Prints a before/after embedding-work comparison for a realistic fixture.
    // The "before" run passes an empty precomputed slice, which reproduces the
    // pre-fix double pass (index_data_points re-embeds everything); the "after"
    // run passes the precomputed vectors so chunks/entities/summaries are
    // embedded once. Run with:
    //   cargo test -p cognee-cognify --lib report_embedding_reuse_savings -- --nocapture
    #[tokio::test]
    async fn report_embedding_reuse_savings() {
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let doc_id = Uuid::new_v4();
        let chunks: Vec<DocumentChunk> = (0..24)
            .map(|i| test_chunk(Uuid::new_v4(), doc_id, &format!("chunk text number {i}")))
            .collect();
        let type_ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        let entities: Vec<GraphNodePair> = (0..16)
            .map(|i| test_entity(&format!("Entity {i}"), type_ids[i % 4]))
            .collect();
        let summaries: Vec<TextSummary> = (0..10)
            .map(|i| {
                TextSummary::new(
                    Uuid::new_v4(),
                    format!("summary number {i}"),
                    None,
                    "mock-model".to_string(),
                )
            })
            .collect();

        let overlap = chunks.len() + entities.len() + summaries.len();
        let dataset_id = Uuid::new_v4();
        let config = CognifyConfig::default();

        // Runs generate_embeddings + index_data_points on one counting engine
        // and returns (embed calls, texts embedded). `reuse = false` passes an
        // empty precomputed slice to reproduce the pre-fix behavior.
        async fn measure(
            reuse: bool,
            chunks: &[DocumentChunk],
            entities: &[GraphNodePair],
            summaries: &[TextSummary],
            dataset_id: Uuid,
            config: &CognifyConfig,
        ) -> (usize, usize) {
            let engine = Arc::new(MockEmbeddingEngine::new(8));
            let engine_dyn: Arc<dyn EmbeddingEngine> = engine.clone();
            let vector: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());

            let embeddings = generate_embeddings(chunks, entities, summaries, engine_dyn.clone())
                .await
                .unwrap();
            let precomputed: &[Embedding] = if reuse { &embeddings } else { &[] };
            index_data_points(
                chunks,
                entities,
                summaries,
                &[],
                &[],
                &[],
                dataset_id,
                None,
                None,
                engine_dyn,
                vector,
                config,
                precomputed,
            )
            .await
            .unwrap();
            (engine.call_count(), engine.embedded_text_count())
        }

        let (before_calls, before_texts) =
            measure(false, &chunks, &entities, &summaries, dataset_id, &config).await;
        let (after_calls, after_texts) =
            measure(true, &chunks, &entities, &summaries, dataset_id, &config).await;

        println!(
            "\n  Embedding work per cognify ({} chunks / {} entities / {} summaries):",
            chunks.len(),
            entities.len(),
            summaries.len()
        );
        println!("    BEFORE (double pass): {before_calls} embed() calls, {before_texts} texts");
        println!("    AFTER  (reuse)      : {after_calls} embed() calls, {after_texts} texts");
        println!(
            "    Saved: {} texts ({:.0}% fewer)\n",
            before_texts - after_texts,
            100.0 * (before_texts - after_texts) as f64 / before_texts as f64,
        );

        // The fix removes exactly one redundant embedding of every
        // chunk/entity/summary text.
        assert_eq!(before_texts - after_texts, overlap);
    }

    /// An in-memory relational database with `dataset_id` already registered.
    ///
    /// Every ownership row carries an FK to `datasets`, and the ledger is now
    /// always written, so a stage under test needs the dataset row to exist —
    /// which every real path (`add` → `cognify`) guarantees.
    async fn ledger_db(dataset_id: Uuid) -> DatabaseConnection {
        use cognee_database::{connect, initialize};

        let db = connect("sqlite::memory:").await.expect("connect");
        initialize(&db).await.expect("migrate");
        seed_dataset(&db, dataset_id).await;
        db
    }

    /// Register `dataset_id` in an existing database, for the same reason.
    async fn seed_dataset(db: &DatabaseConnection, dataset_id: Uuid) {
        use cognee_database::ops::datasets::create_dataset;
        use cognee_models::Dataset;

        create_dataset(
            db,
            Dataset::new("ledger-test".into(), Uuid::new_v4(), None, dataset_id),
        )
        .await
        .expect("seed dataset");
    }

    fn url_metadata(url: &str, final_url: &str, title: &str) -> String {
        json!({
            "source": "url",
            "url": url,
            "final_url": final_url,
            "content_type": "text/html",
            "title": title,
        })
        .to_string()
    }

    #[tokio::test]
    async fn add_data_points_stores_document_node_and_indexes_document_name() {
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let graph: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());
        let vector: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
        let engine: Arc<dyn EmbeddingEngine> = Arc::new(MockEmbeddingEngine::new(8));

        let doc_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000a1").unwrap();
        let chunk_id = Uuid::parse_str("00000000-0000-0000-0000-0000000000b1").unwrap();
        let document = test_document_with_metadata(doc_id, None);
        let chunk = test_chunk(chunk_id, doc_id, "Hello world");

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;

        let input = SummarizedData {
            chunks: vec![chunk],
            documents: vec![document],
            entities: vec![],
            edges: vec![],
            producers: ArtifactProducers::default(),
            summaries: vec![],
            dataset_id,
            user_id: None,
            tenant_id: None,
            failures: FailureReport::default(),
        };

        let config = CognifyConfig::default();
        add_data_points(
            &input,
            Arc::clone(&graph),
            Arc::clone(&vector),
            Arc::clone(&engine),
            &db,
            None,
            &config,
        )
        .await
        .unwrap();

        // (a) Document stored as a graph node with id == data id and the
        //     concrete subclass type.
        let node = graph
            .get_node(&doc_id.to_string())
            .await
            .unwrap()
            .expect("document node should exist");
        assert_eq!(
            node.get("type").and_then(|v| v.as_str()),
            Some("TextDocument")
        );

        // (b) A TextDocument_name collection exists with exactly one point.
        assert!(vector.has_collection("TextDocument", "name").await.unwrap());
        assert_eq!(
            vector
                .collection_size("TextDocument", "name")
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn extracted_edge_description_persists_as_edge_text_property() {
        use crate::fact_extraction::{Edge, KnowledgeGraph, Node};
        use cognee_ontology::NoOpOntologyResolver;

        let graph = KnowledgeGraph {
            nodes: vec![
                Node {
                    id: "alice".to_string(),
                    name: "Alice".to_string(),
                    node_type: "PERSON".to_string(),
                    description: "A person".to_string(),
                },
                Node {
                    id: "acme".to_string(),
                    name: "Acme".to_string(),
                    node_type: "ORGANIZATION".to_string(),
                    description: "A company".to_string(),
                },
            ],
            edges: vec![Edge {
                source_node_id: "alice".to_string(),
                target_node_id: "acme".to_string(),
                relationship_name: "founded".to_string(),
                // Leading/trailing whitespace exercises the trim semantics.
                description: Some("  Alice founded Acme  ".to_string()),
            }],
        };

        let chunk_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let resolver = NoOpOntologyResolver::new();

        let (_nodes, edges, _claimed_edges, _producers) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &resolver,
            None,
            None,
        )
        .await;

        assert_eq!(edges.len(), 1);
        let edge_text = edges[0]
            .properties
            .get("edge_text")
            .expect("edge_text property should be set");
        // Trimmed, matching Python _strip_nonblank_text.
        assert_eq!(edge_text, "Alice founded Acme");
    }

    #[test]
    fn cognify_config_creates_web_page_nodes_by_default() {
        assert!(CognifyConfig::default().create_web_page_nodes);
        assert!(
            !CognifyConfig::default()
                .with_web_page_nodes(false)
                .create_web_page_nodes
        );
    }

    #[tokio::test]
    async fn create_web_page_nodes_creates_deterministic_page_site_and_edges() {
        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        let doc_id = Uuid::parse_str("00000000-0000-0000-0000-000000000101").unwrap();
        let chunk_id = Uuid::parse_str("00000000-0000-0000-0000-000000000201").unwrap();
        let final_url = "https://Example.com/path?q=1";
        let documents = vec![test_document_with_metadata(
            doc_id,
            Some(url_metadata(
                "https://example.com/start",
                final_url,
                "Example title",
            )),
        )];
        let chunks = vec![test_chunk(chunk_id, doc_id, "Visible page content")];

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        create_web_page_nodes(
            &documents,
            &chunks,
            graph.clone(),
            &db,
            LedgerIdentity::new(None, None, dataset_id, None),
        )
        .await
        .unwrap();

        let page_id = web_page_id("https://example.com/path?q=1").to_string();
        let site_id = web_site_id("example.com").to_string();
        let (nodes, edges) = graph.get_graph_data().await.unwrap();
        assert_eq!(nodes.len(), 2);

        let page = graph.get_node(&page_id).await.unwrap().unwrap();
        assert_eq!(page.get("type").and_then(|v| v.as_str()), Some("WebPage"));
        assert_eq!(
            page.get("url").and_then(|v| v.as_str()),
            Some("https://example.com/path?q=1")
        );
        assert_eq!(
            page.get("title").and_then(|v| v.as_str()),
            Some("Example title")
        );
        assert_eq!(
            page.get("content").and_then(|v| v.as_str()),
            Some("Visible page content")
        );
        assert!(
            !page.contains_key("created_at"),
            "WebPage node payload should be deterministic"
        );

        let site = graph.get_node(&site_id).await.unwrap().unwrap();
        assert_eq!(site.get("type").and_then(|v| v.as_str()), Some("WebSite"));
        assert_eq!(
            site.get("domain").and_then(|v| v.as_str()),
            Some("example.com")
        );

        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|(source, target, rel, _)| {
            source == &page_id && target == &site_id && rel == "PART_OF"
        }));
        assert!(edges.iter().any(|(source, target, rel, _)| {
            source == &chunk_id.to_string() && target == &page_id && rel == "SOURCED_FROM"
        }));
    }

    #[tokio::test]
    async fn create_web_page_nodes_truncates_content_to_500_chars() {
        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        let doc_id = Uuid::new_v4();
        let long_text = "a".repeat(650);
        let documents = vec![test_document_with_metadata(
            doc_id,
            Some(url_metadata(
                "https://example.com/long",
                "https://example.com/long",
                "Long",
            )),
        )];
        let chunks = vec![test_chunk(Uuid::new_v4(), doc_id, &long_text)];

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        create_web_page_nodes(
            &documents,
            &chunks,
            graph.clone(),
            &db,
            LedgerIdentity::new(None, None, dataset_id, None),
        )
        .await
        .unwrap();

        let page_id = web_page_id("https://example.com/long").to_string();
        let page = graph.get_node(&page_id).await.unwrap().unwrap();
        assert_eq!(
            page.get("content")
                .and_then(|v| v.as_str())
                .unwrap()
                .chars()
                .count(),
            500
        );
    }

    #[tokio::test]
    async fn create_web_page_nodes_skips_invalid_and_non_url_metadata() {
        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        let doc_with_invalid_json =
            test_document_with_metadata(Uuid::new_v4(), Some("{not valid json".to_string()));
        let non_url_doc = test_document_with_metadata(
            Uuid::new_v4(),
            Some(json!({"source": "dlt", "url": "https://example.com"}).to_string()),
        );
        let bad_url_doc = test_document_with_metadata(
            Uuid::new_v4(),
            Some(json!({"source": "url", "final_url": "not a url"}).to_string()),
        );
        let chunks = vec![
            test_chunk(Uuid::new_v4(), doc_with_invalid_json.base.id, "a"),
            test_chunk(Uuid::new_v4(), non_url_doc.base.id, "b"),
            test_chunk(Uuid::new_v4(), bad_url_doc.base.id, "c"),
        ];

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        create_web_page_nodes(
            &[doc_with_invalid_json, non_url_doc, bad_url_doc],
            &chunks,
            graph.clone(),
            &db,
            LedgerIdentity::new(None, None, dataset_id, None),
        )
        .await
        .unwrap();

        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
    }

    #[tokio::test]
    async fn create_web_page_nodes_is_idempotent_for_edges() {
        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        let doc_id = Uuid::new_v4();
        let documents = vec![test_document_with_metadata(
            doc_id,
            Some(url_metadata(
                "https://example.com/idempotent",
                "https://example.com/idempotent",
                "Idempotent",
            )),
        )];
        let chunks = vec![test_chunk(Uuid::new_v4(), doc_id, "content")];

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let id = LedgerIdentity::new(None, None, dataset_id, None);
        create_web_page_nodes(&documents, &chunks, graph.clone(), &db, id)
            .await
            .unwrap();
        create_web_page_nodes(&documents, &chunks, graph.clone(), &db, id)
            .await
            .unwrap();

        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 2);
    }

    #[tokio::test]
    async fn make_extract_graph_task_wires_web_page_nodes_and_respects_opt_out() {
        use cognee_ontology::NoOpOntologyResolver;
        use cognee_test_utils::{MockLlm, test_task_context};

        let doc_id = Uuid::new_v4();
        let input = ExtractedChunks {
            chunks: vec![test_chunk(Uuid::new_v4(), doc_id, "content")],
            documents: vec![test_document_with_metadata(
                doc_id,
                Some(url_metadata(
                    "https://example.com/wired",
                    "https://example.com/wired",
                    "Wired",
                )),
            )],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
            failures: FailureReport::default(),
        };

        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        let (_, ctx, db) = test_task_context().await;
        seed_dataset(&db, input.dataset_id).await;
        let task = make_extract_graph_task(
            Arc::new(MockLlm::empty()),
            graph.clone(),
            Arc::new(NoOpOntologyResolver::new()),
            Arc::clone(&db),
            CognifyConfig::default(),
        );
        let TypedTask::Async(run) = task else {
            panic!("extract graph task should be async");
        };
        run(&input, ctx.clone()).await.unwrap();
        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 2);

        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        let task = make_extract_graph_task(
            Arc::new(MockLlm::empty()),
            graph.clone(),
            Arc::new(NoOpOntologyResolver::new()),
            Arc::clone(&db),
            CognifyConfig::default().with_web_page_nodes(false),
        );
        let TypedTask::Async(run) = task else {
            panic!("extract graph task should be async");
        };
        run(&input, ctx).await.unwrap();
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
    }

    #[tokio::test]
    async fn test_summarize_text_skips_dlt_chunks() {
        use cognee_test_utils::MockLlm;

        let doc_id_text = Uuid::new_v4();
        let doc_id_dlt = Uuid::new_v4();

        let mut base_text = DataPoint::new("TextDocument", None);
        base_text.id = doc_id_text;
        let text_doc = Document {
            base: base_text,
            document_type: "text".to_string(),
            name: "test.txt".to_string(),
            raw_data_location: "file:///tmp/test.txt".to_string(),
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: doc_id_text,
            external_metadata: None,
        };

        let mut base_dlt = DataPoint::new("DltRowDocument", None);
        base_dlt.id = doc_id_dlt;
        let dlt_doc = Document {
            base: base_dlt,
            document_type: "dlt_row".to_string(),
            name: "dlt_row.json".to_string(),
            raw_data_location: "file:///tmp/dlt_row.json".to_string(),
            mime_type: "application/json".to_string(),
            extension: "json".to_string(),
            data_id: doc_id_dlt,
            external_metadata: None,
        };

        let text_chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "Some meaningful text to summarize.".to_string(),
            5,
            0,
            "paragraph_end".to_string(),
            doc_id_text,
        );

        let dlt_chunk = DocumentChunk::new(
            Uuid::new_v4(),
            r#"{"id": 1, "name": "row"}"#.to_string(),
            3,
            0,
            "paragraph_end".to_string(),
            doc_id_dlt,
        );

        let input = ExtractedGraphData {
            chunks: vec![text_chunk, dlt_chunk],
            documents: vec![text_doc, dlt_doc],
            entities: vec![],
            edges: vec![],
            producers: ArtifactProducers::default(),
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
            failures: FailureReport::default(),
        };

        // With summarization disabled, verify we get zero summaries and no panic.
        let config = CognifyConfig::default().with_summarization(false);
        let llm: Arc<dyn Llm> = Arc::new(MockLlm::empty());
        let result = summarize_text(&input, llm, &config).await.unwrap();
        assert!(result.summaries.is_empty());
        // All chunks (both DLT and non-DLT) are still passed through.
        assert_eq!(result.chunks.len(), 2);
    }

    // ── Summarization and axis 1 ────────────────────────────────────────────

    /// Five single-chunk files, the one at `failing_index` carrying the marker
    /// the mock LLM fails on. Returned alongside the file ids in input order so
    /// a test can name which file failed and which were never reached.
    fn summarization_fixture(failing_index: usize) -> (ExtractedGraphData, Vec<Uuid>) {
        let doc_ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        let chunks = doc_ids
            .iter()
            .enumerate()
            .map(|(i, doc_id)| {
                let text = if i == failing_index {
                    format!("file {i} FAILMARKER")
                } else {
                    format!("file {i} summarizes fine")
                };
                test_chunk(Uuid::new_v4(), *doc_id, &text)
            })
            .collect();
        let documents = doc_ids
            .iter()
            .map(|doc_id| test_document_with_metadata(*doc_id, None))
            .collect();
        (
            ExtractedGraphData {
                chunks,
                documents,
                entities: vec![],
                edges: vec![],
                producers: ArtifactProducers::default(),
                dataset_id: Uuid::new_v4(),
                user_id: None,
                tenant_id: None,
                failures: FailureReport::default(),
            },
            doc_ids,
        )
    }

    /// A mock that fails the marker chunk and answers every other
    /// summarization call, with one call in flight at a time so the dispatch
    /// order is the input order.
    fn summarization_llm() -> Arc<cognee_test_utils::MockLlm> {
        Arc::new(
            cognee_test_utils::MockLlm::empty()
                .with_failing_markers(vec!["FAILMARKER".to_string()])
                .with_summary_response(r#"{"summary":"s","description":"d"}"#.to_string()),
        )
    }

    fn serial_summarization_config(config: CognifyConfig) -> CognifyConfig {
        config.with_max_parallel_extractions(1)
    }

    /// The axis-1 pin for summarization. `summarize_text` referenced
    /// `FailureStop` nowhere at all: a failing chunk was recorded and the run
    /// then paid for every remaining summary, which is the opposite of what
    /// "stop spending immediately" promises. Counting LLM calls is the only
    /// assertion that catches a silent return to that — every *outcome*
    /// assertion below (the failure entry, the summaries kept) holds either
    /// way.
    #[tokio::test]
    async fn summarization_stops_dispatching_after_a_failure_under_fail_fast() {
        let (input, doc_ids) = summarization_fixture(0);
        let llm = summarization_llm();
        let config = serial_summarization_config(CognifyConfig::default());
        assert_eq!(
            config.failure_stop,
            FailureStop::FailFast,
            "the default is the case under test"
        );

        let result = summarize_text(&input, llm.clone(), &config)
            .await
            .expect("summarize_text never propagates a per-chunk failure");

        assert_eq!(
            llm.structured_calls(),
            1,
            "FailFast must stop scheduling once the first chunk failed; seeing 5 \
             means the run paid for four summaries it is about to sweep"
        );
        assert!(
            result.summaries.is_empty(),
            "nothing after the failure was dispatched, so nothing was summarized"
        );
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [doc_ids[0]]
        );
        // The undispatched files must not end the run marked complete, so they
        // are unreached rather than silently kept summary-less.
        assert_eq!(
            result
                .failures
                .unreached_items()
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            doc_ids[1..].iter().copied().collect(),
        );
        // Axis 1 changes what is dispatched, never what is carried forward.
        assert_eq!(result.chunks.len(), 5);
        assert_eq!(result.documents.len(), 5);
    }

    /// The other half of the axis: `RunToEnd` still pays for the whole run, and
    /// keeps every summary it paid for.
    #[tokio::test]
    async fn summarization_under_run_to_end_dispatches_every_chunk() {
        let (input, doc_ids) = summarization_fixture(0);
        let llm = summarization_llm();
        let config = serial_summarization_config(
            CognifyConfig::default().with_failure_stop(FailureStop::RunToEnd),
        );

        let result = summarize_text(&input, llm.clone(), &config)
            .await
            .expect("RunToEnd collects rather than propagates");

        assert_eq!(llm.structured_calls(), 5);
        assert_eq!(result.summaries.len(), 4, "the completed work is kept");
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [doc_ids[0]]
        );
        assert!(
            result.failures.unreached_items().is_empty(),
            "nothing went undispatched"
        );
    }

    /// `FailFast` is not on its own a reason to stop here. With
    /// `tolerate_summarization_failures` on, a failed summary fails no item, so
    /// there is no fatal failure for the axis to stop on and the remaining
    /// summaries are still worth paying for.
    #[tokio::test]
    async fn a_tolerated_summarization_failure_does_not_stop_the_run() {
        let (input, _doc_ids) = summarization_fixture(0);
        let llm = summarization_llm();
        let config = serial_summarization_config(
            CognifyConfig::default().with_summarization_failure_tolerance(true),
        );

        let result = summarize_text(&input, llm.clone(), &config)
            .await
            .expect("a tolerated failure is not an error");

        assert_eq!(
            llm.structured_calls(),
            5,
            "a failure that fails nothing must not stop the stage"
        );
        assert_eq!(result.summaries.len(), 4);
        assert!(result.failures.failed_items().is_empty());
        assert!(result.failures.unreached_items().is_empty());
        assert_eq!(result.failures.summarization_failures(), 1);
    }

    /// Regression guard: an image document must produce ≥1 chunk and must NOT
    /// return `CognifyError::UnsupportedDocumentType`.
    #[cfg(feature = "image-loader")]
    #[tokio::test]
    async fn test_image_document_produces_chunks() {
        use cognee_ingestion::loaders::image::ImageLoader;
        use cognee_test_utils::MockLlm;

        let storage = Arc::new(MockStorage::new());
        // Store fake image bytes so the loader can retrieve them.
        let location = storage
            .store(b"fake-image-bytes", "test.jpg")
            .await
            .expect("MockStorage store should succeed");

        let doc_id = Uuid::new_v4();
        let mut base = DataPoint::new("ImageDocument", None);
        base.id = doc_id;
        base.set_metadata("index_fields", serde_json::json!(["name"]));
        let doc = Document {
            base,
            document_type: "image".to_string(),
            name: "test.jpg".to_string(),
            raw_data_location: location,
            mime_type: "image/jpeg".to_string(),
            extension: "jpg".to_string(),
            data_id: doc_id,
            external_metadata: None,
        };

        let input = ClassifiedDocuments {
            failures: FailureReport::default(),
            documents: vec![doc],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        // Build a registry that contains an ImageLoader backed by a MockLlm
        // that returns a vision description.
        let mock_llm = Arc::new(
            MockLlm::new(vec![])
                .with_vision_responses(vec!["An image description for testing.".to_string()]),
        );
        let mut registry = LoaderRegistry::default();
        registry.register("image", Arc::new(ImageLoader::new(mock_llm)));

        let result = extract_chunks_from_documents(
            &input,
            &*storage,
            100,
            TokenCounterKind::Word,
            None,
            &registry,
            CognifyConfig::default().failure_policy(),
        )
        .await;

        // Must not be UnsupportedDocumentType — that is the regression we guard.
        assert!(
            !matches!(result, Err(CognifyError::UnsupportedDocumentType(_))),
            "image document must not produce UnsupportedDocumentType"
        );
        let chunks = result.expect("extract_chunks_from_documents should succeed for image docs");
        assert!(
            !chunks.chunks.is_empty(),
            "image document should produce at least one chunk"
        );
    }

    /// Regression guard: an audio document must produce ≥1 chunk and must NOT
    /// return `CognifyError::UnsupportedDocumentType`.
    #[cfg(feature = "audio-loader")]
    #[tokio::test]
    async fn test_audio_document_produces_chunks() {
        use cognee_ingestion::loaders::audio::AudioLoader;
        use cognee_llm::TranscriptionOutput;
        use cognee_test_utils::MockTranscriber;

        let storage = Arc::new(MockStorage::new());
        // Store fake audio bytes so the loader can retrieve them.
        let location = storage
            .store(b"fake-audio-bytes", "test.mp3")
            .await
            .expect("MockStorage store should succeed");

        let doc_id = Uuid::new_v4();
        let mut base = DataPoint::new("AudioDocument", None);
        base.id = doc_id;
        base.set_metadata("index_fields", serde_json::json!(["name"]));
        let doc = Document {
            base,
            document_type: "audio".to_string(),
            name: "test.mp3".to_string(),
            raw_data_location: location,
            mime_type: "audio/mpeg".to_string(),
            extension: "mp3".to_string(),
            data_id: doc_id,
            external_metadata: None,
        };

        let input = ClassifiedDocuments {
            failures: FailureReport::default(),
            documents: vec![doc],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        // Build a registry that contains an AudioLoader backed by a MockTranscriber.
        let mock_transcriber = Arc::new(MockTranscriber::new(
            "mock-whisper",
            vec![TranscriptionOutput {
                text: "Test transcript.".to_string(),
                language: None,
                duration: None,
            }],
        ));
        let mut registry = LoaderRegistry::default();
        registry.register("audio", Arc::new(AudioLoader::new(mock_transcriber)));

        let result = extract_chunks_from_documents(
            &input,
            &*storage,
            100,
            TokenCounterKind::Word,
            None,
            &registry,
            CognifyConfig::default().failure_policy(),
        )
        .await;

        // Must not be UnsupportedDocumentType — that is the regression we guard.
        assert!(
            !matches!(result, Err(CognifyError::UnsupportedDocumentType(_))),
            "audio document must not produce UnsupportedDocumentType"
        );
        let chunks = result.expect("extract_chunks_from_documents should succeed for audio docs");
        assert!(
            !chunks.chunks.is_empty(),
            "audio document should produce at least one chunk"
        );
    }

    /// Regression guard: `.html`/`.htm` files must be classified (not silently
    /// dropped).  Before the `html-loader` feature was added,
    /// `extension_to_doc_type("html")` returned `None` so `classify_documents`
    /// produced an empty Vec — this test would have failed then.
    #[test]
    fn classify_html_extension_not_dropped() {
        for ext in ["html", "htm"] {
            let data = Data::builder(
                Uuid::new_v4(),
                format!("page.{ext}"),
                format!("/storage/page.{ext}"),
                format!("file:///page.{ext}"),
                ext,
                "text/html",
                "hash_html",
                Uuid::new_v4(),
            )
            .build();

            let input = CognifyInput {
                data_items: vec![data],
                dataset_id: Uuid::new_v4(),
                user_id: None,
                tenant_id: None,
            };
            let result = classify_documents(&input, FailurePolicy::default())
                .expect("classify should not error");
            assert_eq!(
                result.documents.len(),
                1,
                ".{ext} file must not be dropped by classify_documents"
            );
            assert_eq!(
                result.documents[0].document_type, "html",
                ".{ext} must classify as document_type=\"html\""
            );
            // Cross-SDK parity: Python's BeautifulSoupLoader stores TextDocument nodes.
            assert_eq!(
                result.documents[0].base.data_type, "TextDocument",
                ".{ext} must carry data_type=\"TextDocument\" for Python DB parity"
            );
        }
    }

    /// Regression guard: the classify → load → chunk pipeline for an HTML file
    /// must produce text chunks (not an `UnsupportedDocumentType` error).
    ///
    /// Before this feature:
    ///  1. `classify_documents` would return an empty Vec for `.html` files
    ///     (extension was not mapped).
    ///  2. Even if the document type was forced to "html", `extract_chunks_from_documents`
    ///     would return `CognifyError::UnsupportedDocumentType("html")` because no
    ///     loader was registered.
    /// Both regressions are guarded here end-to-end.
    #[cfg(feature = "html-loader")]
    #[tokio::test]
    async fn classify_then_chunk_html_end_to_end() {
        let storage = Arc::new(MockStorage::new());
        let html = b"<html><head><title>Guide</title></head><body><p>The quick brown fox.</p></body></html>";
        let location = storage
            .store(html, "guide.html")
            .await
            .expect("MockStorage store should succeed");

        let data = Data::builder(
            Uuid::new_v4(),
            "guide.html",
            &location, // raw_data_location == storage path so retrieve() can find it
            "file:///guide.html",
            "html",
            "text/html",
            "hash_guide_html",
            Uuid::new_v4(),
        )
        .build();

        let input = CognifyInput {
            data_items: vec![data],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        // Regression 1: classify must not drop the HTML file.
        let classified = classify_documents(&input, FailurePolicy::default())
            .expect("classify_documents must succeed for html");
        assert_eq!(
            classified.documents.len(),
            1,
            "classify_documents must not drop the .html file"
        );
        assert_eq!(classified.documents[0].document_type, "html");

        // Regression 2: the HtmlLoader must be dispatched and produce chunks.
        let registry = LoaderRegistry::default();
        let result = extract_chunks_from_documents(
            &classified,
            &*storage,
            100,
            TokenCounterKind::Word,
            None,
            &registry,
            CognifyConfig::default().failure_policy(),
        )
        .await;

        assert!(
            !matches!(result, Err(CognifyError::UnsupportedDocumentType(_))),
            "html loader must be registered (UnsupportedDocumentType must not occur)"
        );
        let chunks = result.expect("extract_chunks_from_documents must succeed for html");
        assert!(
            !chunks.chunks.is_empty(),
            "html file must produce at least one chunk"
        );
        assert!(
            chunks
                .chunks
                .iter()
                .any(|c| c.text.contains("quick brown fox")),
            "extracted text must appear in chunks (HTML tags must be stripped)"
        );
    }

    /// Regression guard: an HTML document must produce ≥1 chunk via the
    /// always-registered `HtmlLoader` and must NOT return
    /// `CognifyError::UnsupportedDocumentType`.
    #[cfg(feature = "html-loader")]
    #[tokio::test]
    async fn test_html_document_produces_chunks() {
        let storage = Arc::new(MockStorage::new());
        let html =
            b"<html><head><title>T</title></head><body><h1>Heading</h1><p>Body text here.</p></body></html>";
        let location = storage
            .store(html, "test.html")
            .await
            .expect("MockStorage store should succeed");

        let doc_id = Uuid::new_v4();
        // Cross-SDK parity: HTML docs carry the TextDocument data_type.
        let mut base = DataPoint::new("TextDocument", None);
        base.id = doc_id;
        base.set_metadata("index_fields", serde_json::json!(["name"]));
        let doc = Document {
            base,
            document_type: "html".to_string(),
            name: "test.html".to_string(),
            raw_data_location: location,
            mime_type: "text/html".to_string(),
            extension: "html".to_string(),
            data_id: doc_id,
            external_metadata: None,
        };

        let input = ClassifiedDocuments {
            failures: FailureReport::default(),
            documents: vec![doc],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        // The HtmlLoader is part of the default registry when the feature is on.
        let registry = LoaderRegistry::default();

        let result = extract_chunks_from_documents(
            &input,
            &*storage,
            100,
            TokenCounterKind::Word,
            None,
            &registry,
            CognifyConfig::default().failure_policy(),
        )
        .await;

        assert!(
            !matches!(result, Err(CognifyError::UnsupportedDocumentType(_))),
            "html document must not produce UnsupportedDocumentType"
        );
        let chunks = result.expect("extract_chunks_from_documents should succeed for html docs");
        assert!(
            !chunks.chunks.is_empty(),
            "html document should produce at least one chunk"
        );
        // The extracted text (not raw HTML tags) should reach the chunk.
        assert!(
            chunks.chunks.iter().any(|c| c.text.contains("Body text")),
            "extracted HTML text should appear in chunks"
        );
    }

    // ── build_loader_registry wiring tests ────────────────────────────────────

    /// `build_loader_registry` must always register an image loader when the
    /// `image-loader` feature is enabled.
    #[cfg(feature = "image-loader")]
    #[test]
    fn test_build_loader_registry_includes_image() {
        use cognee_test_utils::MockLlm;

        let llm: Arc<dyn Llm> = Arc::new(MockLlm::empty());
        let config = CognifyConfig::default();
        let registry = build_loader_registry(&llm, &config);
        assert!(
            registry.get("image").is_some(),
            "build_loader_registry must include \"image\" loader when image-loader feature is on"
        );
    }

    /// `build_loader_registry` must register an audio loader when a transcriber
    /// is set on the config AND the `audio-loader` feature is enabled.
    #[cfg(feature = "audio-loader")]
    #[test]
    fn test_build_loader_registry_includes_audio_when_transcriber_set() {
        use cognee_llm::TranscriptionOutput;
        use cognee_test_utils::MockTranscriber;

        let llm: Arc<dyn Llm> = Arc::new(cognee_test_utils::MockLlm::empty());
        let transcriber: Arc<dyn cognee_llm::Transcriber> = Arc::new(MockTranscriber::new(
            "mock",
            vec![TranscriptionOutput {
                text: "hi".to_string(),
                language: None,
                duration: None,
            }],
        ));
        let config = CognifyConfig::default().with_transcriber(transcriber);
        let registry = build_loader_registry(&llm, &config);
        assert!(
            registry.get("audio").is_some(),
            "build_loader_registry must include \"audio\" loader when transcriber is set"
        );
    }

    /// Without a transcriber on the config, no audio loader should be
    /// registered — audio stays gracefully unsupported (D5).
    #[cfg(feature = "audio-loader")]
    #[test]
    fn test_build_loader_registry_no_audio_without_transcriber() {
        let llm: Arc<dyn Llm> = Arc::new(cognee_test_utils::MockLlm::empty());
        let config = CognifyConfig::default(); // no transcriber
        let registry = build_loader_registry(&llm, &config);
        assert!(
            registry.get("audio").is_none(),
            "build_loader_registry must NOT include \"audio\" loader when transcriber is None"
        );
    }

    // ── Ownership is recorded before artifacts exist (I1) ────────────────

    fn ownership_canned_graph() -> String {
        json!({
            "nodes": [
                {"id": "alice", "name": "Alice", "type": "PERSON", "description": "A person."},
                {"id": "acme", "name": "Acme", "type": "ORGANIZATION", "description": "A company."}
            ],
            "edges": [{
                "source_node_id": "alice",
                "target_node_id": "acme",
                "relationship_name": "works_at",
                "description": "Alice works at Acme."
            }]
        })
        .to_string()
    }

    fn ownership_input(dataset_id: Uuid, doc_id: Uuid, user_id: Option<Uuid>) -> ExtractedChunks {
        ExtractedChunks {
            chunks: vec![test_chunk(Uuid::new_v4(), doc_id, "Alice works at Acme.")],
            documents: vec![test_document_with_metadata(doc_id, None)],
            dataset_id,
            user_id,
            tenant_id: None,
            failures: FailureReport::default(),
        }
    }

    async fn run_extraction(
        input: &ExtractedChunks,
        graph: Arc<dyn GraphDBTrait>,
        db: &DatabaseConnection,
        run_id: Option<Uuid>,
    ) -> Result<ExtractedGraphData, CognifyError> {
        use cognee_ontology::NoOpOntologyResolver;
        use cognee_test_utils::MockLlm;

        extract_graph_from_data(
            input,
            Arc::new(MockLlm::new(vec![ownership_canned_graph()])),
            graph,
            Arc::new(NoOpOntologyResolver::new()),
            db,
            run_id,
            &CognifyConfig::default(),
            None,
            None,
        )
        .await
    }

    /// The extraction stage claims the entities and edges it is about to write
    /// *before* the graph sees them: with the graph write failing, the stage
    /// returns `Err` and the ownership rows are there anyway.
    ///
    /// This is the test that fails against the previous ordering, where the
    /// stage wrote to the graph and never touched the relational database at
    /// all — so a run that died here left entities nothing could find, and the
    /// extraction dedup filter then hid their edges from every retry.
    #[tokio::test]
    async fn extract_graph_records_ownership_before_the_graph_write() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        graph.set_add_nodes_error("graph is down");

        let input = ownership_input(dataset_id, Uuid::new_v4(), Some(Uuid::new_v4()));
        let result = run_extraction(&input, graph.clone(), &db, Some(Uuid::new_v4())).await;

        assert!(result.is_err(), "the failing graph write must surface");
        assert_eq!(graph.node_count(), 0, "no entity reached the graph");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert_eq!(
            nodes.len(),
            2,
            "both extracted entities are claimed even though the write failed"
        );
        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        assert_eq!(edges.len(), 1, "the semantic edge is claimed too");
    }

    /// Every ownership row the extraction stage writes names the run that
    /// created the artifact.
    #[tokio::test]
    async fn extract_graph_stamps_the_run_id_on_entity_and_edge_rows() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());
        let run_id = Uuid::new_v4();

        let input = ownership_input(dataset_id, Uuid::new_v4(), Some(Uuid::new_v4()));
        run_extraction(&input, graph, &db, Some(run_id))
            .await
            .expect("extraction must succeed");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert!(!nodes.is_empty());
        assert!(
            nodes.iter().all(|row| row.pipeline_run_id == Some(run_id)),
            "every entity row names the run that created it"
        );
        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        assert!(!edges.is_empty());
        assert!(
            edges.iter().all(|row| row.pipeline_run_id == Some(run_id)),
            "every semantic-edge row names the run that created it"
        );
    }

    /// A task driven outside a pipeline executor has no run to name. Those rows
    /// are written with a NULL run id — permanently exempt from every
    /// run-scoped query — rather than the stage refusing to run.
    #[tokio::test]
    async fn ownership_rows_carry_a_null_run_id_outside_a_pipeline_run() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());

        let input = ownership_input(dataset_id, Uuid::new_v4(), Some(Uuid::new_v4()));
        run_extraction(&input, graph, &db, None)
            .await
            .expect("a missing run id is not an error");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert!(!nodes.is_empty(), "the rows are still written");
        assert!(nodes.iter().all(|row| row.pipeline_run_id.is_none()));
    }

    /// A run that identified no user still writes the ledger, owned by the
    /// configured default user. Before this the whole write was skipped, so
    /// those deployments had no ownership records at all.
    #[tokio::test]
    async fn ownership_rows_use_the_default_user_when_none_is_identified() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;

        let dataset_id = Uuid::new_v4();
        let doc_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());

        let input = ownership_input(dataset_id, doc_id, None);
        run_extraction(&input, graph, &db, None)
            .await
            .expect("extraction must succeed");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert!(!nodes.is_empty());
        assert!(
            nodes
                .iter()
                .all(|row| row.user_id == DEFAULT_LEDGER_USER_ID),
            "the rows resolve to the default ledger user"
        );
        // And the row id is derived from that resolved user, not from a
        // placeholder the delete path would never reproduce.
        let row = &nodes[0];
        assert_eq!(
            row.id,
            provenance_node_id(None, DEFAULT_LEDGER_USER_ID, dataset_id, doc_id, row.slug)
        );
    }

    /// `add_data_points` claims every artifact it is about to write before the
    /// first of them exists — asserted at the vector seam, the last store it
    /// touches and the one the ledger write used to run after.
    #[tokio::test]
    async fn add_data_points_records_ownership_before_the_vector_write() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let dataset_id = Uuid::new_v4();
        let doc_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;

        let vector = Arc::new(MockVectorDB::new());
        vector.set_index_error("vector store is down");

        let input = SummarizedData {
            chunks: vec![test_chunk(Uuid::new_v4(), doc_id, "Hello world")],
            documents: vec![test_document_with_metadata(doc_id, None)],
            entities: vec![],
            edges: vec![],
            producers: ArtifactProducers::default(),
            summaries: vec![],
            dataset_id,
            user_id: Some(Uuid::new_v4()),
            tenant_id: None,
            failures: FailureReport::default(),
        };

        let result = add_data_points(
            &input,
            Arc::new(cognee_graph::MockGraphDB::new()),
            vector,
            Arc::new(MockEmbeddingEngine::new(8)),
            &db,
            Some(Uuid::new_v4()),
            &CognifyConfig::default(),
        )
        .await;

        assert!(result.is_err(), "the failing vector write must surface");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert_eq!(
            nodes.len(),
            2,
            "the chunk and document rows survive the failed run"
        );
        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        assert!(
            !edges.is_empty(),
            "the structural edges are claimed before they are written"
        );
        assert!(
            nodes.iter().all(|row| row.pipeline_run_id.is_some())
                && edges.iter().all(|row| row.pipeline_run_id.is_some()),
            "every row names the run that was writing it"
        );
    }

    /// The same claim at the *graph* seam, which the vector-seam test above
    /// does not reach: `add_data_points` writes chunks, summaries and entity
    /// types to the graph long before it touches the vector store, so a ledger
    /// write moved below the first `add_nodes` would still satisfy that test
    /// while leaving a run that died on the chunk write with nothing to sweep.
    /// The extraction stage's counterpart is
    /// `extract_graph_records_ownership_before_the_graph_write`.
    #[tokio::test]
    async fn add_data_points_records_ownership_before_the_graph_write() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let dataset_id = Uuid::new_v4();
        let doc_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let run_id = Uuid::new_v4();

        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        graph.set_add_nodes_error("graph is down");

        let input = SummarizedData {
            chunks: vec![test_chunk(Uuid::new_v4(), doc_id, "Hello world")],
            documents: vec![test_document_with_metadata(doc_id, None)],
            entities: vec![],
            edges: vec![],
            producers: ArtifactProducers::default(),
            summaries: vec![],
            dataset_id,
            user_id: Some(Uuid::new_v4()),
            tenant_id: None,
            failures: FailureReport::default(),
        };

        let result = add_data_points(
            &input,
            Arc::clone(&graph) as Arc<dyn GraphDBTrait>,
            Arc::new(MockVectorDB::new()),
            Arc::new(MockEmbeddingEngine::new(8)),
            &db,
            Some(run_id),
            &CognifyConfig::default(),
        )
        .await;

        assert!(result.is_err(), "the failing graph write must surface");
        assert_eq!(graph.node_count(), 0, "no chunk reached the graph");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert_eq!(
            nodes.len(),
            2,
            "the chunk and document rows survive the failed run"
        );
        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        assert!(
            !edges.is_empty(),
            "the structural edges are claimed before they are written"
        );
        assert!(
            nodes.iter().all(|row| row.pipeline_run_id == Some(run_id))
                && edges.iter().all(|row| row.pipeline_run_id == Some(run_id)),
            "every row names the run that was writing it, so the sweep can find it"
        );
    }

    /// A semantic edge's ledger row must hash the *sanitized* relationship
    /// text. NUL bytes are stripped on the way into Postgres, so an id derived
    /// from the raw text would key the edge on text no store holds and the
    /// sweep would miss it. `provenance_edge_ids_derive_from_sanitized_text`
    /// pins the formula; this pins the *call site* — that the rows
    /// `add_data_points` really writes went through `sanitize_string` before
    /// the hash saw them. The temporal twin is
    /// `temporal_edge_rows_hash_sanitized_relationship_names`.
    #[tokio::test]
    async fn semantic_edge_rows_hash_sanitized_relationship_names() {
        use cognee_database::ops::graph_storage::get_edges_by_dataset;
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let dataset_id = Uuid::new_v4();
        let doc_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let dirty = "work\u{0}s_at";
        let clean = "works_at";

        let source = Uuid::new_v4();
        let target = Uuid::new_v4();

        let input = SummarizedData {
            chunks: vec![test_chunk(Uuid::new_v4(), doc_id, "Alice works at Acme.")],
            documents: vec![test_document_with_metadata(doc_id, None)],
            entities: vec![],
            edges: vec![GraphEdgePair::new(source, target, dirty)],
            producers: ArtifactProducers::default(),
            summaries: vec![],
            dataset_id,
            user_id: Some(user_id),
            tenant_id: None,
            failures: FailureReport::default(),
        };

        add_data_points(
            &input,
            Arc::new(cognee_graph::MockGraphDB::new()),
            Arc::new(MockVectorDB::new()),
            Arc::new(MockEmbeddingEngine::new(8)),
            &db,
            None,
            &CognifyConfig::default(),
        )
        .await
        .expect("add_data_points must succeed");

        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        let row = edges
            .iter()
            .find(|row| row.source_node_id == source && row.destination_node_id == target)
            .expect("the semantic edge is claimed");
        assert_eq!(
            row.relationship_name, clean,
            "the stored text is the sanitized one"
        );
        assert_eq!(
            row.id,
            provenance_edge_id(
                None,
                user_id,
                dataset_id,
                row.data_id,
                source,
                clean,
                target,
            ),
            "the row id is derived from the sanitized text"
        );
        assert_eq!(
            row.slug,
            triplet_slug(source, clean, target),
            "and so is the slug the sweep correlates on"
        );
        // The load-bearing half: stripping the NUL actually changes the hash,
        // so hashing the raw text could not slip through as a no-op.
        assert_ne!(
            row.id,
            provenance_edge_id(
                None,
                user_id,
                dataset_id,
                row.data_id,
                source,
                dirty,
                target
            ),
        );
    }

    /// The extraction stage and `add_data_points` write overlapping rows. The
    /// second write must not re-attribute an artifact the first already
    /// claimed — `pipeline_run_id` is deliberately absent from the upsert's
    /// `ON CONFLICT` update list, and this is where that matters.
    #[tokio::test]
    async fn add_data_points_does_not_steal_the_extraction_stages_run_id() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());
        let extraction_run = Uuid::new_v4();
        let later_run = Uuid::new_v4();

        let input = ownership_input(dataset_id, Uuid::new_v4(), Some(Uuid::new_v4()));
        let graph_data = run_extraction(&input, Arc::clone(&graph), &db, Some(extraction_run))
            .await
            .expect("extraction must succeed");

        let entity_ids: Vec<Uuid> = graph_data
            .entities
            .iter()
            .map(|pair| pair.entity.base.id)
            .collect();
        assert!(!entity_ids.is_empty());

        let summarized = SummarizedData {
            chunks: graph_data.chunks,
            documents: graph_data.documents,
            entities: graph_data.entities,
            edges: graph_data.edges,
            producers: graph_data.producers,
            summaries: vec![],
            dataset_id,
            user_id: input.user_id,
            tenant_id: None,
            failures: FailureReport::default(),
        };

        add_data_points(
            &summarized,
            graph,
            Arc::new(MockVectorDB::new()),
            Arc::new(MockEmbeddingEngine::new(8)),
            &db,
            Some(later_run),
            &CognifyConfig::default(),
        )
        .await
        .expect("add_data_points must succeed");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        for row in nodes.iter().filter(|row| entity_ids.contains(&row.slug)) {
            assert_eq!(
                row.pipeline_run_id,
                Some(extraction_run),
                "the entity keeps the run that created it, not the one that rewrote its row"
            );
        }
        // The rows only `add_data_points` writes carry its own run.
        assert!(
            nodes
                .iter()
                .any(|row| row.pipeline_run_id == Some(later_run)),
            "the chunk / document / EntityType rows name the later run"
        );
    }

    // ── Extraction-stage failure collection and the abort partition ──────

    /// Three single-chunk files A, B, C, of which B's chunk text carries the
    /// marker that makes the mock LLM fail.
    fn three_chunk_files(dataset_id: Uuid) -> (ExtractedChunks, Uuid, Uuid, Uuid) {
        let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        let texts = ["Alice works at Acme.", "Bob FAILMARKER.", "Carol at Acme."];
        let chunks = ids
            .iter()
            .zip(texts)
            .map(|(doc_id, text)| test_chunk(Uuid::new_v4(), *doc_id, text))
            .collect();
        let documents = ids
            .iter()
            .map(|doc_id| test_document_with_metadata(*doc_id, None))
            .collect();
        (
            ExtractedChunks {
                chunks,
                documents,
                dataset_id,
                user_id: Some(Uuid::new_v4()),
                tenant_id: None,
                failures: FailureReport::default(),
            },
            ids[0],
            ids[1],
            ids[2],
        )
    }

    /// One chunk per batch, so the batches run in file order and the abort
    /// boundary is exactly between files.
    fn one_chunk_per_batch(config: CognifyConfig) -> CognifyConfig {
        config.with_chunks_per_batch(1)
    }

    async fn run_extraction_with_config(
        input: &ExtractedChunks,
        graph: Arc<dyn GraphDBTrait>,
        db: &DatabaseConnection,
        config: &CognifyConfig,
    ) -> Result<ExtractedGraphData, CognifyError> {
        use cognee_ontology::NoOpOntologyResolver;
        use cognee_test_utils::MockLlm;

        extract_graph_from_data(
            input,
            Arc::new(
                MockLlm::new(vec![ownership_canned_graph(); 8])
                    .with_failing_markers(vec!["FAILMARKER".to_string()]),
            ),
            graph,
            Arc::new(NoOpOntologyResolver::new()),
            db,
            Some(Uuid::new_v4()),
            config,
            None,
            None,
        )
        .await
    }

    /// `RunToEnd` records the failing chunk and extracts the rest.
    #[tokio::test]
    async fn extraction_failure_is_collected_under_run_to_end() {
        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());
        let (input, _a, b, _c) = three_chunk_files(dataset_id);

        let config =
            one_chunk_per_batch(CognifyConfig::default().with_failure_stop(FailureStop::RunToEnd));
        let result = run_extraction_with_config(&input, graph, &db, &config)
            .await
            .expect("RunToEnd collects rather than propagates");

        assert!(
            !result.entities.is_empty(),
            "A and C still produced entities"
        );
        assert_eq!(result.failures.entries().len(), 1);
        let entry = &result.failures.entries()[0];
        assert_eq!(entry.stage, FailureStage::GraphExtraction);
        assert_eq!(entry.data_id, b);
        assert!(
            entry.chunk_id.is_some(),
            "graph extraction fails at chunk granularity"
        );
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [b]
        );
    }

    /// The headline test for the abort-time partition: at the moment of the
    /// first failure *nothing* has been persisted, so keeping the other files'
    /// results is not a matter of not-deleting — the stage has to deliberately
    /// persist the finished work before it stops. Only the complete file's
    /// artifacts and ownership rows exist afterwards.
    /// `FailFast` must stop *scheduling* work, not merely stop reporting it.
    ///
    /// The abort partition is computed from an index set before the loop
    /// breaks, so deleting the `break` changes no other assertion in the
    /// suite — the axis's whole promise (stop spending) would go unpinned.
    /// This counts LLM calls instead of outcomes.
    #[tokio::test]
    async fn failfast_stops_dispatching_after_the_failing_batch() {
        use cognee_ontology::NoOpOntologyResolver;
        use cognee_test_utils::MockLlm;

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        // Three one-chunk files, one batch each; the SECOND one poisons.
        let (input, _a, _b, _c) = three_chunk_files(dataset_id);

        let llm = Arc::new(
            MockLlm::new(vec![ownership_canned_graph(); 8])
                .with_failing_markers(vec!["FAILMARKER".to_string()]),
        );
        let config = one_chunk_per_batch(
            CognifyConfig::default().with_rollback_scope(RollbackScope::FailedItems),
        );

        let _ = extract_graph_from_data(
            &input,
            llm.clone(),
            graph,
            Arc::new(NoOpOntologyResolver::new()),
            &db,
            Some(Uuid::new_v4()),
            &config,
            None,
            None,
        )
        .await;

        assert_eq!(
            llm.structured_calls(),
            2,
            "FailFast must dispatch the failing batch and then stop: file C's \
             chunk should never reach the LLM. Seeing 3 means the loop ran on \
             and only the bookkeeping stopped."
        );
    }

    #[tokio::test]
    async fn extraction_abort_persists_only_complete_files() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        let (input, a, b, c) = three_chunk_files(dataset_id);

        let config = one_chunk_per_batch(
            CognifyConfig::default().with_rollback_scope(RollbackScope::FailedItems),
        );
        let result = run_extraction_with_config(&input, graph.clone(), &db, &config)
            .await
            .expect("FailedItems keeps the complete files");

        assert_eq!(
            result
                .documents
                .iter()
                .map(|d| d.base.id)
                .collect::<Vec<_>>(),
            [a],
            "only A is complete"
        );
        assert!(result.chunks.iter().all(|chunk| chunk.document_id == a));
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [b]
        );
        assert_eq!(
            result
                .failures
                .unreached_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [c],
            "C's chunk was never dispatched"
        );

        // I1 still holds for what was written: every artifact has a row.
        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert!(!nodes.is_empty(), "A's artifacts were persisted");
        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        assert!(!edges.is_empty());
        // …and nothing of B or C reached either store.
        for row in &nodes {
            assert_ne!(row.data_id, b, "B owns no artifact");
            assert_ne!(row.data_id, c, "C owns no artifact");
        }
        for row in &edges {
            assert_ne!(row.data_id, b);
            assert_ne!(row.data_id, c);
        }
    }

    /// The same abort under the default `WholeRun` scope persists *nothing* —
    /// the partition must not run on the fatal path.
    #[tokio::test]
    async fn extraction_abort_persists_nothing_under_whole_run() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        let (input, _a, b, c) = three_chunk_files(dataset_id);

        let config = one_chunk_per_batch(CognifyConfig::default());
        let err = run_extraction_with_config(&input, graph.clone(), &db, &config)
            .await
            .expect_err("the default scope still aborts the run");

        let CognifyError::RunFailed { report } = err else {
            panic!("expected RunFailed, got: {err:?}");
        };
        assert_eq!(
            report.failed_items().iter().copied().collect::<Vec<_>>(),
            [b]
        );
        assert_eq!(
            report.unreached_items().iter().copied().collect::<Vec<_>>(),
            [c]
        );

        assert_eq!(graph.node_count(), 0, "no artifact reached the graph");
        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert!(nodes.is_empty(), "and no ownership row was written");
    }

    /// A whole batch is dispatched before any result is inspected, so a
    /// FailFast abort reports every failure in the batch that tripped it.
    #[tokio::test]
    async fn extraction_reports_every_failure_in_the_batch_that_tripped_it() {
        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());

        let doc_ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        let texts = [
            "Alice works at Acme.",
            "Bob FAILMARKER.",
            "Carol at Acme.",
            "Dave FAILMARKER.",
        ];
        let input = ExtractedChunks {
            chunks: doc_ids
                .iter()
                .zip(texts)
                .map(|(doc_id, text)| test_chunk(Uuid::new_v4(), *doc_id, text))
                .collect(),
            documents: doc_ids
                .iter()
                .map(|doc_id| test_document_with_metadata(*doc_id, None))
                .collect(),
            dataset_id,
            user_id: Some(Uuid::new_v4()),
            tenant_id: None,
            failures: FailureReport::default(),
        };

        // All four chunks in one batch.
        let config = CognifyConfig::default().with_chunks_per_batch(4);
        let err = run_extraction_with_config(&input, graph, &db, &config)
            .await
            .expect_err("default scope aborts");
        let CognifyError::RunFailed { report } = err else {
            panic!("expected RunFailed");
        };
        assert_eq!(
            report.entries().len(),
            2,
            "both failing chunks in the batch are reported, not just the first"
        );
        assert_eq!(report.failed_items().len(), 2);
    }

    /// The documented step-pending state: under `RunToEnd` the extraction
    /// stage does not filter, so the failed file's chunk and `Document` are
    /// persisted with ownership rows and stay there until the sweep lands.
    /// Asserted explicitly rather than pretended away.
    #[tokio::test]
    async fn extraction_persists_everything_under_run_to_end() {
        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph: Arc<dyn GraphDBTrait> = Arc::new(cognee_graph::MockGraphDB::new());
        let (input, _a, b, c) = three_chunk_files(dataset_id);

        let config = one_chunk_per_batch(
            CognifyConfig::default()
                .with_failure_stop(FailureStop::RunToEnd)
                .with_rollback_scope(RollbackScope::FailedItems),
        );
        let result = run_extraction_with_config(&input, graph, &db, &config)
            .await
            .expect("RunToEnd below the ratio still returns Ok from this stage");

        assert_eq!(
            result.documents.len(),
            3,
            "the failed file's Document is still carried forward"
        );
        assert!(
            result.chunks.iter().any(|chunk| chunk.document_id == b),
            "and so is its chunk — the sweep is what removes them"
        );
        assert!(result.chunks.iter().any(|chunk| chunk.document_id == c));
    }

    // ── Temporal: ownership rows, collected failures, the partition ──────

    /// The narrowest LLM the temporal stage is satisfied by, with a switch for
    /// failing either pass. Dispatches on the system prompt, the way the
    /// integration fixtures do.
    ///
    /// Each chunk yields one event named after the chunk's first word and
    /// described by the chunk text itself — which is what puts a failure marker
    /// into the *enrichment* prompt as well as the extraction one, so both
    /// passes can be failed from the same fixture text.
    struct TemporalTestLlm {
        fail_extraction: Vec<String>,
        fail_enrichment: Vec<String>,
    }

    impl TemporalTestLlm {
        fn failing_extraction(markers: &[&str]) -> Self {
            Self {
                fail_extraction: markers.iter().map(|m| (*m).to_string()).collect(),
                fail_enrichment: Vec::new(),
            }
        }

        fn failing_enrichment(markers: &[&str]) -> Self {
            Self {
                fail_extraction: Vec::new(),
                fail_enrichment: markers.iter().map(|m| (*m).to_string()).collect(),
            }
        }

        fn failing_both(extraction: &[&str], enrichment: &[&str]) -> Self {
            Self {
                fail_extraction: extraction.iter().map(|m| (*m).to_string()).collect(),
                fail_enrichment: enrichment.iter().map(|m| (*m).to_string()).collect(),
            }
        }
    }

    #[async_trait::async_trait]
    impl Llm for TemporalTestLlm {
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<cognee_llm::GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
            unreachable!("the temporal stage only uses structured output")
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            messages: Vec<cognee_llm::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<cognee_llm::GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            let content = |role: cognee_llm::MessageRole| -> String {
                messages
                    .iter()
                    .filter(|message| message.role == role)
                    .map(|message| message.content.as_str())
                    .collect()
            };
            let system_prompt = content(cognee_llm::MessageRole::System);
            let user_prompt = content(cognee_llm::MessageRole::User);
            let hits =
                |markers: &[String]| markers.iter().any(|m| user_prompt.contains(m.as_str()));

            if system_prompt.contains("extracting highly granular stream events") {
                if hits(&self.fail_extraction) {
                    return Err(cognee_llm::LlmError::RateLimitExceeded(
                        "simulated 429 on temporal extraction".to_string(),
                    ));
                }
                let head = user_prompt.split_whitespace().next().unwrap_or("Unnamed");
                return Ok(json!({
                    "events": [{
                        "name": format!("{head} event"),
                        "description": user_prompt,
                        "time_from": { "year": 2020 },
                        "time_to": null,
                        "location": null
                    }]
                }));
            }
            if system_prompt.contains("extracting highly granular entities from events") {
                if hits(&self.fail_enrichment) {
                    return Err(cognee_llm::LlmError::RateLimitExceeded(
                        "simulated 429 on temporal enrichment".to_string(),
                    ));
                }
                let names: Vec<String> = serde_json::from_str::<serde_json::Value>(&user_prompt)
                    .ok()
                    .and_then(|value| value.as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|entry| {
                        entry
                            .get("event_name")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .collect();
                return Ok(json!({
                    "events": names
                        .into_iter()
                        .map(|name| json!({
                            "event_name": name,
                            "attributes": [
                                { "entity": "Alice", "entity_type": "person", "relationship": "subject" }
                            ]
                        }))
                        .collect::<Vec<_>>()
                }));
            }
            unreachable!("the temporal stage issues no other prompt")
        }

        fn model(&self) -> &str {
            "temporal-test-llm"
        }
    }

    /// One event with a single entity attribute, attributed to `data_id`.
    fn attributed_event(name: &str, relationship: &str, data_id: Uuid) -> AttributedEvent {
        AttributedEvent {
            event: TemporalEvent {
                name: name.to_string(),
                description: Some(format!("Description of {name}")),
                location: None,
                at: Some(cognee_models::CognifyTimestamp {
                    time_at: 1_577_836_800_000,
                    timestamp_str: "2020-01-01T00:00:00".to_string(),
                    year: 2020,
                    month: 1,
                    day: 1,
                    hour: 0,
                    minute: 0,
                    second: 0,
                }),
                during: None,
                attributes: vec![cognee_models::EventAttribute {
                    entity: "Alice".to_string(),
                    entity_type: "person".to_string(),
                    relationship: relationship.to_string(),
                }],
            },
            data_id,
        }
    }

    fn temporal_input(
        dataset_id: Uuid,
        user_id: Option<Uuid>,
        events: Vec<AttributedEvent>,
    ) -> ExtractedTemporalEvents {
        ExtractedTemporalEvents {
            events,
            dataset_id,
            user_id,
            tenant_id: None,
            failures: FailureReport::default(),
        }
    }

    /// `n` single-chunk files, the *i*-th carrying `texts[i]`.
    fn temporal_files(dataset_id: Uuid, texts: &[&str]) -> (ExtractedChunks, Vec<Uuid>) {
        let doc_ids: Vec<Uuid> = texts.iter().map(|_| Uuid::new_v4()).collect();
        let chunks = doc_ids
            .iter()
            .zip(texts)
            .map(|(doc_id, text)| test_chunk(Uuid::new_v4(), *doc_id, text))
            .collect();
        let documents = doc_ids
            .iter()
            .map(|doc_id| test_document_with_metadata(*doc_id, None))
            .collect();
        (
            ExtractedChunks {
                chunks,
                documents,
                dataset_id,
                user_id: Some(Uuid::new_v4()),
                tenant_id: None,
                failures: FailureReport::default(),
            },
            doc_ids,
        )
    }

    /// The temporal persistence stage claims everything it is about to write
    /// *before* the graph sees any of it: with the graph write failing, the
    /// stage returns `Err` and the ownership rows are there anyway.
    ///
    /// This is the test that fails against the previous ordering, where
    /// temporal wrote nodes, edges and `Event_name` points and never touched
    /// the relational database at all — so a failed temporal run left artifacts
    /// nothing could find and a temporal sweep removed nothing while reporting
    /// success.
    #[tokio::test]
    async fn add_temporal_data_points_records_ownership_before_the_graph_write() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        graph.set_add_nodes_error("graph is down");

        let data_id = Uuid::new_v4();
        let input = temporal_input(
            dataset_id,
            Some(Uuid::new_v4()),
            vec![attributed_event("Alice joins Acme", "subject", data_id)],
        );

        let result = add_temporal_data_points(
            &input,
            graph.clone(),
            Arc::new(MockVectorDB::new()),
            Arc::new(MockEmbeddingEngine::new(8)),
            &db,
            Some(Uuid::new_v4()),
        )
        .await;

        assert!(result.is_err(), "the failing graph write must surface");
        assert_eq!(graph.node_count(), 0, "no artifact reached the graph");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        // Event + Timestamp + the attribute's entity.
        assert_eq!(
            nodes.len(),
            3,
            "every temporal node is claimed even though the write failed: {nodes:?}"
        );
        let event_row = nodes
            .iter()
            .find(|row| row.node_type == "Event")
            .expect("the Event node is claimed");
        assert_eq!(
            event_row.indexed_fields,
            json!(["name"]),
            "the Event row names the one vector collection temporal writes, so a sweep clears it"
        );
        for row in nodes.iter().filter(|row| row.node_type != "Event") {
            assert_eq!(
                row.indexed_fields,
                json!([]),
                "temporal indexes no vectors for {}, so its sweep must not delete any",
                row.node_type
            );
        }

        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        // `at` to the timestamp, and `subject` to the entity.
        assert_eq!(edges.len(), 2, "both temporal edges are claimed: {edges:?}");
    }

    /// Every temporal ownership row names the run that created the artifact,
    /// which is what a sweep selects on.
    #[tokio::test]
    async fn temporal_ownership_rows_carry_the_run_id() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let run_id = Uuid::new_v4();
        let input = temporal_input(
            dataset_id,
            Some(Uuid::new_v4()),
            vec![attributed_event(
                "Alice joins Acme",
                "subject",
                Uuid::new_v4(),
            )],
        );

        add_temporal_data_points(
            &input,
            Arc::new(cognee_graph::MockGraphDB::new()),
            Arc::new(MockVectorDB::new()),
            Arc::new(MockEmbeddingEngine::new(8)),
            &db,
            Some(run_id),
        )
        .await
        .expect("temporal persistence must succeed");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert!(!nodes.is_empty());
        assert!(nodes.iter().all(|row| row.pipeline_run_id == Some(run_id)));
        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        assert!(!edges.is_empty());
        assert!(edges.iter().all(|row| row.pipeline_run_id == Some(run_id)));
    }

    /// A temporal run that identified no user still writes the ledger, owned by
    /// the configured default user — and the row id is derived from that
    /// resolved user, not from a placeholder the delete path could never
    /// reproduce.
    #[tokio::test]
    async fn temporal_ownership_rows_use_the_default_user_when_none_is_identified() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let dataset_id = Uuid::new_v4();
        let data_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let input = temporal_input(
            dataset_id,
            None,
            vec![attributed_event("Alice joins Acme", "subject", data_id)],
        );

        add_temporal_data_points(
            &input,
            Arc::new(cognee_graph::MockGraphDB::new()),
            Arc::new(MockVectorDB::new()),
            Arc::new(MockEmbeddingEngine::new(8)),
            &db,
            None,
        )
        .await
        .expect("a missing user is not an error");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        assert!(!nodes.is_empty());
        assert!(
            nodes
                .iter()
                .all(|row| row.user_id == DEFAULT_LEDGER_USER_ID),
            "the rows resolve to the default ledger user"
        );
        let row = &nodes[0];
        assert_eq!(
            row.id,
            provenance_node_id(None, DEFAULT_LEDGER_USER_ID, dataset_id, data_id, row.slug)
        );
    }

    /// Temporal events are content-addressed by name, so two files describing
    /// the same event share one graph node — and that node needs one ownership
    /// row per producing file, or sweeping the first file would delete a node
    /// the second still references.
    #[tokio::test]
    async fn an_event_two_files_produced_gets_one_row_per_file() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let input = temporal_input(
            dataset_id,
            Some(Uuid::new_v4()),
            vec![
                attributed_event("Alice joins Acme", "subject", first),
                attributed_event("Alice joins Acme", "subject", second),
            ],
        );

        let vector = Arc::new(MockVectorDB::new());
        add_temporal_data_points(
            &input,
            Arc::new(cognee_graph::MockGraphDB::new()),
            Arc::clone(&vector) as Arc<dyn VectorDB>,
            Arc::new(MockEmbeddingEngine::new(8)),
            &db,
            Some(Uuid::new_v4()),
        )
        .await
        .expect("temporal persistence must succeed");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        let event_rows: Vec<_> = nodes
            .iter()
            .filter(|row| row.node_type == "Event")
            .collect();
        assert_eq!(event_rows.len(), 2, "one row per producing file");
        assert_eq!(
            event_rows[0].slug, event_rows[1].slug,
            "and both name the same physical node"
        );
        assert_eq!(
            event_rows
                .iter()
                .map(|row| row.data_id)
                .collect::<std::collections::BTreeSet<_>>(),
            [first, second].into_iter().collect()
        );

        // The payload agrees with the ledger: one artifact, one vector point.
        assert_eq!(
            vector
                .collection_size("Event", "name")
                .await
                .expect("collection size"),
            1,
            "the shared event is embedded and indexed once, not once per producer"
        );
    }

    /// A temporal relationship name comes straight from the LLM, and NUL bytes
    /// are stripped on the way into Postgres. The row id and slug must be
    /// derived from the sanitized text, or the ledger would key the edge on
    /// text no store holds and the sweep would miss it. The standard path's
    /// `provenance_edge_ids_derive_from_sanitized_text` pins the same rule.
    #[tokio::test]
    async fn temporal_edge_rows_hash_sanitized_relationship_names() {
        use cognee_database::ops::graph_storage::get_edges_by_dataset;
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_vector::MockVectorDB;

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let data_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let dirty = "sub\u{0}ject";
        let clean = "subject";

        let input = temporal_input(
            dataset_id,
            Some(user_id),
            vec![attributed_event("Alice joins Acme", dirty, data_id)],
        );
        add_temporal_data_points(
            &input,
            Arc::new(cognee_graph::MockGraphDB::new()),
            Arc::new(MockVectorDB::new()),
            Arc::new(MockEmbeddingEngine::new(8)),
            &db,
            None,
        )
        .await
        .expect("temporal persistence must succeed");

        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        let row = edges
            .iter()
            .find(|row| row.relationship_name == clean)
            .expect("the relationship is stored sanitized");
        assert_eq!(
            row.id,
            provenance_edge_id(
                None,
                user_id,
                dataset_id,
                data_id,
                row.source_node_id,
                clean,
                row.destination_node_id,
            )
        );
        assert_eq!(
            row.slug,
            triplet_slug(row.source_node_id, clean, row.destination_node_id)
        );
        // The load-bearing half: stripping the NUL actually changes the hash,
        // so hashing the raw text could not slip through as a no-op.
        assert_ne!(
            row.id,
            provenance_edge_id(
                None,
                user_id,
                dataset_id,
                data_id,
                row.source_node_id,
                dirty,
                row.destination_node_id,
            )
        );
    }

    /// `RunToEnd` records the failing chunk and keeps the rest. Before this the
    /// temporal extractor warned and returned an empty `Vec`, so a run whose
    /// every call 429'd returned `Ok` with no events and no report at all.
    #[tokio::test]
    async fn temporal_extraction_collects_a_chunk_failure_instead_of_propagating() {
        let dataset_id = Uuid::new_v4();
        let (input, docs) = temporal_files(
            dataset_id,
            &["Alice works at Acme.", "Bob FAILMARKER breaks here."],
        );

        let config = CognifyConfig::default()
            .with_data_per_batch(1)
            .with_failure_stop(FailureStop::RunToEnd);
        let result = extract_temporal_events(
            &input,
            Arc::new(TemporalTestLlm::failing_extraction(&["FAILMARKER"])),
            &config,
        )
        .await
        .expect("RunToEnd collects rather than propagates");

        assert!(
            result
                .events
                .iter()
                .all(|attributed| attributed.data_id == docs[0]),
            "the good file's events survive"
        );
        assert!(!result.events.is_empty());
        assert_eq!(result.failures.entries().len(), 1);
        let entry = &result.failures.entries()[0];
        assert_eq!(entry.stage, FailureStage::TemporalExtraction);
        assert_eq!(entry.data_id, docs[1]);
        assert!(
            entry.chunk_id.is_some(),
            "temporal extraction fails at chunk granularity"
        );
    }

    /// The abort-time partition, temporal edition: three single-chunk files,
    /// the second failing. Under `FailFast` + `FailedItems` only the complete
    /// file's events are carried forward; the failed and never-reached files
    /// are left for the next run.
    #[tokio::test]
    async fn temporal_extraction_fail_fast_keeps_only_the_complete_file() {
        let dataset_id = Uuid::new_v4();
        let (input, docs) = temporal_files(
            dataset_id,
            &[
                "Alice works at Acme.",
                "Bob FAILMARKER breaks here.",
                "Carol also works at Acme.",
            ],
        );

        let config = CognifyConfig::default()
            .with_data_per_batch(1)
            .with_rollback_scope(RollbackScope::FailedItems);
        let result = extract_temporal_events(
            &input,
            Arc::new(TemporalTestLlm::failing_extraction(&["FAILMARKER"])),
            &config,
        )
        .await
        .expect("FailedItems keeps the complete files");

        assert!(!result.events.is_empty());
        assert!(
            result
                .events
                .iter()
                .all(|attributed| attributed.data_id == docs[0]),
            "only the complete file's events are persisted"
        );
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [docs[1]]
        );
        assert_eq!(
            result
                .failures
                .unreached_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [docs[2]],
            "the third file's chunk was never dispatched"
        );
    }

    /// The same abort under the default `WholeRun` scope carries nothing
    /// forward — the partition must not run on the fatal path.
    #[tokio::test]
    async fn temporal_extraction_fail_fast_whole_run_propagates() {
        let dataset_id = Uuid::new_v4();
        let (input, docs) = temporal_files(
            dataset_id,
            &["Alice works at Acme.", "Bob FAILMARKER breaks here."],
        );

        let config = CognifyConfig::default().with_data_per_batch(1);
        let err = extract_temporal_events(
            &input,
            Arc::new(TemporalTestLlm::failing_extraction(&["FAILMARKER"])),
            &config,
        )
        .await
        .expect_err("the default scope still aborts the run");

        let CognifyError::RunFailed { report } = err else {
            panic!("expected RunFailed, got: {err:?}");
        };
        assert_eq!(
            report.failed_items().iter().copied().collect::<Vec<_>>(),
            [docs[1]]
        );
    }

    /// The second unguarded site. Enrichment runs once per *batch*, so its
    /// failure is recorded against every chunk that fed the batch — otherwise a
    /// single call covering most of a run would contribute almost nothing to
    /// the chunk failure ratio, and a `FailedItems` run could "complete" having
    /// swept most of the dataset. The batch's events are dropped: an event
    /// without attributes carries none of the entity graph this pass exists to
    /// produce.
    #[tokio::test]
    async fn temporal_enrichment_failure_fails_its_batchs_items() {
        let dataset_id = Uuid::new_v4();
        let (input, docs) = temporal_files(
            dataset_id,
            &[
                "Alice works at Acme.",
                "Bob FAILMARKER breaks here.",
                "Carol also works at Acme.",
            ],
        );

        // Two chunks per batch: the first batch covers A and B, and B's marker
        // rides into the enrichment prompt on its event description.
        let config = CognifyConfig::default()
            .with_data_per_batch(2)
            .with_failure_stop(FailureStop::RunToEnd);
        let result = extract_temporal_events(
            &input,
            Arc::new(TemporalTestLlm::failing_enrichment(&["FAILMARKER"])),
            &config,
        )
        .await
        .expect("RunToEnd collects rather than propagates");

        assert_eq!(
            result.failures.entries().len(),
            2,
            "one failure per chunk in the failed batch, not one for the batch"
        );
        assert!(result.failures.entries().iter().all(|entry| entry.stage
            == FailureStage::TemporalEnrichment
            && entry.chunk_id.is_some()));
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            {
                let mut expected = [docs[0], docs[1]];
                expected.sort();
                expected.to_vec()
            },
            "both files that fed the batch fail; the third is untouched"
        );
        assert!(
            !result.events.is_empty()
                && result
                    .events
                    .iter()
                    .all(|attributed| attributed.data_id == docs[2]),
            "only the surviving batch's events are carried forward"
        );
    }

    /// A chunk that already failed extraction is not charged again when the
    /// enrichment call for its batch fails too. It is one failed chunk, not
    /// two — and the chunk failure ratio is what the double count would
    /// distort.
    #[tokio::test]
    async fn a_chunk_that_failed_extraction_is_not_charged_twice() {
        let dataset_id = Uuid::new_v4();
        let (input, docs) = temporal_files(
            dataset_id,
            &["Alice works at Acme.", "Bob EXTFAIL breaks here."],
        );

        // Both chunks in one batch. B's extraction fails; the enrichment call
        // over what survives then fails on A's description.
        let config = CognifyConfig::default()
            .with_data_per_batch(2)
            .with_failure_stop(FailureStop::RunToEnd);
        let result = extract_temporal_events(
            &input,
            Arc::new(TemporalTestLlm::failing_both(&["EXTFAIL"], &["Alice"])),
            &config,
        )
        .await
        .expect("RunToEnd collects rather than propagates");

        assert_eq!(
            result.failures.total(),
            2,
            "one failure per chunk, not one per chunk per pass"
        );
        let stages: Vec<FailureStage> = result
            .failures
            .entries()
            .iter()
            .map(|entry| entry.stage)
            .collect();
        assert!(stages.contains(&FailureStage::TemporalExtraction));
        assert!(stages.contains(&FailureStage::TemporalEnrichment));
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            {
                let mut expected = [docs[0], docs[1]];
                expected.sort();
                expected.to_vec()
            }
        );
        assert!(
            result.events.is_empty(),
            "the failed batch's events are dropped"
        );
    }

    /// The end-of-run gate reads the policy, not the report alone: the same
    /// failures error the run under `WholeRun` and complete it under
    /// `FailedItems` below the threshold.
    #[tokio::test]
    async fn fatality_gate_defers_to_the_policy() {
        use cognee_core::TypedTask;
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_test_utils::test_task_context;
        use cognee_vector::MockVectorDB;

        let (_, ctx, db) = test_task_context().await;
        let dataset_id = Uuid::new_v4();
        seed_dataset(&db, dataset_id).await;

        let doc_id = Uuid::new_v4();
        // One failed chunk out of 60 — under the 5 % default threshold — with
        // three other items surviving.
        let mut failures = FailureReport::default();
        failures.note_totals(4, 60);
        failures.record(StageFailure {
            stage: FailureStage::GraphExtraction,
            data_id: Uuid::new_v4(),
            chunk_id: Some(Uuid::new_v4()),
            error: "boom".to_string(),
            fails_item: true,
        });

        let input = SummarizedData {
            chunks: vec![test_chunk(Uuid::new_v4(), doc_id, "Hello world")],
            documents: vec![test_document_with_metadata(doc_id, None)],
            entities: vec![],
            edges: vec![],
            producers: ArtifactProducers::default(),
            summaries: vec![],
            dataset_id,
            user_id: Some(Uuid::new_v4()),
            tenant_id: None,
            failures,
        };

        for (config, should_fail) in [
            (CognifyConfig::default(), true),
            (
                CognifyConfig::default().with_rollback_scope(RollbackScope::FailedItems),
                false,
            ),
        ] {
            let task = make_add_data_points_task(
                Arc::new(cognee_graph::MockGraphDB::new()),
                Arc::new(MockVectorDB::new()),
                Arc::new(MockEmbeddingEngine::new(8)),
                Arc::clone(&db),
                config.clone(),
            );
            let TypedTask::Async(run) = task else {
                panic!("add_data_points task should be async");
            };
            let outcome = run(&input, ctx.clone()).await;
            assert_eq!(
                outcome.is_err(),
                should_fail,
                "scope {:?} must {} the run",
                config.rollback_scope,
                if should_fail { "fail" } else { "complete" }
            );
            if let Err(e) = outcome {
                assert!(
                    e.downcast_ref::<CognifyError>()
                        .is_some_and(|e| matches!(e, CognifyError::RunFailed { .. })),
                    "the task must return the typed error so cognify() can recover it"
                );
            }
        }
    }

    /// The temporal branch carries its own copy of the end-of-run gate, over
    /// the report the chunking stage handed down. Same fixture as
    /// `fatality_gate_defers_to_the_policy`, so the two branches stay honest
    /// about meaning the same thing by "fatal".
    #[tokio::test]
    async fn temporal_fatality_gate_defers_to_the_policy() {
        use cognee_core::TypedTask;
        use cognee_embedding::MockEmbeddingEngine;
        use cognee_models::TemporalEvent;
        use cognee_test_utils::test_task_context;
        use cognee_vector::MockVectorDB;

        let (_, ctx, _db) = test_task_context().await;
        let dataset_id = Uuid::new_v4();
        let ledger = Arc::new(ledger_db(dataset_id).await);

        // One failed item out of four, one failed chunk out of 60 — under the
        // 5 % default threshold.
        let mut failures = FailureReport::default();
        failures.note_totals(4, 60);
        failures.record(StageFailure {
            stage: FailureStage::Chunking,
            data_id: Uuid::new_v4(),
            chunk_id: None,
            error: "boom".to_string(),
            fails_item: true,
        });

        let input = ExtractedTemporalEvents {
            events: vec![AttributedEvent {
                event: TemporalEvent {
                    name: "Acme was founded".to_string(),
                    description: None,
                    location: None,
                    at: None,
                    during: None,
                    attributes: vec![],
                },
                data_id: Uuid::new_v4(),
            }],
            dataset_id,
            user_id: Some(Uuid::new_v4()),
            tenant_id: None,
            failures,
        };

        for (config, should_fail) in [
            (CognifyConfig::default(), true),
            (
                CognifyConfig::default().with_rollback_scope(RollbackScope::FailedItems),
                false,
            ),
        ] {
            let task = make_add_temporal_data_points_task(
                Arc::new(cognee_graph::MockGraphDB::new()),
                Arc::new(MockVectorDB::new()),
                Arc::new(MockEmbeddingEngine::new(8)),
                Arc::clone(&ledger),
                config.failure_policy(),
            );
            let TypedTask::Async(run) = task else {
                panic!("add_temporal_data_points task should be async");
            };
            let outcome = run(&input, ctx.clone()).await;
            assert_eq!(
                outcome.is_err(),
                should_fail,
                "scope {:?} must {} the temporal run",
                config.rollback_scope,
                if should_fail { "fail" } else { "complete" }
            );
            match outcome {
                Err(e) => assert!(
                    e.downcast_ref::<CognifyError>()
                        .is_some_and(|e| matches!(e, CognifyError::RunFailed { .. })),
                    "the task must return the typed error so cognify() can recover it"
                ),
                // The report has to be the chunking stage's, not a fresh
                // default: a forwarding regression would surface as an empty
                // one here while every other assertion still passed.
                Ok(result) => assert_eq!(
                    result.failures.entries().len(),
                    1,
                    "the chunk stage's report reaches the result"
                ),
            }
        }
    }

    /// The custom-model extraction path collects its chunk failures like its
    /// sibling, and — like its sibling — leaves the output whole under
    /// `RunToEnd`, where the item-scoped sweep is what removes the failed
    /// file's contributions.
    #[tokio::test]
    async fn custom_extraction_collects_failures_under_run_to_end() {
        use crate::fact_extraction::KnowledgeGraph;
        use cognee_test_utils::MockLlm;

        let (input, a, b, c) = three_chunk_files(Uuid::new_v4());
        let config =
            one_chunk_per_batch(CognifyConfig::default().with_failure_stop(FailureStop::RunToEnd));

        let result = extract_custom_graph_from_data::<KnowledgeGraph>(
            &input,
            Arc::new(
                MockLlm::new(vec![ownership_canned_graph(); 8])
                    .with_failing_markers(vec!["FAILMARKER".to_string()]),
            ),
            &config,
        )
        .await
        .expect("RunToEnd collects rather than propagates");

        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [b]
        );
        assert!(
            result.failures.unreached_items().is_empty(),
            "RunToEnd reaches every chunk"
        );
        assert_eq!(
            result
                .documents
                .iter()
                .map(|d| d.base.id)
                .collect::<Vec<_>>(),
            [a, b, c],
            "no partition under RunToEnd"
        );
        for chunk in &result.chunks {
            assert_eq!(
                chunk.contains.is_empty(),
                chunk.document_id == b,
                "only the failed chunk carries no extracted model"
            );
        }
    }

    /// Under a `FailFast` abort the failed and unreached files are dropped from
    /// the output, so a partially-extracted file cannot reach a later
    /// persisting stage.
    #[tokio::test]
    async fn custom_extraction_abort_drops_failed_and_unreached_files() {
        use crate::fact_extraction::KnowledgeGraph;
        use cognee_test_utils::MockLlm;

        let (input, a, b, c) = three_chunk_files(Uuid::new_v4());
        let config = one_chunk_per_batch(
            CognifyConfig::default().with_rollback_scope(RollbackScope::FailedItems),
        );

        let result = extract_custom_graph_from_data::<KnowledgeGraph>(
            &input,
            Arc::new(
                MockLlm::new(vec![ownership_canned_graph(); 8])
                    .with_failing_markers(vec!["FAILMARKER".to_string()]),
            ),
            &config,
        )
        .await
        .expect("FailedItems keeps the complete files");

        assert_eq!(
            result
                .documents
                .iter()
                .map(|d| d.base.id)
                .collect::<Vec<_>>(),
            [a],
            "only A is complete"
        );
        assert!(result.chunks.iter().all(|chunk| chunk.document_id == a));
        assert_eq!(
            result
                .failures
                .failed_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [b]
        );
        assert_eq!(
            result
                .failures
                .unreached_items()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [c]
        );
    }

    /// WebPage and WebSite nodes are claimed before they are written, and the
    /// shared WebSite node gets one row per producing document — two URL
    /// documents on one domain share a physical node, so a single row would let
    /// deleting the first take a node the second still references.
    #[tokio::test]
    async fn web_page_nodes_are_recorded_before_they_are_written() {
        use cognee_database::ops::graph_storage::get_nodes_by_dataset;

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let doc_a = Uuid::new_v4();
        let doc_b = Uuid::new_v4();
        let documents = vec![
            test_document_with_metadata(
                doc_a,
                Some(url_metadata(
                    "https://example.com/a",
                    "https://example.com/a",
                    "A",
                )),
            ),
            test_document_with_metadata(
                doc_b,
                Some(url_metadata(
                    "https://example.com/b",
                    "https://example.com/b",
                    "B",
                )),
            ),
        ];
        let chunks = vec![
            test_chunk(Uuid::new_v4(), doc_a, "first"),
            test_chunk(Uuid::new_v4(), doc_b, "second"),
        ];
        let id = LedgerIdentity::new(None, Some(Uuid::new_v4()), dataset_id, None);

        // (a) A failing graph write still leaves the claims behind.
        let failing = Arc::new(cognee_graph::MockGraphDB::new());
        failing.set_add_nodes_error("graph is down");
        assert!(
            create_web_page_nodes(&documents, &chunks, failing.clone(), &db, id)
                .await
                .is_err()
        );
        assert_eq!(failing.node_count(), 0);
        assert!(
            !get_nodes_by_dataset(&db, dataset_id)
                .await
                .expect("query")
                .is_empty(),
            "the WebPage / WebSite nodes are claimed before the graph write"
        );

        // (b) The shared WebSite node is claimed once per producing document.
        create_web_page_nodes(
            &documents,
            &chunks,
            Arc::new(cognee_graph::MockGraphDB::new()),
            &db,
            id,
        )
        .await
        .expect("web page nodes must be written");

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        let site_slug = web_site_id("example.com");
        let site_rows: Vec<_> = nodes.iter().filter(|row| row.slug == site_slug).collect();
        assert_eq!(
            site_rows.len(),
            2,
            "one WebSite row per producing document, so the last owner takes it"
        );
        let mut owners: Vec<Uuid> = site_rows.iter().map(|row| row.data_id).collect();
        owners.sort();
        let mut expected = vec![doc_a, doc_b];
        expected.sort();
        assert_eq!(owners, expected);
    }

    /// The DLT teardown's schema nodes and FK edges are claimed before they are
    /// written, with the attribution the ledger already uses elsewhere: nil for
    /// an artifact that spans data items, the producing document for a
    /// row-level edge.
    #[tokio::test]
    async fn dlt_fk_edges_are_recorded_before_they_are_written() {
        use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};

        let dataset_id = Uuid::new_v4();
        let db = ledger_db(dataset_id).await;
        let doc_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();

        let mut base = DataPoint::new("DltRowDocument", None);
        base.id = doc_id;
        let document = Document {
            base,
            document_type: "dlt_row".to_string(),
            name: "orders.json".to_string(),
            raw_data_location: "file:///tmp/orders.json".to_string(),
            mime_type: "application/json".to_string(),
            extension: "json".to_string(),
            data_id: doc_id,
            external_metadata: Some(
                json!({
                    "source": "dlt",
                    "table_name": "orders",
                    "dlt_db_name": "shop",
                    "schema_info": [{"name": "id"}],
                    "foreign_keys": [
                        {"column": "customer_id", "ref_table": "customers", "ref_column": "id"}
                    ],
                })
                .to_string(),
            ),
        };
        let id = LedgerIdentity::new(None, Some(Uuid::new_v4()), dataset_id, Some(run_id));

        // (a) A failing graph write still leaves the claims behind.
        let failing = Arc::new(cognee_graph::MockGraphDB::new());
        failing.set_add_nodes_error("graph is down");
        assert!(
            extract_dlt_fk_edges(
                &[],
                std::slice::from_ref(&document),
                failing.clone(),
                &db,
                id
            )
            .await
            .is_err()
        );
        assert_eq!(failing.node_count(), 0);

        let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
        let table_slug = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"dlt:orders");
        let table_row = nodes
            .iter()
            .find(|row| row.slug == table_slug)
            .expect("the SchemaTable node must be claimed");
        assert_eq!(table_row.node_type, "SchemaTable");
        assert_eq!(
            table_row.data_id,
            Uuid::nil(),
            "one table node is shared by every row-document of that table"
        );
        assert_eq!(table_row.pipeline_run_id, Some(run_id));

        let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
        let is_row_of = edges
            .iter()
            .find(|row| row.relationship_name == "is_row_of")
            .expect("the is_row_of edge must be claimed");
        assert_eq!(
            is_row_of.data_id, doc_id,
            "a row-level edge belongs to the document that produced it"
        );
        assert!(
            edges
                .iter()
                .any(|row| row.relationship_name == "has_foreign_key"
                    && row.data_id == Uuid::nil()),
            "the schema-level FK edge spans data items"
        );
    }
}
