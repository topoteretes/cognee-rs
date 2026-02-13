//! Cognify pipeline skeleton.
//!
//! Orchestrates the cognify process: classify documents → chunk text → (TODO)
//! extract graph → summarize → store.

use std::sync::Arc;

use cognee_models::{Data, Document, DocumentChunk, classify_documents};
use cognee_storage::StorageTrait;

use crate::error::ChunkingError;
use crate::text_chunker::chunk_text;
use crate::token_counter::{TokenCounter, WordCounter};

/// The cognify pipeline. Generic over the storage backend used to read
/// ingested file content.
pub struct CognifyPipeline<S: StorageTrait> {
    storage: Arc<S>,
}

impl<S: StorageTrait> CognifyPipeline<S> {
    pub fn new(storage: Arc<S>) -> Self {
        Self { storage }
    }

    /// Run the cognify pipeline on a set of Data items.
    ///
    /// Currently implements:
    /// 1. Document classification (text/* only)
    /// 2. Text chunking
    ///
    /// Returns the generated chunks. Later stages (graph extraction,
    /// summarization, storage) are marked as TODOs.
    pub async fn cognify(
        &self,
        data_items: Vec<Data>,
        max_chunk_size: usize,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        self.cognify_with_counter(data_items, max_chunk_size, &WordCounter)
            .await
    }

    /// Run cognify with a custom token counter.
    pub async fn cognify_with_counter<C: TokenCounter>(
        &self,
        data_items: Vec<Data>,
        max_chunk_size: usize,
        counter: &C,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        if max_chunk_size == 0 {
            return Err(ChunkingError::InvalidChunkSize(0));
        }

        // Stage 1: Classify documents
        let documents: Vec<Document> = classify_documents(&data_items);

        // Stage 2: Chunk text
        let mut all_chunks = Vec::new();
        for document in &documents {
            let content_bytes = self
                .storage
                .retrieve(&document.raw_data_location)
                .await
                .map_err(|e| ChunkingError::StorageError(e.to_string()))?;

            let content = String::from_utf8(content_bytes)
                .map_err(|e| ChunkingError::InvalidUtf8(e.to_string()))?;

            let chunks = chunk_text(document.id, &content, max_chunk_size, counter);
            all_chunks.extend(chunks);
        }

        // TODO: Stage 3 — extract_graph_from_data
        //   LLM-based knowledge graph extraction from chunks.
        //   For each chunk, use an LLM to extract Node and Edge objects
        //   based on a graph model (e.g. KnowledgeGraph).

        // TODO: Stage 4 — summarize_text
        //   LLM-based text summarization for each chunk.
        //   Creates TextSummary objects linked to their source chunks.

        // TODO: Stage 5 — add_data_points
        //   Store nodes and edges in graph DB.
        //   Create embeddings and store in vector DB.
        //   Optionally create and embed triplets for semantic search.

        Ok(all_chunks)
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
    async fn cognify_invalid_chunk_size() {
        let storage = Arc::new(MockStorage::new());
        let pipeline = CognifyPipeline::new(storage);
        let result = pipeline.cognify(vec![], 0).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cognify_text_data() {
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

        assert!(!chunks.is_empty());
        // All chunks should have text content
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
        }
    }

    #[tokio::test]
    async fn cognify_skips_non_text() {
        let storage = Arc::new(MockStorage::new());

        let data = Data::new(
            Uuid::new_v4(),
            "image.png".into(),
            "/storage/image.png".into(),
            "file://image.png".into(),
            "png".into(),
            "image/png".into(),
            "hash456".into(),
            Uuid::new_v4(),
        );

        let pipeline = CognifyPipeline::new(storage);
        let chunks = pipeline.cognify(vec![data], 100).await.unwrap();
        assert!(chunks.is_empty());
    }
}
