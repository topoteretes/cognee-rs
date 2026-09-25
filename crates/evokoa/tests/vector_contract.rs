#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared adapter contract tests"
)]

#[path = "../../vector/tests/common/mod.rs"]
mod common;

use cognee_evokoa::EvokoaVectorAdapter;
use cognee_test_utils::create_temp_postgres_db;
use sea_orm::Database;

fn test_url() -> Option<String> {
    std::env::var("EVOKOA_TEST_DATABASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
}

macro_rules! vector_contract_test {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            let Some(base_url) = test_url() else {
                eprintln!(
                    "EVOKOA_TEST_DATABASE_URL not set — skipping {}",
                    stringify!($name)
                );
                return;
            };
            let tmp = create_temp_postgres_db(&base_url)
                .await
                .expect("temporary database creation");
            let url = tmp.url().to_string();
            let outcome = tokio::spawn(async move {
                let db = Database::connect(&url).await.expect("SeaORM connection");
                let adapter = EvokoaVectorAdapter::from_connection(db)
                    .await
                    .expect("pgContext adapter");
                common::$name(&adapter).await;
            })
            .await;
            tmp.cleanup().await;
            if let Err(error) = outcome {
                assert!(error.is_panic(), "contract task was cancelled: {error}");
                std::panic::resume_unwind(error.into_panic());
            }
        }
    };
}

vector_contract_test!(test_create_and_has_collection);
vector_contract_test!(test_create_duplicate_errors);
vector_contract_test!(test_delete_collection);
vector_contract_test!(test_list_collections);
vector_contract_test!(test_index_and_collection_size);
vector_contract_test!(test_empty_points_index);
vector_contract_test!(test_dimension_validation);
vector_contract_test!(test_upsert_overwrites);
vector_contract_test!(test_index_and_search);
vector_contract_test!(test_search_returns_top_k);
vector_contract_test!(test_metadata_preserved);
vector_contract_test!(test_uuid_round_trip);
vector_contract_test!(test_delete_points);
vector_contract_test!(test_batch_search);
vector_contract_test!(test_retrieve_round_trip);
vector_contract_test!(test_retrieve_missing_collection);
vector_contract_test!(test_retrieve_empty_ids);
vector_contract_test!(test_retrieve_chunking);
vector_contract_test!(test_upsert_raw_vectors_round_trip);
vector_contract_test!(test_upsert_raw_vectors_empty_noop);
vector_contract_test!(test_search_similar_filtered_filter_then_limit);
vector_contract_test!(test_search_similar_filtered_semantics);
vector_contract_test!(test_search_similar_filtered_and_vs_or);
vector_contract_test!(test_search_similar_filtered_none_matches_all);
vector_contract_test!(test_nul_bytes_in_metadata_are_persistable);
