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
    std::env::var("PGVECTOR_TEST_URL").ok()
}

/// Create an adapter and clean up all vector collections from previous runs.
async fn make_adapter() -> Option<PgVectorAdapter> {
    let url = test_url()?;
    let db = PgVectorAdapter::new(&url, 384).await.ok()?;

    // Best-effort cleanup of any leftover collections from prior runs.
    if let Ok(cols) = db.list_collections().await {
        for (dt, fn_) in cols {
            let _ = db.delete_collection(&dt, &fn_).await;
        }
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
