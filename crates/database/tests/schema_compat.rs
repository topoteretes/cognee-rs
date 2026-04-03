//! Schema compatibility tests.
//!
//! Verifies that after `initialize()` runs all migrations on a fresh in-memory
//! SQLite database, the resulting schema contains every column that the Python
//! cognee SDK expects in the `data` and `datasets` tables.

use cognee_database::{connect, initialize};

/// Return the set of column names for `table` by querying `PRAGMA table_info`.
async fn column_names(db: &cognee_database::DatabaseConnection, table: &str) -> Vec<String> {
    use sea_orm::ConnectionTrait;
    let rows = db
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!("PRAGMA table_info({table})"),
        ))
        .await
        .unwrap_or_else(|e| panic!("PRAGMA table_info({table}) failed: {e}"));

    rows.iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect()
}

#[tokio::test]
async fn data_table_has_all_columns() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    let cols = column_names(&db, "data").await;

    // Original columns from the initial schema
    let required_columns = [
        "id",
        "name",
        "raw_data_location",
        "original_data_location",
        "extension",
        "mime_type",
        "content_hash",
        "owner_id",
        "created_at",
        "updated_at",
        "label",
        "original_extension",
        "original_mime_type",
        "loader_engine",
        "raw_content_hash",
        "tenant_id",
        "external_metadata",
        "node_set",
        "pipeline_status",
        "token_count",
        "data_size",
        "last_accessed",
    ];

    for col in required_columns.iter() {
        assert!(
            cols.iter().any(|c| c == col),
            "data table is missing column '{col}' — full column list: {cols:?}"
        );
    }
}

#[tokio::test]
async fn datasets_table_has_tenant_id() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    let cols = column_names(&db, "datasets").await;

    for col in [
        "id",
        "name",
        "owner_id",
        "tenant_id",
        "created_at",
        "updated_at",
    ] {
        assert!(
            cols.iter().any(|c| c == col),
            "datasets table is missing column '{col}' — full column list: {cols:?}"
        );
    }
}

#[tokio::test]
async fn tenant_id_indexes_exist() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    use sea_orm::ConnectionTrait;
    let check = |table: &'static str, index: &'static str| {
        let db = &db;
        async move {
            let rows = db
                .query_all(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Sqlite,
                    format!(
                        "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='{table}' AND name='{index}'"
                    ),
                ))
                .await
                .unwrap_or_else(|e| panic!("index query failed: {e}"));
            assert!(
                !rows.is_empty(),
                "expected index '{index}' on table '{table}' to exist"
            );
        }
    };

    check("data", "idx_data_tenant_id").await;
    check("datasets", "idx_datasets_tenant_id").await;
}
