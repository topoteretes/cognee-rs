//! Extract text chunks pipeline.
//!
//! Orchestrates the initial stages of the cognify process:
//! classify documents → chunk text.

use std::sync::Arc;

use cognee_models::{Data, Document, DocumentChunk, classify_documents};
use cognee_storage::StorageTrait;

use crate::error::ChunkingError;
use crate::text_chunker::chunk_text;
use crate::token_counter::{TokenCounter, WordCounter};

/// The extract text chunks pipeline. Generic over the storage backend used
/// to read ingested file content.
///
/// This pipeline handles the first two stages of cognify:
/// 1. Document classification (text/* only)
/// 2. Text chunking
pub struct ExtractTextChunksPipeline<S: StorageTrait> {
    storage: Arc<S>,
}

impl<S: StorageTrait> ExtractTextChunksPipeline<S> {
    pub fn new(storage: Arc<S>) -> Self {
        Self { storage }
    }

    /// Extract text chunks from a set of Data items.
    ///
    /// Implements:
    /// 1. Document classification (text/* only)
    /// 2. Text chunking
    ///
    /// Returns the generated chunks.
    pub async fn extract_chunks(
        &self,
        data_items: Vec<Data>,
        max_chunk_size: usize,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        self.extract_chunks_with_counter(data_items, max_chunk_size, &WordCounter)
            .await
    }

    /// Extract text chunks with a custom token counter.
    pub async fn extract_chunks_with_counter<C: TokenCounter>(
        &self,
        data_items: Vec<Data>,
        max_chunk_size: usize,
        counter: &C,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        if max_chunk_size == 0 {
            return Err(ChunkingError::InvalidChunkSize(0));
        }

        let documents: Vec<Document> = classify_documents(&data_items);

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

        Ok(all_chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_storage::MockStorage;
    use uuid::Uuid;

    #[tokio::test]
    async fn extract_chunks_empty_data() {
        let storage = Arc::new(MockStorage::new());
        let pipeline = ExtractTextChunksPipeline::new(storage);
        let chunks = pipeline.extract_chunks(vec![], 100).await.unwrap();
        assert!(chunks.is_empty());
    }

    #[tokio::test]
    async fn extract_chunks_invalid_chunk_size() {
        let storage = Arc::new(MockStorage::new());
        let pipeline = ExtractTextChunksPipeline::new(storage);
        let result = pipeline.extract_chunks(vec![], 0).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn extract_chunks_text_data() {
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

        let pipeline = ExtractTextChunksPipeline::new(storage);
        let chunks = pipeline.extract_chunks(vec![data], 100).await.unwrap();

        assert!(!chunks.is_empty());
        // All chunks should have text content
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
        }
    }

    #[tokio::test]
    async fn extract_chunks_skips_non_text() {
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

        let pipeline = ExtractTextChunksPipeline::new(storage);
        let chunks = pipeline.extract_chunks(vec![data], 100).await.unwrap();
        assert!(chunks.is_empty());
    }
}
