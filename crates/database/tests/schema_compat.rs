#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Schema compatibility tests.
//!
//! Verifies that after `initialize()` runs all migrations on a fresh in-memory
//! SQLite database, the resulting schema contains every column that the Python
//! cognee SDK expects in the `data` and `datasets` tables.

use cognee_database::{connect, initialize, uuid_hex};

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

/// Verify the `nodes` table uses column name `type` (matching Python),
/// not `node_type` (Rust field name).
#[tokio::test]
async fn nodes_table_column_is_type_not_node_type() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    let cols = column_names(&db, "nodes").await;
    assert!(
        cols.iter().any(|c| c == "type"),
        "nodes table must have a 'type' column (Python compat) — got: {cols:?}"
    );
    assert!(
        !cols.iter().any(|c| c == "node_type"),
        "nodes table must NOT have 'node_type' (Rust-only name) — got: {cols:?}"
    );
}

/// Verify that UUIDs are stored as 32-char hex strings (no hyphens)
/// to match Python's SQLAlchemy UUID format on SQLite.
#[tokio::test]
async fn uuids_stored_as_32_char_hex() {
    use sea_orm::{ConnectionTrait, EntityTrait, Set};
    use uuid::Uuid;

    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    let test_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let owner_id = Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap();

    // Insert a dataset via the entity model
    let model = cognee_database::entities::dataset::ActiveModel {
        id: Set(uuid_hex::to_hex(test_id)),
        name: Set("test".into()),
        owner_id: Set(uuid_hex::to_hex(owner_id)),
        tenant_id: Set(None),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(None),
    };
    cognee_database::entities::dataset::Entity::insert(model)
        .exec(&db)
        .await
        .expect("insert dataset");

    // Read the raw text value from SQLite
    let rows = db
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT id, owner_id FROM datasets LIMIT 1".to_string(),
        ))
        .await
        .expect("query");

    let raw_id: String = rows[0].try_get("", "id").expect("id column");
    let raw_owner: String = rows[0].try_get("", "owner_id").expect("owner_id column");

    // Must be 32-char hex without hyphens (Python SQLAlchemy format)
    assert_eq!(
        raw_id.len(),
        32,
        "UUID should be 32-char hex, got: {raw_id}"
    );
    assert!(
        !raw_id.contains('-'),
        "UUID should not contain hyphens: {raw_id}"
    );
    assert_eq!(raw_id, "550e8400e29b41d4a716446655440000");

    assert_eq!(raw_owner.len(), 32);
    assert!(!raw_owner.contains('-'));
    assert_eq!(raw_owner, "660e8400e29b41d4a716446655440001");
}
