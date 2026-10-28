//! GlobalContextSummary - one node of a dataset's global context index.
//!
//! Mirrors Python's `GlobalContextSummary` (`cognee/tasks/summarization/models.py`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DataPoint;
use crate::has_datapoint::HasDataPoint;

/// A bucket or dataset-root summary of the global context index.
///
/// Python's `summarized_in` relation field is not a property here: the index
/// writes its `summarized_in` edges explicitly, and no path sets the field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobalContextSummary {
    /// Base data point fields (id, timestamps, metadata, etc.)
    #[serde(flatten)]
    pub base: DataPoint,

    /// The summary text, the one embedded field.
    pub text: String,

    /// The dataset the index belongs to, as a string (Python's `str` id).
    pub dataset_id: String,

    /// 0 for a bucket over text summaries or a document; higher levels sit
    /// above it; the root is one level above the top bucket level.
    pub level: i64,

    /// Whether this is the dataset root.
    #[serde(default)]
    pub is_root: bool,

    /// Flat id of the document a bucket summarizes; `None` on the root and on
    /// LLM-built buckets. Not embedded: it only travels in the vector payload.
    #[serde(default)]
    pub document_id: Option<String>,

    /// The entities a graph-built level-0 bucket groups by. Rust builds no
    /// graph buckets, so it is always `None` on what Rust writes.
    #[serde(default)]
    pub graph_bucket_entity_ids: Option<Vec<String>>,
}

impl GlobalContextSummary {
    /// The node type, and the vector collection's data type.
    pub const TYPE: &'static str = "GlobalContextSummary";
    /// Index fields to embed for vector search.
    pub const INDEX_FIELDS: &'static [&'static str] = &["text"];
    /// The vector collection holding the summary texts
    /// (`GlobalContextSummary_text`).
    pub const COLLECTION: &'static str = "GlobalContextSummary_text";

    /// A summary with the given id.
    pub fn new(
        id: Uuid,
        text: impl Into<String>,
        dataset_id: impl Into<String>,
        level: i64,
        is_root: bool,
        document_id: Option<String>,
    ) -> Self {
        let mut base = DataPoint::with_metadata(
            Self::TYPE,
            None,
            HashMap::from([(
                "index_fields".to_string(),
                serde_json::json!(Self::INDEX_FIELDS),
            )]),
        );
        base.id = id;
        Self {
            base,
            text: text.into(),
            dataset_id: dataset_id.into(),
            level,
            is_root,
            document_id,
            graph_bucket_entity_ids: None,
        }
    }
}

impl HasDataPoint for GlobalContextSummary {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
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

    #[test]
    fn serializes_python_field_names() {
        let summary =
            GlobalContextSummary::new(Uuid::nil(), "text", "ds", 0, false, Some("doc".to_string()));
        let value = serde_json::to_value(&summary).unwrap();
        assert_eq!(value["type"], "GlobalContextSummary");
        assert_eq!(value["text"], "text");
        assert_eq!(value["dataset_id"], "ds");
        assert_eq!(value["level"], 0);
        assert_eq!(value["is_root"], false);
        assert_eq!(value["document_id"], "doc");
        assert_eq!(
            value["metadata"]["index_fields"],
            serde_json::json!(["text"])
        );
        assert!(value["graph_bucket_entity_ids"].is_null());
    }
}
