use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::DataPoint;
use crate::has_datapoint::HasDataPoint;

/// A chunk of text extracted from a document during the cognify pipeline.
///
/// Extends `DataPoint` (via `#[serde(flatten)]`) following the same pattern
/// used by `Entity`, `EntityType`, and `EdgeType`.
///
/// Python equivalent: `cognee.infrastructure.engine.models.DataPoint` subclass
/// `DocumentChunk` with `metadata = {"index_fields": ["text"]}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentChunk {
    /// Base data point fields (id, timestamps, metadata, type, etc.)
    #[serde(flatten)]
    pub base: DataPoint,
    /// The chunk text content.
    pub text: String,
    /// Token count (word count by default).
    pub chunk_size: usize,
    /// Sequential index within the parent document, starting at 0.
    pub chunk_index: usize,
    /// How the chunk boundary was determined (e.g. "paragraph_end", "sentence_end").
    pub cut_type: String,
    /// ID of the parent document this chunk belongs to (convenience field).
    pub document_id: Uuid,
    /// Document ID for graph edge (mirrors Python's `is_part_of` relationship).
    pub is_part_of: Option<Uuid>,
    /// Entity refs populated during graph extraction (mirrors Python's `contains` list).
    #[serde(default)]
    pub contains: Vec<serde_json::Value>,
}

impl DocumentChunk {
    /// Create a new DocumentChunk with a deterministic ID.
    ///
    /// Sets:
    /// - `base.data_type` = `"DocumentChunk"`
    /// - `base.metadata["index_fields"]` = `["text"]`
    /// - `base.id` = the provided deterministic UUID
    /// - `is_part_of` = `Some(document_id)`
    /// - `contains` = empty
    pub fn new(
        id: Uuid,
        text: String,
        chunk_size: usize,
        chunk_index: usize,
        cut_type: String,
        document_id: Uuid,
    ) -> Self {
        let mut base = DataPoint::new("DocumentChunk", None);
        base.id = id;
        base.set_metadata("index_fields", json!(["text"]));
        Self {
            base,
            text,
            chunk_size,
            chunk_index,
            cut_type,
            document_id,
            is_part_of: Some(document_id),
            contains: vec![],
        }
    }
}

impl HasDataPoint for DocumentChunk {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
    // for_each_child_mut: default no-op — DocumentChunk references its
    // parent `Document` by `document_id: Uuid` (and `is_part_of: Option<Uuid>`),
    // not via an owned child.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_chunk_implements_has_datapoint() {
        let document_id = Uuid::new_v4();
        let chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "hello".into(),
            1,
            0,
            "paragraph_end".into(),
            document_id,
        );
        let dp_id = chunk.base.id;
        assert_eq!(chunk.data_point().id, dp_id);
        let mut chunk2 = chunk;
        assert_eq!(chunk2.data_point_mut().id, dp_id);
    }
}
