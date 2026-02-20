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
}
