//! Mock vector database implementation for testing.
//!
//! Provides an in-memory vector database for unit tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "mock infrastructure — panics are acceptable"
)]

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::node_filter::metadata_matches_node_filter;
use crate::vector_db_trait::VectorDB;
use crate::zero_norm::{warn_zero_norm_points, warn_zero_norm_query};

/// Mock vector database for testing
///
/// Stores vectors in-memory using HashMap. Not optimized for actual similarity search.
#[derive(Clone)]
pub struct MockVectorDB {
    /// Map from (data_type, field_name) -> collection data
    collections: Arc<Mutex<HashMap<String, CollectionData>>>,
    /// Log of `create_collection` invocations, as `(data_type, field_name)` tuples.
    create_collection_calls: Arc<Mutex<Vec<(String, String)>>>,
    /// Log of `index_points` invocations, as `"{data_type}/{field_name}"` strings
    /// (one entry per call — useful for asserting batch counts).
    index_points_calls: Arc<Mutex<Vec<String>>>,
    /// Optional injected error returned from the next `index_points` call.
    index_error: Arc<Mutex<Option<String>>>,
    /// Optional injected error returned from `retrieve` calls.
    retrieve_error: Arc<Mutex<Option<String>>>,
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
            create_collection_calls: Arc::new(Mutex::new(Vec::new())),
            index_points_calls: Arc::new(Mutex::new(Vec::new())),
            index_error: Arc::new(Mutex::new(None)),
            retrieve_error: Arc::new(Mutex::new(None)),
        }
    }

    /// Generate collection key from data_type and field_name
    fn collection_key(data_type: &str, field_name: &str) -> String {
        format!("{data_type}_{field_name}")
    }

    /// Return the number of times `create_collection` was invoked.
    pub fn create_collection_count(&self) -> usize {
        let log = self.create_collection_calls.lock().unwrap(); // lock poison is unrecoverable
        log.len()
    }

    /// Return `true` if `create_collection` was called for `(data_type, field_name)`.
    pub fn was_create_collection_called(&self, data_type: &str, field_name: &str) -> bool {
        let log = self.create_collection_calls.lock().unwrap(); // lock poison is unrecoverable
        log.iter()
            .any(|(dt, fn_)| dt == data_type && fn_ == field_name)
    }

    /// Return the number of times `index_points` was invoked successfully.
    pub fn index_points_call_count(&self) -> usize {
        let log = self.index_points_calls.lock().unwrap(); // lock poison is unrecoverable
        log.len()
    }

    /// Inject an error that will be returned from subsequent `index_points` calls
    /// as `VectorDBError::StorageError`.
    pub fn set_index_error(&self, msg: impl Into<String>) {
        let mut slot = self.index_error.lock().unwrap(); // lock poison is unrecoverable
        *slot = Some(msg.into());
    }

    /// Inject an error that will be returned from subsequent `retrieve` calls
    /// as `VectorDBError::StorageError`.
    pub fn set_retrieve_error(&self, msg: impl Into<String>) {
        let mut slot = self.retrieve_error.lock().unwrap(); // lock poison is unrecoverable
        *slot = Some(msg.into());
    }

    /// Return the metadata payload stored against `point_id` in the
    /// `(data_type, field_name)` collection, or `None` if the collection
    /// or point is unknown.
    ///
    /// Used by provenance-payload regression tests (gap 05-10) to verify
    /// the full DataPoint dump round-trips through `index_points`.
    pub fn get_payload(
        &self,
        data_type: &str,
        field_name: &str,
        point_id: Uuid,
    ) -> Option<HashMap<String, serde_json::Value>> {
        let key = Self::collection_key(data_type, field_name);
        let collections = self.collections.lock().unwrap(); // lock poison is unrecoverable
        let collection = collections.get(&key)?;
        collection
            .points
            .iter()
            .find(|p| p.id == point_id)
            .map(|p| p.metadata.clone())
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
        // Log the call before any validation so tests can see every attempt.
        {
            let mut log = self.create_collection_calls.lock().unwrap(); // lock poison is unrecoverable
            log.push((data_type.to_string(), field_name.to_string()));
        }

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
        // Error-injection hook for tests: fail before any side effects.
        {
            let slot = self.index_error.lock().unwrap(); // lock poison is unrecoverable
            if let Some(msg) = slot.as_ref() {
                return Err(VectorDBError::StorageError(msg.clone()));
            }
        }

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
                    collection: key.clone(),
                    expected: expected_dim,
                    actual: point.vector.len(),
                });
            }
        }
        warn_zero_norm_points("mock", &key, points);

        // Upsert points (replace if ID exists, otherwise append). On replace,
        // union dataset membership so a content-addressed point indexed under
        // several datasets stays retrievable for all of them (cross-dataset
        // dedup parity with the brute-force / production adapters).
        for new_point in points {
            if let Some(existing) = collection.points.iter_mut().find(|p| p.id == new_point.id) {
                let mut merged = new_point.clone();
                merged.merge_dataset_membership(existing);
                *existing = merged;
            } else {
                collection.points.push(new_point.clone());
            }
        }

        // Log the successful call for batch-count assertions.
        drop(collections);
        let mut log = self.index_points_calls.lock().unwrap(); // lock poison is unrecoverable
        log.push(format!("{data_type}/{field_name}"));

        Ok(())
    }

    async fn upsert_raw_vectors(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        // Empty input is a no-op — must not touch `points[0]`.
        if points.is_empty() {
            return Ok(());
        }
        warn_zero_norm_points("mock", &Self::collection_key(data_type, field_name), points);

        let key = Self::collection_key(data_type, field_name);
        let mut collections = self.collections.lock().unwrap(); // lock poison is unrecoverable

        // Self-create the collection when absent, sized from the first vector
        // (nothing else ever creates a system-owned collection like
        // TruthCentroid_vector). `index_points` deliberately does NOT do this.
        let collection = collections.entry(key).or_insert_with(|| CollectionData {
            dimension: points[0].vector.len(),
            points: Vec::new(),
        });

        // Full-metadata by-id insert-or-replace — NO dataset-membership union
        // (that is index_points' job; raw upsert writes each point verbatim).
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
        warn_zero_norm_query("mock", &key, query_vector);
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

    /// Exact server-side node-set filter: drop out-of-set points during the
    /// scan, *before* ranking + `top_k` truncation (filter-then-limit), so an
    /// in-set point is never crowded out by higher-similarity out-of-set points
    /// regardless of collection size (finding F9). With no filter this is
    /// identical to [`search_similar`](Self::search_similar).
    async fn search_similar_filtered(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
        node_name: Option<&[String]>,
        node_name_filter_operator: &str,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let requested: Option<&[String]> = match node_name {
            Some(names) if !names.is_empty() => Some(names),
            _ => None,
        };
        let key = Self::collection_key(data_type, field_name);
        warn_zero_norm_query("mock", &key, query_vector);
        let collections = self.collections.lock().unwrap(); // lock poison is unrecoverable

        let collection = collections
            .get(&key)
            .ok_or_else(|| VectorDBError::CollectionNotFound(key.clone()))?;

        // Filter out-of-set points before scoring, then rank and truncate.
        let mut scored_points: Vec<(usize, f32)> = collection
            .points
            .iter()
            .enumerate()
            .filter(|(_, point)| match requested {
                Some(names) => metadata_matches_node_filter(
                    &point.metadata,
                    Some(names),
                    node_name_filter_operator,
                ),
                None => true,
            })
            .map(|(idx, point)| {
                let score = Self::cosine_similarity(&point.vector, query_vector);
                (idx, score)
            })
            .collect();

        scored_points.sort_by(|a, b| b.1.total_cmp(&a.1));

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

    async fn retrieve(
        &self,
        data_type: &str,
        field_name: &str,
        ids: &[Uuid],
    ) -> VectorDBResult<Vec<SearchResult>> {
        // Error-injection hook for tests: fail before any read.
        {
            let slot = self.retrieve_error.lock().unwrap(); // lock poison is unrecoverable
            if let Some(msg) = slot.as_ref() {
                return Err(VectorDBError::StorageError(msg.clone()));
            }
        }
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let key = Self::collection_key(data_type, field_name);
        let collections = self.collections.lock().unwrap(); // lock poison is unrecoverable
        // Deliberate divergence from the CollectionNotFound idiom: a missing
        // collection yields an empty result (Python-parity — see the trait
        // doc-comment on `retrieve`).
        let Some(collection) = collections.get(&key) else {
            return Ok(vec![]);
        };
        Ok(collection
            .points
            .iter()
            .filter(|p| ids.contains(&p.id))
            .map(|p| SearchResult {
                id: p.id,
                score: 0.0,
                metadata: p.metadata.clone(),
            })
            .collect())
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
    async fn test_mock_retrieve_returns_matching_points_only() {
        let db = MockVectorDB::new();
        db.create_collection("T", "f", 2).await.unwrap();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        let points = vec![
            VectorPoint::new(id1, vec![1.0, 0.0]).with_metadata("k", json!("v1")),
            VectorPoint::new(id2, vec![0.0, 1.0]),
            VectorPoint::new(id3, vec![1.0, 1.0]),
        ];
        db.index_points("T", "f", &points).await.unwrap();

        let unknown = Uuid::new_v4();
        let results = db.retrieve("T", "f", &[id1, id2, unknown]).await.unwrap();

        let ids: std::collections::HashSet<Uuid> = results.iter().map(|r| r.id).collect();
        assert_eq!(ids, [id1, id2].into_iter().collect());
        for r in &results {
            assert_eq!(r.score, 0.0, "retrieve always sets score to 0.0");
        }
        let r1 = results.iter().find(|r| r.id == id1).unwrap();
        assert_eq!(r1.metadata.get("k"), Some(&json!("v1")));
    }

    #[tokio::test]
    async fn test_mock_retrieve_missing_collection_returns_empty() {
        let db = MockVectorDB::new();
        let results = db
            .retrieve("Nope", "field", &[Uuid::new_v4()])
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_mock_retrieve_empty_ids_returns_empty() {
        let db = MockVectorDB::new();
        db.create_collection("T", "f", 2).await.unwrap();
        db.index_points(
            "T",
            "f",
            &[VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0])],
        )
        .await
        .unwrap();
        let results = db.retrieve("T", "f", &[]).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_mock_upsert_raw_vectors_creates_and_replaces() {
        let db = MockVectorDB::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        // No prior create_collection — upsert_raw_vectors self-creates it.
        let points = vec![
            VectorPoint::new(id1, vec![1.0, 0.0]).with_metadata("k", json!("a")),
            VectorPoint::new(id2, vec![0.0, 1.0]).with_metadata("k", json!("b")),
        ];
        db.upsert_raw_vectors("Raw", "vec", &points).await.unwrap();
        assert!(db.has_collection("Raw", "vec").await.unwrap());
        assert_eq!(db.collection_size("Raw", "vec").await.unwrap(), 2);

        // Re-upsert id1 with fully different metadata — full replace, no merge.
        let replace = vec![VectorPoint::new(id1, vec![0.5, 0.5]).with_metadata("k", json!("z"))];
        db.upsert_raw_vectors("Raw", "vec", &replace).await.unwrap();
        assert_eq!(db.collection_size("Raw", "vec").await.unwrap(), 2);
        let got = db.get_payload("Raw", "vec", id1).unwrap();
        assert_eq!(got.get("k"), Some(&json!("z")));

        // Empty upsert is a no-op and does not create a new collection.
        db.upsert_raw_vectors("Other", "vec", &[]).await.unwrap();
        assert!(!db.has_collection("Other", "vec").await.unwrap());
    }

    #[tokio::test]
    async fn test_mock_search_similar_filtered_filter_then_limit() {
        // Many out-of-set points outrank two off-axis in-set points; the
        // server-side filter-then-limit must still return both in-set points.
        let db = MockVectorDB::new();
        db.create_collection("Filt", "f", 2).await.unwrap();
        let mut points = Vec::new();
        for _ in 0..40 {
            points.push(
                VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0])
                    .with_metadata("belongs_to_set", json!(["drop"])),
            );
        }
        let keep1 = Uuid::new_v4();
        let keep2 = Uuid::new_v4();
        points.push(
            VectorPoint::new(keep1, vec![0.8, 0.6])
                .with_metadata("belongs_to_set", json!(["keep"])),
        );
        points.push(
            VectorPoint::new(keep2, vec![0.8, 0.6])
                .with_metadata("belongs_to_set", json!(["keep"])),
        );
        db.index_points("Filt", "f", &points).await.unwrap();

        let names = vec!["keep".to_string()];
        let r = db
            .search_similar_filtered("Filt", "f", &[1.0, 0.0], 2, Some(&names), "OR")
            .await
            .unwrap();
        let got: std::collections::HashSet<Uuid> = r.iter().map(|r| r.id).collect();
        assert_eq!(got, [keep1, keep2].into_iter().collect());
    }

    #[tokio::test]
    async fn test_mock_search_similar_filtered_and_vs_or_and_bare_string() {
        let db = MockVectorDB::new();
        db.create_collection("Sem", "f", 2).await.unwrap();
        let both = Uuid::new_v4();
        let only_a = Uuid::new_v4();
        let bare = Uuid::new_v4();
        db.index_points(
            "Sem",
            "f",
            &[
                VectorPoint::new(both, vec![1.0, 0.0])
                    .with_metadata("belongs_to_set", json!([{"id":"1","name":"a","type":"NodeSet"},{"id":"2","name":"b","type":"NodeSet"}])),
                VectorPoint::new(only_a, vec![1.0, 0.0])
                    .with_metadata("belongs_to_set", json!([{"id":"1","name":"a","type":"NodeSet"}])),
                VectorPoint::new(bare, vec![1.0, 0.0])
                    .with_metadata("belongs_to_set", json!(["a"])),
            ],
        )
        .await
        .unwrap();

        // OR on {a}: the two object-`a` rows plus the bare-string "a" row.
        let r = db
            .search_similar_filtered("Sem", "f", &[1.0, 0.0], 10, Some(&["a".to_string()]), "OR")
            .await
            .unwrap();
        let got: std::collections::HashSet<Uuid> = r.iter().map(|r| r.id).collect();
        assert_eq!(got, [both, only_a, bare].into_iter().collect());

        // AND on {a, b}: only the row carrying both NodeSet names.
        let req = vec!["a".to_string(), "b".to_string()];
        let r = db
            .search_similar_filtered("Sem", "f", &[1.0, 0.0], 10, Some(&req), "AND")
            .await
            .unwrap();
        let got: std::collections::HashSet<Uuid> = r.iter().map(|r| r.id).collect();
        assert_eq!(got, [both].into_iter().collect());
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
