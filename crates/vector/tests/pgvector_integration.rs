#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `PgVectorAdapter` using the shared VectorDB test suite.
//!
//! These tests require a running PostgreSQL instance with the `vector` extension.
//! Set `PGVECTOR_TEST_URL` to a Postgres connection string, e.g.:
//!
//!   PGVECTOR_TEST_URL="postgres://user:pass@localhost:5432/cognee_test_vectors"
//!
//! Tests are skipped automatically when the variable is absent.
//! All tests run serially (shared DB state).
#![cfg(feature = "pgvector")]

mod common;

use cognee_vector::{PgVectorAdapter, VectorDB};
use serial_test::serial;

/// Read the connection URL or skip the test.
fn test_url() -> Option<String> {
    std::env::var("PGVECTOR_TEST_URL")
        .ok()
        .filter(|v| !v.is_empty())
}

/// Create an adapter and clean up all vector collections from previous runs.
///
/// Only an absent `PGVECTOR_TEST_URL` yields `None` (the skip path). Once the URL
/// is known to be set, failing to connect, migrate or clean up panics with the
/// underlying error: collapsing those into `None` would print the
/// "PGVECTOR_TEST_URL not set" line instead, which the CI guard reports as "the
/// suite never reached a live Postgres" — sending whoever reads the log after a
/// real adapter regression to debug a service container that was fine.
async fn make_adapter() -> Option<PgVectorAdapter> {
    let url = test_url()?;
    let db = PgVectorAdapter::new(&url, 384)
        .await
        .expect("PGVECTOR_TEST_URL is set, so the adapter must connect and migrate");

    // Clear any leftover collections from prior runs.
    let cols = db
        .list_collections()
        .await
        .expect("listing collections must succeed against a live Postgres");
    for (dt, fn_) in cols {
        db.delete_collection(&dt, &fn_)
            .await
            .expect("dropping a stale collection must succeed");
    }
    Some(db)
}

macro_rules! pgvector_test {
    ($name:ident) => {
        #[tokio::test]
        #[serial]
        async fn $name() {
            let Some(db) = make_adapter().await else {
                eprintln!("PGVECTOR_TEST_URL not set — skipping {}", stringify!($name));
                return;
            };
            common::$name(&db).await;
        }
    };
}

pgvector_test!(test_create_and_has_collection);
pgvector_test!(test_create_duplicate_errors);
pgvector_test!(test_delete_collection);
pgvector_test!(test_list_collections);
pgvector_test!(test_index_and_collection_size);
pgvector_test!(test_empty_points_index);
pgvector_test!(test_dimension_validation);
pgvector_test!(test_upsert_overwrites);
pgvector_test!(test_index_and_search);
pgvector_test!(test_search_returns_top_k);
pgvector_test!(test_metadata_preserved);
pgvector_test!(test_uuid_round_trip);
pgvector_test!(test_delete_points);
pgvector_test!(test_batch_search);
pgvector_test!(test_retrieve_round_trip);
pgvector_test!(test_retrieve_missing_collection);
pgvector_test!(test_retrieve_empty_ids);
pgvector_test!(test_retrieve_chunking);
pgvector_test!(test_upsert_raw_vectors_round_trip);
pgvector_test!(test_upsert_raw_vectors_empty_noop);
pgvector_test!(test_search_similar_filtered_filter_then_limit);
pgvector_test!(test_search_similar_filtered_semantics);
pgvector_test!(test_search_similar_filtered_and_vs_or);
pgvector_test!(test_search_similar_filtered_none_matches_all);
pgvector_test!(test_nul_bytes_in_metadata_are_persistable);
