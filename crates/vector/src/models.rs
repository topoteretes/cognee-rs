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
}
