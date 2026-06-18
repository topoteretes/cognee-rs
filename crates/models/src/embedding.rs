//! Embedding - Storage model for vector embeddings of data points.
//!
//! Represents an embedding vector for a specific field of a data point.
//! Used to store embeddings in vector databases for semantic search.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Embedding vector for a data point field.
///
/// Each embedding represents a specific field (e.g., "text", "name")
/// of a data point (e.g., DocumentChunk, Entity) as a dense vector.
///
/// # Examples
/// ```
/// use cognee_models::Embedding;
/// use uuid::Uuid;
///
/// let chunk_id = Uuid::new_v4();
/// let embedding = Embedding::new(
///     chunk_id,
///     "DocumentChunk",
///     "text",
///     vec![0.1, 0.2, 0.3], // Dense vector
/// );
///
/// assert_eq!(embedding.dimensions(), 3);
/// assert_eq!(embedding.field_name, "text");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Embedding {
    /// UUID of the data point this embedding belongs to
    pub data_point_id: Uuid,

    /// Type of the data point (e.g., "DocumentChunk", "Entity", "TextSummary")
    pub data_type: String,

    /// Name of the field that was embedded (e.g., "text", "name", "content")
    pub field_name: String,

    /// Dense embedding vector (f32 for compatibility with most vector DBs)
    pub vector: Vec<f32>,
}

impl Embedding {
    /// Create a new embedding.
    ///
    /// # Arguments
    /// * `data_point_id` - UUID of the source data point
    /// * `data_type` - Type discriminator (e.g., "DocumentChunk")
    /// * `field_name` - Field that was embedded (e.g., "text")
    /// * `vector` - Dense embedding vector (usually 384, 768, or 1536 dimensions)
    pub fn new(
        data_point_id: Uuid,
        data_type: impl Into<String>,
        field_name: impl Into<String>,
        vector: Vec<f32>,
    ) -> Self {
        Self {
            data_point_id,
            data_type: data_type.into(),
            field_name: field_name.into(),
            vector,
        }
    }

    /// Get the dimensionality of the embedding vector.
    pub fn dimensions(&self) -> usize {
        self.vector.len()
    }

    /// Calculate L2 norm of the embedding (should be ~1.0 if normalized).
    pub fn norm(&self) -> f32 {
        self.vector.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    /// Calculate cosine similarity with another embedding.
    ///
    /// Both embeddings must be normalized (L2 norm = 1.0).
    /// Returns dot product in range [-1.0, 1.0].
    pub fn cosine_similarity(&self, other: &Embedding) -> Option<f32> {
        if self.vector.len() != other.vector.len() {
            return None;
        }

        let similarity = self
            .vector
            .iter()
            .zip(&other.vector)
            .map(|(a, b)| a * b)
            .sum();

        Some(similarity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_creation() {
        let id = Uuid::new_v4();
        let embedding = Embedding::new(id, "DocumentChunk", "text", vec![0.1, 0.2, 0.3]);

        assert_eq!(embedding.data_point_id, id);
        assert_eq!(embedding.data_type, "DocumentChunk");
        assert_eq!(embedding.field_name, "text");
        assert_eq!(embedding.dimensions(), 3);
    }

    #[test]
    fn test_norm() {
        let embedding = Embedding::new(
            Uuid::new_v4(),
            "Entity",
            "name",
            vec![0.6, 0.8], // 3-4-5 triangle: norm = 1.0
        );

        let norm = embedding.norm();
        assert!((norm - 1.0).abs() < 0.01, "Expected norm ~1.0, got {norm}");
    }

    #[test]
    fn test_cosine_similarity() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        // Normalized vectors
        let e1 = Embedding::new(id1, "Entity", "name", vec![1.0, 0.0, 0.0]);
        let e2 = Embedding::new(id2, "Entity", "name", vec![1.0, 0.0, 0.0]);
        let e3 = Embedding::new(id2, "Entity", "name", vec![0.0, 1.0, 0.0]);

        // Identical vectors
        assert_eq!(e1.cosine_similarity(&e2), Some(1.0));

        // Orthogonal vectors
        assert_eq!(e1.cosine_similarity(&e3), Some(0.0));
    }

    #[test]
    fn test_cosine_similarity_dimension_mismatch() {
        let e1 = Embedding::new(Uuid::new_v4(), "Entity", "name", vec![1.0, 0.0]);
        let e2 = Embedding::new(Uuid::new_v4(), "Entity", "name", vec![1.0, 0.0, 0.0]);

        assert_eq!(e1.cosine_similarity(&e2), None);
    }
}
