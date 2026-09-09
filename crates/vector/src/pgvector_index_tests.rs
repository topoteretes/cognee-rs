//! ANN index tests for [`super::PgVectorAdapter`] (SDK-515).
//!
//! Same gating and isolation as `pgvector_adapter`'s inline migration cases:
//! they need a live Postgres with the `vector` extension via
//! `PGVECTOR_TEST_URL`, and each provisions its own throwaway database so no
//! `#[serial]` is required.
//!
//! These assert that the index *object* exists, not that the planner chose it.
//! Plan choice depends on row count and cost settings, so asserting on `EXPLAIN`
//! would pin a plan shape rather than the behaviour we care about. The behaviour
//! that must hold regardless of plan — filtered search staying exact — is pinned
//! by `test_search_similar_filtered_filter_then_limit` in the shared harness,
//! which runs against this adapter in `pgvector_integration`.

use super::{PgVectorAdapter, VectorDB};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};

fn test_url() -> Option<String> {
    std::env::var("PGVECTOR_TEST_URL")
        .ok()
        .filter(|v| !v.is_empty())
}

/// Mirrors `shared_db_migration_tests::with_temp_db` — see the rationale for the
/// spawned task and the re-raised panic there.
async fn with_temp_db<F, Fut>(what: &str, body: F)
where
    F: FnOnce(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(base_url) = test_url() else {
        eprintln!("PGVECTOR_TEST_URL not set — skipping {what}");
        return;
    };
    let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
        .await
        .expect("PGVECTOR_TEST_URL is set, so CREATE DATABASE must succeed on that server");
    let outcome = tokio::spawn(body(tmp.url().to_string())).await;
    tmp.cleanup().await;
    if let Err(join_err) = outcome {
        assert!(
            join_err.is_panic(),
            "the {what} task was cancelled instead of panicking: {join_err}"
        );
        std::panic::resume_unwind(join_err.into_panic());
    }
}

/// Whether a *usable* index is in place: present and valid. An invalid index —
/// what an interrupted concurrent build leaves — is not usable, because the
/// planner ignores it.
async fn index_present(db: &DatabaseConnection, index: &str) -> bool {
    PgVectorAdapter::vector_index_state(db, index)
        .await
        .expect("index state lookup must succeed")
        == Some(true)
}

#[tokio::test]
async fn create_collection_builds_an_hnsw_index() {
    with_temp_db("create_collection_builds_an_hnsw_index", |url| async move {
        let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
        adapter.create_collection("Idx", "f", 8).await.unwrap();

        let db = Database::connect(&url).await.unwrap();
        assert!(
            index_present(&db, "Idx_f_vector_hnsw").await,
            "create_collection must build the ANN index, or every search is a seq scan"
        );

        // The opclass has to match the `<=>` the searches order by, or the
        // planner ignores the index and nothing is actually fixed.
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT indexdef FROM pg_indexes WHERE indexname = 'Idx_f_vector_hnsw'",
            ))
            .await
            .unwrap()
            .expect("the index just asserted present must have a definition");
        let def: String = row.try_get("", "indexdef").unwrap();
        assert!(
            def.contains("hnsw") && def.contains("vector_cosine_ops"),
            "index must be HNSW over vector_cosine_ops, got: {def}"
        );

        drop(db);
        adapter.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_collection_over_the_dimension_ceiling_is_left_unindexed() {
    with_temp_db(
        "a_collection_over_the_dimension_ceiling_is_left_unindexed",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            // 3072 = text-embedding-3-large, past pgvector's 2000-d ceiling.
            // The collection must stay usable; only the index is skipped.
            adapter.create_collection("Wide", "f", 3072).await.unwrap();

            let db = Database::connect(&url).await.unwrap();
            assert!(
                !index_present(&db, "Wide_f_vector_hnsw").await,
                "pgvector cannot index past 2000 dimensions — creating it would error"
            );
            assert!(
                adapter.has_collection("Wide", "f").await.unwrap(),
                "the collection itself must still be created and usable"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

#[tokio::test]
async fn backfill_indexes_pre_existing_collections_and_is_idempotent() {
    with_temp_db(
        "backfill_indexes_pre_existing_collections_and_is_idempotent",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            adapter.create_collection("Old", "f", 8).await.unwrap();
            adapter.create_collection("Wide", "f", 3072).await.unwrap();

            let db = Database::connect(&url).await.unwrap();
            // Simulate a collection created before indexing existed.
            db.execute_unprepared(r#"DROP INDEX "Old_f_vector_hnsw""#)
                .await
                .unwrap();
            assert!(!index_present(&db, "Old_f_vector_hnsw").await);

            let created = adapter.create_missing_vector_indexes().await.unwrap();
            assert_eq!(
                created, 1,
                "only the indexable collection counts — the 3072-d one is skipped"
            );
            assert!(index_present(&db, "Old_f_vector_hnsw").await);

            // Idempotent: nothing left to do, and nothing recounted.
            let again = adapter.create_missing_vector_indexes().await.unwrap();
            assert_eq!(
                again, 0,
                "a second run must report no work, not re-count existing indexes"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// An interrupted `CREATE INDEX CONCURRENTLY` leaves the index present but
/// marked invalid. The planner ignores it and `IF NOT EXISTS` refuses to replace
/// it, so a presence-only check would skip the collection on every later run and
/// it would keep its sequential scan permanently while looking indexed.
#[tokio::test]
async fn backfill_replaces_an_invalid_index_left_by_a_failed_build() {
    with_temp_db(
        "backfill_replaces_an_invalid_index_left_by_a_failed_build",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            adapter.create_collection("Broken", "f", 8).await.unwrap();

            let db = Database::connect(&url).await.unwrap();

            // Forge the state an interrupted concurrent build leaves behind.
            // There is no supported way to make Postgres produce it on demand,
            // so mark the existing index invalid directly.
            db.execute_unprepared(
                "UPDATE pg_index SET indisvalid = false
                   WHERE indexrelid = (
                     SELECT oid FROM pg_class WHERE relname = 'Broken_f_vector_hnsw'
                   )",
            )
            .await
            .expect(
                "writing to pg_index requires a superuser role; the CI service container \
                 runs as one, so point PGVECTOR_TEST_URL at a superuser (e.g. the default \
                 `postgres`) to run this case locally",
            );

            assert_eq!(
                PgVectorAdapter::vector_index_state(&db, "Broken_f_vector_hnsw")
                    .await
                    .unwrap(),
                Some(false),
                "the forged invalid state must be visible to the state probe"
            );
            assert!(
                !index_present(&db, "Broken_f_vector_hnsw").await,
                "an invalid index must not count as usable — the planner ignores it"
            );

            let created = adapter.create_missing_vector_indexes().await.unwrap();
            assert_eq!(created, 1, "the invalid index must be rebuilt, not skipped");
            assert!(
                index_present(&db, "Broken_f_vector_hnsw").await,
                "after the rebuild the index must be valid and usable"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// An HNSW index scan returns at most `hnsw.ef_search` tuples and then stops, so
/// a `LIMIT` above that default of 40 is silently unmet — no error, just missing
/// rows. cognee's retrieval paths ask for `DEFAULT_WIDE_SEARCH_TOP_K = 100`, so
/// without an `ef_search` floor every graph-completion, triplet and temporal
/// seed set would quietly lose 60% of its candidates.
///
/// The vectors here are tightly clustered on purpose. With uniformly random
/// vectors the scan happens to return the full 100 and the bug hides; real
/// embeddings cluster, which is when it bites.
#[tokio::test]
async fn a_search_returns_top_k_rows_even_above_the_default_ef_search() {
    with_temp_db(
        "a_search_returns_top_k_rows_even_above_the_default_ef_search",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            adapter.create_collection("Clust", "f", 8).await.unwrap();

            // 5000 rows is enough for the planner to prefer the index over a
            // sequential scan, which is the only situation where the cap applies.
            let points: Vec<_> = (0..5000)
                .map(|i| {
                    let jitter = f64::from(i) * 1e-6;
                    #[allow(clippy::cast_possible_truncation)]
                    let v = vec![1.0, jitter as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
                    crate::models::VectorPoint::new(uuid::Uuid::new_v4(), v)
                })
                .collect();
            for batch in points.chunks(500) {
                adapter.index_points("Clust", "f", batch).await.unwrap();
            }

            let query = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
            let hits = adapter
                .search_similar("Clust", "f", &query, 100)
                .await
                .unwrap();
            assert_eq!(
                hits.len(),
                100,
                "top_k=100 must return 100 rows; {} means the HNSW scan stopped at \
                 hnsw.ef_search and the LIMIT went silently unmet",
                hits.len()
            );

            // The batched path runs one index scan per LATERAL subquery, each
            // with its own LIMIT, so it needs the same floor.
            let batched = adapter
                .batch_search_similar("Clust", "f", &[query.clone(), query.clone()], 100)
                .await
                .unwrap();
            assert_eq!(batched.len(), 2);
            for (i, bucket) in batched.iter().enumerate() {
                assert_eq!(
                    bucket.len(),
                    100,
                    "batch query {i} returned {} rows instead of 100",
                    bucket.len()
                );
            }

            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// Postgres truncates identifiers at 63 bytes. The index name adds 12 characters
/// to the collection name, so a collection over 51 characters gets an index
/// whose real name is shorter than the one we computed — and the state probe
/// compares the name as *data*, so it would never match. That silently breaks
/// both guarantees the probe exists for: the backfill would re-issue
/// `CREATE INDEX` and re-count it forever, and an index left invalid by a failed
/// build would read as absent and so never be rebuilt.
#[tokio::test]
async fn a_collection_name_past_the_identifier_limit_is_still_tracked() {
    with_temp_db(
        "a_collection_name_past_the_identifier_limit_is_still_tracked",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            // 57-char collection name -> 69-char index name, past the 63 limit.
            let data_type = "L".repeat(55);
            adapter.create_collection(&data_type, "f", 8).await.unwrap();

            let expected = {
                let mut n = format!("{data_type}_f_vector_hnsw");
                n.truncate(63);
                n
            };

            let db = Database::connect(&url).await.unwrap();
            assert!(
                index_present(&db, &expected).await,
                "the probe must look for the name Postgres actually stored"
            );

            // The regression: with an untruncated name the probe finds nothing,
            // so this reports 1 on every run instead of 0.
            let again = adapter.create_missing_vector_indexes().await.unwrap();
            assert_eq!(
                again, 0,
                "a long-named collection is already indexed; re-counting it means \
                 the probe is blind and an invalid index would never be rebuilt"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}
