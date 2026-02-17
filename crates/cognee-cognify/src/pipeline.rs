//! Cognify pipeline - Full knowledge graph extraction pipeline.
//!
//! Orchestrates the complete cognify process:
//! 1. Extract text chunks (via ExtractTextChunksPipeline)
//! 2. Extract knowledge graph from chunks
//! 3. Summarize text
//! 4. Store data points (nodes, edges, embeddings)

use std::collections::HashMap;
use std::sync::Arc;

use cognee_chunking::ExtractTextChunksPipeline;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_models::{Data, DocumentChunk};
use cognee_storage::StorageTrait;
use uuid::Uuid;

use crate::error::CognifyError;
use crate::fact_extraction::FactExtractor;
use crate::graph_integration::{
    GraphEdgePair, GraphNodePair, deduplicate_nodes_and_edges, expand_with_nodes_and_edges,
    retrieve_existing_edges,
};
use crate::summarization::{SummaryExtractor, TextSummary};

/// The full cognify pipeline. Orchestrates all stages of knowledge graph
/// extraction and storage.
///
/// Generic over the storage backend and graph database used.
pub struct CognifyPipeline<S: StorageTrait, G: GraphDBTrait> {
    text_chunks_pipeline: ExtractTextChunksPipeline<S>,
    graph_db: Arc<G>,
}

impl<S: StorageTrait, G: GraphDBTrait> CognifyPipeline<S, G> {
    pub fn new(storage: Arc<S>, graph_db: Arc<G>) -> Self {
        let text_chunks_pipeline = ExtractTextChunksPipeline::new(storage);
        Self {
            text_chunks_pipeline,
            graph_db,
        }
    }

    /// Run the complete cognify pipeline on a set of Data items.
    ///
    /// Stages:
    /// 1. Document classification and text chunking (via ExtractTextChunksPipeline)
    /// 2. Extract knowledge graphs from chunks (LLM-based, parallel)
    /// 3. Merge and deduplicate graphs
    /// 4. Summarize text chunks (LLM-based, parallel)
    /// 5. TODO: Create embeddings and store in vector database
    ///
    /// Returns CognifyResult with chunks, entities, and edges.
    ///
    /// # Arguments
    /// * `data_items` - Data items to process
    /// * `dataset_id` - Dataset UUID for linking entities
    /// * `max_chunk_size` - Maximum chunk size in tokens
    /// * `llm` - LLM instance for knowledge graph extraction
    ///
    /// # Example
    /// ```ignore
    /// use cognee_cognify::CognifyPipeline;
    /// use cognee_storage::LocalStorage;
    /// use cognee_llm::OpenAIAdapter;
    /// use std::sync::Arc;
    /// use std::path::PathBuf;
    /// use uuid::Uuid;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let storage = Arc::new(LocalStorage::new(PathBuf::from("/tmp/cognee")));
    /// let llm = Arc::new(OpenAIAdapter::new("http://localhost:11434".to_string()));
    /// let pipeline = CognifyPipeline::new(storage);
    ///
    /// let result = pipeline.cognify(
    ///     vec![],
    ///     Uuid::new_v4(),
    ///     512,
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
        max_chunk_size: usize,
        llm: Arc<L>,
    ) -> Result<CognifyResult, CognifyError> {
        // Stage 1: Extract text chunks (classify + chunk)
        let chunks = self
            .text_chunks_pipeline
            .extract_chunks(data_items, max_chunk_size)
            .await
            .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;

        if chunks.is_empty() {
            return Ok(CognifyResult {
                chunks,
                entities: vec![],
                edges: vec![],
                summaries: vec![],
            });
        }

        // Stage 2a: Extract knowledge graphs from all chunks (parallel)
        let fact_extractor = FactExtractor::new(Arc::clone(&llm));

        let mut extract_tasks = Vec::new();
        for chunk in &chunks {
            let extractor = fact_extractor.clone();
            let text = chunk.text.clone();
            extract_tasks.push(tokio::spawn(async move {
                extractor.extract_facts(&text, None).await
            }));
        }

        let graph_results = futures::future::join_all(extract_tasks).await;
        let mut graphs = Vec::new();
        for result in graph_results {
            let graph = result.map_err(|e| CognifyError::FactExtractionError(e.to_string()))??;
            graphs.push(graph);
        }

        // Stage 2b: Database deduplication — query for existing edges
        let existing_edges_set = retrieve_existing_edges(self.graph_db.as_ref(), &graphs).await?;

        // Stage 2c: Merge and deduplicate graphs (with DB awareness)
        let chunk_id = chunks[0].id; // Use first chunk as reference
        let (nodes, edges) =
            expand_with_nodes_and_edges(graphs, chunk_id, dataset_id, &existing_edges_set)
                .await
                .map_err(|e| CognifyError::FactExtractionError(e.to_string()))?;

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

        // Stage 4: Summarize text chunks (parallel)
        let summary_extractor = SummaryExtractor::new(llm);
        let summaries = summary_extractor.summarize_chunks(&chunks, None).await?;

        // TODO: Stage 5 — Create embeddings
        //   Generate embeddings for nodes, edges, and chunks, store in vector DB.

        Ok(CognifyResult {
            chunks,
            entities: dedup_result.unique_nodes,
            edges: dedup_result.unique_edges,
            summaries,
        })
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_graph::MockGraphDB;
    use cognee_storage::MockStorage;

    // Note: Tests that require LLM are in integration tests

    #[tokio::test]
    async fn test_cognify_pipeline_creation() {
        let storage = Arc::new(MockStorage::new());
        let graph_db = Arc::new(MockGraphDB::new());
        let _pipeline = CognifyPipeline::new(storage, graph_db);
        // Pipeline should be created successfully
    }
}
