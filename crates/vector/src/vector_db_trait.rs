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

/// What a [`VectorDB::create_missing_vector_indexes`] pass actually did.
///
/// `built` and `failed` rather than one count, because `built == 0` alone is
/// ambiguous: it is what a fully-indexed store reports and equally what a store
/// reports when every single build failed. An operator running the only repair
/// path there is needs those told apart. `dropped` is the third thing a pass
/// can do — reclaim an index the backend has stopped reading — and it is
/// neither a build nor a failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VectorIndexBackfill {
    /// Indexes actually built by this call. Excludes collections that already
    /// had a usable one.
    pub built: usize,
    /// Collections that needed an index and did not get one. Each was logged
    /// at `warn` with its reason and the pass continued past it.
    pub failed: usize,
    /// Superseded indexes this call removed — a backend that changed its index
    /// *shape* leaves the old one behind under its old name, where it is never
    /// read again but is still maintained on every write.
    ///
    /// Reported separately from `built` because it is not a repair: a store
    /// with none of these is not missing anything. It is what tells an operator
    /// that the pass reclaimed something, so `built == 0 && dropped == 0` means
    /// "nothing to do" and `built == 0 && dropped > 0` means "already indexed,
    /// and the dead weight is gone now".
    ///
    /// Zero for every backend but pgvector, which drops `<coll>_vector_hnsw`
    /// once `<coll>_halfvec_hnsw` is confirmed valid.
    pub dropped: usize,
}

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
    /// - **Post-close operations fail — for backends that actually close
    ///   something.** Deliberate and user-visible. It does *not* bind an
    ///   implementor whose `close` is the no-op default below (brute-force, mock,
    ///   LanceDB): with nothing to release there is nothing to invalidate, so such
    ///   a backend keeps serving — which is what the unit test below asserts.
    /// - The **default body is a no-op**, meaning "this backend owns nothing
    ///   closable beyond memory". That is the *measured* truth for the in-memory
    ///   brute-force store and for LanceDB (which holds no descriptor open
    ///   between calls), so neither overrides it. An adapter that does own OS
    ///   resources must override it, or it will leak invisibly.
    async fn close(&self) -> VectorDBResult<()> {
        Ok(())
    }

    /// Build the backend's approximate-nearest-neighbour index on every
    /// collection that does not have a usable one yet, and report how many were
    /// built.
    ///
    /// The operator entry point for a backend whose index is created alongside
    /// the collection rather than by a migration, so collections can be left
    /// unindexed in two ways: they predate the index existing at all, or their
    /// `create_collection` built the table and then failed to build the index
    /// (the pgvector adapter logs and continues in that case, rather than
    /// leaving a created-but-unregistered table behind). Either way the
    /// collection still answers every query — correctly, by sequential scan —
    /// so nothing surfaces the gap except latency, and nothing repairs it
    /// except this call.
    ///
    /// Contract for an implementor:
    /// - **Idempotent.** A collection that already has a usable index is
    ///   skipped and counted nowhere, so a second run reports all zeroes.
    /// - **Online.** It must not block reads or writes, and must not be
    ///   wrapped in a transaction by its caller — pgvector's implementation
    ///   issues `CREATE INDEX CONCURRENTLY`, which Postgres rejects inside one.
    /// - **Per-collection failures are logged and skipped**, not propagated, so
    ///   one bad entry cannot leave every collection after it unindexed. That
    ///   includes a malformed bookkeeping row, not just a failed build. Each
    ///   one is counted in [`VectorIndexBackfill::failed`], so a caller can
    ///   tell "nothing needed doing" from "nothing could be done" — the two
    ///   are indistinguishable from the built count alone, and conflating them
    ///   reports success to an operator whose only repair path just failed.
    /// - **Never automatic.** Building an ANN index over a large collection is
    ///   expensive; the caller chooses when.
    ///
    /// The **default body returns `Ok(0)`**, meaning "this backend has no such
    /// index to backfill" — the truth for the in-memory brute-force store
    /// (exact scan, no index), for the mock, and for LanceDB (which manages its
    /// own indexing). Only the pgvector adapter overrides it. A backend that
    /// grows a lazily-created index must override it too, or operators get no
    /// way to repair one.
    async fn create_missing_vector_indexes(&self) -> VectorDBResult<VectorIndexBackfill> {
        Ok(VectorIndexBackfill::default())
    }

    /// Tell the store a bulk load is starting: many `index_points` calls
    /// follow (one cognify run), and nothing needs the ANN index to be
    /// maintained row by row until the matching [`end_bulk_load`].
    ///
    /// Contract for an implementor:
    /// - **A hint, never a semantic change.** Every read and write inside the
    ///   scope must stay correct; a backend that defers index maintenance
    ///   serves searches by exact scan meanwhile.
    /// - **Nestable.** Scopes are counted; deferred work runs when the last
    ///   open scope ends.
    /// - **Closed exactly once per open scope**, by *one of* `end_bulk_load`
    ///   (the normal path, which also runs the deferred work) or
    ///   [`abandon_bulk_load`] (the `Drop` path, which only closes the scope).
    ///   What is *not* guaranteed is that `end_bulk_load` is reached at all: a
    ///   caller's future can be dropped mid-load — a client disconnect, a
    ///   `tokio::time::timeout`, a `select!` losing branch — and a `Drop` in an
    ///   async context cannot await. So an implementor must keep the scope
    ///   *depth* correct from the synchronous `abandon_bulk_load` alone, and
    ///   reconcile the deferred work later: at the next scope's end, in
    ///   [`close`](VectorDB::close), or — for work lost to a crash — through
    ///   [`create_missing_vector_indexes`]. [`BulkLoadGuard`] is the pairing;
    ///   callers should use it rather than the two methods directly.
    ///
    /// The default is a no-op.
    ///
    /// [`end_bulk_load`]: VectorDB::end_bulk_load
    /// [`abandon_bulk_load`]: VectorDB::abandon_bulk_load
    /// [`create_missing_vector_indexes`]: VectorDB::create_missing_vector_indexes
    async fn begin_bulk_load(&self) -> VectorDBResult<()> {
        Ok(())
    }

    /// End a scope opened by [`begin_bulk_load`](VectorDB::begin_bulk_load);
    /// when it was the last open one, run any deferred maintenance (index
    /// builds, statistics). The default is a no-op.
    ///
    /// An implementor must **close the scope before its first `await`**, so
    /// that the bookkeeping is done by the time this future can be dropped;
    /// [`BulkLoadGuard`] disarms itself the moment it calls this, and a
    /// deferred close would then be lost to a cancellation mid-maintenance.
    async fn end_bulk_load(&self) -> VectorDBResult<()> {
        Ok(())
    }

    /// Close a scope opened by [`begin_bulk_load`](VectorDB::begin_bulk_load)
    /// **without** running its deferred maintenance, synchronously.
    ///
    /// This is what [`BulkLoadGuard`]'s `Drop` calls, so it runs on the paths
    /// that never reach [`end_bulk_load`](VectorDB::end_bulk_load) — a dropped
    /// future or an unwinding panic. `Drop` cannot await, so an implementor
    /// must do only the bookkeeping here: bring the scope depth back down (the
    /// part whose loss is permanent — a depth stuck above zero makes every
    /// later load defer an index that is then never rebuilt) and leave the
    /// deferred work queued for the next `end_bulk_load`, for
    /// [`close`](VectorDB::close), or for
    /// [`create_missing_vector_indexes`](VectorDB::create_missing_vector_indexes).
    ///
    /// Must not block, must not panic, and must be exactly as nestable as
    /// `end_bulk_load`. The default is a no-op.
    fn abandon_bulk_load(&self) {}

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

/// An open bulk-load scope (see [`VectorDB::begin_bulk_load`]), closed exactly
/// once — by [`finish`](BulkLoadGuard::finish) on the way out, or by `Drop` on
/// every other way out.
///
/// # Why this is not two calls
/// `begin_bulk_load()` … `end_bulk_load()` written as plain sequential
/// statements around an `.await` is only paired when that await *returns*. A
/// dropped future — an axum client disconnect, a `tokio::time::timeout`, a
/// losing `tokio::select!` branch, Ctrl-C — and an unwinding panic both skip
/// the second call. For a backend that counts scopes, the scope depth then
/// stays above zero for the life of the process: the index this load dropped is
/// never rebuilt, every later collection that crosses the defer threshold loses
/// its index too, and the store degrades to exact scans with nothing logged.
///
/// `Drop` cannot await, so it does not run the deferred maintenance; it calls
/// the synchronous [`VectorDB::abandon_bulk_load`], which closes the scope and
/// leaves the work queued for the next scope's end, for
/// [`VectorDB::close`], or for
/// [`VectorDB::create_missing_vector_indexes`]. The invariant the guard
/// actually buys is therefore the one whose loss is unrecoverable: the depth
/// always comes back down.
///
/// # Example
/// ```ignore
/// let bulk = BulkLoadGuard::begin(vector_db.as_ref()).await?;
/// let outcome = run_the_load().await; // dropped here? the scope still closes
/// bulk.finish().await?;               // reached? the deferred work runs now
/// ```
#[must_use = "dropping the guard immediately closes the bulk-load scope again"]
pub struct BulkLoadGuard<'a> {
    db: &'a dyn VectorDB,
    /// `false` once the scope has been closed, so `Drop` does not close it a
    /// second time.
    open: bool,
}

impl<'a> BulkLoadGuard<'a> {
    /// Open a bulk-load scope on `db` and return the guard that closes it.
    ///
    /// An error means no scope was opened (and so there is nothing to close):
    /// the hint never changes results, so a caller may log it and carry on
    /// without one.
    pub async fn begin(db: &'a dyn VectorDB) -> VectorDBResult<Self> {
        db.begin_bulk_load().await?;
        Ok(Self { db, open: true })
    }

    /// Close the scope the normal way: run [`VectorDB::end_bulk_load`], which
    /// performs the deferred maintenance when this was the last open scope.
    ///
    /// Consumes the guard, so the scope cannot be closed twice, and disarms it
    /// before the call — `end_bulk_load` closes the scope before its own first
    /// await by contract.
    pub async fn finish(mut self) -> VectorDBResult<()> {
        self.open = false;
        self.db.end_bulk_load().await
    }
}

impl Drop for BulkLoadGuard<'_> {
    fn drop(&mut self) {
        if self.open {
            self.open = false;
            self.db.abandon_bulk_load();
        }
    }
}

/// Cases for [`BulkLoadGuard`]'s pairing, on `cfg(test)` alone so they run
/// under a plain `cargo test -p cognee-vector` with no features and no
/// Postgres: what they pin is the guard, not any backend's deferral policy.
/// The pgvector half — that an abandoned scope really leaves `depth` at zero
/// and that the next load still rebuilds its index — is
/// `a_dropped_bulk_load_guard_closes_the_scope_and_the_next_load_reindexes` in
/// `pgvector_index_tests`.
#[cfg(test)]
mod bulk_load_guard_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A store that records nothing but how its bulk-load scope was opened and
    /// closed. Every data method is unreachable: no test here moves a point.
    #[derive(Default)]
    struct ScopeRecorder {
        begun: AtomicUsize,
        ended: AtomicUsize,
        abandoned: AtomicUsize,
    }

    impl ScopeRecorder {
        /// `(begun, ended, abandoned)`.
        fn counts(&self) -> (usize, usize, usize) {
            (
                self.begun.load(Ordering::Relaxed),
                self.ended.load(Ordering::Relaxed),
                self.abandoned.load(Ordering::Relaxed),
            )
        }
    }

    #[async_trait]
    impl VectorDB for ScopeRecorder {
        async fn begin_bulk_load(&self) -> VectorDBResult<()> {
            self.begun.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        async fn end_bulk_load(&self) -> VectorDBResult<()> {
            self.ended.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn abandon_bulk_load(&self) {
            self.abandoned.fetch_add(1, Ordering::Relaxed);
        }

        async fn create_collection(&self, _: &str, _: &str, _: usize) -> VectorDBResult<()> {
            unimplemented!("the guard cases never touch data")
        }
        async fn has_collection(&self, _: &str, _: &str) -> VectorDBResult<bool> {
            unimplemented!("the guard cases never touch data")
        }
        async fn index_points(&self, _: &str, _: &str, _: &[VectorPoint]) -> VectorDBResult<()> {
            unimplemented!("the guard cases never touch data")
        }
        async fn search_similar(
            &self,
            _: &str,
            _: &str,
            _: &[f32],
            _: usize,
        ) -> VectorDBResult<Vec<SearchResult>> {
            unimplemented!("the guard cases never touch data")
        }
        async fn delete_collection(&self, _: &str, _: &str) -> VectorDBResult<()> {
            unimplemented!("the guard cases never touch data")
        }
        async fn retrieve(
            &self,
            _: &str,
            _: &str,
            _: &[Uuid],
        ) -> VectorDBResult<Vec<SearchResult>> {
            unimplemented!("the guard cases never touch data")
        }
        async fn collection_size(&self, _: &str, _: &str) -> VectorDBResult<usize> {
            unimplemented!("the guard cases never touch data")
        }
    }

    /// The happy path: `finish()` runs the deferred maintenance, and the
    /// guard it consumed must not then also abandon the scope — that would be
    /// one close too many and would unbalance a nested load.
    #[tokio::test]
    async fn finishing_the_guard_ends_the_scope_exactly_once() {
        let db = ScopeRecorder::default();
        let guard = BulkLoadGuard::begin(&db).await.unwrap();
        guard.finish().await.unwrap();
        assert_eq!(
            db.counts(),
            (1, 1, 0),
            "finish() must end the scope and never also abandon it"
        );
    }

    /// The bug this guard exists for: a load whose future never returns. The
    /// scope must still be closed — synchronously, since `Drop` cannot await —
    /// and `end_bulk_load` must *not* be claimed to have run.
    #[tokio::test]
    async fn dropping_the_guard_abandons_the_scope_instead_of_leaking_it() {
        let db = ScopeRecorder::default();
        {
            let _guard = BulkLoadGuard::begin(&db).await.unwrap();
            // The load would run here; this scope exit stands in for the
            // dropped future / unwinding panic.
        }
        assert_eq!(
            db.counts(),
            (1, 0, 1),
            "a dropped guard must close the scope via abandon_bulk_load, not \
             silently leave it open"
        );
    }

    /// The real cancellation shape: a future holding the guard across an
    /// `.await` that never resolves, dropped by `tokio::time::timeout`. This
    /// is the axum-disconnect / `select!` case, and the one a trailing
    /// `end_bulk_load()` statement skips entirely.
    #[tokio::test]
    async fn a_cancelled_load_future_still_closes_its_scope() {
        let db = ScopeRecorder::default();
        let load = async {
            let guard = BulkLoadGuard::begin(&db).await.unwrap();
            std::future::pending::<()>().await;
            guard.finish().await.unwrap();
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), load)
                .await
                .is_err(),
            "the load must be the one that timed out, or this pins nothing"
        );
        assert_eq!(
            db.counts(),
            (1, 0, 1),
            "the scope opened by a cancelled load must be closed by the guard's Drop"
        );
    }

    /// Scopes are counted, so the guards must nest: two opens, two closes, and
    /// a mixed pair (one finished, one dropped) still balances.
    #[tokio::test]
    async fn nested_guards_close_one_scope_each() {
        let db = ScopeRecorder::default();
        let outer = BulkLoadGuard::begin(&db).await.unwrap();
        {
            let _inner = BulkLoadGuard::begin(&db).await.unwrap();
        }
        outer.finish().await.unwrap();
        assert_eq!(
            db.counts(),
            (2, 1, 1),
            "each guard closes exactly its own scope, however it exits"
        );
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

/// Cases for the defaulted [`VectorDB::create_missing_vector_indexes`].
///
/// Deliberately gated on `cfg(test)` alone, not on `feature = "testing"` like
/// the module above: `BruteForceVectorDB` is always compiled, so these run
/// under a plain `cargo test -p cognee-vector` with no features and no service
/// container — which is the point, since what they pin is the *default* body
/// every non-pgvector backend inherits.
#[cfg(test)]
mod default_index_backfill_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;
    use crate::brute_force_vector_db::BruteForceVectorDB;

    /// A backend with no ANN index reports no work — it must not error, because
    /// `cognee-cli vector-reindex` calls this through `Arc<dyn VectorDB>`
    /// without knowing which backend is configured.
    #[tokio::test]
    async fn default_backfill_reports_no_work_and_is_idempotent() {
        let db = BruteForceVectorDB::new();
        db.create_collection("TestType", "field", 3).await.unwrap();

        assert_eq!(
            db.create_missing_vector_indexes().await.unwrap(),
            VectorIndexBackfill::default(),
            "a backend with nothing to index must report zero built and zero \
             failed, not fail"
        );
        assert_eq!(
            db.create_missing_vector_indexes().await.unwrap(),
            VectorIndexBackfill::default(),
            "and stay at zero on a second run"
        );

        assert!(
            db.has_collection("TestType", "field").await.unwrap(),
            "the no-op must not disturb the collections it looked at"
        );
    }
}
