#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Migration compatibility tests.
//!
//! Verifies that `initialize()` (which runs SeaORM migrations) behaves
//! correctly in idempotent, fresh, and data-preserving scenarios.
//!
//! Each test is instantiated twice: once with SQLite and once with PostgreSQL.
//! The PostgreSQL variant is skipped automatically when the `DB_PROVIDER`
//! environment variable is not set to `"postgres"`.

use cognee_database::migrator::Migrator;

use cognee_database::{connect, initialize};
use sea_orm::ConnectionTrait;

/// Return all user-visible table names using a backend-aware query.
async fn table_names(db: &cognee_database::DatabaseConnection) -> Vec<String> {
    use sea_orm::DatabaseBackend;
    let (sql, col): (&str, &str) = match db.get_database_backend() {
        DatabaseBackend::Sqlite => (
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            "name",
        ),
        DatabaseBackend::Postgres => (
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
            "table_name",
        ),
        _ => panic!("unsupported database backend"),
    };
    let rows = db
        .query_all(sea_orm::Statement::from_string(
            db.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("table_names query failed");

    rows.iter()
        .map(|row| row.try_get::<String>("", col).unwrap())
        .collect()
}

// ---------------------------------------------------------------------------
// D1.1 — initialize() twice must not error; data table must still exist
// ---------------------------------------------------------------------------

async fn impl_migration_is_idempotent(url: &str) {
    let db = connect(url).await.expect("connect");
    initialize(&db).await.expect("first initialize");
    initialize(&db)
        .await
        .expect("second initialize (idempotent)");

    let tables = table_names(&db).await;
    assert!(
        tables.iter().any(|t| t == "data"),
        "data table missing after double initialize — tables: {tables:?}"
    );
}

#[tokio::test]
async fn migration_is_idempotent_sqlite() {
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let db_path = tmp.path().join("idempotent.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    impl_migration_is_idempotent(&url).await;
}

#[tokio::test]
#[serial_test::serial]
async fn migration_is_idempotent_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_migration_is_idempotent(&url).await;
}

// ---------------------------------------------------------------------------
// D1.2 — After initialize(), every expected table must be present
// ---------------------------------------------------------------------------

async fn impl_migration_creates_all_tables(url: &str) {
    let db = connect(url).await.expect("connect");
    initialize(&db).await.expect("initialize");

    let tables = table_names(&db).await;

    let expected_tables = [
        "datasets",
        "data",
        "dataset_data",
        "queries",
        "results",
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

#[tokio::test]
async fn migration_from_empty_db_sqlite() {
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let db_path = tmp.path().join("fresh.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    impl_migration_creates_all_tables(&url).await;
}

#[tokio::test]
#[serial_test::serial]
async fn migration_from_empty_db_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_migration_creates_all_tables(&url).await;
}

// ---------------------------------------------------------------------------
// D1.3 — Data inserted before re-initialize() must survive
// ---------------------------------------------------------------------------

async fn impl_migration_preserves_existing_data(url: &str) {
    let db = connect(url).await.expect("connect");
    initialize(&db).await.expect("first initialize");

    // Insert a row into `datasets` using raw SQL with a unique UUID.
    let dataset_id = uuid::Uuid::new_v4().to_string();
    let owner_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    db.execute(sea_orm::Statement::from_string(
        db.get_database_backend(),
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
            db.get_database_backend(),
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

#[tokio::test]
async fn migration_preserves_existing_data_sqlite() {
    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let db_path = tmp.path().join("preserve.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    impl_migration_preserves_existing_data(&url).await;
}

#[tokio::test]
#[serial_test::serial]
async fn migration_preserves_existing_data_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    impl_migration_preserves_existing_data(&url).await;
}

// ---------------------------------------------------------------------------
// D1.4 — Baseline creates all 30 relational tables + seeds 4 permissions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baseline_creates_full_table_set_sqlite() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    let tables = table_names(&db).await;
    for t in [
        "datasets",
        "data",
        "dataset_data",
        "queries",
        "results",
        "nodes",
        "edges",
        "pipeline_runs",
        "task_runs",
        "graph_metrics",
        "principals",
        "permissions",
        "acls",
        "tenants",
        "users",
        "roles",
        "user_tenants",
        "user_roles",
        "graph_sync_checkpoints",
        "user_api_key",
        "role_default_permissions",
        "user_default_permissions",
        "tenant_default_permissions",
        "principal_configuration",
        "sync_operations",
        "notebooks",
        "pipeline_run_payload_fields",
        "session_records",
        "session_model_usage",
        "dataset_configurations",
    ] {
        assert!(
            tables.iter().any(|x| x == t),
            "missing table {t}: {tables:?}"
        );
    }
    // Seed parity: 4 permissions must exist.
    let perms = db
        .query_all(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT name FROM permissions".to_string(),
        ))
        .await
        .expect("query permissions");
    assert_eq!(perms.len(), 4, "expected 4 seeded permissions");
    // Default user must exist.
    let users = db
        .query_all(sea_orm::Statement::from_string(
            db.get_database_backend(),
            "SELECT id FROM users WHERE id = '00000000000000000000000000000000'".to_string(),
        ))
        .await
        .expect("query default user");
    assert_eq!(users.len(), 1, "default user seed row missing");
}

// ---------------------------------------------------------------------------
// D1.5 — Single-migration invariant
// ---------------------------------------------------------------------------

#[test]
fn relational_chain_has_one_migration() {
    use sea_orm_migration::MigratorTrait;
    assert_eq!(
        Migrator::migrations().len(),
        1,
        "relational chain must have exactly one baseline migration"
    );
}
