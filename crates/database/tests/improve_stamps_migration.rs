#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `m20261031_000001_improve_stamps_to_run_info`: the improve stage
//! watermarks move from `graph_sync_checkpoints` to the `improve` operation
//! rows' `run_info` (Python `cognee/modules/improve/graph_changes.py:46-48`),
//! and the table is dropped.
//!
//! SQLite always; PostgreSQL when `DB_PROVIDER=postgres` (a scratch database
//! is created and dropped on the server).

use chrono::{DateTime, Utc};
use cognee_database::migrator::Migrator;
use cognee_database::{DatabaseConnection, RelationalDb, connect, uuid_hex};
use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::MigratorTrait;
use std::sync::Arc;
use uuid::Uuid;

fn before_move() -> u32 {
    Migrator::migrations()
        .iter()
        .position(|m| m.name().contains("improve_stamps_to_run_info"))
        .expect("the stamps migration is in the chain") as u32
}

async fn exec(db: &DatabaseConnection, sql: &str) {
    db.execute(Statement::from_string(db.get_database_backend(), sql))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn table_exists(db: &DatabaseConnection, table: &str) -> bool {
    let backend = db.get_database_backend();
    let sql = match backend {
        sea_orm::DatabaseBackend::Postgres => format!(
            "SELECT COUNT(*) AS cnt FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = '{table}'"
        ),
        _ => format!(
            "SELECT COUNT(*) AS cnt FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
        ),
    };
    let row = db
        .query_one(Statement::from_string(backend, sql))
        .await
        .unwrap()
        .unwrap();
    row.try_get::<i64>("", "cnt").unwrap() > 0
}

async fn impl_moves_the_stamps_and_drops_the_table(db: Arc<DatabaseConnection>) {
    Migrator::up(&*db, Some(before_move())).await.unwrap();
    assert!(table_exists(&db, "graph_sync_checkpoints").await);

    let dataset = Uuid::new_v4();
    let gone = Uuid::new_v4();
    exec(
        &db,
        &format!(
            "INSERT INTO datasets (id, name, owner_id, created_at) \
             VALUES ('{}', 'main', '{}', '2026-01-01 00:00:00+00:00')",
            uuid_hex::to_hex(dataset),
            uuid_hex::to_hex(Uuid::new_v4())
        ),
    )
    .await;
    let stamped_at: DateTime<Utc> = "2026-03-04T05:06:07+00:00".parse().unwrap();
    for (key, ts) in [
        (
            format!("improve_stage:global_context_index:{dataset}"),
            "2026-03-04 05:06:07+00:00",
        ),
        // A dataset that no longer exists, a foreign key and a malformed one.
        (
            format!("improve_stage:global_context_index:{gone}"),
            "2026-03-04 05:06:07+00:00",
        ),
        ("other_key".to_string(), "2026-03-04 05:06:07+00:00"),
        (
            "improve_stage:global_context_index:nope".to_string(),
            "2026-03-04 05:06:07+00:00",
        ),
    ] {
        exec(
            &db,
            &format!("INSERT INTO graph_sync_checkpoints (key, ts) VALUES ('{key}', '{ts}')"),
        )
        .await;
    }

    Migrator::up(&*db, None).await.unwrap();
    assert!(!table_exists(&db, "graph_sync_checkpoints").await);

    let runs = Arc::clone(&db).pipeline_runs();
    // The migrated stamp reads back like one a run wrote.
    let stamped = runs
        .latest_stamped_run(dataset, "improve", "global_context_index")
        .await
        .unwrap()
        .expect("the stamp moved");
    assert_eq!(stamped.stamp["status"], "completed");
    let started: DateTime<Utc> = stamped.stamp["started_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(started, stamped_at);
    // Nothing else was carried over.
    assert!(
        runs.latest_stamped_run(gone, "improve", "global_context_index")
            .await
            .unwrap()
            .is_none()
    );
    // It is not a change after itself.
    assert_eq!(
        runs.count_runs_created_after(started, Some(dataset), &[], &[stamped.pipeline_run_id])
            .await
            .unwrap(),
        0
    );

    // `down` brings back the (empty) table; `up` drops it again.
    Migrator::down(&*db, Some(1)).await.unwrap();
    assert!(table_exists(&db, "graph_sync_checkpoints").await);
    Migrator::up(&*db, None).await.unwrap();
    assert!(!table_exists(&db, "graph_sync_checkpoints").await);
    assert!(
        runs.latest_stamped_run(dataset, "improve", "global_context_index")
            .await
            .unwrap()
            .is_some()
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn moves_the_stamps_and_drops_the_table_sqlite() {
    let dir = tempfile::TempDir::new().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("stamps.db").display()
    );
    let db = Arc::new(connect(&url).await.unwrap());
    impl_moves_the_stamps_and_drops_the_table(db).await;
}

#[tokio::test]
#[serial_test::serial]
async fn moves_the_stamps_and_drops_the_table_pg() {
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    let admin = connect(&url).await.unwrap();
    let name = format!("stamps_{}", Uuid::new_v4().simple());
    exec(&admin, &format!("CREATE DATABASE {name}")).await;
    let (base, _) = url.rsplit_once('/').unwrap();
    let db = Arc::new(connect(&format!("{base}/{name}")).await.unwrap());
    impl_moves_the_stamps_and_drops_the_table(Arc::clone(&db)).await;
    drop(db);
    exec(&admin, &format!("DROP DATABASE {name}")).await;
}
