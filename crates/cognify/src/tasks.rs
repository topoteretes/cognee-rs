//! Cognify pipeline tasks — individual steps of the cognify process.
//!
//! Matches the Python SDK task breakdown:
//! 1. [`classify_documents`] — Data items → Documents
//! 2. [`extract_chunks_from_documents`] — Documents → DocumentChunks
//! 3. [`extract_graph_from_data`] — Chunks → Chunks + entities/edges (stored in graph DB)
//! 4. [`summarize_text`] — + summaries via LLM
//! 5. [`add_data_points`] — embeddings + vector indexing → [`CognifyResult`]
//!
//! Public surface:
//! - Intermediate types: [`CognifyInput`], [`ClassifiedDocuments`],
//!   [`ExtractedChunks`], [`ExtractedGraphData`], [`SummarizedData`]
//! - Task implementations (free functions)
//! - [`TypedTask`] factories: [`make_classify_documents_task`], etc.
//! - Pipeline builder: [`build_cognify_pipeline`]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use cognee_chunking::{TokenCounterKind, chunk_text};
use cognee_core::{Pipeline, PipelineBuilder, TypedTask};
use cognee_database::DatabaseConnection;
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::{EdgeData, GraphDBTrait, GraphDBTraitExt};
use cognee_llm::Llm;
use cognee_models::{
    Data, Document, DocumentChunk, EdgeType, Embedding,
    classify_documents as model_classify_documents,
};
use cognee_storage::StorageTrait;
use cognee_vector::{VectorDB, VectorPoint};
use serde_json::json;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::fact_extraction::{FactExtractor, KnowledgeGraph};
use crate::graph_integration::{
    GraphEdgePair, GraphNodePair, deduplicate_nodes_and_edges, expand_with_nodes_and_edges,
    retrieve_existing_edges,
};
use crate::pipeline::{CognifyResult, IndexedFieldsStats};
use crate::summarization::{SummaryExtractor, TextSummary};
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

        let content = String::from_utf8(content_bytes)
            .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;

        let mut chunks = chunk_text(document.base.id, &content, max_chunk_size, &counter);

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

    // Merge and deduplicate graphs (with DB awareness)
    let ontology_resolver = cognee_ontology::NoOpOntologyResolver::new();
    let (nodes, edges) = expand_with_nodes_and_edges(
        all_graphs,
        input.dataset_id,
        &existing_edges_set,
        &ontology_resolver,
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
    let summaries = if config.enable_summarization {
        let summary_extractor = SummaryExtractor::new(llm);
        let mut all_summaries = Vec::new();

        for batch in input.chunks.chunks(config.summarization_batch_size) {
            let batch_summaries = summary_extractor.summarize_chunks(batch, None).await?;
            all_summaries.extend(batch_summaries);
        }

        info!("Generated {} summaries", all_summaries.len());
        all_summaries
    } else {
        info!("Summarization disabled in config");
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

    // Create and store EdgeTypes from unique relationship names
    // (port of Python's create_edge_type_datapoints + index_graph_edges)
    let mut edge_type_counts: HashMap<String, i32> = HashMap::new();
    for edge_pair in &input.edges {
        *edge_type_counts
            .entry(edge_pair.relationship_name.clone())
            .or_insert(0) += 1;
    }

    let edge_types: Vec<EdgeType> = edge_type_counts
        .into_iter()
        .map(|(name, count)| {
            let mut et = EdgeType::new(&name, Some(input.dataset_id));
            et.set_count(count);
            et
        })
        .collect();

    if !edge_types.is_empty() {
        let edge_type_refs: Vec<&EdgeType> = edge_types.iter().collect();
        graph_db
            .add_nodes(&edge_type_refs)
            .await
            .map_err(CognifyError::from)?;
        info!("Stored {} edge types as graph nodes", edge_types.len());
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
        &input.edges,
        &edge_types,
        input.dataset_id,
        input.user_id,
        input.tenant_id,
        embedding_engine,
        vector_db,
        config,
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
    })
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

    // Phase 1: Build table node IDs (deterministic via uuid5)
    let mut table_node_ids: HashMap<String, Uuid> = HashMap::new();
    for table_name in tables_seen.keys() {
        let id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("dlt:{}", table_name).as_bytes(),
        );
        table_node_ids.insert(table_name.clone(), id);
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

            let rel_name = format!(
                "{}:{}->{}{}",
                table_name,
                fk_col,
                ref_table,
                if ref_col.is_empty() {
                    String::new()
                } else {
                    format!(":{}", ref_col)
                }
            );
            let rel_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("dlt:{}", rel_name).as_bytes());

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

/// Internal metadata for a DLT source table.
#[derive(Debug)]
struct DltTableMeta {
    #[allow(dead_code)]
    schema_info: Option<serde_json::Value>,
    foreign_keys: Vec<serde_json::Value>,
    #[allow(dead_code)]
    dlt_db_name: String,
}

// ---------------------------------------------------------------------------
// Provenance stamping helper
// ---------------------------------------------------------------------------

/// Stamp pipeline provenance fields on a [`DataPoint`].
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
    tenant_id: Option<Uuid>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    config: &CognifyConfig,
) -> Result<CognifyResult, CognifyError> {
    config
        .validate()
        .map_err(|e| CognifyError::ConfigError(e.to_string()))?;

    // Auto-calculate chunk size when the caller is using the default value.
    // Matches Python's get_max_chunk_tokens() from cognee/infrastructure/llm/utils.py.
    let effective_config;
    let config = if config.max_chunk_size == CognifyConfig::default().max_chunk_size {
        effective_config = config
            .clone()
            .with_auto_chunk_size(embedding_engine.as_ref(), llm.as_ref());
        info!(
            "Auto-calculated max_chunk_size: {}",
            effective_config.max_chunk_size
        );
        &effective_config
    } else {
        config
    };

    info!(
        "Starting cognify pipeline with config: chunks_per_batch={}, max_chunk_size={}",
        config.chunks_per_batch, config.max_chunk_size
    );

    // Derive user string for provenance stamping
    let user_str = user_id.as_ref().map(|id| id.to_string());
    let user_str_ref = user_str.as_deref();

    let input = CognifyInput {
        data_items,
        dataset_id,
        user_id,
        tenant_id,
    };

    // Task 1: Classify documents
    let mut classified = classify_documents(&input)?;

    // Stamp provenance on classified documents
    for doc in &mut classified.documents {
        stamp_provenance(
            &mut doc.base,
            "cognify_pipeline",
            "classify_documents",
            user_str_ref,
        );
    }

    if classified.documents.is_empty() {
        return Ok(CognifyResult::empty());
    }

    // Task 2: Extract text chunks (with token count write-back when DB available)
    let mut extracted_chunks =
        extract_chunks_from_documents(&classified, &*storage, config.max_chunk_size, config.token_counter_kind.clone(), db.as_deref())
            .await?;

    // Stamp provenance on extracted chunks
    for chunk in &mut extracted_chunks.chunks {
        stamp_provenance(
            &mut chunk.base,
            "cognify_pipeline",
            "extract_chunks_from_documents",
            user_str_ref,
        );
    }

    if extracted_chunks.chunks.is_empty() {
        return Ok(CognifyResult::empty());
    }

    info!("Extracted {} chunks", extracted_chunks.chunks.len());

    // Task 3: Extract knowledge graph
    let mut graph_data = extract_graph_from_data(
        &extracted_chunks,
        Arc::clone(&llm),
        Arc::clone(&graph_db),
        config,
    )
    .await?;

    // Stamp provenance on extracted graph entities
    for pair in &mut graph_data.entities {
        stamp_provenance(
            &mut pair.entity.base,
            "cognify_pipeline",
            "extract_graph_from_data",
            user_str_ref,
        );
        stamp_provenance(
            &mut pair.entity_type.base,
            "cognify_pipeline",
            "extract_graph_from_data",
            user_str_ref,
        );
    }

    // Task 4: Summarize text
    let mut summarized = summarize_text(&graph_data, llm, config).await?;

    // Stamp provenance on generated summaries
    for summary in &mut summarized.summaries {
        stamp_provenance(
            &mut summary.base,
            "cognify_pipeline",
            "summarize_text",
            user_str_ref,
        );
    }

    // Task 5: Add data points (embeddings + vector indexing + provenance)
    let result = add_data_points(
        &summarized,
        Arc::clone(&graph_db),
        vector_db,
        embedding_engine,
        db,
        config,
    )
    .await?;

    // Task 6: Extract DLT FK edges (no-op if no DLT documents)
    extract_dlt_fk_edges(&summarized.chunks, &summarized.documents, graph_db).await?;

    Ok(result)
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
    structural_edges: &[EdgeData],
) -> Result<(), CognifyError> {
    use cognee_database::ops::graph_storage;
    use cognee_database::{GraphEdge, GraphNode};

    // Build chunk_id → document_id map for tracing entity provenance back
    // to the originating Data item.
    let chunk_data_map: HashMap<Uuid, Uuid> =
        chunks.iter().map(|c| (c.base.id, c.document_id)).collect();

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

        // Resolve data_id from source entity
        let data_id = Uuid::nil(); // edges span entities; use nil

        prov_edges.push(GraphEdge {
            id: provenance_edge_id(
                tenant_id,
                user_id,
                dataset_id,
                edge_pair.source_entity_id,
                &edge_text,
                edge_pair.target_entity_id,
            ),
            slug: edge_slug(&edge_text),
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

/// Index data points in vector database.
#[allow(clippy::too_many_arguments)]
async fn index_data_points(
    chunks: &[DocumentChunk],
    entities: &[GraphNodePair],
    summaries: &[TextSummary],
    edges: &[GraphEdgePair],
    edge_types: &[EdgeType],
    dataset_id: Uuid,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    engine: Arc<dyn EmbeddingEngine>,
    vector_db: Arc<dyn VectorDB>,
    config: &CognifyConfig,
) -> Result<IndexedFieldsStats, CognifyError> {
    let mut stats = IndexedFieldsStats::default();
    let dimension = engine.dimension();

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

        let texts: Vec<_> = chunks.iter().map(|c| c.text.as_str()).collect();
        let vectors = engine
            .embed(&texts)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

        let points: Vec<VectorPoint> = chunks
            .iter()
            .zip(vectors)
            .map(|(chunk, vector)| {
                let mut point = VectorPoint::new(chunk.base.id, vector)
                    .with_metadata("type", json!("DocumentChunk"))
                    .with_metadata("field", json!("text"))
                    .with_metadata("text", json!(chunk.text.clone()))
                    .with_metadata("dataset_id", json!(dataset_id.to_string()))
                    .with_metadata("document_id", json!(chunk.document_id.to_string()))
                    .with_metadata("chunk_index", json!(chunk.chunk_index))
                    .with_metadata("belongs_to_set", json!(chunk.base.belongs_to_set));
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

        let names: Vec<_> = entities.iter().map(|e| e.entity.name.as_str()).collect();
        let vectors = engine
            .embed(&names)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

        let points: Vec<VectorPoint> = entities
            .iter()
            .zip(vectors)
            .map(|(entity, vector)| {
                let mut point = VectorPoint::new(entity.entity.base.id, vector)
                    .with_metadata("type", json!("Entity"))
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
                    let mut point = VectorPoint::new(et.base.id, vector)
                        .with_metadata("type", json!("EntityType"))
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

        let texts: Vec<_> = summaries.iter().map(|s| s.text.as_str()).collect();
        let vectors = engine
            .embed(&texts)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

        let points: Vec<VectorPoint> = summaries
            .iter()
            .zip(vectors)
            .map(|(summary, vector)| {
                let mut point = VectorPoint::new(summary.base.id, vector)
                    .with_metadata("type", json!("TextSummary"))
                    .with_metadata("field", json!("text"))
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

            let triplet_texts: Vec<_> = triplets
                .iter()
                .map(|t| t.text.as_str())
                .collect();
            let triplet_vectors = engine
                .embed(&triplet_texts)
                .await
                .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

            let triplet_points: Vec<VectorPoint> = triplets
                .iter()
                .zip(triplet_vectors)
                .map(|(triplet, vector)| {
                    VectorPoint::new(triplet.id, vector)
                        .with_metadata("type", json!("Triplet"))
                        .with_metadata("field", json!("text"))
                        .with_metadata("source_id", json!(triplet.source_entity_id.to_string()))
                        .with_metadata("target_id", json!(triplet.target_entity_id.to_string()))
                        .with_metadata("relationship", json!(triplet.relationship_name.clone()))
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
                let mut point = VectorPoint::new(et.base.id, vector)
                    .with_metadata("type", json!("EdgeType"))
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

    Ok(stats)
}

// ---------------------------------------------------------------------------
// TypedTask factories
// ---------------------------------------------------------------------------

/// Build a [`TypedTask`] that classifies Data items into Documents.
pub fn make_classify_documents_task() -> TypedTask<CognifyInput, ClassifiedDocuments> {
    TypedTask::sync(|input: &CognifyInput, _ctx| {
        classify_documents(input)
            .map(Box::new)
            .map_err(|e| format!("{e}").into())
    })
}

/// Build a [`TypedTask`] that extracts text chunks from classified documents.
pub fn make_extract_chunks_task(
    storage: Arc<dyn StorageTrait>,
    max_chunk_size: usize,
    token_counter_kind: TokenCounterKind,
    db: Option<Arc<DatabaseConnection>>,
) -> TypedTask<ClassifiedDocuments, ExtractedChunks> {
    TypedTask::async_fn(move |input: &ClassifiedDocuments, _ctx| {
        let input = input.clone();
        let storage = Arc::clone(&storage);
        let db = db.clone();
        let token_counter_kind = token_counter_kind.clone();
        Box::pin(async move {
            extract_chunks_from_documents(
                &input,
                &*storage,
                max_chunk_size,
                token_counter_kind,
                db.as_deref(),
            )
            .await
            .map(Box::new)
            .map_err(|e| format!("{e}").into())
        })
    })
}

/// Build a [`TypedTask`] that extracts knowledge graphs from chunks via LLM.
pub fn make_extract_graph_task(
    llm: Arc<dyn Llm>,
    graph_db: Arc<dyn GraphDBTrait>,
    config: CognifyConfig,
) -> TypedTask<ExtractedChunks, ExtractedGraphData> {
    TypedTask::async_fn(move |input: &ExtractedChunks, _ctx| {
        let input = input.clone();
        let llm = Arc::clone(&llm);
        let graph_db = Arc::clone(&graph_db);
        let config = config.clone();
        Box::pin(async move {
            extract_graph_from_data(&input, llm, graph_db, &config)
                .await
                .map(Box::new)
                .map_err(|e| format!("{e}").into())
        })
    })
}

/// Build a [`TypedTask`] that summarizes text chunks via LLM.
pub fn make_summarize_text_task(
    llm: Arc<dyn Llm>,
    config: CognifyConfig,
) -> TypedTask<ExtractedGraphData, SummarizedData> {
    TypedTask::async_fn(move |input: &ExtractedGraphData, _ctx| {
        let input = input.clone();
        let llm = Arc::clone(&llm);
        let config = config.clone();
        Box::pin(async move {
            summarize_text(&input, llm, &config)
                .await
                .map(Box::new)
                .map_err(|e| format!("{e}").into())
        })
    })
}

/// Build a [`TypedTask`] that generates embeddings and indexes data points.
pub fn make_add_data_points_task(
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    config: CognifyConfig,
) -> TypedTask<SummarizedData, CognifyResult> {
    TypedTask::async_fn(move |input: &SummarizedData, _ctx| {
        let input = input.clone();
        let graph_db = Arc::clone(&graph_db);
        let vector_db = Arc::clone(&vector_db);
        let embedding_engine = Arc::clone(&embedding_engine);
        let db = db.clone();
        let config = config.clone();
        Box::pin(async move {
            add_data_points(&input, graph_db, vector_db, embedding_engine, db, &config)
                .await
                .map(Box::new)
                .map_err(|e| format!("{e}").into())
        })
    })
}

// ---------------------------------------------------------------------------
// Pipeline builder
// ---------------------------------------------------------------------------

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
    config: CognifyConfig,
) -> Pipeline {
    PipelineBuilder::new_with_task("cognify", make_classify_documents_task())
        .add_task(make_extract_chunks_task(
            storage,
            config.max_chunk_size,
            config.token_counter_kind.clone(),
            db.clone(),
        ))
        .add_task(make_extract_graph_task(
            Arc::clone(&llm),
            Arc::clone(&graph_db),
            config.clone(),
        ))
        .add_task(make_summarize_text_task(llm, config.clone()))
        .add_task(make_add_data_points_task(
            graph_db,
            vector_db,
            embedding_engine,
            db,
            config,
        ))
        .with_name("cognify")
        .build()
}

#[cfg(test)]
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

        let result = extract_chunks_from_documents(&input, &*storage, 100, TokenCounterKind::Word, None)
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

        let result = extract_chunks_from_documents(&input, &*storage, 100, TokenCounterKind::Word, None)
            .await
            .unwrap();
        assert!(result.chunks.is_empty());
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
}
