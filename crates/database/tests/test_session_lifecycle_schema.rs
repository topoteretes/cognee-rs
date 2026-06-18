#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! LIB-03 schema tests for `session_records` and `session_model_usage`.
//!
//! Validates the migration creates both tables with all required columns,
//! the four named indexes on `session_records`, and the SeaORM entity
//! types round-trip via `find_by_id` on the composite primary keys.
//!
//! The repository trait + impl + read-time effective-status helper land
//! in LIB-05 (separate test file `test_session_lifecycle_repo.rs`).

use chrono::{TimeZone, Utc};
use cognee_database::entities::{session_model_usage, session_record};
use cognee_database::{DatabaseConnection, connect, initialize, migrator::Migrator};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseBackend, EntityTrait, Set, Statement,
    TransactionTrait,
};
use sea_orm_migration::MigratorTrait;

async fn make_db() -> DatabaseConnection {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    db
}

async fn column_names(db: &DatabaseConnection, table: &str) -> Vec<String> {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!("PRAGMA table_info({table})"),
        ))
        .await
        .unwrap_or_else(|e| panic!("PRAGMA table_info({table}) failed: {e}"));
    rows.iter()
        .map(|row| {
            row.try_get::<String>("", "name")
                .expect("PRAGMA table_info row missing 'name'")
        })
        .collect()
}

async fn index_names(db: &DatabaseConnection, table: &str) -> Vec<String> {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='{table}' AND name NOT LIKE 'sqlite_%'"
            ),
        ))
        .await
        .unwrap_or_else(|e| panic!("index query failed: {e}"));
    rows.iter()
        .map(|row| {
            row.try_get::<String>("", "name")
                .expect("sqlite_master index row missing 'name'")
        })
        .collect()
}

#[tokio::test]
async fn migration_creates_session_records_table() {
    let db = make_db().await;
    let cols = column_names(&db, "session_records").await;

    let expected = [
        "session_id",
        "user_id",
        "dataset_id",
        "status",
        "started_at",
        "last_activity_at",
        "ended_at",
        "tokens_in",
        "tokens_out",
        "cost_usd",
        "error_count",
        "last_model",
    ];
    for col in expected {
        assert!(
            cols.iter().any(|c| c == col),
            "session_records missing column '{col}' — got {cols:?}"
        );
    }
    assert_eq!(
        cols.len(),
        expected.len(),
        "session_records has unexpected extra columns: {cols:?}"
    );
}

#[tokio::test]
async fn migration_creates_session_model_usage_table() {
    let db = make_db().await;
    let cols = column_names(&db, "session_model_usage").await;

    let expected = [
        "session_id",
        "user_id",
        "model",
        "tokens_in",
        "tokens_out",
        "cost_usd",
        "updated_at",
    ];
    for col in expected {
        assert!(
            cols.iter().any(|c| c == col),
            "session_model_usage missing column '{col}' — got {cols:?}"
        );
    }
    assert_eq!(
        cols.len(),
        expected.len(),
        "session_model_usage has unexpected extra columns: {cols:?}"
    );
}

#[tokio::test]
async fn migration_creates_expected_indexes() {
    let db = make_db().await;
    let indexes = index_names(&db, "session_records").await;

    for ix in [
        "ix_session_records_user_id",
        "ix_session_records_dataset_id",
        "ix_session_records_last_activity_at",
        "ix_session_records_status",
    ] {
        assert!(
            indexes.iter().any(|i| i == ix),
            "missing index '{ix}' — got {indexes:?}"
        );
    }
}

#[tokio::test]
async fn migration_is_idempotent_under_repeat() {
    let db = make_db().await;

    // The Migrator records each migration in `seaql_migrations`. Re-running
    // the migration manually inside a transaction (so we can roll back if
    // the impl is non-idempotent without breaking later tests) must
    // succeed because every step uses `if_not_exists()`.
    let txn = db.begin().await.expect("begin txn");
    Migrator::up(&txn, None)
        .await
        .expect("re-running migrations is idempotent");
    txn.commit().await.expect("commit txn");

    // Tables remain queryable after the second run.
    let cols = column_names(&db, "session_records").await;
    assert!(cols.iter().any(|c| c == "session_id"));
    let cols = column_names(&db, "session_model_usage").await;
    assert!(cols.iter().any(|c| c == "model"));
}

#[tokio::test]
async fn roundtrip_session_record_entity() {
    let db = make_db().await;

    let session_id = "cc_test_abcdef123456".to_string();
    let user_id = "00000000000000000000000000000001".to_string();
    let dataset_id = "00000000000000000000000000000002".to_string();
    let started_at = Utc
        .with_ymd_and_hms(2026, 4, 29, 12, 0, 0)
        .single()
        .expect("valid timestamp");
    let last_activity_at = Utc
        .with_ymd_and_hms(2026, 4, 29, 12, 5, 0)
        .single()
        .expect("valid timestamp");

    let am = session_record::ActiveModel {
        session_id: Set(session_id.clone()),
        user_id: Set(user_id.clone()),
        dataset_id: Set(Some(dataset_id.clone())),
        status: Set("running".to_string()),
        started_at: Set(started_at),
        last_activity_at: Set(last_activity_at),
        ended_at: Set(None),
        tokens_in: Set(123),
        tokens_out: Set(456),
        cost_usd: Set(0.0042_f64),
        error_count: Set(0),
        last_model: Set(Some("gpt-4o-mini".to_string())),
    };
    am.insert(&db).await.expect("insert session_record");

    let row = session_record::Entity::find_by_id((session_id.clone(), user_id.clone()))
        .one(&db)
        .await
        .expect("find_by_id")
        .expect("row present");

    assert_eq!(row.session_id, session_id);
    assert_eq!(row.user_id, user_id);
    assert_eq!(row.dataset_id.as_deref(), Some(dataset_id.as_str()));
    assert_eq!(row.status, "running");
    assert_eq!(row.started_at, started_at);
    assert_eq!(row.last_activity_at, last_activity_at);
    assert!(row.ended_at.is_none());
    assert_eq!(row.tokens_in, 123);
    assert_eq!(row.tokens_out, 456);
    assert!((row.cost_usd - 0.0042).abs() < 1e-9);
    assert_eq!(row.error_count, 0);
    assert_eq!(row.last_model.as_deref(), Some("gpt-4o-mini"));

    // to_dict() preserves the Python field ordering.
    let dict = row.to_dict();
    let keys: Vec<&str> = dict
        .as_object()
        .expect("to_dict returns an object")
        .keys()
        .map(|s| s.as_str())
        .collect();
    assert_eq!(
        keys,
        vec![
            "session_id",
            "user_id",
            "dataset_id",
            "status",
            "started_at",
            "last_activity_at",
            "ended_at",
            "tokens_in",
            "tokens_out",
            "cost_usd",
            "error_count",
            "last_model",
        ]
    );
}

#[tokio::test]
async fn roundtrip_session_model_usage_entity() {
    let db = make_db().await;

    let session_id = "cc_test_zzzzzzzzzzzz".to_string();
    let user_id = "00000000000000000000000000000003".to_string();
    let model = "gpt-4o".to_string();
    let updated_at = Utc
        .with_ymd_and_hms(2026, 4, 29, 13, 0, 0)
        .single()
        .expect("valid timestamp");

    let am = session_model_usage::ActiveModel {
        session_id: Set(session_id.clone()),
        user_id: Set(user_id.clone()),
        model: Set(model.clone()),
        tokens_in: Set(11),
        tokens_out: Set(22),
        cost_usd: Set(0.01_f64),
        updated_at: Set(updated_at),
    };
    am.insert(&db).await.expect("insert session_model_usage");

    let row = session_model_usage::Entity::find_by_id((
        session_id.clone(),
        user_id.clone(),
        model.clone(),
    ))
    .one(&db)
    .await
    .expect("find_by_id")
    .expect("row present");

    assert_eq!(row.session_id, session_id);
    assert_eq!(row.user_id, user_id);
    assert_eq!(row.model, model);
    assert_eq!(row.tokens_in, 11);
    assert_eq!(row.tokens_out, 22);
    assert!((row.cost_usd - 0.01).abs() < 1e-9);
    assert_eq!(row.updated_at, updated_at);

    let dict = row.to_dict();
    let keys: Vec<&str> = dict
        .as_object()
        .expect("to_dict returns an object")
        .keys()
        .map(|s| s.as_str())
        .collect();
    assert_eq!(
        keys,
        vec![
            "session_id",
            "user_id",
            "model",
            "tokens_in",
            "tokens_out",
            "cost_usd",
            "updated_at",
        ]
    );
}
