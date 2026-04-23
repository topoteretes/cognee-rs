use crate::error::VectorDBResult;
use crate::models::{SearchResult, VectorPoint};
use async_trait::async_trait;
use uuid::Uuid;

/// Vector database trait
#[async_trait]
pub trait VectorDB: Send + Sync {
    /// Create a collection for (data_type, field_name) pair
    ///
    /// # Arguments
    /// * `data_type` - Type name (e.g., "DocumentChunk", "Entity")
    /// * `field_name` - Field name (e.g., "text", "name")
    /// * `dimension` - Vector dimension (e.g., 384 for MiniLM)
    ///
    /// # Example
    /// ```ignore
    /// vector_db.create_collection("DocumentChunk", "text", 384).await?;
    /// ```
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()>;

    /// Check if collection exists
    ///
    /// # Arguments
    /// * `data_type` - Type name
    /// * `field_name` - Field name
    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool>;

    /// Index data points (batch upsert with embeddings already generated)
    ///
    /// # Arguments
    /// * `data_type` - Type name
    /// * `field_name` - Field name
    /// * `points` - Vector points with embeddings
    ///
    /// # Example
    /// ```ignore
    /// let points = vec![
    ///     VectorPoint::new(chunk_id, embedding)
    ///         .with_metadata("type", json!("DocumentChunk"))
    ///         .with_metadata("field", json!("text")),
    /// ];
    /// vector_db.index_points("DocumentChunk", "text", &points).await?;
    /// ```
    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()>;

    /// Search for similar vectors
    ///
    /// # Arguments
    /// * `data_type` - Type name
    /// * `field_name` - Field name
    /// * `query_vector` - Query embedding vector
    /// * `top_k` - Number of results to return
    ///
    /// # Returns
    /// Vector of search results sorted by similarity (descending)
    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>>;

    /// Delete collection
    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()>;

    /// Delete points by IDs from an existing collection.
    async fn delete_points(
        &self,
        data_type: &str,
        field_name: &str,
        point_ids: &[Uuid],
    ) -> VectorDBResult<()> {
        let _ = (data_type, field_name, point_ids);
        Ok(())
    }

    /// Get collection statistics
    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize>;

    /// List all existing vector collections as `(data_type, field_name)` pairs.
    ///
    /// Default implementation returns an empty list. Backends should override
    /// to return the actual collections they hold.
    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        Ok(vec![])
    }

    /// Remove all vector collections.
    ///
    /// Default implementation lists all collections and deletes each one.
    /// Backends may override with a more efficient bulk operation.
    ///
    /// Equivalent to Python's `vector_engine.prune()`.
    async fn prune(&self) -> VectorDBResult<()> {
        let collections = self.list_collections().await?;
        for (data_type, field_name) in collections {
            self.delete_collection(&data_type, &field_name).await?;
        }
        Ok(())
    }

    /// Perform multiple vector similarity searches in sequence.
    ///
    /// Default implementation loops over [`search_similar`]. Backends may override
    /// this with a native batch API for better performance.
    async fn batch_search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vectors: &[Vec<f32>],
        top_k: usize,
    ) -> VectorDBResult<Vec<Vec<SearchResult>>> {
        let mut results = Vec::with_capacity(query_vectors.len());
        for query_vector in query_vectors {
            results.push(
                self.search_similar(data_type, field_name, query_vector, top_k)
                    .await?,
            );
        }
        Ok(results)
    }
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use super::*;
    use crate::mock_vector_db::MockVectorDB;

    #[tokio::test]
    async fn batch_search_similar_returns_one_result_per_query() {
        let db = MockVectorDB::new();
        db.create_collection("TestType", "field", 3).await.unwrap();

        // No points indexed — each search returns an empty Vec.
        let query_vectors = vec![vec![1.0_f32, 0.0, 0.0], vec![0.0_f32, 1.0, 0.0]];

        let results = db
            .batch_search_similar("TestType", "field", &query_vectors, 5)
            .await
            .unwrap();

        assert_eq!(results.len(), 2, "one result set per query vector");
        assert!(results[0].is_empty(), "no indexed points → empty result");
        assert!(results[1].is_empty(), "no indexed points → empty result");
    }
}
