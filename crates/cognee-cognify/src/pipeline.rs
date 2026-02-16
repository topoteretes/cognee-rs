//! Cognify pipeline - Full knowledge graph extraction pipeline.
//!
//! Orchestrates the complete cognify process:
//! 1. Extract text chunks (via ExtractTextChunksPipeline)
//! 2. Extract knowledge graph from chunks
//! 3. Summarize text
//! 4. Store data points (nodes, edges, embeddings)

use std::sync::Arc;

use cognee_chunking::ExtractTextChunksPipeline;
use cognee_models::{Data, DocumentChunk};
use cognee_storage::StorageTrait;

use crate::error::CognifyError;

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
    /// 2. Extract knowledge graph from chunks (TODO)
    /// 3. Summarize text (TODO)
    /// 4. Store data points in graph and vector databases (TODO)
    ///
    /// Returns the generated chunks. Later stages will return additional data
    /// structures (nodes, edges, summaries, embeddings).
    pub async fn cognify(
        &self,
        data_items: Vec<Data>,
        max_chunk_size: usize,
    ) -> Result<Vec<DocumentChunk>, CognifyError> {
        // Stage 1: Extract text chunks (classify + chunk)
        let chunks = self
            .text_chunks_pipeline
            .extract_chunks(data_items, max_chunk_size)
            .await
            .map_err(|e| CognifyError::ChunkingError(e.to_string()))?;

        // TODO: Stage 2 — extract_graph_from_data
        //   LLM-based knowledge graph extraction from chunks.
        //   For each chunk, use an LLM to extract Node and Edge objects
        //   based on a graph model (e.g. KnowledgeGraph).
        //
        //   Pseudocode:
        //   ```
        //   let mut nodes = Vec::new();
        //   let mut edges = Vec::new();
        //   for chunk in &chunks {
        //       let graph = llm.extract_knowledge_graph(&chunk.text).await?;
        //       nodes.extend(graph.nodes);
        //       edges.extend(graph.edges);
        //   }
        //   ```

        // TODO: Stage 3 — summarize_text
        //   LLM-based text summarization for each chunk.
        //   Creates TextSummary objects linked to their source chunks.
        //
        //   Pseudocode:
        //   ```
        //   let mut summaries = Vec::new();
        //   for chunk in &chunks {
        //       let summary = llm.summarize(&chunk.text).await?;
        //       summaries.push(TextSummary {
        //           id: Uuid::new_v4(),
        //           chunk_id: chunk.id,
        //           summary_text: summary,
        //       });
        //   }
        //   ```

        // TODO: Stage 4 — add_data_points
        //   Store nodes and edges in graph DB.
        //   Create embeddings and store in vector DB.
        //   Optionally create and embed triplets for semantic search.
        //
        //   Pseudocode:
        //   ```
        //   // Store in graph database
        //   for node in nodes {
        //       graph_db.store_node(&node).await?;
        //   }
        //   for edge in edges {
        //       graph_db.store_edge(&edge).await?;
        //   }
        //
        //   // Generate and store embeddings
        //   for chunk in &chunks {
        //       let embedding = embedding_model.embed(&chunk.text).await?;
        //       vector_db.store(chunk.id, embedding).await?;
        //   }
        //
        //   // Optional: Create and embed triplets (subject-predicate-object)
        //   for edge in &edges {
        //       let triplet_text = format!("{} {} {}",
        //           edge.source_label, edge.relation, edge.target_label);
        //       let triplet_embedding = embedding_model.embed(&triplet_text).await?;
        //       vector_db.store_triplet(edge.id, triplet_embedding).await?;
        //   }
        //   ```

        Ok(chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_storage::MockStorage;
    use uuid::Uuid;

    #[tokio::test]
    async fn cognify_empty_data() {
        let storage = Arc::new(MockStorage::new());
        let pipeline = CognifyPipeline::new(storage);
        let chunks = pipeline.cognify(vec![], 100).await.unwrap();
        assert!(chunks.is_empty());
    }

    #[tokio::test]
    async fn cognify_delegates_to_text_chunks_pipeline() {
        let storage = Arc::new(MockStorage::new());

        // Store some content
        let location = storage
            .store(b"Hello world. This is a test.", "test.txt")
            .await
            .unwrap();

        let data = Data::new(
            Uuid::new_v4(),
            "test.txt".into(),
            location,
            "text://test".into(),
            "txt".into(),
            "text/plain".into(),
            "hash123".into(),
            Uuid::new_v4(),
        );

        let pipeline = CognifyPipeline::new(storage);
        let chunks = pipeline.cognify(vec![data], 100).await.unwrap();

        // Should have chunks from the text chunks pipeline
        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
        }
    }
}
