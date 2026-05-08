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
use shard::operations::point_ops::PointOperations::{DeletePoints, UpsertPoints};
use shard::operations::point_ops::{BatchPersisted, BatchVectorStructPersisted, VectorPersisted};
use shard::query::query_enum::QueryEnum;
use shard::query::{ScoringQuery, ShardQueryRequest};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::{Span, instrument, warn};
use uuid::Uuid;

use cognee_utils::tracing_keys::{
    COGNEE_DB_ROW_COUNT, COGNEE_VECTOR_COLLECTION, COGNEE_VECTOR_RESULT_COUNT,
};

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

        if let Err(e) = adapter.load_existing_shards() {
            warn!("Warning: Failed to load existing shards: {}", e);
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
                    match self.get_or_create_shard(&collection, self.dimension) {
                        Ok(_) => {
                            loaded_count += 1;
                        }
                        Err(e) => {
                            warn!("Warning: Failed to load shard '{}': {}", collection, e);
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
        {
            let shards = self.shards.read().unwrap(); // lock poison is unrecoverable
            if let Some(shard) = shards.get(collection) {
                return Ok(shard.clone());
            }
        }

        let shard_path = self.data_dir.join(collection);
        std::fs::create_dir_all(&shard_path)?;

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

        let shard = Arc::new(
            EdgeShard::load(&shard_path, Some(config))
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?,
        );

        let mut shards = self.shards.write().unwrap(); // lock poison is unrecoverable
        shards.insert(collection.to_string(), shard.clone());

        Ok(shard)
    }

    /// Convert vector points to BatchPersisted (more efficient than PointsList)
    fn points_to_batch(points: &[VectorPoint]) -> BatchPersisted {
        let ids: Vec<ExtendedPointId> =
            points.iter().map(|p| ExtendedPointId::Uuid(p.id)).collect();

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

        if self.has_collection(data_type, field_name).await? {
            return Err(VectorDBError::CollectionExists(collection));
        }

        self.get_or_create_shard(&collection, dimension)?;

        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let collection = Self::collection_name(data_type, field_name);

        {
            let shards = self.shards.read().unwrap(); // lock poison is unrecoverable
            if shards.contains_key(&collection) {
                return Ok(true);
            }
        }

        let shard_path = self.data_dir.join(&collection);
        Ok(shard_path.exists() && shard_path.is_dir())
    }

    #[instrument(
        name = "cognee.db.vector.upsert",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "qdrant",
            cognee.vector.collection = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
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
        Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

        let expected_dim = points[0].vector.len();
        for point in points {
            if point.vector.len() != expected_dim {
                return Err(VectorDBError::DimensionMismatch {
                    expected: expected_dim,
                    actual: point.vector.len(),
                });
            }
        }

        let shard = self.get_or_create_shard(&collection, expected_dim)?;

        let batch = Self::points_to_batch(points);

        shard
            .update(PointOperation(UpsertPoints(PointsBatch(batch))))
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Span::current().record(COGNEE_DB_ROW_COUNT, points.len() as i64);
        Ok(())
    }

    #[instrument(
        name = "cognee.db.vector.search",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "qdrant",
            cognee.vector.collection = tracing::field::Empty,
            cognee.vector.result_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let collection = Self::collection_name(data_type, field_name);
        Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

        let shard = self.get_or_create_shard(&collection, self.dimension)?;

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

        let mapped: Vec<SearchResult> = results.iter().map(Self::from_qdrant_result).collect();
        Span::current().record(COGNEE_VECTOR_RESULT_COUNT, mapped.len() as i64);
        Ok(mapped)
    }

    #[instrument(
        name = "cognee.db.vector.delete_collection",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "qdrant",
            cognee.vector.collection = tracing::field::Empty,
        ),
        err,
    )]
    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        let collection = Self::collection_name(data_type, field_name);
        Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

        let mut shards = self.shards.write().unwrap(); // lock poison is unrecoverable

        shards.remove(&collection);

        let shard_path = self.data_dir.join(&collection);
        if shard_path.exists() {
            std::fs::remove_dir_all(&shard_path)?;
        }

        Ok(())
    }

    #[instrument(
        name = "cognee.db.vector.delete",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "qdrant",
            cognee.vector.collection = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn delete_points(
        &self,
        data_type: &str,
        field_name: &str,
        point_ids: &[Uuid],
    ) -> VectorDBResult<()> {
        if point_ids.is_empty() {
            return Ok(());
        }

        let collection = Self::collection_name(data_type, field_name);
        Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

        let shard = self.get_or_create_shard(&collection, self.dimension)?;

        let ids: Vec<ExtendedPointId> = point_ids
            .iter()
            .map(|id| ExtendedPointId::Uuid(*id))
            .collect();

        shard
            .update(PointOperation(DeletePoints { ids }))
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Span::current().record(COGNEE_DB_ROW_COUNT, point_ids.len() as i64);
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

    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        // Prefer the in-memory shard map (already loaded), then fall back to
        // the filesystem for any shards that were not preloaded.
        let collection_names: Vec<String> = {
            let shards = self.shards.read().unwrap(); // lock poison is unrecoverable
            shards.keys().cloned().collect()
        };

        // Also scan the data directory for on-disk shards not yet in memory.
        let mut all_names: std::collections::HashSet<String> =
            collection_names.into_iter().collect();

        if self.data_dir.exists()
            && let Ok(entries) = std::fs::read_dir(&self.data_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir()
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                {
                    all_names.insert(name.to_string());
                }
            }
        }

        // Parse "{data_type}_{field_name}" by splitting on the first '_'
        let pairs = all_names
            .into_iter()
            .filter_map(|name| {
                name.split_once('_')
                    .map(|(dt, fn_)| (dt.to_string(), fn_.to_string()))
            })
            .collect();

        Ok(pairs)
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
    async fn test_delete_points_removes_from_search_and_count() {
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 2);

        adapter.create_collection("Test", "field", 2).await.unwrap();

        let deleted_id = Uuid::new_v4();
        let kept_id = Uuid::new_v4();
        let points = vec![
            VectorPoint::new(deleted_id, vec![1.0, 0.0]).with_metadata("name", json!("deleted")),
            VectorPoint::new(kept_id, vec![0.0, 1.0]).with_metadata("name", json!("kept")),
        ];

        adapter
            .index_points("Test", "field", &points)
            .await
            .unwrap();
        adapter
            .delete_points("Test", "field", &[deleted_id])
            .await
            .unwrap();

        let size = adapter.collection_size("Test", "field").await.unwrap();
        assert_eq!(size, 1);

        let results = adapter
            .search_similar("Test", "field", &[1.0, 0.0], 10)
            .await
            .unwrap();
        let ids: Vec<_> = results.into_iter().map(|result| result.id).collect();
        assert!(!ids.contains(&deleted_id));
        assert!(ids.contains(&kept_id));
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
    async fn test_uuid_round_trip_preserves_full_128_bits() {
        // Regression test: storing a UUID as NumId(uuid.as_u128() as u64) truncated the
        // upper 64 bits, causing the returned ID to differ from the stored ID.
        // The fix is to use ExtendedPointId::Uuid which preserves all 128 bits.
        let temp_dir = TempDir::new().unwrap();
        let adapter = QdrantAdapter::new(temp_dir.path().to_path_buf(), 2);

        adapter.create_collection("Test", "field", 2).await.unwrap();

        // Use a UUID with significant bits in the upper half
        let stored_id = Uuid::parse_str("f7ab8d87-553f-4509-b595-463cedc998be").unwrap();
        let points = vec![VectorPoint::new(stored_id, vec![1.0, 0.0])];

        adapter
            .index_points("Test", "field", &points)
            .await
            .unwrap();

        let results = adapter
            .search_similar("Test", "field", &[1.0, 0.0], 1)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].id, stored_id,
            "UUID round-trip must preserve all 128 bits; got {:?} expected {:?}",
            results[0].id, stored_id
        );
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
