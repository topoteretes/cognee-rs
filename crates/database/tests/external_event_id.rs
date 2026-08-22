#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use cognee_database::migrator::Migrator;
use cognee_database::{connect, ops};
use cognee_models::{Data, Dataset};
use sea_orm::ConnectionTrait;
use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

async fn old_database_with_membership() -> (cognee_database::DatabaseConnection, Uuid, Uuid) {
    let db = connect("sqlite::memory:").await.expect("connect");
    Migrator::up(&db, Some(1)).await.expect("old baseline");

    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();
    let data_id = Uuid::new_v4();
    ops::datasets::create_dataset(
        &db,
        Dataset::new("agent_sessions".into(), owner_id, None, dataset_id),
    )
    .await
    .expect("dataset");
    ops::data::create_data(
        &db,
        Data::builder(
            data_id,
            "before-migration",
            "file:///before-migration.txt",
            "file:///before-migration.txt",
            "txt",
            "text/plain",
            "before-migration-hash",
            owner_id,
        )
        .build(),
    )
    .await
    .expect("data");
    db.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Sqlite,
        "INSERT INTO dataset_data (dataset_id, data_id, created_at) VALUES (?, ?, CURRENT_TIMESTAMP)",
        [
            cognee_database::uuid_hex::to_hex(dataset_id).into(),
            cognee_database::uuid_hex::to_hex(data_id).into(),
        ],
    ))
    .await
    .expect("old-schema membership");

    (db, dataset_id, data_id)
}

#[tokio::test]
async fn external_event_migration_preserves_old_rows_and_is_idempotent() {
    let (db, dataset_id, data_id) = old_database_with_membership().await;

    Migrator::up(&db, None).await.expect("upgrade");
    Migrator::up(&db, None).await.expect("repeat upgrade");

    let columns = db
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA table_info(dataset_data)".to_string(),
        ))
        .await
        .expect("table info");
    assert!(
        columns
            .iter()
            .any(|row| row.try_get::<String>("", "name").unwrap() == "external_event_id"),
        "the forward migration must add the nullable external_event_id column"
    );

    let row = db
        .query_one(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT data_id, external_event_id FROM dataset_data WHERE dataset_id = ?",
            [cognee_database::uuid_hex::to_hex(dataset_id).into()],
        ))
        .await
        .expect("membership query")
        .expect("pre-existing membership");
    assert_eq!(
        row.try_get::<String>("", "data_id").unwrap(),
        cognee_database::uuid_hex::to_hex(data_id)
    );
    assert_eq!(
        row.try_get::<Option<String>>("", "external_event_id")
            .unwrap(),
        None,
        "old memberships must remain readable with a null event key"
    );
}

#[tokio::test]
async fn external_event_index_is_unique_per_dataset_but_allows_nulls() {
    let (db, _dataset_id, _data_id) = old_database_with_membership().await;
    Migrator::up(&db, None).await.expect("upgrade");

    let indexes = db
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA index_list(dataset_data)".to_string(),
        ))
        .await
        .expect("index list");
    let index = indexes
        .iter()
        .find(|row| row.try_get::<String>("", "name").unwrap() == "idx_dataset_data_external_event")
        .expect("external event index");
    assert_eq!(index.try_get::<i32>("", "unique").unwrap(), 1);

    let fields = db
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA index_info(idx_dataset_data_external_event)".to_string(),
        ))
        .await
        .expect("index fields");
    let names: Vec<String> = fields
        .iter()
        .map(|row| row.try_get("", "name").unwrap())
        .collect();
    assert_eq!(names, ["dataset_id", "external_event_id"]);
}

#[tokio::test]
async fn external_event_membership_is_exact_once_and_dataset_scoped() {
    let db = connect("sqlite::memory:").await.expect("connect");
    Migrator::up(&db, None).await.expect("migrate");

    let owner_id = Uuid::new_v4();
    let first_dataset_id = Uuid::new_v4();
    let second_dataset_id = Uuid::new_v4();
    let first_data_id = Uuid::new_v4();
    let second_data_id = Uuid::new_v4();

    for (name, dataset_id) in [
        ("agent_sessions", first_dataset_id),
        ("other_sessions", second_dataset_id),
    ] {
        ops::datasets::create_dataset(&db, Dataset::new(name.into(), owner_id, None, dataset_id))
            .await
            .expect("dataset");
    }
    for (data_id, suffix) in [(first_data_id, "first"), (second_data_id, "second")] {
        ops::data::create_data(
            &db,
            Data::builder(
                data_id,
                suffix,
                format!("file:///{suffix}.txt"),
                format!("file:///{suffix}.txt"),
                "txt",
                "text/plain",
                format!("{suffix}-hash"),
                owner_id,
            )
            .build(),
        )
        .await
        .expect("data");
    }

    ops::datasets::attach_data_to_dataset_with_external_event(
        &db,
        first_dataset_id,
        first_data_id,
        Some("evt-123"),
    )
    .await
    .expect("first attach");
    ops::datasets::attach_data_to_dataset_with_external_event(
        &db,
        first_dataset_id,
        first_data_id,
        Some("evt-123"),
    )
    .await
    .expect("identical replay");

    assert_eq!(
        ops::datasets::get_data_id_for_external_event(&db, first_dataset_id, "evt-123")
            .await
            .expect("lookup"),
        Some(first_data_id)
    );
    assert!(
        ops::datasets::contains_external_event(&db, first_dataset_id, "evt-123")
            .await
            .expect("contains")
    );
    assert_eq!(
        ops::datasets::count_dataset_data(&db, first_dataset_id)
            .await
            .expect("count"),
        1
    );

    let conflict = ops::datasets::attach_data_to_dataset_with_external_event(
        &db,
        first_dataset_id,
        second_data_id,
        Some("evt-123"),
    )
    .await
    .expect_err("same event with different data must conflict");
    assert!(matches!(
        conflict,
        cognee_database::DatabaseError::UniqueViolation(_)
    ));

    ops::datasets::attach_data_to_dataset_with_external_event(
        &db,
        second_dataset_id,
        second_data_id,
        Some("evt-123"),
    )
    .await
    .expect("same event is valid in a different dataset");
    assert!(
        ops::datasets::contains_external_event(&db, second_dataset_id, "evt-123")
            .await
            .expect("second dataset contains")
    );
}
