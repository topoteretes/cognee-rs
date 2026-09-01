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

/// The run-ownership column must be NULLABLE on both provenance tables: rows
/// written before it existed carry no run and must stay permanently exempt
/// from every run-scoped sweep rather than being ambiguously owned.
#[tokio::test]
async fn nodes_and_edges_have_nullable_pipeline_run_id() {
    use sea_orm::ConnectionTrait;

    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    for table in ["nodes", "edges"] {
        let rows = db
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                format!("PRAGMA table_info({table})"),
            ))
            .await
            .unwrap_or_else(|e| panic!("PRAGMA table_info({table}) failed: {e}"));

        let notnull = rows
            .iter()
            .find(|row| {
                row.try_get::<String>("", "name")
                    .is_ok_and(|n| n == "pipeline_run_id")
            })
            .unwrap_or_else(|| panic!("{table} table is missing column 'pipeline_run_id'"))
            .try_get::<i32>("", "notnull")
            .expect("notnull flag");
        assert_eq!(notnull, 0, "{table}.pipeline_run_id must be nullable");
    }
}

/// Index names are copied verbatim from Python's alembic revision
/// `aa753a730673` so that whichever SDK migrates a shared file first, the
/// other skips instead of creating a second index on the same column.
#[tokio::test]
async fn pipeline_run_id_indexes_exist() {
    use sea_orm::ConnectionTrait;

    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    for (table, index) in [
        ("nodes", "ix_nodes_pipeline_run_id"),
        ("edges", "ix_edges_pipeline_run_id"),
    ] {
        let rows = db
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                format!(
                    "SELECT name FROM sqlite_master \
                     WHERE type='index' AND tbl_name='{table}' AND name='{index}'"
                ),
            ))
            .await
            .unwrap_or_else(|e| panic!("index query failed: {e}"));
        assert!(
            !rows.is_empty(),
            "expected index '{index}' on table '{table}' to exist"
        );
    }
}

/// The rollback sweep's exclusivity check (`get_unique_nodes_for_run` /
/// `get_unique_edges_for_run`) correlates a subquery on `slug` alone. Without a
/// standalone `slug` index on each table SQLite plans that subquery as
/// `SCAN n2`, making the sweep O(n²) in the run's row count — a 40 000-row run
/// took 52.8 s in one call. These indexes are what makes it a `SEARCH`, so a
/// future migration edit must not silently drop them.
///
/// `nodes` keeps the baseline's `(dataset_id, slug)` composite alongside: it is
/// declared on Python's model too, and `slug` is its second column, so it
/// cannot serve the correlation on its own.
#[tokio::test]
async fn node_composites_carry_pythons_names_only() {
    use sea_orm::ConnectionTrait;

    // Rust and Python declare the same two composites on `nodes` under
    // different names, and either SDK may migrate a shared database first.
    // Before m20260918 a database both had touched carried two byte-identical
    // indexes over the same columns. Rust adopted Python's names because
    // Python is the reference and its names already ship.
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    for superseded in ["idx_nodes_dataset_slug", "idx_nodes_dataset_data"] {
        let rows = db
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                format!(
                    "SELECT name FROM sqlite_master \
                     WHERE type='index' AND tbl_name='nodes' AND name='{superseded}'"
                ),
            ))
            .await
            .unwrap_or_else(|e| panic!("index query failed: {e}"));
        assert!(
            rows.is_empty(),
            "'{superseded}' should have been superseded by Python's name; \
             leaving both is the duplicate this migration removes"
        );
    }
}

#[tokio::test]
async fn slug_indexes_exist() {
    use sea_orm::ConnectionTrait;

    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    for (table, index) in [
        ("nodes", "ix_nodes_slug"),
        ("edges", "ix_edges_slug"),
        // The composite the standalone index sits next to, not replaces. It
        // carries Python's name since m20260918 — the two SDKs share one
        // database and were creating the same index twice under two names.
        ("nodes", "index_node_dataset_slug"),
        ("nodes", "index_node_dataset_data"),
    ] {
        let rows = db
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                format!(
                    "SELECT name FROM sqlite_master \
                     WHERE type='index' AND tbl_name='{table}' AND name='{index}'"
                ),
            ))
            .await
            .unwrap_or_else(|e| panic!("index query failed: {e}"));
        assert!(
            !rows.is_empty(),
            "expected index '{index}' on table '{table}' to exist"
        );
    }
}

/// The standalone `slug` indexes must actually be *used* by the exclusivity
/// subquery — an index the planner ignores costs write throughput and buys
/// nothing. Assert on SQLite's plan rather than on the index's existence alone.
#[tokio::test]
async fn slug_exclusivity_subquery_uses_the_slug_index() {
    use sea_orm::ConnectionTrait;

    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");

    for (table, alias, index) in [
        ("nodes", "n2", "ix_nodes_slug"),
        ("edges", "e2", "ix_edges_slug"),
    ] {
        let plan = db
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                format!(
                    "EXPLAIN QUERY PLAN \
                     SELECT \"{table}\".\"id\" FROM \"{table}\" \
                     WHERE \"{table}\".\"pipeline_run_id\" = 'r' \
                       AND \"{table}\".\"dataset_id\" = 'd' \
                       AND NOT EXISTS (SELECT 1 FROM \"{table}\" AS \"{alias}\" \
                         WHERE \"{alias}\".\"slug\" = \"{table}\".\"slug\" \
                           AND (\"{alias}\".\"pipeline_run_id\" IS NULL \
                                OR \"{alias}\".\"pipeline_run_id\" <> 'r' \
                                OR \"{alias}\".\"dataset_id\" <> 'd')) \
                     ORDER BY \"{table}\".\"created_at\" ASC"
                ),
            ))
            .await
            .unwrap_or_else(|e| panic!("EXPLAIN QUERY PLAN failed: {e}"))
            .iter()
            .map(|row| row.try_get::<String>("", "detail").unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            plan.contains(&format!("SEARCH {alias} USING INDEX {index}")),
            "exclusivity subquery over '{table}' must use '{index}'; plan was:\n{plan}"
        );
        assert!(
            !plan.contains(&format!("SCAN {alias}")),
            "exclusivity subquery over '{table}' still full-scans; plan was:\n{plan}"
        );
    }
}
