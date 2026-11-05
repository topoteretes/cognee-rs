use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::DataPoint;
use crate::has_datapoint::HasDataPoint;

/// Type name of a DLT row chunk; its vector collection is `DltRow_text`.
pub const DLT_ROW_TYPE: &str = "DltRow";

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
    /// Display name of the node. Only DLT rows carry one (`table:primary_key`,
    /// Python's `DltRow.name`); document chunks serialise none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
            name: None,
            chunk_size,
            chunk_index,
            cut_type,
            document_id,
            is_part_of: Some(document_id),
            contains: vec![],
        }
    }

    /// Create the chunk of one DLT row (Python's `DltRow(DocumentChunk)`).
    ///
    /// Same field shape as [`DocumentChunk::new`], but the node's type name is
    /// [`DLT_ROW_TYPE`], so the graph node and the vector collection
    /// (`DltRow_text`) are the row's own rather than `DocumentChunk`'s, and
    /// `name` labels the row as `table:primary_key`. `cut_type` is
    /// `"dlt_row"`.
    pub fn dlt_row(
        id: Uuid,
        name: String,
        text: String,
        chunk_size: usize,
        chunk_index: usize,
        document_id: Uuid,
    ) -> Self {
        let mut chunk = Self::new(
            id,
            text,
            chunk_size,
            chunk_index,
            "dlt_row".to_string(),
            document_id,
        );
        chunk.base.data_type = DLT_ROW_TYPE.to_string();
        chunk.name = Some(name);
        chunk
    }

    /// Whether this chunk is a DLT row (its type name is [`DLT_ROW_TYPE`]).
    pub fn is_dlt_row(&self) -> bool {
        self.base.data_type == DLT_ROW_TYPE
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
#[allow(
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
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

    #[test]
    fn dlt_row_is_its_own_type_with_a_name() {
        let chunk = DocumentChunk::dlt_row(
            Uuid::new_v4(),
            "orders:42".into(),
            "Table: orders".into(),
            2,
            0,
            Uuid::new_v4(),
        );
        assert!(chunk.is_dlt_row());
        assert_eq!(chunk.base.data_type, "DltRow");
        assert_eq!(chunk.cut_type, "dlt_row");
        let value = serde_json::to_value(&chunk).unwrap();
        assert_eq!(value["type"], "DltRow");
        assert_eq!(value["name"], "orders:42");

        let plain = DocumentChunk::new(
            Uuid::new_v4(),
            "t".into(),
            1,
            0,
            "word".into(),
            Uuid::new_v4(),
        );
        assert!(!plain.is_dlt_row());
        assert!(serde_json::to_value(&plain).unwrap().get("name").is_none());
    }
}
