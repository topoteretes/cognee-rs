//! Cognify pipeline - Full knowledge graph extraction pipeline.
//!
//! Orchestrates the complete cognify process:
//! 1. Extract text chunks (via ExtractTextChunksPipeline)
//! 2. Extract knowledge graph from chunks
//! 3. Summarize text
//! 4. Store data points (nodes, edges, embeddings)

use std::sync::Arc;

use cognee_chunking::ExtractTextChunksPipeline;
use cognee_llm::Llm;
use cognee_models::{Data, DocumentChunk};
use cognee_storage::StorageTrait;
use uuid::Uuid;

use crate::error::CognifyError;
use crate::fact_extraction::FactExtractor;
use crate::graph_integration::{
    GraphEdgePair, GraphNodePair, deduplicate_nodes_and_edges, expand_with_nodes_and_edges,
};

/// The full cognify pipeline. Orchestrates all stages of knowledge graph
/// extraction and storage.
///
/// Generic over the storage backend used to read ingested file content.
pub struct CognifyPipeline<S: StorageTrait> {
    text_chunks_pipeline: ExtractTextChunksPipeline<S>,
}

impl<S: StorageTrait> CognifyPipeline<S> {
    pub fn new(storage: Arc<S>) -> Self {
        let text_chunks_pipeline = ExtractTextChunksPipeline::new(storage);
        Self {
            text_chunks_pipeline,
        }
    }

    /// Run the complete cognify pipeline on a set of Data items.
    ///
    /// Stages:
    /// 1. Document classification and text chunking (via ExtractTextChunksPipeline)
    /// 2. Extract knowledge graphs from chunks (LLM-based, parallel)
    /// 3. Merge and deduplicate graphs
    /// 4. TODO: Summarize text
    /// 5. TODO: Store data points in graph and vector databases
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
            });
        }

        // Stage 2a: Extract knowledge graphs from all chunks (parallel)
        let fact_extractor = FactExtractor::new(llm);

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

        // Stage 2b: Merge and deduplicate graphs
        let chunk_id = chunks[0].id; // Use first chunk as reference
        let (nodes, edges) = expand_with_nodes_and_edges(graphs, chunk_id, dataset_id)
            .await
            .map_err(|e| CognifyError::FactExtractionError(e.to_string()))?;

        // Stage 2c: Final deduplication pass
        let dedup_result = deduplicate_nodes_and_edges(nodes, edges);

        // TODO: Stage 2d — Database deduplication (mirrors Python's retrieve_existing_edges)
        //   Query database for existing edges to prevent duplicates:
        //   ```
        //   let existing_edges = database
        //       .retrieve_existing_edge_keys(dataset_id)
        //       .await?;
        //
        //   // Filter out edges that already exist in the database
        //   let new_edges: Vec<_> = dedup_result.unique_edges
        //       .into_iter()
        //       .filter(|edge| {
        //           let key = format!("{}_{}_{}",
        //               edge.source_entity_id,
        //               edge.target_entity_id,
        //               edge.relationship_name
        //           );
        //           !existing_edges.contains_key(&key)
        //       })
        //       .collect();
        //
        //   // Similarly, check for existing nodes by entity ID
        //   let existing_nodes = database
        //       .retrieve_existing_entity_ids(dataset_id)
        //       .await?;
        //
        //   let new_nodes: Vec<_> = dedup_result.unique_nodes
        //       .into_iter()
        //       .filter(|node| !existing_nodes.contains(&node.entity.base.id))
        //       .collect();
        //   ```

        // TODO: Stage 3 — summarize_text
        //   LLM-based text summarization for each chunk.

        // TODO: Stage 4 — add_data_points
        //   Store nodes and edges in graph DB.
        //   Create embeddings and store in vector DB.

        Ok(CognifyResult {
            chunks,
            entities: dedup_result.unique_nodes,
            edges: dedup_result.unique_edges,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_storage::MockStorage;

    // Note: Tests that require LLM are in integration tests

    #[tokio::test]
    async fn test_cognify_pipeline_creation() {
        let storage = Arc::new(MockStorage::new());
        let _pipeline = CognifyPipeline::new(storage);
        // Pipeline should be created successfully
    }
}
