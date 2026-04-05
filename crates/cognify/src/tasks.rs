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

use std::collections::HashMap;
use std::sync::Arc;

use cognee_chunking::{WordCounter, chunk_text};
use cognee_core::{Pipeline, PipelineBuilder, TypedTask};
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::{GraphDBTrait, GraphDBTraitExt};
use cognee_llm::Llm;
use cognee_models::{
    Data, Document, DocumentChunk, Embedding, classify_documents as model_classify_documents,
};
use cognee_storage::StorageTrait;
use cognee_vector::{VectorDB, VectorPoint};
use serde_json::json;
use tokio::sync::Semaphore;
use tracing::info;
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

// ---------------------------------------------------------------------------
// Intermediate types
// ---------------------------------------------------------------------------

/// Input to the cognify pipeline.
///
/// Wraps all data items for a dataset along with the dataset identifier.
#[derive(Debug, Clone)]
pub struct CognifyInput {
    pub data_items: Vec<Data>,
    pub dataset_id: Uuid,
}

/// Output of [`classify_documents`]: classified documents ready for chunking.
#[derive(Debug, Clone)]
pub struct ClassifiedDocuments {
    pub documents: Vec<Document>,
    pub dataset_id: Uuid,
}

/// Output of [`extract_chunks_from_documents`]: text chunks ready for graph extraction.
#[derive(Debug, Clone)]
pub struct ExtractedChunks {
    pub chunks: Vec<DocumentChunk>,
    pub dataset_id: Uuid,
}

/// Output of [`extract_graph_from_data`]: chunks plus extracted entities and edges
/// (already stored in graph DB).
#[derive(Debug, Clone)]
pub struct ExtractedGraphData {
    pub chunks: Vec<DocumentChunk>,
    pub entities: Vec<GraphNodePair>,
    pub edges: Vec<GraphEdgePair>,
    pub dataset_id: Uuid,
}

/// Output of [`summarize_text`]: graph data plus generated summaries.
#[derive(Debug, Clone)]
pub struct SummarizedData {
    pub chunks: Vec<DocumentChunk>,
    pub entities: Vec<GraphNodePair>,
    pub edges: Vec<GraphEdgePair>,
    pub summaries: Vec<TextSummary>,
    pub dataset_id: Uuid,
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
    })
}

// ---------------------------------------------------------------------------
// Task 2: extract_chunks_from_documents
// ---------------------------------------------------------------------------

/// Extract text chunks from classified documents (Task 2).
///
/// For each document, reads content from storage and applies the
/// word → sentence → paragraph → text chunker hierarchy.
pub async fn extract_chunks_from_documents(
    input: &ClassifiedDocuments,
    storage: &dyn StorageTrait,
    max_chunk_size: usize,
) -> Result<ExtractedChunks, CognifyError> {
    let counter = WordCounter;
    let mut all_chunks = Vec::new();

    for document in &input.documents {
        let content_bytes = storage
            .retrieve(&document.raw_data_location)
            .await
            .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;

        let content = String::from_utf8(content_bytes)
            .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;

        let chunks = chunk_text(document.id, &content, max_chunk_size, &counter);
        all_chunks.extend(chunks);
    }

    info!(total_chunks = all_chunks.len(), "chunking complete");
    Ok(ExtractedChunks {
        chunks: all_chunks,
        dataset_id: input.dataset_id,
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
            entities: vec![],
            edges: vec![],
            dataset_id: input.dataset_id,
        });
    }

    let batch_size = config.chunks_per_batch;
    let mut all_graphs: Vec<(Uuid, KnowledgeGraph)> = Vec::new();
    let semaphore = Arc::new(Semaphore::new(config.max_parallel_extractions));

    for (batch_idx, batch) in input.chunks.chunks(batch_size).enumerate() {
        let fact_extractor = FactExtractor::new(Arc::clone(&llm));
        let mut extract_tasks = Vec::new();
        let mut chunk_ids = Vec::new();

        for chunk in batch {
            let extractor = fact_extractor.clone();
            let text = chunk.text.clone();
            let sem = Arc::clone(&semaphore);
            let prompt = config.custom_extraction_prompt.clone();

            chunk_ids.push(chunk.id);
            extract_tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
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
            input.chunks.len().div_ceil(batch_size),
            batch.len()
        );
    }

    // Database deduplication — query for existing edges
    let graphs_only: Vec<KnowledgeGraph> = all_graphs.iter().map(|(_, g)| g.clone()).collect();
    let existing_edges_set = retrieve_existing_edges(graph_db.as_ref(), &graphs_only).await?;

    // Merge and deduplicate graphs (with DB awareness)
    let (nodes, edges) =
        expand_with_nodes_and_edges(all_graphs, input.dataset_id, &existing_edges_set).await;

    // Final deduplication pass (in-memory only after DB filtering)
    let dedup_result = deduplicate_nodes_and_edges(nodes, edges);

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
        chunks: input.chunks.clone(),
        entities: dedup_result.unique_nodes,
        edges: dedup_result.unique_edges,
        dataset_id: input.dataset_id,
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
        entities: input.entities.clone(),
        edges: input.edges.clone(),
        summaries,
        dataset_id: input.dataset_id,
    })
}

// ---------------------------------------------------------------------------
// Task 5: add_data_points
// ---------------------------------------------------------------------------

/// Generate embeddings and index all data points in vector DB (Task 5).
///
/// Generates embeddings for chunks, entities (name + description), summaries,
/// and optionally triplets. Creates vector collections and indexes points.
pub async fn add_data_points(
    input: &SummarizedData,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    config: &CognifyConfig,
) -> Result<CognifyResult, CognifyError> {
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
        input.dataset_id,
        embedding_engine,
        vector_db,
        config,
    )
    .await?;

    Ok(CognifyResult {
        chunks: input.chunks.clone(),
        entities: input.entities.clone(),
        edges: input.edges.clone(),
        summaries: input.summaries.clone(),
        embeddings,
        indexed_fields,
    })
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
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    config: &CognifyConfig,
) -> Result<CognifyResult, CognifyError> {
    config
        .validate()
        .map_err(|e| CognifyError::ConfigError(e.to_string()))?;

    info!(
        "Starting cognify pipeline with config: chunks_per_batch={}, max_chunk_size={}",
        config.chunks_per_batch, config.max_chunk_size
    );

    let input = CognifyInput {
        data_items,
        dataset_id,
    };

    // Task 1: Classify documents
    let classified = classify_documents(&input)?;

    if classified.documents.is_empty() {
        return Ok(CognifyResult::empty());
    }

    // Task 2: Extract text chunks
    let extracted_chunks =
        extract_chunks_from_documents(&classified, &*storage, config.max_chunk_size).await?;

    if extracted_chunks.chunks.is_empty() {
        return Ok(CognifyResult::empty());
    }

    info!("Extracted {} chunks", extracted_chunks.chunks.len());

    // Task 3: Extract knowledge graph
    let graph_data =
        extract_graph_from_data(&extracted_chunks, Arc::clone(&llm), graph_db, config).await?;

    // Task 4: Summarize text
    let summarized = summarize_text(&graph_data, llm, config).await?;

    // Task 5: Add data points (embeddings + vector indexing)
    add_data_points(&summarized, vector_db, embedding_engine, config).await
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

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
            embeddings.push(Embedding::new(chunk.id, "DocumentChunk", "text", vector));
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
                summary.chunk_id,
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
    dataset_id: Uuid,
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
                VectorPoint::new(chunk.id, vector)
                    .with_metadata("type", json!("DocumentChunk"))
                    .with_metadata("field", json!("text"))
                    .with_metadata("text", json!(chunk.text.clone()))
                    .with_metadata("dataset_id", json!(dataset_id.to_string()))
                    .with_metadata("document_id", json!(chunk.document_id.to_string()))
                    .with_metadata("chunk_index", json!(chunk.chunk_index))
            })
            .collect();

        vector_db
            .index_points("DocumentChunk", "text", &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        stats.chunk_text_count = chunks.len();
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
                VectorPoint::new(entity.entity.base.id, vector)
                    .with_metadata("type", json!("Entity"))
                    .with_metadata("field", json!("name"))
                    .with_metadata("dataset_id", json!(dataset_id.to_string()))
                    .with_metadata("entity_type", json!(entity.entity_type.name.clone()))
            })
            .collect();

        vector_db
            .index_points("Entity", "name", &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        stats.entity_name_count = entities.len();
        info!("Indexed {} entity names", entities.len());
    }

    // 2b. Index Entity.description field
    if !entities.is_empty() {
        if !vector_db
            .has_collection("Entity", "description")
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
        {
            vector_db
                .create_collection("Entity", "description", dimension)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
        }

        let descriptions: Vec<_> = entities
            .iter()
            .map(|e| e.entity.description.as_str())
            .collect();
        let vectors = engine
            .embed(&descriptions)
            .await
            .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

        let points: Vec<VectorPoint> = entities
            .iter()
            .zip(vectors)
            .map(|(entity, vector)| {
                VectorPoint::new(entity.entity.base.id, vector)
                    .with_metadata("type", json!("Entity"))
                    .with_metadata("field", json!("description"))
                    .with_metadata("dataset_id", json!(dataset_id.to_string()))
                    .with_metadata("entity_type", json!(entity.entity_type.name.clone()))
                    .with_metadata("entity_name", json!(entity.entity.name.clone()))
            })
            .collect();

        vector_db
            .index_points("Entity", "description", &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        stats.entity_description_count = entities.len();
        info!("Indexed {} entity descriptions", entities.len());
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
                VectorPoint::new(summary.id, vector)
                    .with_metadata("type", json!("TextSummary"))
                    .with_metadata("field", json!("text"))
                    .with_metadata("dataset_id", json!(dataset_id.to_string()))
                    .with_metadata("chunk_id", json!(summary.chunk_id.to_string()))
            })
            .collect();

        vector_db
            .index_points("TextSummary", "text", &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        stats.summary_text_count = summaries.len();
        info!("Indexed {} summaries", summaries.len());
    }

    // 4. Index triplets (if enabled in config)
    if config.embed_triplets && !edges.is_empty() && !entities.is_empty() {
        use crate::triplet_creation::create_triplets_from_graph;

        let triplets = create_triplets_from_graph(entities, edges);

        if !triplets.is_empty() {
            if !vector_db
                .has_collection("Triplet", "embeddable_text")
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
            {
                vector_db
                    .create_collection("Triplet", "embeddable_text", dimension)
                    .await
                    .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
            }

            let triplet_texts: Vec<_> = triplets
                .iter()
                .map(|t| t.embeddable_text.as_str())
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
                        .with_metadata("field", json!("embeddable_text"))
                        .with_metadata("source_id", json!(triplet.source_entity_id.to_string()))
                        .with_metadata("target_id", json!(triplet.target_entity_id.to_string()))
                        .with_metadata("relationship", json!(triplet.relationship_name.clone()))
                })
                .collect();

            vector_db
                .index_points("Triplet", "embeddable_text", &triplet_points)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

            stats.triplet_count = triplets.len();
            info!("Indexed {} triplets", triplets.len());
        }
    } else if config.embed_triplets {
        info!("Triplet embedding enabled but no edges/entities to index");
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
) -> TypedTask<ClassifiedDocuments, ExtractedChunks> {
    TypedTask::async_fn(move |input: &ClassifiedDocuments, _ctx| {
        let input = input.clone();
        let storage = Arc::clone(&storage);
        Box::pin(async move {
            extract_chunks_from_documents(&input, &*storage, max_chunk_size)
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
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    config: CognifyConfig,
) -> TypedTask<SummarizedData, CognifyResult> {
    TypedTask::async_fn(move |input: &SummarizedData, _ctx| {
        let input = input.clone();
        let vector_db = Arc::clone(&vector_db);
        let embedding_engine = Arc::clone(&embedding_engine);
        let config = config.clone();
        Box::pin(async move {
            add_data_points(&input, vector_db, embedding_engine, &config)
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
/// For composable pipeline-based execution (with concurrency, retry, progress
/// tracking, etc.), pass the result to [`cognee_core::execute`].
pub fn build_cognify_pipeline(
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    llm: Arc<dyn Llm>,
    config: CognifyConfig,
) -> Pipeline {
    PipelineBuilder::new_with_task("cognify", make_classify_documents_task())
        .add_task(make_extract_chunks_task(storage, config.max_chunk_size))
        .add_task(make_extract_graph_task(
            Arc::clone(&llm),
            graph_db,
            config.clone(),
        ))
        .add_task(make_summarize_text_task(llm, config.clone()))
        .add_task(make_add_data_points_task(
            vector_db,
            embedding_engine,
            config,
        ))
        .with_name("cognify")
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_storage::MockStorage;

    #[test]
    fn test_classify_documents_empty() {
        let input = CognifyInput {
            data_items: vec![],
            dataset_id: Uuid::new_v4(),
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
        };
        let result = classify_documents(&input).unwrap();
        assert_eq!(result.documents.len(), 1);
    }

    #[test]
    fn test_classify_documents_skips_non_text() {
        let data = Data::builder(
            Uuid::new_v4(),
            "image.png",
            "/storage/image.png",
            "file://image.png",
            "png",
            "image/png",
            "hash456",
            Uuid::new_v4(),
        )
        .build();

        let input = CognifyInput {
            data_items: vec![data],
            dataset_id: Uuid::new_v4(),
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

        let doc = Document {
            id: Uuid::new_v4(),
            name: "test.txt".to_string(),
            raw_data_location: location,
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: Uuid::new_v4(),
        };

        let input = ClassifiedDocuments {
            documents: vec![doc],
            dataset_id: Uuid::new_v4(),
        };

        let result = extract_chunks_from_documents(&input, &*storage, 100)
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
        };

        let result = extract_chunks_from_documents(&input, &*storage, 100)
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
        };
        let result = classify_documents(&input).unwrap();
        assert_eq!(result.dataset_id, dataset_id);
    }
}
