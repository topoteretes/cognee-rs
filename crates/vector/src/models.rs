use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Vector point to be indexed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorPoint {
    /// Data point ID
    pub id: Uuid,

    /// Embedding vector
    pub vector: Vec<f32>,

    /// Metadata (type, field, original data)
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Result from similarity search
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Data point ID
    pub id: Uuid,

    /// Similarity score (higher = more similar)
    pub score: f32,

    /// Metadata from the indexed point
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Configuration for vector collection
#[derive(Debug, Clone)]
pub struct CollectionConfig {
    /// Collection name (e.g., "DocumentChunk_text")
    pub name: String,

    /// Vector dimension
    pub dimension: usize,

    /// Distance metric (Cosine, Euclidean, Dot)
    pub distance: DistanceMetric,
}

/// Distance metric used for vector similarity comparisons.
#[derive(Debug, Clone, Copy)]
pub enum DistanceMetric {
    /// Cosine similarity (angle-based, ignores magnitude).
    Cosine,
    /// Euclidean (L2) distance.
    Euclidean,
    /// Dot-product similarity.
    Dot,
}

impl VectorPoint {
    /// Create a new vector point
    pub fn new(id: Uuid, vector: Vec<f32>) -> Self {
        Self {
            id,
            vector,
            metadata: HashMap::new(),
        }
    }

    /// Add metadata field
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Accumulate the dataset membership recorded on a `previous` point (an
    /// existing point with the same id) into `self`'s [`DATASET_IDS_KEY`] array.
    ///
    /// Point IDs are content-addressed (UUID v5 of the content), so the *same*
    /// point is indexed once per dataset that contains that content. Vector
    /// adapters upsert by id with full replacement, so a plain replace keeps
    /// only the last dataset's scalar `dataset_id` and silently drops the
    /// earlier datasets' membership — making the content unretrievable when a
    /// search is scoped to one of those earlier datasets (the cross-dataset
    /// dedup bug). Calling this in `index_points` upsert paths before replacing
    /// an existing point keeps `dataset_ids` as the union of every dataset the
    /// content belongs to, mirroring Python's `belongs_to_set` union semantics.
    pub fn merge_dataset_membership(&mut self, previous: &VectorPoint) {
        let mut ids: Vec<String> = Vec::new();
        // `previous` first so membership order is stable (oldest dataset first).
        collect_dataset_ids(previous, &mut ids);
        collect_dataset_ids(self, &mut ids);
        if !ids.is_empty() {
            self.metadata.insert(
                DATASET_IDS_KEY.to_string(),
                serde_json::Value::Array(ids.into_iter().map(serde_json::Value::String).collect()),
            );
        }
    }
}

/// Metadata key holding the array of dataset-ID strings a point belongs to.
/// This is the union accumulated across every dataset the content-addressed
/// point has been indexed under (see [`VectorPoint::merge_dataset_membership`]).
pub const DATASET_IDS_KEY: &str = "dataset_ids";

/// Scalar metadata key written by the cognify indexer for the single dataset
/// currently being indexed. Retained for back-compat; the authoritative
/// membership is the union in [`DATASET_IDS_KEY`].
pub const DATASET_ID_KEY: &str = "dataset_id";

/// Append every dataset-ID string recorded on `point` (from both the
/// [`DATASET_IDS_KEY`] array and the scalar [`DATASET_ID_KEY`]) into `out`,
/// skipping empties and duplicates.
fn collect_dataset_ids(point: &VectorPoint, out: &mut Vec<String>) {
    if let Some(arr) = point
        .metadata
        .get(DATASET_IDS_KEY)
        .and_then(|v| v.as_array())
    {
        for v in arr {
            if let Some(s) = v.as_str()
                && !s.is_empty()
                && !out.iter().any(|x| x == s)
            {
                out.push(s.to_string());
            }
        }
    }
    if let Some(s) = point.metadata.get(DATASET_ID_KEY).and_then(|v| v.as_str())
        && !s.is_empty()
        && !out.iter().any(|x| x == s)
    {
        out.push(s.to_string());
    }
}
