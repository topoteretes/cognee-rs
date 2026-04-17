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

#[cfg(all(test, feature = "testing"))]
mod tests {
    use super::*;
    use cognee_embedding::MockEmbeddingEngine;
    use cognee_models::Triplet;
    use cognee_vector::MockVectorDB;

    fn make_triplet(src_name: &str, tgt_name: &str, rel: &str) -> Triplet {
        let src_id = Uuid::new_v4();
        let tgt_id = Uuid::new_v4();
        let text = format!("{src_name} -\u{203a} {rel}-\u{203a}{tgt_name}");
        Triplet::new(src_id, tgt_id, rel.to_string(), text)
            .with_names(src_name.to_string(), tgt_name.to_string())
    }

    #[tokio::test]
    async fn test_index_empty_triplets() {
        let vector_db = MockVectorDB::new();
        let engine = MockEmbeddingEngine::new(4);

        let result = index_triplets(&[], &vector_db, &engine, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.indexed_count, 0);
        assert_eq!(result.batch_count, 0);
        // No collection should have been created
        assert!(
            !vector_db.has_collection("Triplet", "text").await.unwrap(),
            "collection should not be created for empty input"
        );
    }

    #[tokio::test]
    async fn test_index_creates_collection() {
        let vector_db = MockVectorDB::new();
        let engine = MockEmbeddingEngine::new(4);
        let triplets = vec![make_triplet("Alice", "Bob", "knows")];

        let result = index_triplets(&triplets, &vector_db, &engine, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.indexed_count, 1);
        assert_eq!(result.batch_count, 1);
        assert!(
            vector_db.has_collection("Triplet", "text").await.unwrap(),
            "Triplet:text collection should be created"
        );
        assert_eq!(
            vector_db.collection_size("Triplet", "text").await.unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn test_index_metadata_fields() {
        let vector_db = MockVectorDB::new();
        let engine = MockEmbeddingEngine::new(4);
        let dataset_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let triplet = make_triplet("Src", "Tgt", "rel");
        let triplet_id = triplet.id;
        let source_id = triplet.source_entity_id;
        let target_id = triplet.target_entity_id;

        index_triplets(
            &[triplet],
            &vector_db,
            &engine,
            Some(dataset_id),
            Some(user_id),
            Some(tenant_id),
        )
        .await
        .unwrap();

        // Retrieve the point via search (cosine with zero vector returns all points equally)
        let results = vector_db
            .search_similar("Triplet", "text", &[0.0; 4], 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        let meta = &results[0].metadata;
        assert_eq!(results[0].id, triplet_id);
        assert_eq!(meta.get("type").unwrap(), &json!("Triplet"));
        assert_eq!(meta.get("field").unwrap(), &json!("text"));
        assert_eq!(
            meta.get("source_id").unwrap(),
            &json!(source_id.to_string())
        );
        assert_eq!(
            meta.get("target_id").unwrap(),
            &json!(target_id.to_string())
        );
        assert_eq!(meta.get("relationship").unwrap(), &json!("rel"));
        assert_eq!(
            meta.get("dataset_id").unwrap(),
            &json!(dataset_id.to_string())
        );
        assert_eq!(meta.get("user_id").unwrap(), &json!(user_id.to_string()));
        assert_eq!(
            meta.get("tenant_id").unwrap(),
            &json!(tenant_id.to_string())
        );
    }

    #[tokio::test]
    async fn test_index_batching() {
        let vector_db = MockVectorDB::new();
        // batch_size=2, so 5 triplets should produce 3 batches
        let engine = MockEmbeddingEngine::with_batch_size(4, 2);

        let triplets: Vec<Triplet> = (0..5)
            .map(|i| make_triplet(&format!("S{i}"), &format!("T{i}"), "rel"))
            .collect();

        let result = index_triplets(&triplets, &vector_db, &engine, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.indexed_count, 5);
        assert_eq!(result.batch_count, 3); // ceil(5/2) = 3
        assert_eq!(
            vector_db.collection_size("Triplet", "text").await.unwrap(),
            5
        );
    }

    #[tokio::test]
    async fn test_index_existing_collection_no_recreate() {
        let vector_db = MockVectorDB::new();
        let engine = MockEmbeddingEngine::new(4);

        // Pre-create the collection; index_triplets must see it via has_collection
        // and skip create_collection entirely.
        vector_db
            .create_collection("Triplet", "text", engine.dimension())
            .await
            .unwrap();
        assert_eq!(vector_db.create_collection_count(), 1);

        let triplets = vec![make_triplet("Alice", "Bob", "knows")];
        index_triplets(&triplets, &vector_db, &engine, None, None, None)
            .await
            .unwrap();

        // Exactly one create_collection invocation total (the manual one above).
        assert_eq!(
            vector_db.create_collection_count(),
            1,
            "index_triplets must not recreate an existing collection"
        );
        assert!(vector_db.was_create_collection_called("Triplet", "text"));
    }

    #[tokio::test]
    async fn test_index_metadata_values_match_triplet() {
        let vector_db = MockVectorDB::new();
        let engine = MockEmbeddingEngine::new(4);
        let dataset_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();

        // Build 3 triplets with distinct source/target/relationship values.
        let triplets: Vec<Triplet> = vec![
            make_triplet("Alice", "Bob", "knows"),
            make_triplet("Bob", "Charlie", "mentors"),
            make_triplet("Charlie", "Dana", "manages"),
        ];
        // Snapshot field values for later exact-value comparison.
        let expected: Vec<(Uuid, Uuid, Uuid, String)> = triplets
            .iter()
            .map(|t| {
                (
                    t.id,
                    t.source_entity_id,
                    t.target_entity_id,
                    t.relationship_name.clone(),
                )
            })
            .collect();

        index_triplets(
            &triplets,
            &vector_db,
            &engine,
            Some(dataset_id),
            Some(user_id),
            Some(tenant_id),
        )
        .await
        .unwrap();

        // Fetch all points via search.
        let results = vector_db
            .search_similar("Triplet", "text", &[0.0; 4], 10)
            .await
            .unwrap();
        assert_eq!(results.len(), expected.len());

        for (id, source_id, target_id, relationship) in expected {
            let point = results
                .iter()
                .find(|r| r.id == id)
                .expect("every indexed triplet must produce a point with a matching id");
            let meta = &point.metadata;
            assert_eq!(meta.get("type").unwrap(), &json!("Triplet"));
            assert_eq!(meta.get("field").unwrap(), &json!("text"));
            assert_eq!(
                meta.get("source_id").unwrap(),
                &json!(source_id.to_string())
            );
            assert_eq!(
                meta.get("target_id").unwrap(),
                &json!(target_id.to_string())
            );
            assert_eq!(meta.get("relationship").unwrap(), &json!(relationship));
            assert_eq!(
                meta.get("dataset_id").unwrap(),
                &json!(dataset_id.to_string())
            );
            assert_eq!(meta.get("user_id").unwrap(), &json!(user_id.to_string()));
            assert_eq!(
                meta.get("tenant_id").unwrap(),
                &json!(tenant_id.to_string())
            );
        }
    }

    #[tokio::test]
    async fn test_index_large_batch_multiple_requests() {
        let vector_db = MockVectorDB::new();
        // batch_size=100 paired with 1000 triplets → exactly 10 batches.
        let engine = MockEmbeddingEngine::with_batch_size(4, 100);

        let triplets: Vec<Triplet> = (0..1000)
            .map(|i| {
                let src_id = Uuid::new_v4();
                let tgt_id = Uuid::new_v4();
                let src_name = format!("S{i}");
                let tgt_name = format!("T{i}");
                let text = format!("{src_name} -\u{203a} rel-\u{203a}{tgt_name}");
                Triplet::new(src_id, tgt_id, "rel".to_string(), text).with_names(src_name, tgt_name)
            })
            .collect();

        let result = index_triplets(&triplets, &vector_db, &engine, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.indexed_count, 1000);
        assert_eq!(result.batch_count, 10);
        assert_eq!(
            vector_db.index_points_call_count(),
            10,
            "exactly 10 index_points calls expected for 1000/100 batches"
        );
    }

    #[tokio::test]
    async fn test_index_embedding_error_propagates() {
        let vector_db = MockVectorDB::new();
        let engine = MockEmbeddingEngine::new(4);
        engine.set_failure_after(0); // fail on the very first embed call

        let triplets = vec![make_triplet("Alice", "Bob", "knows")];
        let err = index_triplets(&triplets, &vector_db, &engine, None, None, None)
            .await
            .expect_err("embedding failure must propagate as MemifyError");

        match err {
            MemifyError::EmbeddingError(msg) => {
                assert!(
                    msg.contains("injected failure"),
                    "error message should preserve embedding-engine context, got: {msg}"
                );
            }
            other => panic!("expected MemifyError::EmbeddingError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_index_vector_db_error_propagates() {
        let vector_db = MockVectorDB::new();
        let engine = MockEmbeddingEngine::new(4);
        vector_db.set_index_error("boom");

        let triplets = vec![make_triplet("Alice", "Bob", "knows")];
        let err = index_triplets(&triplets, &vector_db, &engine, None, None, None)
            .await
            .expect_err("vector-db failure must propagate as MemifyError");

        match err {
            MemifyError::VectorDBError(msg) => {
                assert!(
                    msg.contains("boom"),
                    "error message should contain injected text, got: {msg}"
                );
            }
            other => panic!("expected MemifyError::VectorDBError, got {other:?}"),
        }
    }
}
