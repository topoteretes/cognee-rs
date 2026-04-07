//! Migration compatibility tests.
//!
//! Verifies that `initialize()` (which runs SeaORM migrations) behaves
//! correctly in idempotent, fresh, and data-preserving scenarios when
//! pointed at a file-backed SQLite database.

use cognee_database::{connect, initialize};
use sea_orm::ConnectionTrait;

/// Helper: query `sqlite_master` for all user table names.
async fn table_names(db: &cognee_database::DatabaseConnection) -> Vec<String> {
    let rows = db
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
                .to_owned(),
        ))
        .await
        .expect("sqlite_master query failed");

    rows.iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect()
}

/// D1.1 — Calling `initialize()` twice on the same database must not error,
/// and the `data` table must still exist afterward.
#[tokio::test]
async fn migration_is_idempotent() {
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let db_path = tmp.path().join("idempotent.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    let db = connect(&url).await.expect("connect");

    // First initialization — creates all tables.
    initialize(&db).await.expect("first initialize");

    // Second initialization — must succeed without error.
    initialize(&db)
        .await
        .expect("second initialize (idempotent)");

    // Verify the `data` table still exists.
    let tables = table_names(&db).await;
    assert!(
        tables.iter().any(|t| t == "data"),
        "data table missing after double initialize — tables: {tables:?}"
    );
}

/// D1.2 — Starting from a completely empty SQLite file, a single
/// `initialize()` must create every expected table.
#[tokio::test]
async fn migration_from_empty_db() {
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let db_path = tmp.path().join("fresh.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    let db = connect(&url).await.expect("connect");
    initialize(&db).await.expect("initialize");

    let tables = table_names(&db).await;

    let expected_tables = [
        "datasets",
        "data",
        "dataset_data",
        "queries",
        "results",
        "artifact_references",
        "nodes",
        "edges",
        "pipeline_runs",
        "task_runs",
        "graph_metrics",
    ];

    for table in expected_tables {
        assert!(
            tables.iter().any(|t| t == table),
            "expected table '{table}' missing after initialize — tables: {tables:?}"
        );
    }
}

/// D1.3 — Data inserted before a second `initialize()` call must survive
/// the re-initialization (migrations must not drop/recreate tables).
#[tokio::test]
async fn migration_preserves_existing_data() {
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let db_path = tmp.path().join("preserve.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    let db = connect(&url).await.expect("connect");
    initialize(&db).await.expect("first initialize");

    // Insert a row into `datasets` using raw SQL.
    let dataset_id = uuid::Uuid::new_v4().to_string();
    let owner_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    db.execute(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        format!(
            "INSERT INTO datasets (id, name, owner_id, created_at) \
             VALUES ('{dataset_id}', 'test-dataset', '{owner_id}', '{now}')"
        ),
    ))
    .await
    .expect("insert dataset row");

    // Re-initialize — must not destroy the row.
    initialize(&db).await.expect("second initialize");

    // Verify the row is still present.
    let rows = db
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            format!("SELECT id, name FROM datasets WHERE id = '{dataset_id}'"),
        ))
        .await
        .expect("select dataset row");

    assert_eq!(
        rows.len(),
        1,
        "expected 1 row for dataset {dataset_id}, got {}",
        rows.len()
    );

    let name: String = rows[0].try_get("", "name").unwrap();
    assert_eq!(name, "test-dataset", "dataset name mismatch after re-init");
}
