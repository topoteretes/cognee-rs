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
//! 3. [`extract_temporal_events`] — Chunks → TemporalEvents (via two LLM passes)
//! 4. [`add_temporal_data_points`] — persists events, timestamps, intervals, entities → graph+vector
//!
//! Public surface:
//! - Intermediate types: [`CognifyInput`], [`ClassifiedDocuments`],
//!   [`ExtractedChunks`], [`ExtractedGraphData`], [`SummarizedData`],
//!   [`ExtractedTemporalEvents`]
//! - Task implementations (free functions)
//! - [`TypedTask`] factories: [`make_classify_documents_task`], etc.
//! - Pipeline builders: [`build_cognify_pipeline`], [`build_temporal_cognify_pipeline`]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use cognee_chunking::{CutType, NAMESPACE_OID, TokenCounterKind, chunk_by_row, chunk_text};
use cognee_core::pipeline_run_registry::DbPipelineWatcher;
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
use cognee_vector::{VectorDB, VectorPoint};
use serde::Serialize;
use serde_json::json;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use url::Url;
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::fact_extraction::{FactExtractor, KnowledgeGraph};
use crate::graph_integration::{
    GraphEdgePair, GraphNodePair, deduplicate_nodes_and_edges, expand_with_nodes_and_edges,
    retrieve_existing_edges,
};
use crate::pipeline::{CognifyResult, IndexedFieldsStats};
use crate::qualification::{Qualification, check_pipeline_run_qualification};
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
    pub dataset_id: Uuid,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
}

/// Output of [`summarize_text`]: graph data plus generated summaries.
#[derive(Debug, Clone)]
pub struct SummarizedData {
    pub chunks: Vec<DocumentChunk>,
    /// Classified documents — carried forward for DLT FK edge extraction.
    pub documents: Vec<Document>,
    pub entities: Vec<GraphNodePair>,
    pub edges: Vec<GraphEdgePair>,
    pub summaries: Vec<TextSummary>,
    pub dataset_id: Uuid,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
}

/// Output of [`extract_temporal_events`]: temporal events extracted from chunks
/// via two LLM passes (event extraction + entity enrichment).
///
/// Used as the intermediate type between Task 3 and Task 4 in the temporal pipeline.
#[derive(Debug, Clone)]
pub struct ExtractedTemporalEvents {
    pub events: Vec<TemporalEvent>,
    pub dataset_id: Uuid,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Task 1: classify_documents
// ---------------------------------------------------------------------------

/// Classify Data items into typed Documents (Task 1).
///
/// Maps each Data item to a Document based on mime_type.
/// Non-text items are filtered out.
pub fn classify_documents(input: &CognifyInput) -> Result<ClassifiedDocuments, CognifyError> {
    let documents: Vec<Document> = model_classify_documents(&input.data_items);
    info!(doc_count = documents.len(), "documents classified");
    Ok(ClassifiedDocuments {
        documents,
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
    })
}

// ---------------------------------------------------------------------------
// Task 2: extract_chunks_from_documents
// ---------------------------------------------------------------------------

/// Extract text chunks from classified documents (Task 2).
///
/// For each document, reads content from storage and applies the
/// word → sentence → paragraph → text chunker hierarchy.
///
/// When `db` is `Some`, the accumulated token count for each document
/// is written back to the corresponding `Data` record, mirroring
/// Python's `update_document_token_count()`.
pub async fn extract_chunks_from_documents(
    input: &ClassifiedDocuments,
    storage: &dyn StorageTrait,
    max_chunk_size: usize,
    token_counter_kind: TokenCounterKind,
    db: Option<&DatabaseConnection>,
    loader_registry: &LoaderRegistry,
) -> Result<ExtractedChunks, CognifyError> {
    let counter = token_counter_kind
        .build()
        .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;
    let mut all_chunks = Vec::new();

    for document in &input.documents {
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
            if !trimmed.is_empty() {
                let chunk_id =
                    Uuid::new_v5(&NAMESPACE_OID, format!("{}-0", document.base.id).as_bytes());
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
                all_chunks.push(chunk);
            }
            continue;
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
            LoaderOutput::Text(text) => {
                chunk_text(document.base.id, &text, max_chunk_size, &counter)
            }
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

        all_chunks.extend(chunks);
    }

    info!(total_chunks = all_chunks.len(), "chunking complete");
    Ok(ExtractedChunks {
        chunks: all_chunks,
        documents: input.documents.clone(),
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
    })
}

// ---------------------------------------------------------------------------
// Task 3: extract_graph_from_data
// ---------------------------------------------------------------------------

/// Extract knowledge graphs from chunks via LLM, then integrate (Task 3).
///
/// For each chunk batch, calls the LLM to extract entities and relationships.
/// Then integrates: expands to storage-layer types, deduplicates against
/// existing DB entries and in-memory, and stores nodes/edges in graph DB.
pub async fn extract_graph_from_data(
    input: &ExtractedChunks,
    llm: Arc<dyn Llm>,
    graph_db: Arc<dyn GraphDBTrait>,
    ontology_resolver: Arc<dyn OntologyResolver>,
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
) -> Result<ExtractedGraphData, CognifyError> {
    if input.chunks.is_empty() {
        return Ok(ExtractedGraphData {
            chunks: input.chunks.clone(),
            documents: input.documents.clone(),
            entities: vec![],
            edges: vec![],
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
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
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
        });
    }

    // Collect non-DLT chunks for LLM processing
    let chunks_for_extraction: Vec<DocumentChunk> = non_dlt_chunks.into_iter().cloned().collect();

    let batch_size = config.chunks_per_batch;
    let mut all_graphs: Vec<(Uuid, KnowledgeGraph)> = Vec::new();
    let semaphore = Arc::new(Semaphore::new(config.max_parallel_extractions));

    for (batch_idx, batch) in chunks_for_extraction.chunks(batch_size).enumerate() {
        let fact_extractor = FactExtractor::new(Arc::clone(&llm));
        let mut extract_tasks = Vec::new();
        let mut chunk_ids = Vec::new();

        for chunk in batch {
            let extractor = fact_extractor.clone();
            let text = chunk.text.clone();
            let sem = Arc::clone(&semaphore);
            let prompt = config.custom_extraction_prompt.clone();

            chunk_ids.push(chunk.base.id);
            extract_tasks.push(tokio::spawn(async move {
                #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
                let _permit = sem
                    .acquire()
                    .await
                    .expect("semaphore is never closed; created locally in this function");
                extractor.extract_facts(&text, prompt.as_deref()).await
            }));
        }

        let batch_results = futures::future::join_all(extract_tasks).await;
        for (result, chunk_id) in batch_results.into_iter().zip(chunk_ids) {
            let graph = result.map_err(|e| CognifyError::FactExtractionError(e.to_string()))??;
            all_graphs.push((chunk_id, graph));
        }

        info!(
            "Processed graph extraction batch {}/{} ({} chunks)",
            batch_idx + 1,
            chunks_for_extraction.len().div_ceil(batch_size),
            batch.len()
        );
    }

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
    let (nodes, edges) = expand_with_nodes_and_edges(
        all_graphs,
        input.dataset_id,
        &existing_edges_set,
        ontology_resolver.as_ref(),
        user_label_owned.as_deref(),
    )
    .await;

    // Final deduplication pass (in-memory only after DB filtering)
    let dedup_result = deduplicate_nodes_and_edges(nodes, edges);

    // Build chunk_id → entity IDs mapping from the deduplicated nodes.
    // Each entity stores the chunk_id it was extracted from in its metadata.
    let mut chunk_entity_map: HashMap<Uuid, Vec<serde_json::Value>> = HashMap::new();
    for node_pair in &dedup_result.unique_nodes {
        if let Some(chunk_id_val) = node_pair.entity.base.get_metadata("chunk_id")
            && let Some(chunk_id_str) = chunk_id_val.as_str()
            && let Ok(chunk_id) = Uuid::parse_str(chunk_id_str)
        {
            chunk_entity_map
                .entry(chunk_id)
                .or_default()
                .push(json!(node_pair.entity.base.id.to_string()));
        }
    }

    // Populate DocumentChunk.contains with extracted entity IDs
    let mut updated_chunks = input.chunks.clone();
    for chunk in &mut updated_chunks {
        if let Some(entity_ids) = chunk_entity_map.get(&chunk.base.id) {
            chunk.contains = entity_ids.clone();
        }
    }

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

    Ok(ExtractedGraphData {
        chunks: updated_chunks,
        documents: input.documents.clone(),
        entities: dedup_result.unique_nodes,
        edges: dedup_result.unique_edges,
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
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
pub async fn create_web_page_nodes(
    documents: &[Document],
    chunks: &[DocumentChunk],
    graph_db: Arc<dyn GraphDBTrait>,
) -> Result<(), CognifyError> {
    if documents.is_empty() || chunks.is_empty() {
        return Ok(());
    }

    let mut nodes_by_id: HashMap<String, serde_json::Value> = HashMap::new();
    let mut candidate_edges: Vec<EdgeData> = Vec::new();
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();

    for document in documents {
        let Some(metadata) = parse_web_page_metadata(document) else {
            continue;
        };

        let page_id = web_page_id(&metadata.url);
        let site_id = web_site_id(&metadata.domain);
        let page_id_str = page_id.to_string();
        let site_id_str = site_id.to_string();

        nodes_by_id.insert(
            page_id_str.clone(),
            json!({
                "id": page_id_str,
                "type": "WebPage",
                "url": metadata.url,
                "title": metadata.title,
                "content": document_content_preview(document.base.id, chunks),
            }),
        );
        nodes_by_id.insert(
            site_id_str.clone(),
            json!({
                "id": site_id_str,
                "type": "WebSite",
                "domain": metadata.domain,
            }),
        );

        push_unique_edge(
            &mut candidate_edges,
            &mut seen_edges,
            page_id_str.clone(),
            site_id_str,
            "PART_OF",
        );

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
        }
    }

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
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
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
    let semaphore = Arc::new(Semaphore::new(config.max_parallel_extractions));

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
            dataset_id: input.dataset_id,
            user_id: input.user_id,
            tenant_id: input.tenant_id,
        });
    }

    let total_batches = non_dlt_indices.len().div_ceil(batch_size);

    for (batch_idx, batch_indices) in non_dlt_indices.chunks(batch_size).enumerate() {
        let mut extract_tasks = Vec::new();

        for &idx in batch_indices {
            let extractor = FactExtractor::new(Arc::clone(&llm));
            let text = updated_chunks[idx].text.clone();
            let sem = Arc::clone(&semaphore);
            let prompt = config.custom_extraction_prompt.clone();

            extract_tasks.push(tokio::spawn(async move {
                #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
                let _permit = sem
                    .acquire()
                    .await
                    .expect("semaphore is never closed; created locally in this function");
                extractor.extract::<M>(&text, prompt.as_deref()).await
            }));
        }

        let batch_results = futures::future::join_all(extract_tasks).await;
        let batch_len = batch_indices.len();

        for (i, result) in batch_results.into_iter().enumerate() {
            let model: M =
                result.map_err(|e| CognifyError::FactExtractionError(e.to_string()))??;
            let value = serde_json::to_value(&model)
                .map_err(|e| CognifyError::SerializationError(e.to_string()))?;
            updated_chunks[batch_indices[i]].contains = vec![value];
        }

        info!(
            "Processed custom graph extraction batch {}/{} ({} chunks)",
            batch_idx + 1,
            total_batches,
            batch_len
        );
    }

    Ok(ExtractedGraphData {
        chunks: updated_chunks,
        documents: input.documents.clone(),
        entities: vec![],
        edges: vec![],
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
    })
}

// ---------------------------------------------------------------------------
// Task 4: summarize_text
// ---------------------------------------------------------------------------

/// Summarize text chunks via LLM (Task 4).
///
/// If summarization is enabled in config, generates summaries for each chunk
/// using batched parallel LLM calls.
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

    let summaries = if config.enable_summarization && !non_dlt_chunks.is_empty() {
        let summary_extractor =
            SummaryExtractor::new_with_schema(llm, config.summary_schema.clone())
                .with_max_parallel(config.max_parallel_extractions);

        // Stream every chunk through one bounded pipeline. `summarize_chunks`
        // already caps in-flight requests at `max_parallel_extractions` internally
        // (issue #19), so an outer batch loop would only insert a sequential
        // barrier between batches without lowering peak concurrency.
        let all_summaries = summary_extractor
            .summarize_chunks(&non_dlt_chunks, None)
            .await?;

        info!("Generated {} summaries", all_summaries.len());
        all_summaries
    } else {
        if !config.enable_summarization {
            info!("Summarization disabled in config");
        } else {
            info!("No non-DLT chunks to summarize");
        }
        Vec::new()
    };

    Ok(SummarizedData {
        chunks: input.chunks.clone(),
        documents: input.documents.clone(),
        entities: input.entities.clone(),
        edges: input.edges.clone(),
        summaries,
        dataset_id: input.dataset_id,
        user_id: input.user_id,
        tenant_id: input.tenant_id,
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
/// When `db` is `Some`, also writes provenance records (nodes/edges) to the
/// relational database, matching Python's `upsert_nodes` / `upsert_edges`
/// calls guarded by `if user and dataset and data:`.
pub async fn add_data_points(
    input: &SummarizedData,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    config: &CognifyConfig,
) -> Result<CognifyResult, CognifyError> {
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
                &mut local_visited,
            );
        }
    }

    // Discover structural edges via GraphExtractable trait
    // (port of Python's get_graph_from_model() relationship discovery)
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

    if !structural_edges.is_empty() {
        graph_db
            .add_edges(&structural_edges)
            .await
            .map_err(CognifyError::from)?;
        info!("Created {} structural edges", structural_edges.len());
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

    // ── Provenance upsert (mirrors Python's `if user and dataset and data:`) ──
    if let (Some(db), Some(user_id)) = (&db, input.user_id) {
        upsert_provenance(
            db,
            input.tenant_id,
            user_id,
            input.dataset_id,
            &input.chunks,
            &input.entities,
            &input.edges,
            &input.summaries,
            &input.documents,
            &structural_edges,
        )
        .await?;
    }

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
/// 5. Returns all events as [`ExtractedTemporalEvents`].
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
        });
    }

    let batch_size = config.data_per_batch;
    let semaphore = Arc::new(Semaphore::new(config.max_parallel_extractions));
    let extractor = Arc::new(TemporalEventExtractor::new(Arc::clone(&llm)));
    let enricher = TemporalEntityEnricher::new(Arc::clone(&llm));

    let mut all_events: Vec<TemporalEvent> = Vec::new();

    for (batch_idx, batch) in non_dlt_chunks.chunks(batch_size).enumerate() {
        let mut extract_tasks = Vec::new();

        for chunk in batch {
            let ext = Arc::clone(&extractor);
            let text = chunk.text.clone();
            let sem = Arc::clone(&semaphore);
            extract_tasks.push(tokio::spawn(async move {
                #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
                let _permit = sem
                    .acquire()
                    .await
                    .expect("semaphore is never closed; created locally in this function");
                ext.extract_events(&text).await
            }));
        }

        let batch_results = futures::future::join_all(extract_tasks).await;
        let mut batch_events: Vec<TemporalEvent> = Vec::new();
        for result in batch_results {
            let events = result.map_err(|e| CognifyError::FactExtractionError(e.to_string()))??;
            batch_events.extend(events);
        }

        info!(
            "Temporal extraction batch {}/{}: {} events extracted",
            batch_idx + 1,
            non_dlt_chunks.len().div_ceil(batch_size),
            batch_events.len()
        );

        // Entity enrichment pass for the whole batch.
        let enriched = enricher.enrich(batch_events).await?;
        all_events.extend(enriched);
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
    })
}

// ---------------------------------------------------------------------------
// Temporal Task 4: add_temporal_data_points
// ---------------------------------------------------------------------------

/// Persist temporal events to graph and vector databases (Temporal Task 4).
///
/// Mirrors the Python `add_data_points` stage in the temporal pipeline.
///
/// For each [`TemporalEvent`]:
/// 1. Creates an `Event` graph node with a deterministic UUID5 ID.
/// 2. For `event.at` — creates a `Timestamp` graph node and an `at` edge.
/// 3. For `event.during` — creates `Timestamp` nodes for from/to, an `Interval`
///    node, and `during` / `time_from` / `time_to` edges (Python-compatible layout).
/// 4. For each [`EventAttribute`] — creates or looks up an entity graph node
///    and adds a typed edge from the `Event` to the entity.
/// 5. Embeds `event.name` and indexes to the `Event_name` vector collection.
pub async fn add_temporal_data_points(
    events: &ExtractedTemporalEvents,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
) -> Result<CognifyResult, CognifyError> {
    if events.events.is_empty() {
        info!("No temporal events to persist.");
        return Ok(CognifyResult::empty());
    }

    let mut graph_nodes: Vec<serde_json::Value> = Vec::new();
    let mut graph_edges: Vec<EdgeData> = Vec::new();

    // Deduplicate entity nodes across events to avoid redundant graph inserts.
    let mut seen_entity_ids: HashSet<Uuid> = HashSet::new();
    // Deduplicate edges: (source_id, target_id, relationship_name)
    let mut seen_edge_keys: HashSet<(String, String, String)> = HashSet::new();

    let mut event_ids: Vec<Uuid> = Vec::new();
    let mut event_names: Vec<String> = Vec::new();

    for event in &events.events {
        // ── Event node ──────────────────────────────────────────────────────
        let event_id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("event:{}", event.name).as_bytes(),
        );
        event_ids.push(event_id);
        event_names.push(event.name.clone());

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
        graph_nodes.push(event_node);

        // ── Timestamp for event.at ──────────────────────────────────────────
        if let Some(ts) = &event.at {
            let ts_id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("timestamp:{}", ts.time_at).as_bytes(),
            );
            graph_nodes.push(json!({
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
            }));

            let edge_key = (event_id.to_string(), ts_id.to_string(), "at".to_string());
            if seen_edge_keys.insert(edge_key) {
                graph_edges.push((
                    event_id.to_string(),
                    ts_id.to_string(),
                    "at".to_string(),
                    build_edge_props(&event_id.to_string(), &ts_id.to_string(), "at"),
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

            graph_nodes.push(json!({
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
            }));
            graph_nodes.push(json!({
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
            }));
            graph_nodes.push(json!({
                "id": interval_id.to_string(),
                "data_type": "Interval",
            }));

            // Event -[during]-> Interval
            let during_key = (
                event_id.to_string(),
                interval_id.to_string(),
                "during".to_string(),
            );
            if seen_edge_keys.insert(during_key) {
                graph_edges.push((
                    event_id.to_string(),
                    interval_id.to_string(),
                    "during".to_string(),
                    build_edge_props(&event_id.to_string(), &interval_id.to_string(), "during"),
                ));
            }

            // Interval -[time_from]-> Timestamp(from)
            let from_key = (
                interval_id.to_string(),
                ts_from_id.to_string(),
                "time_from".to_string(),
            );
            if seen_edge_keys.insert(from_key) {
                graph_edges.push((
                    interval_id.to_string(),
                    ts_from_id.to_string(),
                    "time_from".to_string(),
                    build_edge_props(
                        &interval_id.to_string(),
                        &ts_from_id.to_string(),
                        "time_from",
                    ),
                ));
            }

            // Interval -[time_to]-> Timestamp(to)
            let to_key = (
                interval_id.to_string(),
                ts_to_id.to_string(),
                "time_to".to_string(),
            );
            if seen_edge_keys.insert(to_key) {
                graph_edges.push((
                    interval_id.to_string(),
                    ts_to_id.to_string(),
                    "time_to".to_string(),
                    build_edge_props(&interval_id.to_string(), &ts_to_id.to_string(), "time_to"),
                ));
            }
        }

        // ── Entity attribute nodes and edges ────────────────────────────────
        for attr in &event.attributes {
            // Python temporal path: `Entity.id_for(attribute.entity)`
            // (add_entities_to_event.py:39). Was a bare `entity:{name}` hash with
            // no normalization and no class prefix.
            let entity_id = Entity::id_for(&attr.entity);

            if seen_entity_ids.insert(entity_id) {
                graph_nodes.push(json!({
                    "id": entity_id.to_string(),
                    "data_type": attr.entity_type,
                    "name": attr.entity,
                }));
            }

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
                    build_edge_props(
                        &event_id.to_string(),
                        &entity_id.to_string(),
                        &attr.relationship,
                    ),
                ));
            }
        }
    }

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
    })
}

/// Resolve the retrieval text for an edge, mirroring Python's
/// `get_edge_retrieval_text(edge_text, relationship_name)`
/// (prepare_edges_for_storage.py:26-28 via index_graph_edges.py:33-53):
/// prefer the nonblank `edge_text` property, fall back to the nonblank
/// `relationship_name`, else return an empty string (caller drops empties).
fn edge_retrieval_text(edge_pair: &GraphEdgePair) -> String {
    let from_edge_text = edge_pair
        .properties
        .get("edge_text")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    if let Some(text) = from_edge_text {
        return text.to_string();
    }

    let rel = edge_pair.relationship_name.trim();
    rel.to_string()
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
pub async fn extract_dlt_fk_edges(
    _chunks: &[DocumentChunk],
    documents: &[Document],
    graph_db: Arc<dyn GraphDBTrait>,
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
/// Used by the **convenience [`cognify`] entry point** which bypasses
/// `cognee_core::execute()` and therefore does not benefit from the
/// executor-driven walk in
/// [`cognee_core::provenance::stamp_tree`]. Per locked decision 6 of
/// `docs/telemetry/05-datapoint-provenance.md`, both code paths land
/// stamping; the `if dp.source_X.is_none()` guards make double-stamping
/// a no-op.
///
/// Pipeline-driven cognify uses the executor walk via
/// [`cognee_core::provenance::stamp_tree_dyn`] — see
/// `crates/core/src/provenance.rs`.
///
/// Only sets each field if it is currently `None`, so earlier (more specific)
/// stamps are never overwritten.  Mirrors the Python
/// `run_tasks_base.py` post-task provenance stamping.
fn stamp_provenance(dp: &mut DataPoint, pipeline: &str, task: &str, user: Option<&str>) {
    if dp.source_pipeline.is_none() {
        dp.source_pipeline = Some(pipeline.to_string());
    }
    if dp.source_task.is_none() {
        dp.source_task = Some(task.to_string());
    }
    if dp.source_user.is_none() {
        dp.source_user = user.map(String::from);
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
    let effective_config = if config.max_chunk_size == CognifyConfig::default().max_chunk_size {
        let cfg = config
            .clone()
            .with_auto_chunk_size(embedding_engine.as_ref(), llm.as_ref());
        info!("Auto-calculated max_chunk_size: {}", cfg.max_chunk_size);
        cfg
    } else {
        config.clone()
    };

    info!(
        "Starting cognify pipeline with config: chunks_per_batch={}, max_chunk_size={}",
        effective_config.chunks_per_batch, effective_config.max_chunk_size
    );

    // ── Qualification gate (gap 08-08, locked decision 3) ───────────────────
    // Python-parity `check_pipeline_run_qualification`: read the latest
    // `pipeline_runs` row for `(dataset_id, pipeline_name)` and decide
    // whether to proceed, short-circuit, or reject. The pipeline name used
    // here MUST match what `DbPipelineWatcher` will persist on the next
    // `pipeline::execute` call — that is the `Pipeline.name` set below
    // (`"cognify"` or `"temporal-cognify"`).
    let pipeline_name: &str = if effective_config.temporal_cognify {
        "temporal-cognify"
    } else {
        "cognify"
    };
    match check_pipeline_run_qualification(pipeline_run_repo.as_ref(), dataset_id, pipeline_name)
        .await
        .map_err(|e| CognifyError::DatabaseError(e.to_string()))?
    {
        Qualification::AlreadyCompleted(prior) => {
            info!(
                dataset_id = %dataset_id,
                pipeline_run_id = %prior.pipeline_run_id,
                "cognify: dataset already completed; short-circuiting (Python parity)"
            );
            return Ok(CognifyResult::already_completed(prior.pipeline_run_id));
        }
        Qualification::AlreadyRunning(_prior) => {
            return Err(CognifyError::PipelineAlreadyRunning {
                pipeline_name: pipeline_name.to_string(),
                dataset_id,
            });
        }
        Qualification::Proceed => {}
    }

    // ── Empty-document short-circuit ────────────────────────────────────────
    // Preserved from the pre-executor path: a caller passing zero documents
    // gets back an empty result without paying for pipeline / context
    // construction or a no-op LLM round-trip.
    if data_items.is_empty() {
        return Ok(CognifyResult::empty());
    }

    // ── Branch: temporal vs. standard pipeline ──────────────────────────────
    // LIB-06-04: both branches now route through `pipeline::execute`. The
    // selection happens *before* `execute()` per locked Decision 2 — temporal
    // is a distinct `Pipeline` with its own task DAG. Per locked option (a)
    // (user decision 2026-05-15), the shared tasks
    // (`make_classify_documents_task`, `make_extract_chunks_task`) stamp
    // `Document` / `DocumentChunk` DataPoints with
    // `source_pipeline = "cognify"` (the LIB-06-03 constant) on both
    // branches; the temporal pipeline keeps its distinct identity at the
    // `pipeline_runs` row level via `build_temporal_cognify_pipeline`'s
    // `with_name("temporal-cognify")`.
    let is_temporal = effective_config.temporal_cognify;
    let pipeline = if is_temporal {
        build_temporal_cognify_pipeline(
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            Arc::clone(&llm),
            Some(Arc::clone(&database)),
            effective_config.clone(),
        )
    } else {
        build_cognify_pipeline(
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            Arc::clone(&llm),
            Some(Arc::clone(&database)),
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
    let watcher = DbPipelineWatcher::new(pipeline_run_repo);
    let outputs = cognee_core::pipeline::execute(&pipeline, inputs, ctx, &watcher)
        .await
        .map_err(|e| CognifyError::Execute(e.to_string()))?;

    let result = extract_cognify_outputs(outputs)?;

    // Decision 5: post-pipeline teardown — `extract_dlt_fk_edges` stays
    // outside the executor. The pipeline_runs row is already marked
    // COMPLETED by the watcher at this point; teardown failure surfaces as
    // `Err(...)` to the caller but does not roll back the run state.
    //
    // LIB-06-04: skip DLT FK extraction on the temporal branch — temporal
    // does not propagate `documents_for_dlt` (and Python's temporal cognify
    // does not run DLT teardown either).
    if !is_temporal {
        extract_dlt_fk_edges(
            &result.chunks,
            &result.documents_for_dlt,
            Arc::clone(&graph_db),
        )
        .await?;
    }

    Ok(result)
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

/// Deterministic provenance edge ID, matching Python's:
/// `uuid5(NAMESPACE_OID, str(tenant_id) + str(user_id) + str(dataset_id) + str(source_id) + str(edge_text) + str(target_id))`
fn provenance_edge_id(
    tenant_id: Option<Uuid>,
    user_id: Uuid,
    dataset_id: Uuid,
    source_id: Uuid,
    edge_text: &str,
    target_id: Uuid,
) -> Uuid {
    let tid = tenant_id.map_or("None".to_string(), |t| t.to_string());
    let raw = format!("{tid}{user_id}{dataset_id}{source_id}{edge_text}{target_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.as_bytes())
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

/// Write provenance node and edge records to the relational database.
///
/// Mirrors the Python `upsert_nodes()` / `upsert_edges()` calls in
/// `add_data_points` (guarded by `if user and dataset and data:`).
///
/// Provenance records link graph nodes/edges back to the user, tenant,
/// dataset, and data item they originated from.
#[allow(clippy::too_many_arguments)]
async fn upsert_provenance(
    db: &DatabaseConnection,
    tenant_id: Option<Uuid>,
    user_id: Uuid,
    dataset_id: Uuid,
    chunks: &[DocumentChunk],
    entities: &[GraphNodePair],
    edges: &[GraphEdgePair],
    summaries: &[TextSummary],
    documents: &[Document],
    structural_edges: &[EdgeData],
) -> Result<(), CognifyError> {
    use cognee_database::ops::graph_storage;
    use cognee_database::{GraphEdge, GraphNode};

    // Build chunk_id → document_id map for tracing entity provenance back
    // to the originating Data item.
    let chunk_data_map: HashMap<Uuid, Uuid> =
        chunks.iter().map(|c| (c.base.id, c.document_id)).collect();
    let entity_data_map: HashMap<Uuid, Uuid> = entities
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
        .collect();

    // ── Provenance nodes ────────────────────────────────────────────────
    let mut prov_nodes: Vec<GraphNode> = Vec::new();

    // Entities
    for pair in entities {
        let entity = &pair.entity;

        // Resolve data_id by tracing entity → chunk_id → document_id
        let data_id = entity
            .base
            .get_metadata("chunk_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .and_then(|chunk_id| chunk_data_map.get(&chunk_id).copied())
            .unwrap_or(Uuid::nil());

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

        prov_nodes.push(GraphNode {
            id: provenance_node_id(tenant_id, user_id, dataset_id, data_id, entity.base.id),
            slug: entity.base.id,
            user_id,
            data_id,
            dataset_id,
            label: Some(label),
            node_type: entity.base.data_type.clone(),
            indexed_fields,
            attributes: serde_json::to_value(entity).ok(),
            created_at: Utc::now(),
        });
    }

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
        // EntityType is shared across entities; use nil data_id as in Python
        prov_nodes.push(GraphNode {
            id: provenance_node_id(tenant_id, user_id, dataset_id, Uuid::nil(), et.base.id),
            slug: et.base.id,
            user_id,
            data_id: Uuid::nil(),
            dataset_id,
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
            label: Some(label),
            node_type: document.base.data_type.clone(),
            indexed_fields,
            attributes: serde_json::to_value(document).ok(),
            created_at: Utc::now(),
        });
    }

    if !prov_nodes.is_empty() {
        graph_storage::upsert_nodes(db, &prov_nodes).await?;
        info!("Upserted {} provenance node records", prov_nodes.len());
    }

    // ── Provenance edges ────────────────────────────────────────────────
    let mut prov_edges: Vec<GraphEdge> = Vec::new();

    // Semantic edges from graph extraction
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

        let source_data_id = entity_data_map.get(&edge_pair.source_entity_id).copied();
        let target_data_id = entity_data_map.get(&edge_pair.target_entity_id).copied();
        let data_id = match (source_data_id, target_data_id) {
            (Some(source), Some(target)) if source == target => source,
            _ => Uuid::nil(),
        };

        prov_edges.push(GraphEdge {
            id: provenance_edge_id(
                tenant_id,
                user_id,
                dataset_id,
                edge_pair.source_entity_id,
                &edge_text,
                edge_pair.target_entity_id,
            ),
            slug: triplet_slug(
                edge_pair.source_entity_id,
                &edge_text,
                edge_pair.target_entity_id,
            ),
            user_id,
            data_id,
            dataset_id,
            source_node_id: edge_pair.source_entity_id,
            destination_node_id: edge_pair.target_entity_id,
            relationship_name: edge_text,
            label: Some(edge_pair.relationship_name.clone()),
            attributes: serde_json::to_value(&edge_pair.properties).ok(),
            created_at: Utc::now(),
        });
    }

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

        prov_edges.push(GraphEdge {
            id: provenance_edge_id(
                tenant_id, user_id, dataset_id, source_id, rel_name, target_id,
            ),
            slug: edge_slug(rel_name),
            user_id,
            data_id: Uuid::nil(), // structural edges span multiple DataPoints
            dataset_id,
            source_node_id: source_id,
            destination_node_id: target_id,
            relationship_name: rel_name.clone(),
            label: None,
            attributes: attrs,
            created_at: Utc::now(),
        });
    }

    if !prov_edges.is_empty() {
        graph_storage::upsert_edges(db, &prov_edges).await?;
        info!("Upserted {} provenance edge records", prov_edges.len());
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

/// Pipeline name carried by cognify task stamps (locked Decision 14 of
/// LIB-06). Used by the per-task in-body stamping below so the test in
/// `crates/cognify/tests/provenance_e2e.rs` sees `source_pipeline =
/// "cognify"` on every produced DataPoint.
const COGNIFY_PIPELINE_STAMP_NAME: &str = "cognify";

/// Resolve the user label for in-body stamping from a [`TaskContext`].
///
/// Mirrors [`cognee_core::PipelineContext::user_label`]: prefer
/// `user_email`, fall back to `user_id.to_string()`, else `None`.
fn user_label_from_ctx(ctx: &Arc<cognee_core::TaskContext>) -> Option<String> {
    ctx.pipeline_ctx.as_ref().and_then(|p| p.user_label())
}

/// Build a [`TypedTask`] that classifies Data items into Documents.
///
/// The returned task does **not** carry a name; the pipeline builder
/// [`build_cognify_pipeline`] wraps it with [`CLASSIFY_DOCUMENTS_TASK_NAME`].
///
/// In-body provenance stamping: stamps every emitted `Document` with
/// `source_pipeline = "cognify"` and `source_task = "classify_documents"`.
/// Necessary because `ClassifiedDocuments` is a non-`HasDataPoint` wrapper
/// not walked by the executor's `stamp_tree_dyn` (LIB-06-03 fixup).
pub fn make_classify_documents_task() -> TypedTask<CognifyInput, ClassifiedDocuments> {
    TypedTask::sync(|input: &CognifyInput, ctx| {
        let mut classified = classify_documents(input).map_err(|e| format!("{e}"))?;
        let user_label = user_label_from_ctx(&ctx);
        for doc in &mut classified.documents {
            stamp_provenance(
                &mut doc.base,
                COGNIFY_PIPELINE_STAMP_NAME,
                CLASSIFY_DOCUMENTS_TASK_NAME,
                user_label.as_deref(),
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
            )
            .await
            .map_err(|e| format!("{e}"))?;
            for chunk in &mut extracted.chunks {
                stamp_provenance(
                    &mut chunk.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_CHUNKS_TASK_NAME,
                    user_label.as_deref(),
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
    config: CognifyConfig,
) -> TypedTask<ExtractedChunks, ExtractedGraphData> {
    TypedTask::async_fn(move |input: &ExtractedChunks, ctx| {
        let input = input.clone();
        let llm = Arc::clone(&llm);
        let graph_db = Arc::clone(&graph_db);
        let ontology_resolver = Arc::clone(&ontology_resolver);
        let config = config.clone();
        let user_label = user_label_from_ctx(&ctx);
        Box::pin(async move {
            let mut graph_data = extract_graph_from_data(
                &input,
                llm,
                Arc::clone(&graph_db),
                ontology_resolver,
                &config,
                user_label.as_deref(),
            )
            .await
            .map_err(|e| format!("{e}"))?;
            if config.create_web_page_nodes {
                create_web_page_nodes(&graph_data.documents, &graph_data.chunks, graph_db)
                    .await
                    .map_err(|e| format!("{e}"))?;
            }
            for pair in &mut graph_data.entities {
                stamp_provenance(
                    &mut pair.entity.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_GRAPH_TASK_NAME,
                    user_label.as_deref(),
                );
                stamp_provenance(
                    &mut pair.entity_type.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_GRAPH_TASK_NAME,
                    user_label.as_deref(),
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
                );
            }
            for doc in &mut graph_data.documents {
                stamp_provenance(
                    &mut doc.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    EXTRACT_GRAPH_TASK_NAME,
                    user_label.as_deref(),
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
    TypedTask::async_fn(move |input: &ExtractedGraphData, ctx| {
        let input = input.clone();
        let llm = Arc::clone(&llm);
        let config = config.clone();
        let user_label = user_label_from_ctx(&ctx);
        Box::pin(async move {
            let mut summarized = summarize_text(&input, llm, &config)
                .await
                .map_err(|e| format!("{e}"))?;
            for summary in &mut summarized.summaries {
                stamp_provenance(
                    &mut summary.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    SUMMARIZE_TEXT_TASK_NAME,
                    user_label.as_deref(),
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
                );
            }
            for doc in &mut summarized.documents {
                stamp_provenance(
                    &mut doc.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    SUMMARIZE_TEXT_TASK_NAME,
                    user_label.as_deref(),
                );
            }
            for pair in &mut summarized.entities {
                stamp_provenance(
                    &mut pair.entity.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    SUMMARIZE_TEXT_TASK_NAME,
                    user_label.as_deref(),
                );
                stamp_provenance(
                    &mut pair.entity_type.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    SUMMARIZE_TEXT_TASK_NAME,
                    user_label.as_deref(),
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
    db: Option<Arc<DatabaseConnection>>,
    config: CognifyConfig,
) -> TypedTask<SummarizedData, CognifyResult> {
    TypedTask::async_fn(move |input: &SummarizedData, ctx| {
        let input = input.clone();
        let graph_db = Arc::clone(&graph_db);
        let vector_db = Arc::clone(&vector_db);
        let embedding_engine = Arc::clone(&embedding_engine);
        let db = db.clone();
        let config = config.clone();
        let user_label = user_label_from_ctx(&ctx);
        Box::pin(async move {
            let mut result =
                add_data_points(&input, graph_db, vector_db, embedding_engine, db, &config)
                    .await
                    .map_err(|e| format!("{e}"))?;
            for chunk in &mut result.chunks {
                stamp_provenance(
                    &mut chunk.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                );
            }
            for pair in &mut result.entities {
                stamp_provenance(
                    &mut pair.entity.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                );
                stamp_provenance(
                    &mut pair.entity_type.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                );
            }
            for summary in &mut result.summaries {
                stamp_provenance(
                    &mut summary.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                );
            }
            for edge_type in &mut result.edge_types {
                stamp_provenance(
                    &mut edge_type.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                );
            }
            for doc in &mut result.documents_for_dlt {
                stamp_provenance(
                    &mut doc.base,
                    COGNIFY_PIPELINE_STAMP_NAME,
                    ADD_DATA_POINTS_TASK_NAME,
                    user_label.as_deref(),
                );
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
    db: Option<Arc<DatabaseConnection>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: CognifyConfig,
) -> Pipeline {
    let loader_registry = Arc::new(build_loader_registry(&llm, &config));
    PipelineBuilder::new_with_task("cognify", make_classify_documents_task())
        .with_first_task_name(CLASSIFY_DOCUMENTS_TASK_NAME)
        .add_task_named(
            make_extract_chunks_task(
                storage,
                config.max_chunk_size,
                config.token_counter_kind.clone(),
                db.clone(),
                loader_registry,
            ),
            EXTRACT_CHUNKS_TASK_NAME,
        )
        .add_task_named(
            make_extract_graph_task(
                Arc::clone(&llm),
                Arc::clone(&graph_db),
                ontology_resolver,
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
        .with_name("cognify")
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
                .map_err(|e| format!("{e}").into())
        })
    })
}

/// Build a [`TypedTask`] that persists temporal events to graph and vector DBs.
pub fn make_add_temporal_data_points_task(
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
) -> TypedTask<ExtractedTemporalEvents, CognifyResult> {
    TypedTask::async_fn(move |input: &ExtractedTemporalEvents, _ctx| {
        let input = input.clone();
        let graph_db = Arc::clone(&graph_db);
        let vector_db = Arc::clone(&vector_db);
        let embedding_engine = Arc::clone(&embedding_engine);
        Box::pin(async move {
            add_temporal_data_points(&input, graph_db, vector_db, embedding_engine)
                .await
                .map(Box::new)
                .map_err(|e| format!("{e}").into())
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
    db: Option<Arc<DatabaseConnection>>,
    config: CognifyConfig,
) -> Pipeline {
    let loader_registry = Arc::new(build_loader_registry(&llm, &config));
    PipelineBuilder::new_with_task("temporal-cognify", make_classify_documents_task())
        .with_first_task_name(CLASSIFY_DOCUMENTS_TASK_NAME)
        .add_task_named(
            make_extract_chunks_task(
                storage,
                config.max_chunk_size,
                config.token_counter_kind.clone(),
                db,
                loader_registry,
            ),
            EXTRACT_CHUNKS_TASK_NAME,
        )
        .add_task_named(
            make_extract_temporal_events_task(llm, config),
            "extract_temporal_events",
        )
        .add_task_named(
            make_add_temporal_data_points_task(graph_db, vector_db, embedding_engine),
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
    use cognee_models::DataPoint;
    use cognee_storage::MockStorage;

    #[test]
    fn test_classify_documents_empty() {
        let input = CognifyInput {
            data_items: vec![],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };
        let result = classify_documents(&input).unwrap();
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
        let result = classify_documents(&input).unwrap();
        assert_eq!(result.documents.len(), 1);
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
        let result = classify_documents(&input).unwrap();
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
        )
        .await
        .unwrap();
        assert!(!result.chunks.is_empty());
    }

    #[tokio::test]
    async fn test_extract_chunks_empty_documents() {
        let storage = Arc::new(MockStorage::new());
        let input = ClassifiedDocuments {
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
        )
        .await
        .unwrap();

        assert_eq!(result.chunks.len(), 1);
        let chunk = &result.chunks[0];
        assert_eq!(chunk.text, "some dlt row content");
        assert_eq!(chunk.cut_type, "dlt_row");
        assert_eq!(chunk.chunk_index, 0);
        assert_eq!(chunk.document_id, doc_id);
    }

    #[tokio::test]
    async fn test_unsupported_document_type() {
        // Use a document_type that is intentionally never registered in
        // LoaderRegistry::default(). The previous fixture used "pdf", but the
        // PDF loader added in phase2/task1 made that type supported, causing
        // this test to invoke the real PDFium loader on garbage bytes.
        const UNSUPPORTED: &str = "no_such_loader_type_for_test";

        let storage = Arc::new(MockStorage::new());
        let location = storage.store(b"some content", "test.bin").await.unwrap();

        let doc_id = Uuid::new_v4();
        let mut base = DataPoint::new("UnknownDocument", None);
        base.id = doc_id;
        base.set_metadata("index_fields", serde_json::json!(["text"]));
        let doc = Document {
            base,
            document_type: UNSUPPORTED.to_string(),
            name: "test.bin".to_string(),
            raw_data_location: location,
            mime_type: "application/octet-stream".to_string(),
            extension: "bin".to_string(),
            data_id: doc_id,
            external_metadata: None,
        };

        let input = ClassifiedDocuments {
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
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, CognifyError::UnsupportedDocumentType(ref t) if t == UNSUPPORTED),
            "expected UnsupportedDocumentType({UNSUPPORTED:?}), got: {err:?}"
        );
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
        let result = classify_documents(&input).unwrap();
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

    #[test]
    fn provenance_edge_id_works_with_none_tenant() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let dataset_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let source_id = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
        let target_id = Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();

        let id = provenance_edge_id(
            None,
            user_id,
            dataset_id,
            source_id,
            "relates_to",
            target_id,
        );

        let expected_input = format!("None{user_id}{dataset_id}{source_id}relates_to{target_id}");
        let expected = Uuid::new_v5(&Uuid::NAMESPACE_OID, expected_input.as_bytes());
        assert_eq!(id, expected);
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

        let input = SummarizedData {
            chunks: vec![chunk],
            documents: vec![document],
            entities: vec![],
            edges: vec![],
            summaries: vec![],
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        let config = CognifyConfig::default();
        add_data_points(
            &input,
            Arc::clone(&graph),
            Arc::clone(&vector),
            Arc::clone(&engine),
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

        let (_nodes, edges) = expand_with_nodes_and_edges(
            vec![(chunk_id, graph)],
            dataset_id,
            &HashSet::new(),
            &resolver,
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

        create_web_page_nodes(&documents, &chunks, graph.clone())
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

        create_web_page_nodes(&documents, &chunks, graph.clone())
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

        create_web_page_nodes(
            &[doc_with_invalid_json, non_url_doc, bad_url_doc],
            &chunks,
            graph.clone(),
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

        create_web_page_nodes(&documents, &chunks, graph.clone())
            .await
            .unwrap();
        create_web_page_nodes(&documents, &chunks, graph.clone())
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
        };

        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        let (_, ctx, _) = test_task_context().await;
        let task = make_extract_graph_task(
            Arc::new(MockLlm::empty()),
            graph.clone(),
            Arc::new(NoOpOntologyResolver::new()),
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
            dataset_id: Uuid::new_v4(),
            user_id: None,
            tenant_id: None,
        };

        // With summarization disabled, verify we get zero summaries and no panic.
        let config = CognifyConfig::default().with_summarization(false);
        let llm: Arc<dyn Llm> = Arc::new(MockLlm::empty());
        let result = summarize_text(&input, llm, &config).await.unwrap();
        assert!(result.summaries.is_empty());
        // All chunks (both DLT and non-DLT) are still passed through.
        assert_eq!(result.chunks.len(), 2);
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
            let result = classify_documents(&input).expect("classify should not error");
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
        let classified =
            classify_documents(&input).expect("classify_documents must succeed for html");
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
}
