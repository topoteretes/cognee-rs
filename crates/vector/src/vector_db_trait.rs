use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::node_filter::metadata_matches_node_filter;
use async_trait::async_trait;
use uuid::Uuid;

/// Upper bound on the over-fetch window used by the *default* (client-side)
/// [`VectorDB::search_similar_filtered`] fallback.
///
/// The default fallback cannot filter inside the engine, so it over-fetches by
/// pure similarity and drops out-of-set rows afterwards (limit-then-filter).
/// Widening the fetch to the whole collection whenever it fits under this cap
/// makes that limit-then-filter *exactly* equal to a server-side
/// filter-then-limit (the window can no longer be exhausted by out-of-set rows),
/// so the fallback is exact for any collection at or below the cap. Only
/// collections above the cap keep a bounded heuristic window and a residual
/// recall gap. Adapters that override `search_similar_filtered` with a real
/// server-side predicate (in-memory scan, pgvector JSONB `WHERE`) never touch
/// this constant and are exact at any size.
pub const NODE_FILTER_RECALL_FETCH_CAP: usize = 4096;

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

    /// Search for similar vectors, scoped to rows whose `belongs_to_set`
    /// membership satisfies a NodeSet filter (finding F9's server-side
    /// filter-then-limit).
    ///
    /// # Arguments
    /// * `data_type` / `field_name` / `query_vector` / `top_k` — as
    ///   [`search_similar`](Self::search_similar).
    /// * `node_name` — requested NodeSet names. `None`/empty means "no filter",
    ///   in which case this is exactly [`search_similar`](Self::search_similar).
    /// * `node_name_filter_operator` — `"AND"` (requested ⊆ row's set) or
    ///   anything else (`"OR"`, non-empty intersection); see
    ///   [`crate::node_filter`] for the full membership semantics.
    ///
    /// Direct port of the `node_name` argument Python threads into
    /// `vector_engine.search(...)`, which filters **inside the engine before
    /// applying the limit** so every returned row is in-set and no valid in-set
    /// row is ever crowded out by higher-similarity out-of-set rows.
    ///
    /// # Default implementation (bounded, client-side)
    /// The provided default cannot push the predicate into the engine, so it
    /// over-fetches by pure similarity, drops out-of-set rows via
    /// [`crate::node_filter::metadata_matches_node_filter`], then truncates to
    /// `top_k` (limit-then-filter). It widens the fetch to the whole collection
    /// whenever that fits under [`NODE_FILTER_RECALL_FETCH_CAP`], making it
    /// **exact** at or below the cap and only bounded above it. This is the
    /// correct behavior for engines that cannot express the nested-array
    /// membership predicate (e.g. the LanceDB adapter, whose `metadata` is an
    /// opaque JSON string) and for the in-tree test doubles, which inherit it
    /// unchanged.
    ///
    /// Adapters that *can* filter server-side (the in-memory scanners and
    /// pgvector's JSONB `WHERE`) override this with an **exact** filter-then-limit
    /// at any collection size.
    async fn search_similar_filtered(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
        node_name: Option<&[String]>,
        node_name_filter_operator: &str,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let requested = match node_name {
            Some(names) if !names.is_empty() => names,
            // No filter requested — identical to plain search_similar.
            _ => {
                return self
                    .search_similar(data_type, field_name, query_vector, top_k)
                    .await;
            }
        };

        // Limit-then-filter with a bounded over-fetch. Widen the window to the
        // whole collection when it fits under the cap so dropping out-of-set
        // rows and truncating to `top_k` reproduces server-side
        // filter-then-limit exactly; only collections above the cap keep the
        // bounded heuristic window and its residual recall gap.
        let heuristic_window = top_k.saturating_mul(4).max(top_k + 20);
        let collection_size = self.collection_size(data_type, field_name).await?;
        let fetch_limit = heuristic_window.max(collection_size.min(NODE_FILTER_RECALL_FETCH_CAP));
        let results = self
            .search_similar(data_type, field_name, query_vector, fetch_limit)
            .await?;
        Ok(results
            .into_iter()
            .filter(|r| {
                metadata_matches_node_filter(
                    &r.metadata,
                    Some(requested),
                    node_name_filter_operator,
                )
            })
            .take(top_k)
            .collect())
    }

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

    /// Upsert caller-provided vectors into `(data_type, field_name)` without
    /// invoking any embedding engine.
    ///
    /// This is the escape hatch for **small, system-owned vector state**
    /// (e.g. truth-subspace centroids) where the caller has *already* computed
    /// the vector and re-embedding it from text would be wrong. It is NOT the
    /// content-indexing path — use [`index_points`](Self::index_points) for
    /// content-addressed data points.
    ///
    /// Direct port of Python's `VectorDBInterface.upsert_raw_vectors`
    /// (`vector_db_interface.py:66-79`), whose base likewise
    /// `raise NotImplementedError`. Only the four real adapters (Mock,
    /// BruteForce, LanceDB, PgVector) override it; every other implementor
    /// inherits this error-returning default (the correct "unsupported" answer).
    ///
    /// # Semantics (for overriding adapters)
    /// * **Empty `points`** → `Ok(())` no-op (never touches storage; do not
    ///   read `points[0]`).
    /// * **Missing collection** → self-created with `points[0].vector.len()` as
    ///   the dimension (nothing else ever creates a system-owned collection like
    ///   `TruthCentroid_vector`, so the raw-upsert path must bootstrap it).
    /// * **By-id insert-or-replace with FULL metadata replace** — unlike
    ///   [`index_points`](Self::index_points), this does **not** union
    ///   `dataset_ids`/`dataset_id` membership from a prior point at the same id.
    ///   Each raw point is written verbatim (its id already scopes it), matching
    ///   Python's raw write.
    ///
    /// # API divergence from Python
    /// Python threads a `payload_schema: Optional[Any]` argument for
    /// provider-side schema declaration. Rust has no adapter-level runtime schema
    /// hook — validation happens where the caller deserializes the retrieved
    /// metadata — so the parameter is dropped entirely rather than threaded
    /// through and ignored.
    async fn upsert_raw_vectors(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        let _ = (data_type, field_name, points);
        Err(VectorDBError::StorageError(
            "upsert_raw_vectors is not implemented for this adapter".to_string(),
        ))
    }

    /// Fetch stored points by ID (direct lookup, no similarity search).
    ///
    /// Returns the stored `metadata` payload for each of the requested `ids`
    /// that exists in the `(data_type, field_name)` collection, with a
    /// placeholder `score` of `0.0` on every result (the field only carries
    /// meaning for similarity search, so callers must not read it as a
    /// similarity value). Direct port of Python's `VectorDBInterface.retrieve`.
    ///
    /// # Semantics
    /// * **Empty `ids`** → `Ok(vec![])` without touching storage.
    /// * **Unknown collection** → `Ok(vec![])` (a *deliberate* divergence from
    ///   [`search_similar`](Self::search_similar) /
    ///   [`delete_points`](Self::delete_points) /
    ///   [`collection_size`](Self::collection_size), which return
    ///   `CollectionNotFound`; faithful to Python, whose adapters special-case
    ///   a missing collection to `[]`).
    /// * **IDs not present** in the collection are silently absent from the
    ///   result — no error, no placeholder entry.
    /// * **Result order is NOT guaranteed** to match input-`ids` order; callers
    ///   needing a specific order must re-index by [`SearchResult::id`].
    async fn retrieve(
        &self,
        data_type: &str,
        field_name: &str,
        ids: &[Uuid],
    ) -> VectorDBResult<Vec<SearchResult>>;

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

    /// Release the OS resources this store owns, instead of waiting for `Drop`.
    ///
    /// The vector-store twin of `cognee_graph::GraphDBTrait::close`; see that
    /// method for the full mechanism. In short: a `Drop` is not a close. The
    /// pgvector adapter owns its **own** sqlx pool, and dropping a pool only
    /// flags it closed and lets each connection tear down on an arbitrary
    /// thread, so the server-side backends stay open long after the last `Arc`
    /// is gone.
    ///
    /// Contract:
    /// - **Idempotent.** Calling it twice is a no-op the second time.
    /// - **Safe to call while other `Arc` clones are alive.** Surviving clones
    ///   fail their next operation with a "closed" error rather than silently
    ///   reconnecting.
    /// - **Post-close operations fail.** Deliberate and user-visible.
    /// - The **default body is a no-op**, meaning "this backend owns nothing
    ///   closable beyond memory". That is the *measured* truth for the in-memory
    ///   brute-force store and for LanceDB (which holds no descriptor open
    ///   between calls), so neither overrides it. An adapter that does own OS
    ///   resources must override it, or it will leak invisibly.
    async fn close(&self) -> VectorDBResult<()> {
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
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;
    use crate::mock_vector_db::MockVectorDB;

    /// The defaulted `close()` is a no-op and idempotent for a store that owns
    /// nothing closable — the measured case for the in-memory brute-force store
    /// and for LanceDB, and what keeps every existing impl compiling.
    #[tokio::test]
    async fn default_close_is_a_noop_and_idempotent() {
        let db = MockVectorDB::new();
        db.create_collection("TestType", "field", 3).await.unwrap();
        assert!(db.close().await.is_ok());
        assert!(db.close().await.is_ok());
        assert!(db.has_collection("TestType", "field").await.unwrap());
    }

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
