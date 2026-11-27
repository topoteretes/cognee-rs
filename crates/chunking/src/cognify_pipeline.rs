//! Extract text chunks pipeline.
//!
//! Orchestrates the initial stages of the cognify process:
//! classify documents → chunk text.

use std::sync::Arc;

use cognee_models::{Data, Document, DocumentChunk, classify_documents, is_legacy_binary_row};
use cognee_storage::StorageTrait;
use tracing::{debug, info, info_span, instrument};

use crate::error::ChunkingError;
use crate::text_chunker::chunk_text;
use crate::token_counter::{TokenCountMode, TokenCounter, WordCounter};

/// The extract text chunks pipeline.
///
/// This pipeline handles the first two stages of cognify:
/// 1. Document classification ([`classify_documents`]: every item, an
///    unlisted extension as text, as Python does; a legacy binary row,
///    [`is_legacy_binary_row`], is skipped)
/// 2. Text chunking of each document's stored bytes, decoded as UTF-8 (bytes
///    that are not UTF-8 fail the call)
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
    /// 1. Document classification (see [`ExtractTextChunksPipeline`])
    /// 2. Text chunking
    ///
    /// Returns the generated chunks.
    pub async fn extract_chunks(
        &self,
        data_items: Vec<Data>,
        max_chunk_size: usize,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        self.extract_chunks_with_counter(
            data_items,
            max_chunk_size,
            &WordCounter,
            TokenCountMode::default(),
        )
        .await
    }

    /// Extract text chunks with a custom token counter and sizing mode.
    #[instrument(name = "chunking.extract_chunks", skip(self, data_items, counter), fields(max_chunk_size, data_count = data_items.len()))]
    pub async fn extract_chunks_with_counter<C: TokenCounter>(
        &self,
        data_items: Vec<Data>,
        max_chunk_size: usize,
        counter: &C,
        mode: TokenCountMode,
    ) -> Result<Vec<DocumentChunk>, ChunkingError> {
        if max_chunk_size == 0 {
            return Err(ChunkingError::InvalidChunkSize(0));
        }

        let data_items: Vec<Data> = data_items
            .into_iter()
            .filter(|item| !is_legacy_binary_row(item))
            .collect();
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
            // Python reads stored text in text mode, so chunks never hold
            // `\r`; cognify gets the same from its loader rerun.
            let content = cognee_utils::universal_newlines(&content);

            let chunks = chunk_text(document.base.id, &content, max_chunk_size, counter, mode);
            debug!(chunk_count = chunks.len(), document_id = %document.base.id, "document chunked");
            all_chunks.extend(chunks);
        }

        info!(total_chunks = all_chunks.len(), "chunking complete");
        Ok(all_chunks)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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

    /// Store `bytes` as a `.txt` document and return its chunk texts.
    async fn chunk_texts_of(bytes: &[u8]) -> Vec<String> {
        let storage = Arc::new(MockStorage::new());
        let location = storage.store(bytes, "test.txt").await.unwrap();
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
        chunks.into_iter().map(|chunk| chunk.text).collect()
    }

    #[tokio::test]
    async fn extract_chunks_translates_line_endings_as_python() {
        let crlf = chunk_texts_of(b"First one.\r\n\r\nSecond\rone.\r\n").await;
        let lf = chunk_texts_of(b"First one.\n\nSecond\none.\n").await;
        assert!(!lf.is_empty());
        assert_eq!(crlf, lf, "stored CRLF text chunks as its LF version");
    }

    /// An extension the table does not list is read as text, as Python's
    /// `document_class_for` does (`classify_documents.py:131`).
    #[tokio::test]
    async fn extract_chunks_reads_an_unknown_extension_as_text() {
        let storage = Arc::new(MockStorage::new());
        let location = storage
            .store(b"Hello world. This is a test.", "data.xyz")
            .await
            .unwrap();

        let data = Data::builder(
            Uuid::new_v4(),
            "data.xyz",
            location,
            "file://data.xyz",
            "xyz",
            "application/octet-stream",
            "hash456",
            Uuid::new_v4(),
        )
        .build();

        let pipeline = ExtractTextChunksPipeline::new(storage);
        let chunks = pipeline.extract_chunks(vec![data], 100).await.unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks[0].text.contains("Hello world"));
    }

    /// A legacy binary row is skipped without being read (its stored file
    /// does not exist here), so it does not fail the call.
    #[tokio::test]
    async fn extract_chunks_skips_a_legacy_binary_row() {
        let storage = Arc::new(MockStorage::new());
        let data = Data::builder(
            Uuid::new_v4(),
            "archive",
            "text_0123456789abcdef0123456789abcdef.bin",
            "file://archive.zip",
            "zip",
            "application/zip",
            "hash789",
            Uuid::new_v4(),
        )
        .build();

        let pipeline = ExtractTextChunksPipeline::new(storage);
        let chunks = pipeline.extract_chunks(vec![data], 100).await.unwrap();
        assert!(chunks.is_empty());
    }
}
