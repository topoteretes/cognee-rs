use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A chunk of text extracted from a document during the cognify pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentChunk {
    /// Deterministic ID: uuid5(NAMESPACE_OID, "{document_id}-{chunk_index}").
    pub id: Uuid,
    /// The chunk text content.
    pub text: String,
    /// Token count (word count by default).
    pub chunk_size: usize,
    /// Sequential index within the parent document, starting at 0.
    pub chunk_index: usize,
    /// How the chunk boundary was determined (e.g. "paragraph_end", "sentence_end").
    pub cut_type: String,
    /// ID of the parent document this chunk belongs to.
    pub document_id: Uuid,
}
