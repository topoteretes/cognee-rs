use cognee_embedding::EmbeddingEngine;
use cognee_models::Triplet;
use cognee_vector::{VectorDB, VectorPoint};
use serde_json::json;
use tracing::info;
use uuid::Uuid;

use super::error::MemifyError;

/// Result of indexing triplets.
#[derive(Debug, Clone)]
pub struct IndexResult {
    /// Number of triplets embedded and indexed.
    pub indexed_count: usize,
    /// Number of embedding batches processed.
    pub batch_count: usize,
}

/// Embed and index triplets into the vector database.
///
/// Creates/ensures the "Triplet"/"text" collection, embeds triplet texts
/// in batches (using `EmbeddingEngine::batch_size()`), and upserts vectors.
///
/// Metadata on each `VectorPoint` matches the existing cognify triplet
/// indexing (tasks.rs) so that `TripletRetriever` works with triplets from
/// both cognify and memify.
pub async fn index_triplets(
    triplets: &[Triplet],
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
) -> Result<IndexResult, MemifyError> {
    if triplets.is_empty() {
        return Ok(IndexResult {
            indexed_count: 0,
            batch_count: 0,
        });
    }

    let dimension = embedding_engine.dimension();

    // Ensure collection exists (idempotent).
    // Matches cognify: tasks.rs collection creation pattern.
    if !vector_db
        .has_collection("Triplet", "text")
        .await
        .map_err(|e| MemifyError::VectorDBError(e.to_string()))?
    {
        vector_db
            .create_collection("Triplet", "text", dimension)
            .await
            .map_err(|e| MemifyError::VectorDBError(e.to_string()))?;
    }

    // Batch by embedding engine's preferred batch size.
    // Python: batch_size = vector_engine.embedding_engine.get_batch_size()
    // Rust: EmbeddingEngine::batch_size() returns the same value per provider.
    let batch_size = embedding_engine.batch_size();
    let mut indexed_count = 0;
    let mut batch_count = 0;

    for chunk in triplets.chunks(batch_size) {
        let texts: Vec<&str> = chunk.iter().map(|t| t.text.as_str()).collect();

        let vectors = embedding_engine
            .embed(&texts)
            .await
            .map_err(|e| MemifyError::EmbeddingError(e.to_string()))?;

        let points: Vec<VectorPoint> = chunk
            .iter()
            .zip(vectors)
            .map(|(triplet, vector)| {
                // Metadata MUST match cognify's add_data_points() so
                // TripletRetriever works consistently.
                let mut point = VectorPoint::new(triplet.id, vector)
                    .with_metadata("type", json!("Triplet"))
                    .with_metadata("field", json!("text"))
                    .with_metadata("source_id", json!(triplet.source_entity_id.to_string()))
                    .with_metadata("target_id", json!(triplet.target_entity_id.to_string()))
                    .with_metadata("relationship", json!(triplet.relationship_name.clone()));

                // Additional metadata for multi-tenant support.
                if let Some(did) = dataset_id {
                    point = point.with_metadata("dataset_id", json!(did.to_string()));
                }
                if let Some(uid) = user_id {
                    point = point.with_metadata("user_id", json!(uid.to_string()));
                }
                if let Some(tid) = tenant_id {
                    point = point.with_metadata("tenant_id", json!(tid.to_string()));
                }

                point
            })
            .collect();

        vector_db
            .index_points("Triplet", "text", &points)
            .await
            .map_err(|e| MemifyError::VectorDBError(e.to_string()))?;

        batch_count += 1;
        indexed_count += chunk.len();

        info!(
            batch = batch_count,
            indexed = indexed_count,
            total = triplets.len(),
            "Indexed triplet batch"
        );
    }

    Ok(IndexResult {
        indexed_count,
        batch_count,
    })
}
