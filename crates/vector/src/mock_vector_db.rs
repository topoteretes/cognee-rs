//! Mock vector database implementation for testing.
//!
//! Provides an in-memory vector database for unit tests.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::vector_db_trait::VectorDB;

/// Mock vector database for testing
///
/// Stores vectors in-memory using HashMap. Not optimized for actual similarity search.
#[derive(Clone)]
pub struct MockVectorDB {
    /// Map from (data_type, field_name) -> collection data
    collections: Arc<Mutex<HashMap<String, CollectionData>>>,
}

#[derive(Clone)]
struct CollectionData {
    dimension: usize,
    points: Vec<VectorPoint>,
}

impl MockVectorDB {
    /// Create a new mock vector database
    pub fn new() -> Self {
        Self {
            collections: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Generate collection key from data_type and field_name
    fn collection_key(data_type: &str, field_name: &str) -> String {
        format!("{}_{}", data_type, field_name)
    }

    /// Compute cosine similarity between two vectors
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if mag_a == 0.0 || mag_b == 0.0 {
            0.0
        } else {
            dot / (mag_a * mag_b)
        }
    }
}

impl Default for MockVectorDB {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VectorDB for MockVectorDB {
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        let key = Self::collection_key(data_type, field_name);
        let mut collections = self.collections.lock().unwrap(); // lock poison is unrecoverable

        if collections.contains_key(&key) {
            return Err(VectorDBError::CollectionExists(key));
        }

        collections.insert(
            key,
            CollectionData {
                dimension,
                points: Vec::new(),
            },
        );

        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let key = Self::collection_key(data_type, field_name);
        let collections = self.collections.lock().unwrap(); // lock poison is unrecoverable
        Ok(collections.contains_key(&key))
    }

    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        if points.is_empty() {
            return Ok(());
        }

        let key = Self::collection_key(data_type, field_name);
        let mut collections = self.collections.lock().unwrap(); // lock poison is unrecoverable

        let collection = collections
            .get_mut(&key)
            .ok_or_else(|| VectorDBError::CollectionNotFound(key.clone()))?;

        // Validate dimension
        let expected_dim = collection.dimension;
        for point in points {
            if point.vector.len() != expected_dim {
                return Err(VectorDBError::DimensionMismatch {
                    expected: expected_dim,
                    actual: point.vector.len(),
                });
            }
        }

        // Upsert points (replace if ID exists, otherwise append)
        for new_point in points {
            if let Some(existing) = collection.points.iter_mut().find(|p| p.id == new_point.id) {
                *existing = new_point.clone();
            } else {
                collection.points.push(new_point.clone());
            }
        }

        Ok(())
    }

    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let key = Self::collection_key(data_type, field_name);
        let collections = self.collections.lock().unwrap(); // lock poison is unrecoverable

        let collection = collections
            .get(&key)
            .ok_or_else(|| VectorDBError::CollectionNotFound(key.clone()))?;

        // Compute cosine similarity for all points
        let mut scored_points: Vec<(usize, f32)> = collection
            .points
            .iter()
            .enumerate()
            .map(|(idx, point)| {
                let score = Self::cosine_similarity(&point.vector, query_vector);
                (idx, score)
            })
            .collect();

        // Sort by score descending
        scored_points.sort_by(|a, b| b.1.total_cmp(&a.1));

        // Take top k
        let results: Vec<SearchResult> = scored_points
            .into_iter()
            .take(top_k)
            .map(|(idx, score)| {
                let point = &collection.points[idx];
                SearchResult {
                    id: point.id,
                    score,
                    metadata: point.metadata.clone(),
                }
            })
            .collect();

        Ok(results)
    }

    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        let key = Self::collection_key(data_type, field_name);
        let mut collections = self.collections.lock().unwrap(); // lock poison is unrecoverable
        collections.remove(&key);
        Ok(())
    }

    async fn delete_points(
        &self,
        data_type: &str,
        field_name: &str,
        point_ids: &[Uuid],
    ) -> VectorDBResult<()> {
        let key = Self::collection_key(data_type, field_name);
        let mut collections = self.collections.lock().unwrap(); // lock poison is unrecoverable

        let collection = collections
            .get_mut(&key)
            .ok_or_else(|| VectorDBError::CollectionNotFound(key.clone()))?;

        collection
            .points
            .retain(|point| !point_ids.contains(&point.id));

        Ok(())
    }

    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        let key = Self::collection_key(data_type, field_name);
        let collections = self.collections.lock().unwrap(); // lock poison is unrecoverable

        let collection = collections
            .get(&key)
            .ok_or_else(|| VectorDBError::CollectionNotFound(key.clone()))?;

        Ok(collection.points.len())
    }

    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        let collections = self.collections.lock().unwrap(); // lock poison is unrecoverable
        let pairs = collections
            .keys()
            .filter_map(|key| {
                // Keys are stored as "{data_type}_{field_name}"; split on the first '_'
                key.split_once('_')
                    .map(|(dt, fn_)| (dt.to_string(), fn_.to_string()))
            })
            .collect();
        Ok(pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_mock_create_collection() {
        let db = MockVectorDB::new();

        db.create_collection("Test", "field", 3).await.unwrap();
        assert!(db.has_collection("Test", "field").await.unwrap());
    }

    #[tokio::test]
    async fn test_mock_index_and_search() {
        let db = MockVectorDB::new();

        db.create_collection("Entity", "name", 3).await.unwrap();

        let points = vec![
            VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0]).with_metadata("name", json!("A")),
            VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0, 0.0]).with_metadata("name", json!("B")),
            VectorPoint::new(Uuid::new_v4(), vec![0.0, 0.0, 1.0]).with_metadata("name", json!("C")),
        ];

        db.index_points("Entity", "name", &points).await.unwrap();

        // Search for similar to first vector
        let query = vec![1.0, 0.0, 0.0];
        let results = db
            .search_similar("Entity", "name", &query, 2)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].score >= results[1].score);
    }

    #[tokio::test]
    async fn test_list_collections_returns_created_collections() {
        let db = MockVectorDB::new();

        // Empty database returns no collections
        let initial = db.list_collections().await.unwrap();
        assert!(initial.is_empty(), "no collections initially");

        db.create_collection("DocumentChunk", "text", 3)
            .await
            .unwrap();
        db.create_collection("Entity", "name", 3).await.unwrap();

        let mut collections = db.list_collections().await.unwrap();
        // Sort for deterministic comparison
        collections.sort();

        assert_eq!(collections.len(), 2);
        assert!(
            collections.contains(&("DocumentChunk".to_string(), "text".to_string())),
            "DocumentChunk:text should be listed"
        );
        assert!(
            collections.contains(&("Entity".to_string(), "name".to_string())),
            "Entity:name should be listed"
        );
    }

    #[tokio::test]
    async fn test_mock_collection_size() {
        let db = MockVectorDB::new();

        db.create_collection("Test", "field", 2).await.unwrap();

        let points = vec![
            VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0]),
            VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0]),
        ];

        db.index_points("Test", "field", &points).await.unwrap();

        let size = db.collection_size("Test", "field").await.unwrap();
        assert_eq!(size, 2);
    }
}
