#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `m20261029_000001_drop_run_info_data`: the `data` key leaves stored
//! `pipeline_runs.run_info`, mirroring Python's alembic `b7e4c2a9d6f1`
//! (COG-6089).
//!
//! Seeds a database migrated up to the revision before this one with rows in
//! every shape the old writers left (and a few they did not), runs the rest of
//! the chain, and checks that only the `data` key went:
//!
//! - `{"data": [...]}` and `{"data": "None"}` become `{}`;
//! - `{"data": …, "error": …}` keeps `error`;
//! - `{"data": …, "cognify_failures": …}` keeps `cognify_failures`;
//! - `{}`, `{"error": …}`, `NULL` and a non-object are left as they were;
//! - a second run (`down` then `up`) changes nothing.
//!
//! SQLite always; PostgreSQL when `DB_PROVIDER=postgres` (a scratch database
//! is created and dropped on the server).

use cognee_database::migrator::Migrator;
use cognee_database::{DatabaseConnection, connect, uuid_hex};
use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use uuid::Uuid;

/// The chain up to and excluding the run_info migration.
fn before_drop() -> u32 {
    Migrator::migrations()
        .iter()
        .position(|m| m.name().contains("drop_run_info_data"))
        .expect("the run_info migration is in the chain") as u32
}

async fn exec(db: &DatabaseConnection, sql: &str) {
    db.execute(Statement::from_string(db.get_database_backend(), sql))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Insert one `pipeline_runs` row whose `run_info` is the SQL literal
/// `run_info_sql` (already quoted, or `NULL`).
async fn insert_run(db: &DatabaseConnection, id: Uuid, status: &str, run_info_sql: &str) {
    let cast = match db.get_database_backend() {
        sea_orm::DatabaseBackend::Postgres if run_info_sql != "NULL" => "::json",
        _ => "",
    };
    exec(
        db,
        &format!(
            "INSERT INTO pipeline_runs \
             (id, created_at, status, pipeline_run_id, pipeline_name, pipeline_id, dataset_id, run_info) \
             VALUES ('{id}', '2026-01-01 00:00:00+00:00', '{status}', '{prid}', 'cognify_pipeline', \
                     '{pid}', NULL, {run_info_sql}{cast})",
            id = uuid_hex::to_hex(id),
            prid = uuid_hex::to_hex(Uuid::new_v4()),
            pid = uuid_hex::to_hex(Uuid::new_v4()),
        ),
    )
    .await;
}

/// The stored `run_info` of row `id`, parsed (`None` for SQL `NULL`).
async fn run_info(db: &DatabaseConnection, id: Uuid) -> Option<Value> {
    let backend = db.get_database_backend();
    let column = match backend {
        sea_orm::DatabaseBackend::Postgres => "run_info::text",
        _ => "run_info",
    };
    let row = db
        .query_one(Statement::from_string(
            backend,
            format!(
                "SELECT {column} AS info FROM pipeline_runs WHERE id = '{}'",
                uuid_hex::to_hex(id)
            ),
        ))
        .await
        .unwrap()
        .expect("row present");
    let text: Option<String> = row.try_get("", "info").unwrap();
    text.map(|t| serde_json::from_str(&t).unwrap_or(Value::String(t)))
}

async fn impl_drops_only_the_data_key(db: &DatabaseConnection) {
    Migrator::up(db, Some(before_drop())).await.unwrap();

    let cases: Vec<(Uuid, &str, &str, Option<Value>)> = vec![
        (
            Uuid::new_v4(),
            "DATASET_PROCESSING_STARTED",
            r#"'{"data": ["00000000-0000-0000-0000-000000000001"]}'"#,
            Some(json!({})),
        ),
        (
            Uuid::new_v4(),
            "DATASET_PROCESSING_COMPLETED",
            r#"'{"data": "None"}'"#,
            Some(json!({})),
        ),
        (
            Uuid::new_v4(),
            "DATASET_PROCESSING_ERRORED",
            r#"'{"data": "None", "error": "boom"}'"#,
            Some(json!({"error": "boom"})),
        ),
        (
            Uuid::new_v4(),
            "DATASET_PROCESSING_COMPLETED",
            r#"'{"data": ["a"], "cognify_failures": {"failure_count": 1}}'"#,
            Some(json!({"cognify_failures": {"failure_count": 1}})),
        ),
        (
            Uuid::new_v4(),
            "DATASET_PROCESSING_INITIATED",
            "'{}'",
            Some(json!({})),
        ),
        (
            Uuid::new_v4(),
            "DATASET_PROCESSING_ERRORED",
            r#"'{"error": "server_restart_orphan"}'"#,
            Some(json!({"error": "server_restart_orphan"})),
        ),
        (Uuid::new_v4(), "DATASET_PROCESSING_STARTED", "NULL", None),
        (
            Uuid::new_v4(),
            "DATASET_PROCESSING_STARTED",
            r#"'["data"]'"#,
            Some(json!(["data"])),
        ),
    ];
    for (id, status, info, _) in &cases {
        insert_run(db, *id, status, info).await;
    }

    Migrator::up(db, None).await.unwrap();
    for (id, status, info, expected) in &cases {
        assert_eq!(
            &run_info(db, *id).await,
            expected,
            "{status} row seeded with {info}"
        );
    }

    // Idempotent: running the revision again changes nothing.
    Migrator::down(db, Some(1)).await.unwrap();
    Migrator::up(db, None).await.unwrap();
    for (id, status, info, expected) in &cases {
        assert_eq!(
            &run_info(db, *id).await,
            expected,
            "{status} row seeded with {info}, after a second run"
        );
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn drops_only_the_data_key_sqlite() {
    let dir = tempfile::TempDir::new().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("run_info.db").display()
    );
    let db = connect(&url).await.unwrap();
    impl_drops_only_the_data_key(&db).await;
}

#[tokio::test]
#[serial_test::serial]
async fn drops_only_the_data_key_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    let admin = connect(&url).await.unwrap();
    let name = format!("run_info_{}", Uuid::new_v4().simple());
    exec(&admin, &format!("CREATE DATABASE {name}")).await;
    let (base, _) = url.rsplit_once('/').unwrap();
    let db = connect(&format!("{base}/{name}")).await.unwrap();
    impl_drops_only_the_data_key(&db).await;
    db.close().await.unwrap();
    exec(&admin, &format!("DROP DATABASE {name}")).await;
}
