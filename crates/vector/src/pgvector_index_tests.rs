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
            index_present(&db, "Idx_f_halfvec_hnsw").await,
            "create_collection must build the ANN index, or every search is a seq scan"
        );

        // The opclass has to match the `<=>` the searches order by, or the
        // planner ignores the index and nothing is actually fixed.
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT indexdef FROM pg_indexes WHERE indexname = 'Idx_f_halfvec_hnsw'",
            ))
            .await
            .unwrap()
            .expect("the index just asserted present must have a definition");
        let def: String = row.try_get("", "indexdef").unwrap();
        assert!(
            def.contains("hnsw") && def.contains("halfvec_cosine_ops"),
            "index must be HNSW over halfvec_cosine_ops, got: {def}"
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
                !index_present(&db, "Wide_f_halfvec_hnsw").await,
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
            db.execute_unprepared(r#"DROP INDEX "Old_f_halfvec_hnsw""#)
                .await
                .unwrap();
            assert!(!index_present(&db, "Old_f_halfvec_hnsw").await);

            let created = adapter.create_missing_vector_indexes().await.unwrap().built;
            assert_eq!(
                created, 1,
                "only the indexable collection counts — the 3072-d one is skipped"
            );
            assert!(index_present(&db, "Old_f_halfvec_hnsw").await);

            // Idempotent: nothing left to do, and nothing recounted.
            let again = adapter.create_missing_vector_indexes().await.unwrap().built;
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

/// The CLI holds an `Arc<dyn VectorDB>` and never learns which backend it was
/// handed, so the backfill has to be reachable through the trait. Calling the
/// inherent function is what every other case here does and would pass even if
/// the trait method were left on its `Ok(0)` default — which would make
/// `cognee-cli vector-reindex` silently report no work on every store.
#[tokio::test]
async fn backfill_is_reachable_through_the_trait_object() {
    with_temp_db(
        "backfill_is_reachable_through_the_trait_object",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            adapter.create_collection("Dyn", "f", 8).await.unwrap();

            let db = Database::connect(&url).await.unwrap();
            // Simulate a collection whose best-effort index build failed.
            db.execute_unprepared(r#"DROP INDEX "Dyn_f_halfvec_hnsw""#)
                .await
                .unwrap();
            assert!(!index_present(&db, "Dyn_f_halfvec_hnsw").await);

            // Erased exactly as the CLI holds it.
            let erased: std::sync::Arc<dyn VectorDB> = std::sync::Arc::new(adapter);
            let created = erased.create_missing_vector_indexes().await.unwrap().built;

            assert_eq!(
                created, 1,
                "the trait method must delegate to the adapter, not return the Ok(0) default"
            );
            assert!(index_present(&db, "Dyn_f_halfvec_hnsw").await);

            drop(db);
            erased.close().await.unwrap();
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
                     SELECT oid FROM pg_class WHERE relname = 'Broken_f_halfvec_hnsw'
                   )",
            )
            .await
            .expect(
                "writing to pg_index requires a superuser role; the CI service container \
                 runs as one, so point PGVECTOR_TEST_URL at a superuser (e.g. the default \
                 `postgres`) to run this case locally",
            );

            assert_eq!(
                PgVectorAdapter::vector_index_state(&db, "Broken_f_halfvec_hnsw")
                    .await
                    .unwrap(),
                Some(false),
                "the forged invalid state must be visible to the state probe"
            );
            assert!(
                !index_present(&db, "Broken_f_halfvec_hnsw").await,
                "an invalid index must not count as usable — the planner ignores it"
            );

            let created = adapter.create_missing_vector_indexes().await.unwrap().built;
            assert_eq!(created, 1, "the invalid index must be rebuilt, not skipped");
            assert!(
                index_present(&db, "Broken_f_halfvec_hnsw").await,
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

/// Postgres truncates identifiers at 63 bytes. The index name adds 13 characters
/// to the collection name, so a collection over 50 characters gets an index name
/// the adapter has had to trim — and the state probe compares the name as
/// *data*, so a mismatch would never show up. That silently breaks both
/// guarantees the probe exists for: the backfill would re-issue `CREATE INDEX`
/// and re-count it forever, and an index left invalid by a failed build would
/// read as absent and so never be rebuilt.
///
/// The trimming takes the bytes off the *collection* part, so the `_halfvec_hnsw`
/// suffix survives — see `index_name_tests` for why that matters.
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
                // 63 bytes total, with the 13-byte suffix kept whole.
                let mut n = format!("{data_type}_f");
                n.truncate(63 - "_halfvec_hnsw".len());
                n.push_str("_halfvec_hnsw");
                n
            };

            let db = Database::connect(&url).await.unwrap();
            assert!(
                index_present(&db, &expected).await,
                "the probe must look for the name Postgres actually stored"
            );

            // The regression: with an untruncated name the probe finds nothing,
            // so this reports 1 on every run instead of 0.
            let again = adapter.create_missing_vector_indexes().await.unwrap().built;
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

/// A bulk-load scope must drop the HNSW index once it is cheaper to rebuild it
/// at the end — and the collection must stay *correct* the whole time.
///
/// The three things this pins, in order of what breaks worst if they regress:
/// searches inside the scope still return the right rows (an unindexed
/// collection is an exact scan, not an error and not a short result); the index
/// is back and valid when the last scope ends; and `end_bulk_load` is what does
/// it, not the next write.
#[tokio::test]
async fn a_bulk_load_scope_defers_the_index_and_rebuilds_it_at_the_end() {
    with_temp_db(
        "a_bulk_load_scope_defers_the_index_and_rebuilds_it_at_the_end",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            adapter.create_collection("Bulk", "f", 8).await.unwrap();
            let db = Database::connect(&url).await.unwrap();
            assert!(index_present(&db, "Bulk_f_halfvec_hnsw").await);

            let point = |i: usize| {
                let jitter = f64::from(i as u32) * 1e-6;
                #[allow(clippy::cast_possible_truncation)]
                let v = vec![1.0, jitter as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
                crate::models::VectorPoint::new(uuid::Uuid::from_u128(0xB0 + i as u128), v)
            };
            let points: Vec<_> = (0..2000).map(point).collect();

            adapter.begin_bulk_load().await.unwrap();
            // Nested, to pin that scopes are counted rather than boolean: the
            // inner end must not trigger the build.
            adapter.begin_bulk_load().await.unwrap();
            for batch in points.chunks(500) {
                adapter.index_points("Bulk", "f", batch).await.unwrap();
            }
            assert!(
                !index_present(&db, "Bulk_f_halfvec_hnsw").await,
                "a 2000-point load into an empty collection is past the defer \
                 ratio, so the index must have been dropped"
            );

            // Unindexed is not degraded: the planner falls back to an exact
            // scan, which returns the true top k.
            let query = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
            let mid = adapter
                .search_similar("Bulk", "f", &query, 50)
                .await
                .unwrap();
            assert_eq!(
                mid.len(),
                50,
                "a search inside the scope must still be answered, exactly"
            );

            adapter.end_bulk_load().await.unwrap();
            assert!(
                !index_present(&db, "Bulk_f_halfvec_hnsw").await,
                "the inner scope ending must not run the deferred build"
            );
            adapter.end_bulk_load().await.unwrap();
            assert!(
                index_present(&db, "Bulk_f_halfvec_hnsw").await,
                "the outermost end_bulk_load must rebuild the index, valid"
            );

            assert_eq!(
                adapter.collection_size("Bulk", "f").await.unwrap(),
                2000,
                "the deferred load must have written every row"
            );
            let after = adapter
                .search_similar("Bulk", "f", &query, 50)
                .await
                .unwrap();
            assert_eq!(after.len(), 50);
            assert_eq!(
                after.iter().map(|h| h.id).collect::<Vec<_>>(),
                mid.iter().map(|h| h.id).collect::<Vec<_>>(),
                "the rebuilt index must agree with the exact scan it replaced"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// [`PgVectorAdapter::from_connection`] wraps a pool whose connect options this
/// adapter never chose, so the session tuning [`PgVectorAdapter::new`] applies
/// there has to ride in per statement instead. Two things that only a server can
/// answer:
///
/// - the statements **parse**. A `SET LOCAL` of a GUC the server does not define
///   is an `ERROR` that aborts the transaction, and pgvector reserves the `hnsw`
///   prefix — so a wrong name or value here does not degrade a search, it fails
///   every one of them on this constructor. A unit test over the strings cannot
///   see that.
/// - every search path is covered. The three that run SQL of their own are
///   `search_similar`, `search_similar_filtered` (which took no settings at all
///   before, and is the measured case for `force_custom_plan`) and
///   `batch_search_similar`; `retrieve` is an id lookup with no ANN levers to
///   set. All of them must answer exactly what the `new()`-built adapter does.
#[tokio::test]
async fn a_caller_owned_connection_answers_every_search_the_same_way() {
    with_temp_db(
        "a_caller_owned_connection_answers_every_search_the_same_way",
        |url| async move {
            use serde_json::json;
            let owned = PgVectorAdapter::new(&url, 4).await.unwrap();
            owned.create_collection("Shared", "f", 4).await.unwrap();

            // Distinct distances, and half the rows in the `alpha` NodeSet so
            // the filtered search has something to prefilter.
            let points: Vec<_> = (0..300u32)
                .map(|i| {
                    #[allow(clippy::cast_possible_truncation)]
                    let jitter = (f64::from(i) * 1e-4) as f32;
                    let p = crate::models::VectorPoint::new(
                        uuid::Uuid::from_u128(0x5A_0000 + u128::from(i)),
                        vec![1.0, jitter, 0.0, 0.0],
                    );
                    if i % 2 == 0 {
                        p.with_metadata("belongs_to_set", json!(["alpha"]))
                    } else {
                        p.with_metadata("belongs_to_set", json!(["beta"]))
                    }
                })
                .collect();
            owned.index_points("Shared", "f", &points).await.unwrap();

            // The caller's pool: no `hnsw.*`, no `plan_cache_mode`.
            let caller_owned = Database::connect(&url).await.unwrap();
            let shared = PgVectorAdapter::from_connection(caller_owned.clone(), 4)
                .await
                .expect("from_connection");
            assert!(
                !shared.tuned_sessions,
                "the premise: this adapter knows it did not open the pool"
            );

            // The settings themselves, through the very wrapper the searches
            // use: they parse (a `SET LOCAL` of a GUC under pgvector's reserved
            // `hnsw` prefix that it does not define is an ERROR, and that is
            // what only a server can tell us), and they are in effect for the
            // statement that follows them in the transaction.
            assert!(
                shared.iterative_scan,
                "the suite runs on pgvector 0.8+, where hnsw.iterative_scan exists"
            );
            let locals = PgVectorAdapter::ann_search_locals(
                shared.tuned_sessions,
                shared.iterative_scan,
                100,
                false,
            )
            .expect("a caller-owned connection always needs its settings");
            let applied = shared
                .query_all_with_locals(
                    &locals,
                    Statement::from_string(
                        DatabaseBackend::Postgres,
                        "SELECT current_setting('hnsw.ef_search') AS ef, \
                                current_setting('hnsw.iterative_scan') AS it, \
                                current_setting('plan_cache_mode') AS pc"
                            .to_string(),
                    ),
                )
                .await
                .expect("the SET LOCALs must parse on the server");
            let row = applied.first().expect("one row");
            assert_eq!(row.try_get::<String>("", "ef").unwrap(), "200");
            assert_eq!(
                row.try_get::<String>("", "it").unwrap(),
                "relaxed_order",
                "without this an HNSW scan whose beam runs dry silently returns \
                 fewer than top_k rows"
            );
            assert_eq!(
                row.try_get::<String>("", "pc").unwrap(),
                "force_custom_plan",
                "without this the filtered search costs its NodeSet array blind"
            );

            let query = vec![1.0, 0.0, 0.0, 0.0];
            let ids = |hits: &[crate::models::SearchResult]| -> Vec<uuid::Uuid> {
                hits.iter().map(|h| h.id).collect()
            };

            // An ordinary ANN search, and one past HNSW_EF_SEARCH_MAX, where the
            // locals also force the exact scan.
            for top_k in [100usize, super::HNSW_EF_SEARCH_MAX + 1] {
                let mine = owned
                    .search_similar("Shared", "f", &query, top_k)
                    .await
                    .unwrap();
                let theirs = shared
                    .search_similar("Shared", "f", &query, top_k)
                    .await
                    .unwrap();
                assert_eq!(
                    ids(&theirs),
                    ids(&mine),
                    "a top-{top_k} search must not depend on which constructor \
                     opened the connection"
                );
            }

            let names = ["alpha".to_string()];
            let mine = owned
                .search_similar_filtered("Shared", "f", &query, 50, Some(&names), "OR")
                .await
                .unwrap();
            let theirs = shared
                .search_similar_filtered("Shared", "f", &query, 50, Some(&names), "OR")
                .await
                .unwrap();
            assert_eq!(
                ids(&theirs),
                ids(&mine),
                "the filtered search is the path force_custom_plan exists for, \
                 and the one that carried no settings at all"
            );
            assert_eq!(theirs.len(), 50, "150 rows are in the alpha set");

            let batch = vec![query.clone(), vec![0.0, 1.0, 0.0, 0.0]];
            let mine = owned
                .batch_search_similar("Shared", "f", &batch, 20)
                .await
                .unwrap();
            let theirs = shared
                .batch_search_similar("Shared", "f", &batch, 20)
                .await
                .unwrap();
            assert_eq!(
                theirs.iter().map(|b| ids(b)).collect::<Vec<_>>(),
                mine.iter().map(|b| ids(b)).collect::<Vec<_>>(),
                "every LATERAL subquery needs the same settings as a single search"
            );

            // A no-op for a caller-owned connection, by contract — the caller's
            // pool must still be usable afterwards.
            shared.close().await.unwrap();
            assert_eq!(
                shared.collection_size("Shared", "f").await.unwrap(),
                300,
                "close() must not have touched the caller's pool"
            );
            drop(caller_owned);
            owned.close().await.unwrap();
        },
    )
    .await;
}

/// The bulk-load contract — [`VectorDB::begin_bulk_load`] calls itself "a hint,
/// never a semantic change", with searches "served by exact scan meanwhile" —
/// held against the fp16 candidate order of
/// [`PgVectorAdapter::candidate_order_expr`].
///
/// Two rows and a `top_k` of 1, picked so the two precisions disagree outright
/// (checked against the pgvector 0.8.2 this suite runs on):
///
/// | row | stored `vector(3)`      | as `halfvec(3)`             | distance to `[1,0,0]` |
/// |-----|-------------------------|-----------------------------|-----------------------|
/// | A   | `[1, 0.49987, 0.49987]` | `[1, 0.4997559, 0.4997559]` | fp32 0.1834327 / fp16 0.1833705 |
/// | B   | `[1, 0.49988, 0.49980]` | `[1, 0.5,       0.4997559]` | fp32 0.1834163 / fp16 0.1834370 |
///
/// B is the true nearest neighbour, by 1.6e-5; A is the fp16 one, by 6.6e-5.
/// Rounding to nearest is monotone *per component*, so a single varying
/// component can only ever produce a tie — the inversion needs two: fp16 pulls
/// both of A's down to the grid point below 0.5 while pushing B's first one up
/// to 0.5, and a sum of squares does not preserve the order under that. Both
/// gaps are strict and pgvector computes either distance in `double` over the
/// stored `float4`/`half`, so neither comparison sits near its own resolution.
/// At `top_k = 1` the outer `ORDER BY score DESC` cannot repair the choice:
/// there is one row to re-sort.
///
/// Inside the scope there is no index, so the only plan is a scan and the
/// ordering expression alone decides the winner — which is what makes the
/// assertion plan-independent, in the spirit of this file's header. The
/// *premise* (that these two rows do invert) is therefore checked as plain
/// arithmetic rather than by searching with the index in place: over three rows
/// the planner may or may not pick the HNSW scan, and pinning which would be
/// pinning a plan shape.
#[tokio::test]
async fn a_search_inside_a_bulk_load_returns_the_exact_nearest_neighbour() {
    with_temp_db(
        "a_search_inside_a_bulk_load_returns_the_exact_nearest_neighbour",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 3).await.unwrap();
            if !adapter.halfvec {
                // Below HALFVEC_MIN_VERSION there is no fp16 ordering to be
                // wrong about, so the case this pins cannot arise.
                eprintln!("pgvector has no halfvec — skipping the fp16 inversion");
                adapter.close().await.unwrap();
                return;
            }
            adapter.create_collection("Exact", "f", 3).await.unwrap();
            let db = Database::connect(&url).await.unwrap();

            let a = uuid::Uuid::from_u128(0xA);
            let b = uuid::Uuid::from_u128(0xB);
            let far = uuid::Uuid::from_u128(0xF);
            let query = vec![1.0f32, 0.0, 0.0];
            async fn top1(adapter: &PgVectorAdapter, query: &[f32]) -> uuid::Uuid {
                let hits = adapter
                    .search_similar("Exact", "f", query, 1)
                    .await
                    .unwrap();
                assert_eq!(hits.len(), 1, "two rows are present, so top-1 must hit");
                hits[0].id
            }

            adapter
                .index_points(
                    "Exact",
                    "f",
                    &[
                        crate::models::VectorPoint::new(a, vec![1.0, 0.49987, 0.49987]),
                        crate::models::VectorPoint::new(b, vec![1.0, 0.49988, 0.4998]),
                    ],
                )
                .await
                .unwrap();
            assert!(index_present(&db, "Exact_f_halfvec_hnsw").await);

            // The premise, as arithmetic the server does: A really is the fp16
            // winner and B really is the fp32 one. Without this the test would
            // go quietly green on two rows that had stopped inverting — a
            // different pgvector rounding, a changed literal, anything.
            let premise = db
                .query_one(Statement::from_string(
                    DatabaseBackend::Postgres,
                    r#"SELECT
                         (SELECT vector <=> '[1,0,0]'::vector FROM "Exact_f" WHERE id = '00000000-0000-0000-0000-00000000000b')
                       < (SELECT vector <=> '[1,0,0]'::vector FROM "Exact_f" WHERE id = '00000000-0000-0000-0000-00000000000a')
                           AS fp32_prefers_b,
                         (SELECT vector::halfvec(3) <=> '[1,0,0]'::halfvec(3) FROM "Exact_f" WHERE id = '00000000-0000-0000-0000-00000000000a')
                       < (SELECT vector::halfvec(3) <=> '[1,0,0]'::halfvec(3) FROM "Exact_f" WHERE id = '00000000-0000-0000-0000-00000000000b')
                           AS fp16_prefers_a"#
                        .to_string(),
                ))
                .await
                .unwrap()
                .expect("both rows were just written");
            assert!(
                premise.try_get::<bool>("", "fp32_prefers_b").unwrap(),
                "B must be the true nearest neighbour, or there is nothing to test"
            );
            assert!(
                premise.try_get::<bool>("", "fp16_prefers_a").unwrap(),
                "A must be the fp16 nearest neighbour, or there is nothing to test"
            );

            adapter.begin_bulk_load().await.unwrap();
            // One more point past BULK_DEFER_RATIO x the two already there, so
            // the scope drops the index for the rest of the load. Orthogonal to
            // the query, so it cannot become the top hit itself.
            adapter
                .index_points(
                    "Exact",
                    "f",
                    &[crate::models::VectorPoint::new(far, vec![0.0, 1.0, 0.0])],
                )
                .await
                .unwrap();
            assert!(
                !index_present(&db, "Exact_f_halfvec_hnsw").await,
                "the scope must have dropped the index, or there is nothing to test"
            );

            assert_eq!(
                top1(&adapter, &query).await,
                b,
                "with no index to match, the scan has to be exact: entering a \
                 bulk load must not reorder rows at the LIMIT boundary"
            );

            // The scope is not sticky: the index comes back, and the ordering
            // decision goes back to being the index's. Which row that picks is
            // the planner's business (above), so only the state is asserted.
            adapter.end_bulk_load().await.unwrap();
            assert!(index_present(&db, "Exact_f_halfvec_hnsw").await);
            assert!(
                !adapter.is_deferred("Exact_f"),
                "end_bulk_load built the index, so no search may still claim it \
                 is indexless"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// Crash safety of the bulk-load scope: a process that dies mid-load never
/// calls `end_bulk_load` or `close`, so the index stays dropped. That must be a
/// *correct* state — exact scans, every row present — and
/// `create_missing_vector_indexes` must be the one operator action that
/// restores the index.
///
/// Dropping the adapter without `close()` is the closest in-process stand-in
/// for the crash: `close()` is the path that would have run the deferred build.
#[tokio::test]
async fn an_interrupted_bulk_load_leaves_correct_rows_that_the_backfill_reindexes() {
    with_temp_db(
        "an_interrupted_bulk_load_leaves_correct_rows_that_the_backfill_reindexes",
        |url| async move {
            let query = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
            let point = |i: usize| {
                let jitter = f64::from(i as u32) * 1e-6;
                #[allow(clippy::cast_possible_truncation)]
                let v = vec![1.0, jitter as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
                crate::models::VectorPoint::new(uuid::Uuid::from_u128(0xC0 + i as u128), v)
            };

            let interrupted = {
                let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
                adapter.create_collection("Crash", "f", 8).await.unwrap();
                adapter.begin_bulk_load().await.unwrap();
                let points: Vec<_> = (0..2000).map(point).collect();
                for batch in points.chunks(500) {
                    adapter.index_points("Crash", "f", batch).await.unwrap();
                }
                let hits = adapter
                    .search_similar("Crash", "f", &query, 50)
                    .await
                    .unwrap();
                // No end_bulk_load, no close: the deferred build never runs.
                drop(adapter);
                hits
            };

            let db = Database::connect(&url).await.unwrap();
            assert!(
                !index_present(&db, "Crash_f_halfvec_hnsw").await,
                "the interrupted load must have left the index dropped — \
                 otherwise this test is not exercising the repair path"
            );

            let reopened = PgVectorAdapter::new(&url, 8).await.unwrap();
            assert_eq!(
                reopened.collection_size("Crash", "f").await.unwrap(),
                2000,
                "every row written before the interruption must be there: the \
                 index is deferred, the heap writes are not"
            );
            let before_repair = reopened
                .search_similar("Crash", "f", &query, 50)
                .await
                .unwrap();
            assert_eq!(
                before_repair.iter().map(|h| h.id).collect::<Vec<_>>(),
                interrupted.iter().map(|h| h.id).collect::<Vec<_>>(),
                "an unindexed collection answers exactly, so the results must \
                 not change across the interruption"
            );

            let backfill = reopened.create_missing_vector_indexes().await.unwrap();
            assert_eq!(
                backfill.built, 1,
                "the backfill is the documented repair for a load that never \
                 finished; it must find exactly the one missing index"
            );
            assert!(index_present(&db, "Crash_f_halfvec_hnsw").await);
            assert_eq!(
                reopened
                    .create_missing_vector_indexes()
                    .await
                    .unwrap()
                    .built,
                0,
                "and it must be idempotent"
            );

            drop(db);
            reopened.close().await.unwrap();
        },
    )
    .await;
}

/// The length at which both index names used to meet: a 62-byte collection gave
/// `coll + "_"` for the HNSW index *and* for the GIN membership index, and a
/// 63-byte one gave the table's own name for both. `CREATE INDEX IF NOT EXISTS`
/// reports a taken name as a NOTICE, not an error, so the second index was
/// simply never built — filtered searches scanned, and the backfill probing the
/// same name found the first index, answered "valid" and never repaired it.
///
/// Both have to exist, and the collection has to work.
#[tokio::test]
async fn both_indexes_exist_on_a_collection_at_the_identifier_limit() {
    with_temp_db(
        "both_indexes_exist_on_a_collection_at_the_identifier_limit",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 4).await.unwrap();
            let db = Database::connect(&url).await.unwrap();

            // 62 and 63 bytes: `<data_type>_f` is the collection name.
            for len in [60usize, 61] {
                let data_type = "L".repeat(len);
                adapter.create_collection(&data_type, "f", 4).await.unwrap();
                let coll = format!("{data_type}_f");

                let hnsw = PgVectorAdapter::vector_index_name(&coll, true);
                let gin = PgVectorAdapter::set_names_index_name(&coll);
                assert_ne!(
                    hnsw,
                    gin,
                    "the two names must differ at {} bytes",
                    coll.len()
                );
                assert!(
                    index_present(&db, &hnsw).await,
                    "the {}-byte collection must have its HNSW index ({hnsw})",
                    coll.len()
                );
                assert!(
                    index_present(&db, &gin).await,
                    "and its GIN membership index ({gin}) — this is the one the \
                     name collision silently swallowed"
                );

                // And the backfill must agree there is nothing left to do.
                assert_eq!(
                    adapter.create_missing_vector_indexes().await.unwrap().built,
                    0,
                    "both indexes are in place, so the repair pass must find no work"
                );
            }

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// `upsert_raw_vectors` self-creates its collection, because nothing else ever
/// creates a system-owned one like `TruthCentroid_vector`. That guard asks
/// `has_collection`, which now answers from a positive-only cache — so a
/// collection dropped behind this adapter's back (a second adapter, the CLI,
/// the Python SDK) is still remembered, the guard skips the create, and the
/// write lands on a table that is not there. Before the cache the guard
/// recovered transparently; without the retry it costs one hard failure and
/// only recovers on the call after.
#[tokio::test]
async fn a_raw_upsert_recreates_a_collection_dropped_behind_the_adapters_back() {
    with_temp_db(
        "a_raw_upsert_recreates_a_collection_dropped_behind_the_adapters_back",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 4).await.unwrap();
            let centroid = vec![crate::models::VectorPoint::new(
                uuid::Uuid::from_u128(0x7001),
                vec![1.0, 0.0, 0.0, 0.0],
            )];

            // First write creates the collection and caches it as known.
            adapter
                .upsert_raw_vectors("TruthCentroid", "vector", &centroid)
                .await
                .unwrap();
            assert_eq!(
                adapter
                    .collection_size("TruthCentroid", "vector")
                    .await
                    .unwrap(),
                1
            );

            // A different process drops it. This adapter is not told.
            let other = PgVectorAdapter::new(&url, 4).await.unwrap();
            other
                .delete_collection("TruthCentroid", "vector")
                .await
                .unwrap();
            other.close().await.unwrap();

            // The same call as before must still succeed, not fail once and
            // work on the retry the caller has to write itself.
            adapter
                .upsert_raw_vectors("TruthCentroid", "vector", &centroid)
                .await
                .expect("a raw upsert must recreate a collection dropped under it");
            assert_eq!(
                adapter
                    .collection_size("TruthCentroid", "vector")
                    .await
                    .unwrap(),
                1,
                "and the point must actually be in the recreated collection"
            );

            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// A bulk load can end two ways: `end_bulk_load`, or `close()` when the scope
/// was abandoned (a dropped future cannot await the maintenance). Both have to
/// run *both* steps. `close()` ran only the deferred index builds, so a load
/// finished that way left its collections planned from `reltuples = -1` or a
/// count taken from their first batch — the very problem the `ANALYZE` step was
/// added for, and one a collection filled faster than autovacuum's 60-second
/// naptime cannot fix by itself.
///
/// The collection here deliberately does *not* cross the defer ratio, so no
/// index is rebuilt on close: a `CREATE INDEX` updates `reltuples` as a side
/// effect, which would mask a missing `ANALYZE`.
#[tokio::test]
async fn a_bulk_load_finished_by_close_analyzes_what_it_wrote() {
    with_temp_db(
        "a_bulk_load_finished_by_close_analyzes_what_it_wrote",
        |url| async move {
            let point = |i: usize| {
                let jitter = f64::from(i as u32) * 1e-6;
                #[allow(clippy::cast_possible_truncation)]
                let v = vec![1.0, jitter as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
                crate::models::VectorPoint::new(uuid::Uuid::from_u128(0x2000 + i as u128), v)
            };
            let reltuples = |db: DatabaseConnection| async move {
                let row = db
                    .query_one(Statement::from_string(
                        DatabaseBackend::Postgres,
                        "SELECT reltuples FROM pg_class WHERE relname = 'Closed_f'",
                    ))
                    .await
                    .unwrap()
                    .expect("the collection table must exist");
                row.try_get::<f32>("", "reltuples").unwrap()
            };

            {
                let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
                adapter.create_collection("Closed", "f", 8).await.unwrap();
                // Seeded in batches under HNSW_REBUILD_MIN_ROWS and outside any
                // scope, so nothing rebuilds the index and nothing analyses.
                let seed: Vec<_> = (0..100).map(point).collect();
                for chunk in seed.chunks(50) {
                    adapter.index_points("Closed", "f", chunk).await.unwrap();
                }
                let db = Database::connect(&url).await.unwrap();
                assert_eq!(
                    reltuples(db).await,
                    -1.0,
                    "nothing has analysed this collection yet, so it still \
                     carries the `never analysed` sentinel — otherwise the \
                     assertion below proves nothing"
                );

                // 20 points is well under 0.25 x (100 + 20), so the index stays
                // live and only the ANALYZE is left to do.
                adapter.begin_bulk_load().await.unwrap();
                let more: Vec<_> = (100..120).map(point).collect();
                adapter.index_points("Closed", "f", &more).await.unwrap();
                // No end_bulk_load: close() is the one that has to finish it.
                adapter.close().await.unwrap();
            }

            let db = Database::connect(&url).await.unwrap();
            assert!(
                index_present(&db, "Closed_f_halfvec_hnsw").await,
                "the index was never deferred, so it must still be there"
            );
            assert_eq!(
                reltuples(db).await,
                120.0,
                "close() must ANALYZE every collection the load wrote, or the \
                 planner costs this collection as empty"
            );
        },
    )
    .await;
}

/// Incremental re-cognify: small batches into a collection that is already
/// large. The deferral decision needs the collection's size, and reading it per
/// batch used to mean a `count(*)` — a scan-sized read — for every one of them,
/// in the scenario the bulk-load scope exists to speed up. The size is now
/// counted once per collection per load and carried forward by the points
/// written since.
///
/// What has to stay true is the *decision*, since that is what the cheaper
/// accounting could silently change: batches that do not add up to a third of
/// the starting rows must leave the index in place and maintained, and the
/// batch that crosses the line must drop it — at the same point the per-batch
/// count would have.
#[tokio::test]
async fn small_batches_into_a_large_collection_defer_at_the_same_point() {
    with_temp_db(
        "small_batches_into_a_large_collection_defer_at_the_same_point",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            adapter.create_collection("Incr", "f", 8).await.unwrap();
            let db = Database::connect(&url).await.unwrap();

            let point = |i: usize| {
                let jitter = f64::from(i as u32) * 1e-6;
                #[allow(clippy::cast_possible_truncation)]
                let v = vec![1.0, jitter as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
                crate::models::VectorPoint::new(uuid::Uuid::from_u128(0x1000 + i as u128), v)
            };

            // 900 rows outside any scope, so the index is live and the load
            // below starts against a real base.
            let seed: Vec<_> = (0..900).map(point).collect();
            for chunk in seed.chunks(300) {
                adapter.index_points("Incr", "f", chunk).await.unwrap();
            }
            assert!(index_present(&db, "Incr_f_halfvec_hnsw").await);

            // 0.25 x (900 + w) <= w  =>  w >= 300. In 50-point batches of new
            // rows that is the sixth batch, and not before it.
            let guard = crate::BulkLoadGuard::begin(&adapter).await.unwrap();
            let fresh: Vec<_> = (900..1200).map(point).collect();
            for (n, chunk) in fresh.chunks(50).enumerate() {
                adapter.index_points("Incr", "f", chunk).await.unwrap();
                let written = (n + 1) * 50;
                assert_eq!(
                    index_present(&db, "Incr_f_halfvec_hnsw").await,
                    written < 300,
                    "after {written} of 900 starting rows the index must be \
                     {} — a third of the starting rows is the documented line",
                    if written < 300 { "kept" } else { "dropped" }
                );
            }

            guard.finish().await.unwrap();
            assert!(
                index_present(&db, "Incr_f_halfvec_hnsw").await,
                "and the end of the load rebuilds it"
            );
            assert_eq!(
                adapter.collection_size("Incr", "f").await.unwrap(),
                1200,
                "every seeded and every incrementally loaded row is present"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// A bulk load whose future is **cancelled** — the axum client disconnect, the
/// `tokio::time::timeout`, the losing `tokio::select!` branch — never reaches
/// `end_bulk_load`. That used to leave the scope depth above zero for the life
/// of the adapter, and the damage was not limited to the cancelled run: every
/// later collection that crossed the defer ratio had its index dropped inside
/// the still-open scope and no outermost `end_bulk_load` to build it again, so
/// the store degraded to exact scans permanently.
///
/// What [`BulkLoadGuard`]'s `Drop` has to guarantee is therefore the *depth*:
/// it cannot await, so the deferred build does not run here, but the next load
/// must be able to reconcile it. This pins both halves — depth back to zero,
/// and the next scope's end building both the abandoned collection's index and
/// its own.
#[tokio::test]
async fn a_dropped_bulk_load_guard_closes_the_scope_and_the_next_load_reindexes() {
    with_temp_db(
        "a_dropped_bulk_load_guard_closes_the_scope_and_the_next_load_reindexes",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            adapter
                .create_collection("Cancelled", "f", 8)
                .await
                .unwrap();
            adapter.create_collection("Next", "f", 8).await.unwrap();
            let db = Database::connect(&url).await.unwrap();

            let point = |tag: u128, i: usize| {
                let jitter = f64::from(i as u32) * 1e-6;
                #[allow(clippy::cast_possible_truncation)]
                let v = vec![1.0, jitter as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
                crate::models::VectorPoint::new(uuid::Uuid::from_u128(tag + i as u128), v)
            };
            let batch_of = |tag: u128| (0..2000).map(|i| point(tag, i)).collect::<Vec<_>>();

            // A load that parks forever after writing, and a `select!` that
            // drops it there. The `Notify` makes the cancellation point
            // deterministic: the load signals once its writes are in, so the
            // losing branch is always this future and always after the index
            // was deferred.
            let parked = tokio::sync::Notify::new();
            {
                let load = async {
                    let guard = crate::BulkLoadGuard::begin(&adapter).await.unwrap();
                    for chunk in batch_of(0xD0).chunks(500) {
                        adapter.index_points("Cancelled", "f", chunk).await.unwrap();
                    }
                    parked.notify_one();
                    std::future::pending::<()>().await;
                    guard.finish().await.unwrap();
                };
                tokio::select! {
                    () = load => panic!("the parked load must not run to completion"),
                    () = parked.notified() => {}
                }
            }

            assert!(
                !index_present(&db, "Cancelled_f_halfvec_hnsw").await,
                "the cancelled load must have deferred the index, or this test \
                 is not exercising the leak"
            );
            assert_eq!(
                adapter.bulk.lock().expect("bulk-load state lock").depth,
                0,
                "the guard's Drop must have closed the abandoned scope: a depth \
                 stuck above zero is what makes every later load lose its index"
            );

            // The next load is the proof the adapter is not poisoned. With the
            // scope leaked, this `end_bulk_load` would only decrement the depth
            // back to one and build nothing at all.
            let guard = crate::BulkLoadGuard::begin(&adapter).await.unwrap();
            for chunk in batch_of(0xE0).chunks(500) {
                adapter.index_points("Next", "f", chunk).await.unwrap();
            }
            guard.finish().await.unwrap();

            assert!(
                index_present(&db, "Next_f_halfvec_hnsw").await,
                "a load after an abandoned one must still get its index built"
            );
            assert!(
                index_present(&db, "Cancelled_f_halfvec_hnsw").await,
                "and the abandoned load's deferred build must be reconciled by \
                 it, not lost"
            );
            assert_eq!(
                adapter.collection_size("Cancelled", "f").await.unwrap(),
                2000,
                "the cancelled load's rows were written before the cancellation \
                 and stay written — only the index was deferred"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// A stored vector of norm zero scores `NaN` (`1 - (0 <=> q)`), and Postgres
/// sorts `NaN` as **greater than** every other float. The outer
/// `ORDER BY score DESC` that re-sorts an iterative scan's candidates therefore
/// used to hand that row back as `results[0]` of every search over a collection
/// at or below `top_k` — ahead of genuine matches, where the single
/// distance-ASC ordering it replaced had put it last. Only
/// `brute_force_triplet_search` re-ranks on its own; every other consumer of
/// `search_similar` reads the order given.
///
/// Both search paths are covered: `search_similar` and the one-round-trip
/// `batch_search_similar`, which has its own `ORDER BY` and so its own copy of
/// the bug.
#[tokio::test]
async fn a_zero_norm_stored_vector_sorts_last_rather_than_first() {
    with_temp_db(
        "a_zero_norm_stored_vector_sorts_last_rather_than_first",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 4).await.unwrap();
            adapter.create_collection("Nan", "f", 4).await.unwrap();

            // Three rows, one of them all-zero: the shape `warn_zero_norm_points`
            // warns about and still writes. `top_k` is above the row count, so
            // the zero row survives candidate selection and has to be ordered
            // by the outer re-sort — which is the code under test.
            let zero = uuid::Uuid::from_u128(0xF0);
            let near = uuid::Uuid::from_u128(0xF1);
            let far = uuid::Uuid::from_u128(0xF2);
            let points = vec![
                crate::models::VectorPoint::new(zero, vec![0.0, 0.0, 0.0, 0.0]),
                crate::models::VectorPoint::new(near, vec![1.0, 0.0, 0.0, 0.0]),
                crate::models::VectorPoint::new(far, vec![0.0, 1.0, 0.0, 0.0]),
            ];
            adapter.index_points("Nan", "f", &points).await.unwrap();

            let query = vec![1.0, 0.0, 0.0, 0.0];

            // The bug lives on the *exact-scan* paths, so the index has to go
            // first. An HNSW index scan never yields the zero-norm row at all
            // (its distance to every query is NaN, so the graph walk cannot
            // reach it), which hides the outer sort; the exact scan returns it
            // like any other row, and that is the plan the adapter deliberately
            // uses for a collection over the 2000-d index ceiling, for a
            // `top_k` above 1000, inside a bulk-load scope, and for any
            // collection whose index was never built. Dropping the index is the
            // cheapest way to reproduce all four.
            let indexed = adapter
                .search_similar("Nan", "f", &query, 10)
                .await
                .unwrap();
            assert_ne!(
                indexed.first().map(|h| h.id),
                Some(zero),
                "sanity: the indexed path must not lead with the zero-norm row either"
            );
            let db = Database::connect(&url).await.unwrap();
            db.execute_unprepared(r#"DROP INDEX "Nan_f_halfvec_hnsw""#)
                .await
                .unwrap();
            drop(db);

            let hits = adapter
                .search_similar("Nan", "f", &query, 10)
                .await
                .unwrap();
            assert_eq!(
                hits.len(),
                3,
                "an exact scan returns every row as a candidate at top_k = 10"
            );
            assert_eq!(
                hits[0].id, near,
                "the nearest real vector must lead the results, not the \
                 zero-norm row whose NaN score Postgres sorts highest"
            );
            assert_eq!(
                hits.last().map(|h| h.id),
                Some(zero),
                "the zero-norm row must come last: its score is not a similarity"
            );
            assert_eq!(
                hits[1].id, far,
                "and the real rows must keep their own descending order"
            );

            let batched = adapter
                .batch_search_similar("Nan", "f", std::slice::from_ref(&query), 10)
                .await
                .unwrap();
            assert_eq!(batched.len(), 1);
            assert_eq!(
                batched[0].iter().map(|h| h.id).collect::<Vec<_>>(),
                hits.iter().map(|h| h.id).collect::<Vec<_>>(),
                "the batch path must order NaN the same way the single-query \
                 path does — it has its own ORDER BY"
            );

            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// The GIN membership prefilter runs through `cognee_vector_set_names`, which
/// replaces a `jsonb_array_elements` scan. An expression index is only as
/// correct as that function: every row shape the corpus can produce has to map
/// to the same verdict `node_filter`'s client-side predicate gives, including
/// the shapes that are *not* an array of strings.
///
/// `belongs_to_set` in the wild is written by two SDKs over several schema
/// generations, so a row can carry JSON `null`, a bare scalar where an array
/// belongs, objects with and without a string `name`, or nothing at all. Under
/// the old scan those rows simply failed the `EXISTS`; under a STRICT
/// IMMUTABLE function feeding a GIN index, getting one of them wrong either
/// drops in-set rows or invents them — and the index would happily cache the
/// wrong answer.
#[tokio::test]
async fn the_membership_prefilter_handles_null_and_scalar_belongs_to_set() {
    with_temp_db(
        "the_membership_prefilter_handles_null_and_scalar_belongs_to_set",
        |url| async move {
            use serde_json::json;
            let adapter = PgVectorAdapter::new(&url, 2).await.unwrap();
            adapter.create_collection("Odd", "f", 2).await.unwrap();

            // Every shape, paired with whether a request for ["alpha"] keeps it.
            let shapes: Vec<(&str, serde_json::Value, bool)> = vec![
                ("array-of-strings", json!(["alpha", "beta"]), true),
                ("array-other-string", json!(["beta"]), false),
                ("named-objects", json!([{"name": "alpha"}]), true),
                ("object-without-name", json!([{"id": "alpha"}]), false),
                ("object-nonstring-name", json!([{"name": 7}]), false),
                ("mixed", json!([7, null, {"name": "alpha"}, "beta"]), true),
                ("empty-array", json!([]), false),
                ("json-null", serde_json::Value::Null, false),
                ("bare-scalar-string", json!("alpha"), false),
                ("bare-scalar-number", json!(3), false),
                ("object-not-array", json!({"name": "alpha"}), false),
            ];

            let mut expected_in_set: Vec<uuid::Uuid> = Vec::new();
            let mut points = Vec::new();
            for (i, (label, value, in_set)) in shapes.iter().enumerate() {
                let id = uuid::Uuid::from_u128(0xF0_0000 + i as u128);
                if *in_set {
                    expected_in_set.push(id);
                }
                points.push(
                    crate::models::VectorPoint::new(id, vec![1.0, 0.0])
                        .with_metadata("belongs_to_set", value.clone())
                        .with_metadata("shape", json!(label)),
                );
            }
            // A row with no `belongs_to_set` key at all.
            points.push(crate::models::VectorPoint::new(
                uuid::Uuid::from_u128(0xF0_FFFF),
                vec![1.0, 0.0],
            ));
            adapter.index_points("Odd", "f", &points).await.unwrap();

            for op in ["OR", "AND"] {
                let hits = adapter
                    .search_similar_filtered(
                        "Odd",
                        "f",
                        &[1.0, 0.0],
                        50,
                        Some(&["alpha".to_string()]),
                        op,
                    )
                    .await
                    .unwrap();
                let mut got: Vec<uuid::Uuid> = hits.iter().map(|h| h.id).collect();
                got.sort_unstable();
                let mut want = expected_in_set.clone();
                want.sort_unstable();
                assert_eq!(
                    got, want,
                    "the {op} prefilter must keep exactly the rows \
                     node_filter's client-side predicate keeps; the shapes \
                     that are not an array of names contribute nothing"
                );
            }

            // The index over the function exists and the backfill agrees it is
            // already there — a filtered search that fell back to a scan would
            // still be correct, so the index has to be asserted separately.
            let db = Database::connect(&url).await.unwrap();
            let row = db
                .query_one(Statement::from_string(
                    DatabaseBackend::Postgres,
                    "SELECT indexdef FROM pg_indexes WHERE indexname = 'Odd_f_set_names'",
                ))
                .await
                .unwrap()
                .expect("create_collection must build the membership GIN index");
            let def: String = row.try_get("", "indexdef").unwrap();
            assert!(
                def.contains("cognee_vector_set_names"),
                "the GIN index must be over the function the filter calls, got {def}"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// The in-batch duplicate fold and the server-side membership merge are two
/// different unions, and both have to survive in the batched upsert: the fold
/// collapses ids repeated inside one call, the `ON CONFLICT` expression unions
/// in whatever the stored row already had. Each was added for its own bug —
/// an aborted statement (#256) and the cross-dataset dedup bug — and the
/// batched path rewrote the statement they both live in, so this pins them
/// holding *together*: a re-index whose batch repeats an id under two new
/// datasets, against rows that already carry two.
#[tokio::test]
async fn an_in_batch_duplicate_fold_unions_with_the_membership_already_stored() {
    with_temp_db(
        "an_in_batch_duplicate_fold_unions_with_the_membership_already_stored",
        |url| async move {
            use serde_json::json;
            let adapter = PgVectorAdapter::new(&url, 2).await.unwrap();
            adapter.create_collection("Merge", "f", 2).await.unwrap();

            let id = uuid::Uuid::from_u128(0xED_0001);
            let tagged = |ds: &str| {
                crate::models::VectorPoint::new(id, vec![1.0, 0.0])
                    .with_metadata("text", json!("relationship"))
                    .with_metadata("dataset_id", json!(ds))
            };

            // First, the state in the database: two datasets, written one at a
            // time so the stored row carries a real `dataset_ids` array.
            adapter
                .index_points("Merge", "f", &[tagged("ds-1")])
                .await
                .unwrap();
            adapter
                .index_points("Merge", "f", &[tagged("ds-2")])
                .await
                .unwrap();

            // Then one call that repeats the id twice more under two further
            // datasets. Without the fold Postgres aborts the statement;
            // without the server-side merge ds-1 and ds-2 are lost.
            adapter
                .index_points("Merge", "f", &[tagged("ds-3"), tagged("ds-4")])
                .await
                .unwrap();

            assert_eq!(
                adapter.collection_size("Merge", "f").await.unwrap(),
                1,
                "one content-addressed id is one row"
            );
            let rows = adapter.retrieve("Merge", "f", &[id]).await.unwrap();
            assert_eq!(rows.len(), 1);
            let members: Vec<String> = rows[0]
                .metadata
                .get("dataset_ids")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                members,
                vec!["ds-1", "ds-2", "ds-3", "ds-4"],
                "membership must be the union of what was stored and every \
                 duplicate in the incoming batch, oldest first"
            );
            assert_eq!(
                rows[0].metadata.get("text"),
                Some(&json!("relationship")),
                "ordinary metadata is still last-wins, not unioned"
            );

            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// The bulk-load contract — "a hint, never a semantic change … served by exact
/// scan meanwhile" — against the store shape it was actually broken on: one
/// upgraded from before the half-precision index, which still carries the
/// legacy full-precision `<coll>_vector_hnsw`.
///
/// Phrasing the indexless ordering in full precision keeps the *fp16* index
/// from matching, but `vector <=> $1::vector` is exactly what the legacy index
/// serves, so the ordering alone hands the search to an approximate scan over
/// an index the adapter never built, is not maintaining during the load, and
/// will not rebuild at the end of it.
///
/// This is the one place in this file that asserts on `EXPLAIN`, and
/// deliberately: the file's header rules out pinning a plan the planner is free
/// to choose, and that is the opposite of what is checked here — the premise
/// establishes that the planner *would* choose the legacy index, and the
/// assertion is that the adapter has taken the choice away. Only a server can
/// answer either.
#[tokio::test]
async fn a_stray_full_precision_index_cannot_serve_an_indexless_search() {
    with_temp_db(
        "a_stray_full_precision_index_cannot_serve_an_indexless_search",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            if !adapter.halfvec {
                // Below HALFVEC_MIN_VERSION `_vector_hnsw` is the *active*
                // index, so there is no stray one to be served by.
                eprintln!("pgvector has no halfvec — skipping the legacy-index case");
                adapter.close().await.unwrap();
                return;
            }
            adapter.create_collection("Legacy", "f", 8).await.unwrap();
            let db = Database::connect(&url).await.unwrap();

            let point = |i: usize| {
                let jitter = f64::from(i as u32) * 1e-5;
                #[allow(clippy::cast_possible_truncation)]
                let v = vec![1.0, jitter as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
                crate::models::VectorPoint::new(uuid::Uuid::from_u128(0x1_0000 + i as u128), v)
            };
            let base: Vec<_> = (0..6000).map(point).collect();
            adapter.index_points("Legacy", "f", &base).await.unwrap();

            // The upgraded store, built by hand because nothing in-tree creates
            // this shape any more: the index a pre-halfvec `create_collection`
            // left behind, same name, same opclass, same parameters.
            db.execute_unprepared(
                r#"CREATE INDEX "Legacy_f_vector_hnsw" ON "Legacy_f"
                   USING hnsw (vector vector_cosine_ops)
                   WITH (m = 24, ef_construction = 128)"#,
            )
            .await
            .unwrap();
            db.execute_unprepared(r#"ANALYZE "Legacy_f""#)
                .await
                .unwrap();

            // Past BULK_DEFER_RATIO x 8000, so the scope drops the halfvec index
            // and the legacy one is the only index left on the table.
            adapter.begin_bulk_load().await.unwrap();
            let more: Vec<_> = (6000..8000).map(point).collect();
            adapter.index_points("Legacy", "f", &more).await.unwrap();
            assert!(
                !index_present(&db, "Legacy_f_halfvec_hnsw").await,
                "the scope must have dropped the halfvec index"
            );
            assert!(
                index_present(&db, "Legacy_f_vector_hnsw").await,
                "the bulk path must leave an index it never created alone — it \
                 has no rebuild for it"
            );
            assert!(adapter.is_deferred("Legacy_f"));

            let query = "'[1,0,0,0,0,0,0,0]'";
            let order = PgVectorAdapter::candidate_order_expr(query, 8, 50, adapter.halfvec, true);
            let explain = format!(r#"EXPLAIN SELECT id FROM "Legacy_f" ORDER BY {order} LIMIT 50"#);
            fn plan(rows: &[sea_orm::QueryResult]) -> String {
                rows.iter()
                    .map(|r| r.try_get::<String>("", "QUERY PLAN").unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("\n")
            }

            // The premise: with no session settings, this ordering is an ANN
            // scan over the legacy index. Without it the assertion below would
            // pass on a server that never wanted the index anyway.
            // Run on the adapter's *own* pool, not the bare handle above: the
            // planner's cost for an HNSW scan depends on `hnsw.ef_search` and
            // `hnsw.iterative_scan`, which `new()` sets as connection options,
            // so a premise measured on a pool without them is a premise about a
            // different server.
            let unguarded = plan(
                &adapter
                    .connection()
                    .query_all(Statement::from_string(
                        DatabaseBackend::Postgres,
                        explain.clone(),
                    ))
                    .await
                    .unwrap(),
            );
            assert!(
                unguarded.contains("Legacy_f_vector_hnsw"),
                "the premise fails: the planner did not want the legacy index \
                 here, so there is nothing to take away\n{unguarded}"
            );

            // And what the adapter actually sends for the same search.
            let locals = PgVectorAdapter::ann_search_locals(
                adapter.tuned_sessions,
                adapter.iterative_scan,
                50,
                true,
            )
            .expect("an indexless collection can never run with no settings");
            let guarded = plan(
                &adapter
                    .query_all_with_locals(
                        &locals,
                        Statement::from_string(DatabaseBackend::Postgres, explain),
                    )
                    .await
                    .unwrap(),
            );
            assert!(
                guarded.contains("Seq Scan") && !guarded.contains("Index Scan"),
                "a search inside a bulk load must be an exact scan whatever \
                 indexes the collection happens to carry\n{guarded}"
            );

            // And it still answers.
            let hits = adapter
                .search_similar("Legacy", "f", &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 50)
                .await
                .unwrap();
            assert_eq!(hits.len(), 50);

            adapter.end_bulk_load().await.unwrap();
            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// `create_missing_vector_indexes` on an upgraded store: build the
/// half-precision index, then reclaim the full-precision one it supersedes.
///
/// Left in place that index is not idle — it is maintained, so every upsert
/// pays two HNSW inserts and a bulk-load scope that drops
/// `<coll>_halfvec_hnsw` keeps maintaining it row by row, which is the entire
/// cost the scope exists to avoid. Nothing else would ever remove it: the two
/// `DROP INDEX CONCURRENTLY` sites in the backfill target an *invalid* index of
/// the same name, and the bulk path deliberately leaves an index it cannot
/// rebuild alone.
///
/// Both orders are covered, because the drop has two call sites: the collection
/// that already has a valid halfvec index (nothing to build) and the one that
/// does not (built here, then the legacy one goes).
#[tokio::test]
async fn the_backfill_reclaims_the_superseded_full_precision_index() {
    with_temp_db(
        "the_backfill_reclaims_the_superseded_full_precision_index",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 4).await.unwrap();
            if !adapter.halfvec {
                eprintln!("pgvector has no halfvec — skipping the legacy-index reclaim");
                adapter.close().await.unwrap();
                return;
            }
            let db = Database::connect(&url).await.unwrap();

            // `Both` keeps its valid halfvec index; `OnlyOld` is the real
            // upgraded shape — legacy index, no halfvec one.
            for coll in ["Both", "OnlyOld"] {
                adapter.create_collection(coll, "f", 4).await.unwrap();
                db.execute_unprepared(&format!(
                    r#"CREATE INDEX "{coll}_f_vector_hnsw" ON "{coll}_f"
                       USING hnsw (vector vector_cosine_ops)"#
                ))
                .await
                .unwrap();
            }
            db.execute_unprepared(r#"DROP INDEX "OnlyOld_f_halfvec_hnsw""#)
                .await
                .unwrap();

            // The runtime notice is wired into `has_collection`'s cold path, so
            // a store nobody has reindexed is visible without reading the docs.
            // A second adapter is what makes that path reachable, and is the
            // faithful shape anyway: an upgraded store is one this process did
            // not create, so its collection is not in `known` and the first
            // `has_collection` goes to the catalog. The log line itself is not
            // observable here; that the check ran once for this collection is.
            let observer = PgVectorAdapter::new(&url, 4).await.unwrap();
            assert!(observer.has_collection("OnlyOld", "f").await.unwrap());
            assert!(
                observer
                    .legacy_checked
                    .read()
                    .unwrap()
                    .contains("OnlyOld_f"),
                "has_collection must check a collection's index shape once, or \
                 an un-reindexed store stays silent"
            );
            observer.close().await.unwrap();

            let report = adapter.create_missing_vector_indexes().await.unwrap();
            assert_eq!(
                report.built, 1,
                "only OnlyOld_f needed the halfvec index: {report:?}"
            );
            assert_eq!(
                report.dropped, 2,
                "both collections carried a superseded index: {report:?}"
            );
            assert_eq!(report.failed, 0, "{report:?}");

            for coll in ["Both", "OnlyOld"] {
                assert!(
                    index_present(&db, &format!("{coll}_f_halfvec_hnsw")).await,
                    "{coll} must end with a valid half-precision index"
                );
                assert_eq!(
                    PgVectorAdapter::vector_index_state(&db, &format!("{coll}_f_vector_hnsw"))
                        .await
                        .unwrap(),
                    None,
                    "{coll}'s superseded full-precision index must be gone"
                );
            }

            // Idempotent: a second pass has nothing left to build or reclaim.
            let again = adapter.create_missing_vector_indexes().await.unwrap();
            assert_eq!(
                (again.built, again.dropped, again.failed),
                (0, 0, 0),
                "{again:?}"
            );

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}

/// A failed end-of-load index build must stay queued, not be forgotten.
///
/// `take_deferred` used to *drain* the queue before `build_deferred` ran, so a
/// `CREATE INDEX` that failed — `ENOSPC`, `statement_timeout`, a lock wait, or
/// the `finish()` future dropped mid-build by a client disconnect, which closes
/// the connection and makes Postgres cancel the build — left the collection
/// permanently unindexed *and* unrecorded: `is_deferred` answered `false`, so
/// searches went back to fp16 ordering over a sequential scan; the next bulk
/// load would not re-defer it (`vector_index_state` answers `None`, so
/// `upsert_points` skips that branch); `close()` had nothing left to build; and
/// the caller only ever saw a `warn!`. A manual `vector-reindex` was the only
/// repair.
///
/// Renaming the table away is the cheapest deterministic stand-in for those
/// failures — what is pinned is the bookkeeping, and that does not depend on
/// *why* the build failed.
#[tokio::test]
async fn a_failed_end_of_load_build_stays_queued_and_the_next_one_repairs_it() {
    with_temp_db(
        "a_failed_end_of_load_build_stays_queued_and_the_next_one_repairs_it",
        |url| async move {
            let adapter = PgVectorAdapter::new(&url, 8).await.unwrap();
            adapter.create_collection("Requeue", "f", 8).await.unwrap();
            let db = Database::connect(&url).await.unwrap();
            let index = PgVectorAdapter::vector_index_name("Requeue_f", adapter.halfvec);

            let point = |i: usize| {
                let jitter = f64::from(i as u32) * 1e-5;
                #[allow(clippy::cast_possible_truncation)]
                let v = vec![1.0, jitter as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
                crate::models::VectorPoint::new(uuid::Uuid::from_u128(0x2_0000 + i as u128), v)
            };
            let points: Vec<_> = (0..600).map(point).collect();

            adapter.begin_bulk_load().await.unwrap();
            adapter.index_points("Requeue", "f", &points).await.unwrap();
            assert!(
                !index_present(&db, &index).await,
                "the scope must have dropped the index, or there is nothing to test"
            );

            // Make the build fail, deterministically, without losing a row.
            db.execute_unprepared(r#"ALTER TABLE "Requeue_f" RENAME TO "Requeue_f_hidden""#)
                .await
                .unwrap();
            assert!(
                adapter.end_bulk_load().await.is_err(),
                "a build against a missing relation has to be reported"
            );
            assert!(
                adapter.is_deferred("Requeue_f"),
                "a build that did not commit must leave the collection recorded \
                 as indexless — otherwise nothing ever builds it again and every \
                 search is silently ordered in fp16 over a sequential scan"
            );

            // The repair is the ordinary one: the next scope end (or close())
            // picks the queued build up.
            db.execute_unprepared(r#"ALTER TABLE "Requeue_f_hidden" RENAME TO "Requeue_f""#)
                .await
                .unwrap();
            adapter.begin_bulk_load().await.unwrap();
            adapter.end_bulk_load().await.unwrap();
            assert!(
                index_present(&db, &index).await,
                "the queued build must run at the next scope end"
            );
            assert!(
                !adapter.is_deferred("Requeue_f"),
                "and only then may a search stop treating it as indexless"
            );
            assert_eq!(adapter.collection_size("Requeue", "f").await.unwrap(), 600);

            drop(db);
            adapter.close().await.unwrap();
        },
    )
    .await;
}
