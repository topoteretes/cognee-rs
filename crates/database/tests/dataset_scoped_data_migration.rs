#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `m20261028_000001_dataset_scoped_data`: the backfill of `data.dataset_id`
//! from `dataset_data`, mirroring Python's alembic `d6e8f0a2b4c6`.
//!
//! Each scenario seeds a database migrated up to the revision before this one
//! (so `dataset_data` still exists and `data` has no `dataset_id`), runs the
//! rest of the chain, and checks the result:
//!
//! - a row in one dataset keeps its id and is stamped;
//! - a row shared by several datasets: the oldest membership keeps the id,
//!   every other dataset gets a uuid4 copy with `legacy_id` = the original,
//!   and the `nodes` **and** `edges` ledger rows of those datasets follow it;
//! - a row with no membership is left alone;
//! - `dataset_data` is dropped; `down` restores it from `data.dataset_id`.
//!
//! SQLite always; PostgreSQL when `DB_PROVIDER=postgres` (a scratch database
//! is created and dropped on the server).

use cognee_database::migrator::Migrator;
use cognee_database::{DatabaseConnection, connect, uuid_hex};
use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

/// The chain up to and excluding the dataset-scoping migration.
fn before_scoping() -> u32 {
    let names: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    names
        .iter()
        .position(|name| name.contains("dataset_scoped_data"))
        .expect("the dataset-scoping migration is in the chain") as u32
}

async fn exec(db: &DatabaseConnection, sql: &str) {
    db.execute(Statement::from_string(db.get_database_backend(), sql))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn rows(db: &DatabaseConnection, sql: &str) -> Vec<sea_orm::QueryResult> {
    db.query_all(Statement::from_string(db.get_database_backend(), sql))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

async fn table_exists(db: &DatabaseConnection, table: &str) -> bool {
    let sql = match db.get_database_backend() {
        sea_orm::DatabaseBackend::Postgres => format!(
            "SELECT COUNT(*) AS cnt FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = '{table}'"
        ),
        _ => format!(
            "SELECT COUNT(*) AS cnt FROM sqlite_master WHERE type='table' AND name='{table}'"
        ),
    };
    rows(db, &sql).await[0].try_get::<i64>("", "cnt").unwrap() > 0
}

fn hex(id: Uuid) -> String {
    uuid_hex::to_hex(id)
}

struct DataRow {
    id: String,
    dataset_id: Option<String>,
    legacy_id: Option<String>,
    name: String,
    content_hash: String,
    token_count: i64,
}

async fn data_rows(db: &DatabaseConnection) -> Vec<DataRow> {
    rows(
        db,
        "SELECT id, dataset_id, legacy_id, name, content_hash, token_count FROM data ORDER BY id",
    )
    .await
    .iter()
    .map(|r| DataRow {
        id: r.try_get("", "id").unwrap(),
        dataset_id: r.try_get("", "dataset_id").unwrap(),
        legacy_id: r.try_get("", "legacy_id").unwrap(),
        name: r.try_get("", "name").unwrap(),
        content_hash: r.try_get("", "content_hash").unwrap(),
        token_count: r.try_get("", "token_count").unwrap(),
    })
    .collect()
}

async fn insert_dataset(db: &DatabaseConnection, id: Uuid, name: &str, owner: Uuid) {
    exec(
        db,
        &format!(
            "INSERT INTO datasets (id, name, owner_id, created_at) \
             VALUES ('{}', '{name}', '{}', '2026-01-01 00:00:00+00:00')",
            hex(id),
            hex(owner)
        ),
    )
    .await;
}

async fn insert_data(db: &DatabaseConnection, id: Uuid, name: &str, owner: Uuid) {
    exec(
        db,
        &format!(
            "INSERT INTO data (id, name, raw_data_location, original_data_location, extension, \
             mime_type, content_hash, owner_id, created_at, token_count, data_size) \
             VALUES ('{}', '{name}', 'file:///s/{name}', 'file:///o/{name}', 'txt', \
             'text/plain', 'hash-{name}', '{}', '2026-01-01 00:00:00+00:00', 7, 11)",
            hex(id),
            hex(owner)
        ),
    )
    .await;
}

async fn insert_membership(db: &DatabaseConnection, dataset: Uuid, data: Uuid, at: &str) {
    exec(
        db,
        &format!(
            "INSERT INTO dataset_data (dataset_id, data_id, created_at) \
             VALUES ('{}', '{}', '{at}')",
            hex(dataset),
            hex(data)
        ),
    )
    .await;
}

/// One `nodes` and one `edges` ledger row of `data` in `dataset`.
async fn insert_ledger(db: &DatabaseConnection, dataset: Uuid, data: Uuid, user: Uuid) {
    let node = hex(Uuid::new_v4());
    exec(
        db,
        &format!(
            "INSERT INTO nodes (id, slug, user_id, data_id, dataset_id, type, indexed_fields, \
             created_at) VALUES ('{node}', '{}', '{}', '{}', '{}', 'Entity', '[]', \
             '2026-01-01 00:00:00+00:00')",
            hex(Uuid::new_v4()),
            hex(user),
            hex(data),
            hex(dataset)
        ),
    )
    .await;
    exec(
        db,
        &format!(
            "INSERT INTO edges (id, slug, user_id, data_id, dataset_id, source_node_id, \
             destination_node_id, relationship_name, created_at) VALUES ('{}', '{}', '{}', \
             '{}', '{}', '{node}', '{node}', 'is_a', '2026-01-01 00:00:00+00:00')",
            hex(Uuid::new_v4()),
            hex(Uuid::new_v4()),
            hex(user),
            hex(data),
            hex(dataset)
        ),
    )
    .await;
}

/// The `data_id` of every `table` row of `dataset`.
async fn ledger_data_ids(db: &DatabaseConnection, table: &str, dataset: Uuid) -> Vec<String> {
    rows(
        db,
        &format!(
            "SELECT data_id FROM {table} WHERE dataset_id = '{}'",
            hex(dataset)
        ),
    )
    .await
    .iter()
    .map(|r| r.try_get("", "data_id").unwrap())
    .collect()
}

async fn impl_backfill_stamps_sole_rows_and_forks_shared_rows(db: &DatabaseConnection) {
    Migrator::up(db, Some(before_scoping())).await.unwrap();

    let owner = Uuid::new_v4();
    let (ds1, ds2, ds3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    for (id, name) in [(ds1, "ds1"), (ds2, "ds2"), (ds3, "ds3")] {
        insert_dataset(db, id, name, owner).await;
    }
    let (sole, shared, orphan) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    insert_data(db, sole, "sole", owner).await;
    insert_data(db, shared, "shared", owner).await;
    insert_data(db, orphan, "orphan", owner).await;

    insert_membership(db, ds2, sole, "2026-01-02 00:00:00+00:00").await;
    // ds3 is the OLDEST membership of the shared row, so it keeps the id even
    // though ds1 sorts first by id and by insertion.
    insert_membership(db, ds1, shared, "2026-01-03 00:00:00+00:00").await;
    insert_membership(db, ds2, shared, "2026-01-04 00:00:00+00:00").await;
    insert_membership(db, ds3, shared, "2026-01-01 00:00:00+00:00").await;
    for ds in [ds1, ds2, ds3] {
        insert_ledger(db, ds, shared, owner).await;
    }
    insert_ledger(db, ds2, sole, owner).await;

    Migrator::up(db, None).await.unwrap();

    assert!(
        !table_exists(db, "dataset_data").await,
        "dataset_data is dropped"
    );

    let all = data_rows(db).await;
    assert_eq!(all.len(), 5, "3 original rows + 2 forks");
    let by_id = |id: Uuid| all.iter().find(|r| r.id == hex(id)).unwrap();

    // Sole membership: same id, stamped.
    assert_eq!(by_id(sole).dataset_id.as_deref(), Some(hex(ds2).as_str()));
    assert_eq!(by_id(sole).legacy_id, None);
    // Orphan: untouched.
    assert_eq!(by_id(orphan).dataset_id, None);
    // Shared: the oldest membership (ds3) keeps the id.
    assert_eq!(by_id(shared).dataset_id.as_deref(), Some(hex(ds3).as_str()));
    assert_eq!(by_id(shared).legacy_id, None);

    for fork_ds in [ds1, ds2] {
        let fork = all
            .iter()
            .find(|r| {
                r.legacy_id.as_deref() == Some(hex(shared).as_str())
                    && r.dataset_id.as_deref() == Some(hex(fork_ds).as_str())
            })
            .unwrap_or_else(|| panic!("a fork of the shared row for {fork_ds}"));
        let fork_id = uuid_hex::from_hex(&fork.id).unwrap();
        assert_eq!(fork_id.get_version_num(), 4, "a fork gets a fresh uuid4");
        assert_eq!(fork.name, "shared", "every column is copied");
        assert_eq!(fork.content_hash, "hash-shared");
        assert_eq!(fork.token_count, 7);

        for table in ["nodes", "edges"] {
            let ids = ledger_data_ids(db, table, fork_ds).await;
            assert!(
                ids.contains(&fork.id) && !ids.contains(&hex(shared)),
                "{table} rows of {fork_ds} follow the fork: {ids:?}"
            );
        }
    }
    for table in ["nodes", "edges"] {
        assert_eq!(
            ledger_data_ids(db, table, ds3).await,
            vec![hex(shared)],
            "the keeper's {table} rows keep the original id"
        );
        assert!(
            ledger_data_ids(db, table, ds2).await.contains(&hex(sole)),
            "a sole row's {table} rows are untouched"
        );
    }

    // `down` restores one membership per scoped row; `up` again is a no-op.
    let chain = Migrator::migrations().len() as u32;
    Migrator::down(db, Some(chain - before_scoping()))
        .await
        .unwrap();
    assert!(table_exists(db, "dataset_data").await);
    let memberships = rows(db, "SELECT COUNT(*) AS cnt FROM dataset_data").await[0]
        .try_get::<i64>("", "cnt")
        .unwrap();
    assert_eq!(
        memberships, 4,
        "sole + keeper + 2 forks; the orphan has none"
    );
    Migrator::up(db, None).await.unwrap();
    assert!(!table_exists(db, "dataset_data").await);
    assert_eq!(data_rows(db).await.len(), 5, "a re-run forks nothing");
}

/// A database Python already migrated: rows carry `dataset_id`, and
/// `dataset_data` (created empty by Rust's baseline) has nothing to add.
async fn impl_already_scoped_rows_are_left_alone(db: &DatabaseConnection) {
    Migrator::up(db, Some(before_scoping())).await.unwrap();
    let owner = Uuid::new_v4();
    let ds = Uuid::new_v4();
    insert_dataset(db, ds, "py", owner).await;
    let row = Uuid::new_v4();
    insert_data(db, row, "py_row", owner).await;
    // Python's own columns, as alembic created them.
    // SQLAlchemy's `Uuid` is CHAR(32) on SQLite; on PostgreSQL this stands in
    // with `text` (Rust and Python do not share a PostgreSQL schema).
    let uuid_type = match db.get_database_backend() {
        sea_orm::DatabaseBackend::Postgres => "TEXT",
        _ => "CHAR(32)",
    };
    for column in ["dataset_id", "legacy_id"] {
        exec(
            db,
            &format!("ALTER TABLE data ADD COLUMN {column} {uuid_type}"),
        )
        .await;
    }
    exec(
        db,
        &format!(
            "UPDATE data SET dataset_id = '{}' WHERE id = '{}'",
            hex(ds),
            hex(row)
        ),
    )
    .await;

    Migrator::up(db, None).await.unwrap();

    let all = data_rows(db).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].dataset_id.as_deref(), Some(hex(ds).as_str()));
    assert!(!table_exists(db, "dataset_data").await);
}

#[cfg(feature = "sqlite")]
fn sqlite_url(dir: &tempfile::TempDir) -> String {
    format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("scoped.db").display()
    )
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn backfill_stamps_sole_rows_and_forks_shared_rows_sqlite() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = connect(&sqlite_url(&dir)).await.unwrap();
    impl_backfill_stamps_sole_rows_and_forks_shared_rows(&db).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn already_scoped_rows_are_left_alone_sqlite() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = connect(&sqlite_url(&dir)).await.unwrap();
    impl_already_scoped_rows_are_left_alone(&db).await;
}

/// A scratch database on the `DB_PROVIDER=postgres` server, dropped after.
async fn with_pg_scratch<F, Fut>(run: F)
where
    F: FnOnce(DatabaseConnection) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let Some(url) = cognee_test_utils::pg_test_url() else {
        return;
    };
    let admin = connect(&url).await.unwrap();
    let name = format!("scoped_{}", Uuid::new_v4().simple());
    exec(&admin, &format!("CREATE DATABASE {name}")).await;
    let (base, _) = url.rsplit_once('/').unwrap();
    let db = connect(&format!("{base}/{name}")).await.unwrap();
    run(db.clone()).await;
    db.close().await.unwrap();
    exec(&admin, &format!("DROP DATABASE {name}")).await;
}

#[tokio::test]
#[serial_test::serial]
async fn backfill_stamps_sole_rows_and_forks_shared_rows_pg() {
    with_pg_scratch(|db| async move {
        impl_backfill_stamps_sole_rows_and_forks_shared_rows(&db).await;
    })
    .await;
}

#[tokio::test]
#[serial_test::serial]
async fn already_scoped_rows_are_left_alone_pg() {
    with_pg_scratch(|db| async move {
        impl_already_scoped_rows_are_left_alone(&db).await;
    })
    .await;
}
