//! Extract text chunks pipeline.
//!
//! Orchestrates the initial stages of the cognify process:
//! classify documents → chunk text.

use std::sync::Arc;

use cognee_models::{Data, Document, DocumentChunk, classify_documents};
use cognee_storage::StorageTrait;
use tracing::{debug, info, info_span, instrument};

use crate::error::ChunkingError;
use crate::text_chunker::chunk_text;
use crate::token_counter::{TokenCounter, WordCounter};

/// The extract text chunks pipeline.
///
/// This pipeline handles the first two stages of cognify:
/// 1. Document classification (text/* only)
/// 2. Text chunking
pub struct ExtractTextChunksPipeline {
    storage: Arc<dyn StorageTrait>,
}

impl ExtractTextChunksPipeline {
    pub fn new(storage: Arc<dyn StorageTrait>) -> Self {
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
    #[instrument(name = "chunking.extract_chunks", skip(self, data_items, counter), fields(max_chunk_size, data_count = data_items.len()))]
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
        info!(doc_count = documents.len(), "documents classified");

        let mut all_chunks = Vec::new();
        for document in &documents {
            let _doc_span = info_span!(
                "chunking.process_document",
                document_id = %document.base.id,
                mime_type = %document.mime_type,
            )
            .entered();

            let content_bytes = self
                .storage
                .retrieve(&document.raw_data_location)
                .await
                .map_err(|e| ChunkingError::StorageError(e.to_string()))?;

            let content = String::from_utf8(content_bytes)
                .map_err(|e| ChunkingError::InvalidUtf8(e.to_string()))?;

            let chunks = chunk_text(document.base.id, &content, max_chunk_size, counter);
            debug!(chunk_count = chunks.len(), document_id = %document.base.id, "document chunked");
            all_chunks.extend(chunks);
        }

        info!(total_chunks = all_chunks.len(), "chunking complete");
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

        let data = Data::builder(
            Uuid::new_v4(),
            "test.txt",
            location,
            "text://test",
            "txt",
            "text/plain",
            "hash123",
            Uuid::new_v4(),
        )
        .build();

        let pipeline = ExtractTextChunksPipeline::new(storage);
        let chunks = pipeline.extract_chunks(vec![data], 100).await.unwrap();

        assert!(!chunks.is_empty());
        // All chunks should have text content
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
        }
    }

    #[tokio::test]
    async fn extract_chunks_skips_unknown_extension() {
        let storage = Arc::new(MockStorage::new());

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

        let pipeline = ExtractTextChunksPipeline::new(storage);
        let chunks = pipeline.extract_chunks(vec![data], 100).await.unwrap();
        assert!(chunks.is_empty());
    }
}
