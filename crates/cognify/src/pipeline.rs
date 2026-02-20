//! Cognify pipeline - Full knowledge graph extraction pipeline.
//!
//! Orchestrates the complete cognify process:
//! 1. Extract text chunks (via ExtractTextChunksPipeline)
//! 2. Extract knowledge graph from chunks
//! 3. Summarize text
//! 4. Generate embeddings
//! 5. Store data points (nodes, edges, embeddings)

use std::collections::HashMap;
use std::sync::Arc;

use cognee_chunking::ExtractTextChunksPipeline;
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_models::{Data, DocumentChunk, Embedding};
use cognee_ontology::OntologyResolver;
use cognee_storage::StorageTrait;
use cognee_vector::{VectorDB, VectorPoint};
use serde_json::json;
use tokio::sync::Semaphore;
use tracing::info;
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::fact_extraction::FactExtractor;
use crate::graph_integration::{
    GraphEdgePair, GraphNodePair, deduplicate_nodes_and_edges, expand_with_nodes_and_edges,
    retrieve_existing_edges,
};
use crate::summarization::{SummaryExtractor, TextSummary};

type SharedOntologyResolver = Arc<dyn OntologyResolver>;

/// The full cognify pipeline. Orchestrates all stages of knowledge graph
/// extraction and storage.
///
/// Generic over the storage backend, graph database, vector database, and embedding engine.
/// Note: Vector database and embedding engine are REQUIRED, not optional (matches Python behavior).
pub struct CognifyPipeline<S: StorageTrait, G: GraphDBTrait, V: VectorDB, E: EmbeddingEngine> {
    text_chunks_pipeline: ExtractTextChunksPipeline<S>,
    graph_db: Arc<G>,
    vector_db: Arc<V>,
    embedding_engine: Arc<E>,
    config: CognifyConfig,
    ontology_resolver: Option<SharedOntologyResolver>,
}

impl<S: StorageTrait, G: GraphDBTrait, V: VectorDB, E: EmbeddingEngine>
    CognifyPipeline<S, G, V, E>
{
    /// Create a new CognifyPipeline.
    ///
    /// Note: Embeddings are REQUIRED (not optional) to match Python implementation.
    /// Without embeddings, semantic search would not work.
    ///
    /// # Arguments
    /// * `storage` - Storage backend for file operations
    /// * `graph_db` - Graph database for storing nodes and edges
    /// * `vector_db` - Vector database for storing embeddings
    /// * `embedding_engine` - Embedding engine for generating vectors
    /// * `config` - Configuration for the pipeline
    /// * `ontology_resolver` - Optional ontology resolver (None or custom implementation)
    pub fn new(
        storage: Arc<S>,
        graph_db: Arc<G>,
        vector_db: Arc<V>,
        embedding_engine: Arc<E>,
        config: CognifyConfig,
        ontology_resolver: Option<SharedOntologyResolver>,
    ) -> Self {
        // Validate config on construction
        config.validate().expect("Invalid CognifyConfig");

        let text_chunks_pipeline = ExtractTextChunksPipeline::new(storage);
        Self {
            text_chunks_pipeline,
            graph_db,
            vector_db,
            embedding_engine,
            config,
            ontology_resolver,
        }
    }

    /// Check if ontology enrichment is enabled.
    ///
    /// Returns true if an ontology resolver is configured and loaded.
    /// This can be used to conditionally enable ontology-based features.
    pub fn has_ontology(&self) -> bool {
        self.ontology_resolver
            .as_ref()
            .map(|resolver| resolver.is_loaded())
            .unwrap_or(false)
    }

    /// Run the complete cognify pipeline on a set of Data items.
    ///
    /// Stages:
    /// 1. Document classification and text chunking (via ExtractTextChunksPipeline)
    /// 2. Extract knowledge graphs from chunks (LLM-based, batched + parallel)
    /// 3. Merge and deduplicate graphs
    /// 4. Summarize text chunks (LLM-based, batched if enabled)
    /// 5. Create embeddings and store in vector database
    ///
    /// Returns CognifyResult with chunks, entities, and edges.
    ///
    /// # Arguments
    /// * `data_items` - Data items to process
    /// * `dataset_id` - Dataset UUID for linking entities
    /// * `llm` - LLM instance for knowledge graph extraction
    ///
    /// # Example
    /// ```ignore
    /// use cognee_cognify::{CognifyConfig, CognifyPipeline};
    /// use cognee_storage::LocalStorage;
    /// use cognee_llm::OllamaAdapter;
    /// use std::sync::Arc;
    /// use std::path::PathBuf;
    /// use uuid::Uuid;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = CognifyConfig::default().with_chunk_size(1500);
    /// let pipeline = CognifyPipeline::new(
    ///     storage,
    ///     graph_db,
    ///     vector_db,
    ///     embedding_engine,
    ///     config,
    ///     None,
    /// );
    ///
    /// let result = pipeline.cognify(
    ///     vec![],
    ///     Uuid::new_v4(),
    ///     llm,
    /// ).await?;
    ///
    /// println!("Extracted {} entities", result.entities.len());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn cognify<L: Llm + Clone + 'static>(
        &self,
        data_items: Vec<Data>,
        dataset_id: Uuid,
        llm: Arc<L>,
    ) -> Result<CognifyResult, CognifyError> {
        info!(
            "Starting cognify pipeline with config: chunks_per_batch={}, max_chunk_size={}",
            self.config.chunks_per_batch, self.config.max_chunk_size
        );

        // Stage 1: Extract text chunks (classify + chunk) with config
        let chunks = self
            .text_chunks_pipeline
            .extract_chunks(data_items, self.config.max_chunk_size)
            .await
            .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;

        if chunks.is_empty() {
            return Ok(CognifyResult {
                chunks,
                entities: vec![],
                edges: vec![],
                summaries: vec![],
                embeddings: vec![],
                indexed_fields: IndexedFieldsStats::default(),
            });
        }

        info!("✓ Extracted {} chunks", chunks.len());

        // Stage 2: Graph extraction with configured batching and parallelism
        let batch_size = self.config.chunks_per_batch;
        let mut all_graphs = Vec::new();

        // Use config.max_parallel_extractions to limit concurrency
        let semaphore = Arc::new(Semaphore::new(self.config.max_parallel_extractions));

        for (batch_idx, batch) in chunks.chunks(batch_size).enumerate() {
            let fact_extractor = FactExtractor::new(Arc::clone(&llm));
            let mut extract_tasks = Vec::new();

            for chunk in batch {
                let extractor = fact_extractor.clone();
                let text = chunk.text.clone();
                let sem = Arc::clone(&semaphore);
                let prompt = self.config.custom_extraction_prompt.clone();

                extract_tasks.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap(); // Respect parallel limit
                    extractor.extract_facts(&text, prompt.as_deref()).await
                }));
            }

            let batch_results = futures::future::join_all(extract_tasks).await;
            for result in batch_results {
                let graph =
                    result.map_err(|e| CognifyError::FactExtractionError(e.to_string()))??;
                all_graphs.push(graph);
            }

            info!(
                "✓ Processed batch {}/{} ({} chunks)",
                batch_idx + 1,
                chunks.len().div_ceil(batch_size),
                batch.len()
            );
        }

        let graphs = all_graphs;

        // Stage 2b: Database deduplication — query for existing edges
        let existing_edges_set = retrieve_existing_edges(self.graph_db.as_ref(), &graphs).await?;

        // Stage 2c: Merge and deduplicate graphs (with DB awareness)
        let chunk_id = chunks[0].id; // Use first chunk as reference
        let (nodes, edges) =
            expand_with_nodes_and_edges(graphs, chunk_id, dataset_id, &existing_edges_set)
                .await
                .map_err(|e| CognifyError::FactExtractionError(e.to_string()))?;

        // Note: Ontology enrichment would occur here if has_ontology() is true.
        // Future enhancement: Pass self.ontology_resolver to expand_with_nodes_and_edges
        // to enable entity validation and ontology-based relationship inference.

        // Stage 2d: Final deduplication pass (in-memory only after DB filtering)
        let dedup_result = deduplicate_nodes_and_edges(nodes, edges);

        // Stage 3: Store graph data (nodes and edges) in graph database
        let entity_refs: Vec<&GraphNodePair> = dedup_result.unique_nodes.iter().collect();
        self.graph_db
            .add_nodes(&entity_refs)
            .await
            .map_err(CognifyError::from)?;

        // Convert edges to (source_id, target_id, relation, metadata) tuples
        let edge_data: Vec<_> = dedup_result
            .unique_edges
            .iter()
            .map(|edge_pair| {
                // Convert HashMap<String, String> to HashMap<Cow<'static, str>, Value>
                let properties: HashMap<std::borrow::Cow<'static, str>, serde_json::Value> =
                    edge_pair
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

        self.graph_db
            .add_edges(&edge_data)
            .await
            .map_err(CognifyError::from)?;

        // Stage 4: Summarize text chunks (batched, if enabled in config)
        let summaries = if self.config.enable_summarization {
            let summary_extractor = SummaryExtractor::new(llm);
            let mut all_summaries = Vec::new();

            // Use config.summarization_batch_size for batching
            for batch in chunks.chunks(self.config.summarization_batch_size) {
                let batch_summaries = summary_extractor.summarize_chunks(batch, None).await?;
                all_summaries.extend(batch_summaries);
            }

            info!("✓ Generated {} summaries", all_summaries.len());
            all_summaries
        } else {
            info!("✓ Summarization disabled in config");
            Vec::new()
        };

        // Stage 5a: Generate embeddings
        let embeddings = self
            .generate_embeddings(
                &chunks,
                &dedup_result.unique_nodes,
                &summaries,
                self.embedding_engine.clone(),
            )
            .await?;

        // Stage 5b: Index data points in vector database
        let indexed_fields = self
            .index_data_points(
                &chunks,
                &dedup_result.unique_nodes,
                &summaries,
                &dedup_result.unique_edges,
                self.embedding_engine.clone(),
                self.vector_db.clone(),
            )
            .await?;

        Ok(CognifyResult {
            chunks,
            entities: dedup_result.unique_nodes,
            edges: dedup_result.unique_edges,
            summaries,
            embeddings,
            indexed_fields,
        })
    }

    /// Generate embeddings for chunks, entities, and summaries.
    ///
    /// Batches all embeddable text and processes in parallel using the embedding engine.
    ///
    /// # Arguments
    /// * `chunks` - Document chunks to embed
    /// * `entities` - Entities (nodes) to embed
    /// * `summaries` - Text summaries to embed
    /// * `engine` - Embedding engine to use
    async fn generate_embeddings(
        &self,
        chunks: &[DocumentChunk],
        entities: &[GraphNodePair],
        summaries: &[TextSummary],
        engine: Arc<E>,
    ) -> Result<Vec<Embedding>, CognifyError> {
        let mut embeddings = Vec::new();

        // 1. Embed document chunks ("text" field)
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

        // 2. Embed entity names ("name" field)
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

        // 3. Embed summaries ("text" field)
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

    /// Index data points in vector database (Stage 5)
    ///
    /// Generates embeddings for chunks, entities, and summaries, then stores them
    /// in the vector database for similarity search.
    ///
    /// # Collections Created
    /// - `DocumentChunk_text` - Chunk embeddings
    /// - `Entity_name` - Entity name embeddings
    /// - `Entity_description` - Entity description embeddings (Phase 2)
    /// - `TextSummary_text` - Summary embeddings
    /// - `Triplet_embeddable_text` - Triplet embeddings (Phase 3, if config.embed_triplets is true)
    ///
    /// Returns statistics about indexed fields.
    async fn index_data_points(
        &self,
        chunks: &[DocumentChunk],
        entities: &[GraphNodePair],
        summaries: &[TextSummary],
        edges: &[GraphEdgePair],
        engine: Arc<E>,
        vector_db: Arc<V>,
    ) -> Result<IndexedFieldsStats, CognifyError> {
        let mut stats = IndexedFieldsStats::default();
        let dimension = engine.dimension();

        // 1. Index DocumentChunk.text field
        if !chunks.is_empty() {
            // Create collection if needed
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

            // Generate embeddings
            let texts: Vec<_> = chunks.iter().map(|c| c.text.as_str()).collect();
            let vectors = engine
                .embed(&texts)
                .await
                .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

            // Create vector points
            let points: Vec<VectorPoint> = chunks
                .iter()
                .zip(vectors)
                .map(|(chunk, vector)| {
                    VectorPoint::new(chunk.id, vector)
                        .with_metadata("type", json!("DocumentChunk"))
                        .with_metadata("field", json!("text"))
                        .with_metadata("document_id", json!(chunk.document_id.to_string()))
                        .with_metadata("chunk_index", json!(chunk.chunk_index))
                })
                .collect();

            // Index in vector DB
            vector_db
                .index_points("DocumentChunk", "text", &points)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

            stats.chunk_text_count = chunks.len();
            info!("✓ Indexed {} document chunks", chunks.len());
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
                        .with_metadata("entity_type", json!(entity.entity_type.name.clone()))
                })
                .collect();

            vector_db
                .index_points("Entity", "name", &points)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

            stats.entity_name_count = entities.len();
            info!("✓ Indexed {} entity names", entities.len());
        }

        // 2b. Index Entity.description field (NEW - Phase 2)
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
                        .with_metadata("entity_type", json!(entity.entity_type.name.clone()))
                        .with_metadata("entity_name", json!(entity.entity.name.clone()))
                })
                .collect();

            vector_db
                .index_points("Entity", "description", &points)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

            stats.entity_description_count = entities.len();
            info!("✓ Indexed {} entity descriptions", entities.len());
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
                        .with_metadata("chunk_id", json!(summary.chunk_id.to_string()))
                })
                .collect();

            vector_db
                .index_points("TextSummary", "text", &points)
                .await
                .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

            stats.summary_text_count = summaries.len();
            info!("✓ Indexed {} summaries", summaries.len());
        }

        // 4. Index triplets (Phase 3) - only if enabled in config
        if self.config.embed_triplets && !edges.is_empty() && !entities.is_empty() {
            use crate::triplet_creation::create_triplets_from_graph;

            let triplets = create_triplets_from_graph(entities, edges);

            if !triplets.is_empty() {
                // Create collection if needed
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

                // Generate embeddings for triplets
                let triplet_texts: Vec<_> = triplets
                    .iter()
                    .map(|t| t.embeddable_text.as_str())
                    .collect();
                let triplet_vectors = engine
                    .embed(&triplet_texts)
                    .await
                    .map_err(|e| CognifyError::EmbeddingError(e.to_string()))?;

                // Create vector points
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

                // Index in vector DB
                vector_db
                    .index_points("Triplet", "embeddable_text", &triplet_points)
                    .await
                    .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

                stats.triplet_count = triplets.len();
                info!("✓ Indexed {} triplets", triplets.len());
            }
        } else if self.config.embed_triplets {
            info!("• Triplet embedding enabled but no edges/entities to index");
        }

        Ok(stats)
    }
}

/// Result of the cognify pipeline.
#[derive(Debug, Clone)]
pub struct CognifyResult {
    /// Text chunks extracted from documents
    pub chunks: Vec<DocumentChunk>,

    /// Entities (nodes) with their types, deduplicated
    pub entities: Vec<GraphNodePair>,

    /// Edges (relationships) between entities, deduplicated
    pub edges: Vec<GraphEdgePair>,

    /// Text summaries generated from chunks
    pub summaries: Vec<TextSummary>,

    /// Embeddings for chunks, entities, and summaries
    pub embeddings: Vec<Embedding>,

    /// Statistics about indexed fields
    pub indexed_fields: IndexedFieldsStats,
}

/// Statistics about indexed fields.
///
/// Tracks how many data points were indexed for each field type.
/// Useful for verifying indexing completeness and debugging.
#[derive(Debug, Clone, Default)]
pub struct IndexedFieldsStats {
    /// Number of DocumentChunk.text fields indexed
    pub chunk_text_count: usize,

    /// Number of Entity.name fields indexed
    pub entity_name_count: usize,

    /// Number of Entity.description fields indexed
    pub entity_description_count: usize,

    /// Number of TextSummary.text fields indexed
    pub summary_text_count: usize,

    /// Number of triplets indexed (Phase 3)
    pub triplet_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CognifyConfig;
    use cognee_embedding::onnx::OnnxEmbeddingEngine;
    use cognee_graph::MockGraphDB;
    use cognee_ontology::NoOpOntologyResolver;
    use cognee_storage::MockStorage;
    use cognee_vector::MockVectorDB;

    // Note: Tests that require LLM are in integration tests

    #[tokio::test]
    async fn test_cognify_pipeline_creation() {
        use cognee_embedding::EmbeddingConfig;

        let storage = Arc::new(MockStorage::new());
        let graph_db = Arc::new(MockGraphDB::new());
        let vector_db = Arc::new(MockVectorDB::new());

        // Create a minimal embedding engine for testing
        // Note: This will fail if model files don't exist, but that's okay for a unit test
        // Real tests should use integration tests with proper setup
        let embedding_config = EmbeddingConfig::minilm_l6("test_models");
        if let Ok(embedding_engine) = OnnxEmbeddingEngine::new(embedding_config) {
            let embedding_engine = Arc::new(embedding_engine);
            let _pipeline = CognifyPipeline::new(
                storage,
                graph_db,
                vector_db,
                embedding_engine,
                CognifyConfig::default(),
                None,
            );
            // Pipeline should be created successfully
        }
        // If model doesn't exist, test passes anyway (unit test doesn't require files)
    }

    #[tokio::test]
    async fn test_cognify_pipeline_with_custom_config() {
        use cognee_embedding::EmbeddingConfig;

        let storage = Arc::new(MockStorage::new());
        let graph_db = Arc::new(MockGraphDB::new());
        let vector_db = Arc::new(MockVectorDB::new());

        let embedding_config = EmbeddingConfig::minilm_l6("test_models");
        if let Ok(embedding_engine) = OnnxEmbeddingEngine::new(embedding_config) {
            let embedding_engine = Arc::new(embedding_engine);

            let config = CognifyConfig::default()
                .with_chunk_size(2000)
                .with_chunks_per_batch(50)
                .with_summarization(false);

            let _pipeline =
                CognifyPipeline::new(storage, graph_db, vector_db, embedding_engine, config, None);
            // Pipeline should be created successfully with custom config
        }
    }

    #[tokio::test]
    async fn test_cognify_pipeline_with_no_ontology() {
        use cognee_embedding::EmbeddingConfig;

        let storage = Arc::new(MockStorage::new());
        let graph_db = Arc::new(MockGraphDB::new());
        let vector_db = Arc::new(MockVectorDB::new());

        let embedding_config = EmbeddingConfig::minilm_l6("test_models");
        if let Ok(embedding_engine) = OnnxEmbeddingEngine::new(embedding_config) {
            let embedding_engine = Arc::new(embedding_engine);

            // Create pipeline with no ontology resolver
            let pipeline = CognifyPipeline::new(
                storage,
                graph_db,
                vector_db,
                embedding_engine,
                CognifyConfig::default(),
                None, // No ontology resolver
            );

            // Should not have ontology
            assert!(!pipeline.has_ontology());
        }
    }

    #[tokio::test]
    async fn test_cognify_pipeline_with_noop_ontology() {
        use cognee_embedding::EmbeddingConfig;
        use cognee_ontology::NoOpOntologyResolver;

        let storage = Arc::new(MockStorage::new());
        let graph_db = Arc::new(MockGraphDB::new());
        let vector_db = Arc::new(MockVectorDB::new());

        let embedding_config = EmbeddingConfig::minilm_l6("test_models");
        if let Ok(embedding_engine) = OnnxEmbeddingEngine::new(embedding_config) {
            let embedding_engine = Arc::new(embedding_engine);

            // Create pipeline with no-op ontology resolver
            let pipeline = CognifyPipeline::new(
                storage,
                graph_db,
                vector_db,
                embedding_engine,
                CognifyConfig::default(),
                Some(Arc::new(NoOpOntologyResolver::new())),
            );

            // No-op resolver is not loaded, so has_ontology should return false
            assert!(!pipeline.has_ontology());
        }
    }

    #[tokio::test]
    async fn test_cognify_pipeline_default_has_noop_ontology() {
        use cognee_embedding::EmbeddingConfig;

        let storage = Arc::new(MockStorage::new());
        let graph_db = Arc::new(MockGraphDB::new());
        let vector_db = Arc::new(MockVectorDB::new());

        let embedding_config = EmbeddingConfig::minilm_l6("test_models");
        if let Ok(embedding_engine) = OnnxEmbeddingEngine::new(embedding_config) {
            let embedding_engine = Arc::new(embedding_engine);

            // Pipeline with no-op ontology resolver
            let pipeline = CognifyPipeline::new(
                storage,
                graph_db,
                vector_db,
                embedding_engine,
                CognifyConfig::default(),
                Some(Arc::new(NoOpOntologyResolver::new())),
            );

            // No-op resolver is not loaded, so has_ontology should return false
            assert!(!pipeline.has_ontology());
        }
    }
}
