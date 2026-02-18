use async_trait::async_trait;
use edge::EdgeShard;
use segment::data_types::vectors::{NamedQuery, VectorInternal};
use segment::types::{
    Distance, ExtendedPointId, Payload, PayloadStorageType, ScoredPoint, SegmentConfig,
    VectorDataConfig, VectorStorageType, WithPayloadInterface, WithVector,
};
use shard::count::CountRequestInternal;
use shard::operations::CollectionUpdateOperations::PointOperation;
use shard::operations::point_ops::PointInsertOperationsInternal::PointsBatch;
use shard::operations::point_ops::PointOperations::UpsertPoints;
use shard::operations::point_ops::{BatchPersisted, BatchVectorStructPersisted, VectorPersisted};
use shard::query::query_enum::QueryEnum;
use shard::query::{ScoringQuery, ShardQueryRequest};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::vector_db_trait::VectorDB;

/// Qdrant adapter using in-process EdgeShard storage
///
/// Uses Qdrant's embedded segment storage for on-device deployment.
/// Each (data_type, field_name) pair gets its own collection (EdgeShard).
pub struct QdrantAdapter {
    /// Map from collection_name -> Arc<EdgeShard>
    shards: Arc<RwLock<HashMap<String, Arc<EdgeShard>>>>,

    /// Base directory for shard storage
    data_dir: PathBuf,

    /// Default vector dimension
    dimension: usize,
}

impl QdrantAdapter {
    /// Create a new Qdrant adapter
    ///
    /// Automatically loads any existing shards from the data directory.
    ///
    /// # Arguments
    /// * `data_dir` - Directory for shard storage
    /// * `dimension` - Default vector dimension (384 for MiniLM)
    pub fn new(data_dir: PathBuf, dimension: usize) -> Self {
        let adapter = Self {
            shards: Arc::new(RwLock::new(HashMap::new())),
            data_dir,
            dimension,
        };

        // Auto-load existing shards (ignore errors, they'll surface on actual operations)
        if let Err(e) = adapter.load_existing_shards() {
            eprintln!("Warning: Failed to load existing shards: {}", e);
        }

        adapter
    }

    /// Load all existing shards from disk into memory
    ///
    /// This method is called automatically by `new()`, but can be called again
    /// to explicitly reload shards from disk (e.g., if shards were added by another process).
    ///
    /// # Returns
    /// Number of shards loaded
    fn load_existing_shards(&self) -> VectorDBResult<usize> {
        let mut loaded_count = 0;

        if !self.data_dir.exists() {
            return Ok(0);
        }

        // Scan data directory for collection folders
        let entries = std::fs::read_dir(&self.data_dir)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let collection_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string());

                if let Some(collection) = collection_name {
                    // Try to load the shard (use default dimension, will be validated on use)
                    match self.get_or_create_shard(&collection, self.dimension) {
                        Ok(_) => {
                            loaded_count += 1;
                        }
                        Err(e) => {
                            // Log error but continue loading other shards
                            eprintln!("Warning: Failed to load shard '{}': {}", collection, e);
                        }
                    }
                }
            }
        }

        Ok(loaded_count)
    }

    /// Generate collection name from data_type and field_name
    ///
    /// Example: ("DocumentChunk", "text") -> "DocumentChunk_text"
    fn collection_name(data_type: &str, field_name: &str) -> String {
        format!("{}_{}", data_type, field_name)
    }

    /// Get or create EdgeShard for a collection
    ///
    /// Creates shard on disk if it doesn't exist, otherwise loads existing.
    fn get_or_create_shard(
        &self,
        collection: &str,
        dimension: usize,
    ) -> VectorDBResult<Arc<EdgeShard>> {
        // Check if already loaded
        {
            let shards = self.shards.read().unwrap();
            if let Some(shard) = shards.get(collection) {
                return Ok(shard.clone());
            }
        }

        // Create shard directory
        let shard_path = self.data_dir.join(collection);
        std::fs::create_dir_all(&shard_path)?;

        // Configure segment
        let config = SegmentConfig {
            vector_data: HashMap::from([(
                "default".to_string(),
                VectorDataConfig {
                    size: dimension,
                    distance: Distance::Cosine,
                    storage_type: VectorStorageType::ChunkedMmap,
                    index: Default::default(),
                    quantization_config: None,
                    multivector_config: None,
                    datatype: None,
                },
            )]),
            sparse_vector_data: HashMap::new(),
            payload_storage_type: PayloadStorageType::Mmap,
        };

        // Load or create EdgeShard
        let shard = Arc::new(
            EdgeShard::load(&shard_path, Some(config))
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?,
        );

        // Store in map
        let mut shards = self.shards.write().unwrap();
        shards.insert(collection.to_string(), shard.clone());

        Ok(shard)
    }

    /// Convert vector points to BatchPersisted (more efficient than PointsList)
    fn points_to_batch(points: &[VectorPoint]) -> BatchPersisted {
        let ids: Vec<ExtendedPointId> = points
            .iter()
            .map(|p| ExtendedPointId::NumId(p.id.as_u128() as u64))
            .collect();

        // Use Named variant with "default" vector name to match queries
        // Convert each Vec<f32> to VectorPersisted::Dense
        let vectors: Vec<VectorPersisted> = points
            .iter()
            .map(|p| VectorPersisted::Dense(p.vector.clone()))
            .collect();

        let mut named_vectors = HashMap::new();
        named_vectors.insert("default".to_string(), vectors);

        let payloads: Vec<Option<Payload>> = points
            .iter()
            .map(|p| {
                if p.metadata.is_empty() {
                    None
                } else {
                    let mut payload = Payload::default();
                    for (k, v) in &p.metadata {
                        payload.0.insert(k.clone(), v.clone());
                    }
                    Some(payload)
                }
            })
            .collect();

        BatchPersisted {
            ids,
            vectors: BatchVectorStructPersisted::Named(named_vectors),
            payloads: Some(payloads),
        }
    }

    /// Convert Qdrant scored point to SearchResult
    fn from_qdrant_result(scored: &ScoredPoint) -> SearchResult {
        let metadata: HashMap<String, serde_json::Value> = scored
            .payload
            .as_ref()
            .map(|p| {
                p.0.iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let id = match &scored.id {
            ExtendedPointId::NumId(n) => Uuid::from_u128(*n as u128),
            ExtendedPointId::Uuid(s) => *s,
        };

        SearchResult {
            id,
            score: scored.score,
            metadata,
        }
    }
}

#[async_trait]
impl VectorDB for QdrantAdapter {
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        let collection = Self::collection_name(data_type, field_name);

        // Check if already exists
        if self.has_collection(data_type, field_name).await? {
            return Err(VectorDBError::CollectionExists(collection));
        }

        // Create shard (will be created on first use)
        self.get_or_create_shard(&collection, dimension)?;

        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let collection = Self::collection_name(data_type, field_name);

        // First check in-memory cache
        {
            let shards = self.shards.read().unwrap();
            if shards.contains_key(&collection) {
                return Ok(true);
            }
        }

        // Check if collection exists on disk
        let shard_path = self.data_dir.join(&collection);
        Ok(shard_path.exists() && shard_path.is_dir())
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

        let collection = Self::collection_name(data_type, field_name);

        // Validate dimension
        let expected_dim = points[0].vector.len();
        for point in points {
            if point.vector.len() != expected_dim {
                return Err(VectorDBError::DimensionMismatch {
                    expected: expected_dim,
                    actual: point.vector.len(),
                });
            }
        }

        // Get or create shard
        let shard = self.get_or_create_shard(&collection, expected_dim)?;

        // Convert to batch format (more efficient than PointsList)
        let batch = Self::points_to_batch(points);

        // Upsert into shard
        shard
            .update(PointOperation(UpsertPoints(PointsBatch(batch))))
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Ok(())
    }

    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let collection = Self::collection_name(data_type, field_name);

        // Get shard
        let shard = self.get_or_create_shard(&collection, self.dimension)?;

        // Perform query
        let query_vec: VectorInternal = query_vector.to_vec().into();
        let results = shard
            .query(ShardQueryRequest {
                prefetches: vec![],
                query: Some(ScoringQuery::Vector(QueryEnum::Nearest(NamedQuery {
                    query: query_vec,
                    using: Some("default".to_string()),
                }))),
                filter: None,
                score_threshold: None,
                limit: top_k,
                offset: 0,
                params: None,
                with_vector: WithVector::Bool(false),
                with_payload: WithPayloadInterface::Bool(true),
            })
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Ok(results.iter().map(Self::from_qdrant_result).collect())
    }

    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        let collection = Self::collection_name(data_type, field_name);
        let mut shards = self.shards.write().unwrap();

        shards.remove(&collection);

        // Delete shard directory
        let shard_path = self.data_dir.join(&collection);
        if shard_path.exists() {
            std::fs::remove_dir_all(&shard_path)?;
        }

        Ok(())
    }

    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        let collection = Self::collection_name(data_type, field_name);
        let shard = self.get_or_create_shard(&collection, self.dimension)?;

        let count = shard
            .count(CountRequestInternal {
                filter: None,
                exact: true,
            })
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::VectorPoint;
    use serde_json::json;
    use tempfile::TempDir;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_create_collection() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 384);

        adapter
            .create_collection("DocumentChunk", "text", 384)
            .await
            .unwrap();

        assert!(
            adapter
                .has_collection("DocumentChunk", "text")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_collection_already_exists() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 384);

        adapter
            .create_collection("Entity", "name", 384)
            .await
            .unwrap();

        // Try to create again should error
        let result = adapter.create_collection("Entity", "name", 384).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_index_and_search() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 3);

        // Create collection
        adapter
            .create_collection("Entity", "name", 3)
            .await
            .unwrap();

        // Create test points
        let points = vec![
            VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0])
                .with_metadata("name", json!("Cognee")),
            VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0, 0.0])
                .with_metadata("name", json!("Knowledge Graph")),
            VectorPoint::new(Uuid::new_v4(), vec![0.0, 0.0, 1.0])
                .with_metadata("name", json!("Rust")),
        ];

        adapter
            .index_points("Entity", "name", &points)
            .await
            .unwrap();

        // Search for similar to first vector
        let query = vec![1.0, 0.0, 0.0];
        let results = adapter
            .search_similar("Entity", "name", &query, 2)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].score > results[1].score); // First result should be most similar
    }

    #[tokio::test]
    async fn test_collection_size() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 2);

        adapter.create_collection("Test", "field", 2).await.unwrap();

        let points = vec![
            VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0]),
            VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0]),
        ];

        adapter
            .index_points("Test", "field", &points)
            .await
            .unwrap();

        let size = adapter.collection_size("Test", "field").await.unwrap();
        assert_eq!(size, 2);
    }

    #[tokio::test]
    async fn test_dimension_validation() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 3);

        adapter.create_collection("Test", "field", 3).await.unwrap();

        // Mismatched dimensions should error
        let points = vec![
            VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0]),
            VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0]), // Wrong dimension!
        ];

        let result = adapter.index_points("Test", "field", &points).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_collection() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 2);

        adapter.create_collection("Test", "field", 2).await.unwrap();
        assert!(adapter.has_collection("Test", "field").await.unwrap());

        adapter.delete_collection("Test", "field").await.unwrap();
        assert!(!adapter.has_collection("Test", "field").await.unwrap());
    }

    #[tokio::test]
    async fn test_search_returns_top_k() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 2);

        adapter.create_collection("Test", "field", 2).await.unwrap();

        // Create 10 points
        let points: Vec<VectorPoint> = (0..10)
            .map(|i| {
                VectorPoint::new(
                    Uuid::new_v4(),
                    vec![i as f32 / 10.0, 1.0 - (i as f32 / 10.0)],
                )
            })
            .collect();

        adapter
            .index_points("Test", "field", &points)
            .await
            .unwrap();

        // Request top 3
        let query = vec![0.5, 0.5];
        let results = adapter
            .search_similar("Test", "field", &query, 3)
            .await
            .unwrap();

        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn test_empty_points_index() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 2);

        adapter.create_collection("Test", "field", 2).await.unwrap();

        // Indexing empty points should succeed
        let points: Vec<VectorPoint> = vec![];
        let result = adapter.index_points("Test", "field", &points).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_metadata_preserved() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 2);

        adapter.create_collection("Test", "field", 2).await.unwrap();

        let test_id = Uuid::new_v4();
        let points = vec![
            VectorPoint::new(test_id, vec![1.0, 0.0])
                .with_metadata("type", json!("DocumentChunk"))
                .with_metadata("document_id", json!("test-doc-123"))
                .with_metadata("chunk_index", json!(42)),
        ];

        adapter
            .index_points("Test", "field", &points)
            .await
            .unwrap();

        // Search and verify metadata
        let query = vec![1.0, 0.0];
        let results = adapter
            .search_similar("Test", "field", &query, 1)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].metadata.get("type"),
            Some(&json!("DocumentChunk"))
        );
        assert_eq!(
            results[0].metadata.get("document_id"),
            Some(&json!("test-doc-123"))
        );
        assert_eq!(results[0].metadata.get("chunk_index"), Some(&json!(42)));
    }

    #[tokio::test]
    async fn test_persistence_and_reload() {
        let temp_dir = TempDir::new().unwrap();
        let data_path = temp_dir.path().to_path_buf();

        // Create adapter and add some data
        {
            let adapter = QdrantAdapter::new(data_path.clone(), 3);

            adapter
                .create_collection("Entity", "name", 3)
                .await
                .unwrap();

            let points = vec![
                VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0])
                    .with_metadata("name", json!("Cognee")),
                VectorPoint::new(Uuid::new_v4(), vec![0.0, 1.0, 0.0])
                    .with_metadata("name", json!("Rust")),
            ];

            adapter
                .index_points("Entity", "name", &points)
                .await
                .unwrap();

            let size = adapter.collection_size("Entity", "name").await.unwrap();
            assert_eq!(size, 2);
        }

        // Create NEW adapter pointing to same directory
        // Existing shards are auto-loaded in new()
        let adapter2 = QdrantAdapter::new(data_path.clone(), 3);

        // Should detect collection exists (auto-loaded)
        assert!(adapter2.has_collection("Entity", "name").await.unwrap());

        // Should be able to query the existing data
        let size = adapter2.collection_size("Entity", "name").await.unwrap();
        assert_eq!(size, 2);

        // Should be able to search
        let query = vec![1.0, 0.0, 0.0];
        let results = adapter2
            .search_similar("Entity", "name", &query, 1)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].metadata.get("name"), Some(&json!("Cognee")));
    }

    #[tokio::test]
    async fn test_has_collection_without_preload() {
        let temp_dir = TempDir::new().unwrap();
        let data_path = temp_dir.path().to_path_buf();

        // Create collection with first adapter
        {
            let adapter = QdrantAdapter::new(data_path.clone(), 2);
            adapter.create_collection("Test", "field", 2).await.unwrap();
        }

        // Create new adapter WITHOUT calling load_existing_shards
        let adapter2 = QdrantAdapter::new(data_path, 2);

        // has_collection should still return true (checks filesystem)
        assert!(adapter2.has_collection("Test", "field").await.unwrap());
    }
}
