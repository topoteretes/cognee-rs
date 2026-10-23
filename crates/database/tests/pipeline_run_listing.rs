#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `PipelineRunRepository::list_runs` and `pipeline_runs.user_id`.
//!
//! Python reference (`37f788bd2`): `get_activity_router.py:126-171` (visibility
//! `user_id IN visible OR dataset_id IN permitted`, `pipeline_name`, `ORDER BY
//! created_at DESC, id DESC LIMIT/OFFSET` in one statement) and alembic
//! `a7f3c9e1b5d2` (nullable `user_id`, index `ix_pipeline_runs_user_id`).
//!
//! The listing suite runs on in-memory SQLite and, when `TEST_POSTGRES_URL` is
//! set, on a throwaway Postgres database.

use std::sync::Arc;

use chrono::{Duration, Utc};
use cognee_database::entities::pipeline_run;
use cognee_database::migrator::Migrator;
use cognee_database::{
    DatabaseConnection, PipelineRunEntry, PipelineRunListQuery, PipelineRunRepository,
    PipelineRunStatus, PipelineRunVisibility, SeaOrmPipelineRunRepository, connect, initialize,
    ops, uuid_hex,
};
use cognee_models::Dataset;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DatabaseBackend, Set, Statement};
use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

async fn sqlite() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

async fn dataset(db: &DatabaseConnection, owner: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    ops::datasets::create_dataset(db, Dataset::new(format!("ds-{id}"), owner, None, id))
        .await
        .expect("create dataset");
    id
}

/// Insert one row `age_secs` seconds in the past (larger = older).
async fn run_row(
    db: &DatabaseConnection,
    age_secs: i64,
    name: &str,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    pipeline_run::ActiveModel {
        id: Set(uuid_hex::to_hex(id)),
        created_at: Set(Utc::now() - Duration::seconds(age_secs)),
        status: Set(pipeline_run::PipelineRunStatus::Completed),
        pipeline_run_id: Set(uuid_hex::to_hex(Uuid::new_v4())),
        pipeline_name: Set(name.to_string()),
        pipeline_id: Set(uuid_hex::to_hex(Uuid::new_v4())),
        dataset_id: Set(uuid_hex::to_hex_opt(dataset_id)),
        run_info: Set(None),
        user_id: Set(uuid_hex::to_hex_opt(user_id)),
    }
    .insert(db)
    .await
    .expect("insert run row");
    id
}

fn query(
    visibility: Option<PipelineRunVisibility>,
    limit: u64,
    offset: u64,
) -> PipelineRunListQuery {
    PipelineRunListQuery {
        visibility,
        limit,
        offset,
        ..PipelineRunListQuery::default()
    }
}

fn ids(rows: &[cognee_database::PipelineRunWithAttributionRow]) -> Vec<Uuid> {
    rows.iter().map(|r| r.id).collect()
}

/// The whole listing contract against one database.
async fn listing_suite(db: Arc<DatabaseConnection>) {
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let (caller, agent, stranger) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let readable = dataset(&db, stranger).await;
    let unreadable = dataset(&db, stranger).await;

    // Newest first: r0 is the newest.
    let r_mine_unreadable =
        run_row(&db, 1, "cognify_pipeline", Some(unreadable), Some(caller)).await;
    let r_agent = run_row(&db, 2, "add_pipeline", Some(unreadable), Some(agent)).await;
    let r_readable_other =
        run_row(&db, 3, "cognify_pipeline", Some(readable), Some(stranger)).await;
    let r_old_readable = run_row(&db, 4, "cognify_pipeline", Some(readable), None).await;
    let _r_hidden = run_row(&db, 5, "cognify_pipeline", Some(unreadable), Some(stranger)).await;
    let _r_old_hidden = run_row(&db, 6, "cognify_pipeline", Some(unreadable), None).await;
    let r_mine_no_dataset = run_row(&db, 7, "add_pipeline", None, Some(caller)).await;

    let visibility = PipelineRunVisibility {
        user_ids: vec![caller, agent],
        dataset_ids: vec![readable],
    };

    // Visibility: triggered by the caller or its agent, or on a readable
    // dataset; a NULL user_id matches only through its dataset.
    let rows = repo
        .list_runs(&query(Some(visibility.clone()), 50, 0))
        .await
        .unwrap();
    assert_eq!(
        ids(&rows),
        vec![
            r_mine_unreadable,
            r_agent,
            r_readable_other,
            r_old_readable,
            r_mine_no_dataset
        ],
        "visibility = user term OR dataset term, newest first"
    );
    assert_eq!(rows[0].user_id, Some(caller));
    assert_eq!(rows[3].user_id, None, "old rows keep a NULL user_id");
    assert_eq!(
        rows[2].owner_id,
        Some(stranger),
        "dataset attribution is joined"
    );

    // pipeline_name.
    let rows = repo
        .list_runs(&PipelineRunListQuery {
            pipeline_name: Some("add_pipeline".into()),
            ..query(Some(visibility.clone()), 50, 0)
        })
        .await
        .unwrap();
    assert_eq!(ids(&rows), vec![r_agent, r_mine_no_dataset]);

    // Explicit dataset, no visibility (the caller already authorized it).
    let rows = repo
        .list_runs(&PipelineRunListQuery {
            dataset_id: Some(readable),
            ..query(None, 50, 0)
        })
        .await
        .unwrap();
    assert_eq!(ids(&rows), vec![r_readable_other, r_old_readable]);

    // No visibility at all: every row. Empty visibility: none, no query.
    assert_eq!(repo.list_runs(&query(None, 50, 0)).await.unwrap().len(), 7);
    assert!(
        repo.list_runs(&query(Some(PipelineRunVisibility::default()), 50, 0))
            .await
            .unwrap()
            .is_empty()
    );

    // Paging is the database's: offset 1, limit 2 of the visible five.
    let rows = repo
        .list_runs(&query(Some(visibility.clone()), 2, 1))
        .await
        .unwrap();
    assert_eq!(ids(&rows), vec![r_agent, r_readable_other]);

    // The chunked path (bind limit 1: every id its own statement) answers
    // exactly what the single statement does, for every page.
    let chunked = SeaOrmPipelineRunRepository::new(Arc::clone(&db)).with_list_bind_limit(1);
    for (limit, offset) in [(50, 0), (2, 0), (2, 1), (2, 3), (1, 4), (3, 9)] {
        let single = repo
            .list_runs(&query(Some(visibility.clone()), limit, offset))
            .await
            .unwrap();
        let merged = chunked
            .list_runs(&query(Some(visibility.clone()), limit, offset))
            .await
            .unwrap();
        assert_eq!(
            ids(&merged),
            ids(&single),
            "chunked page (limit {limit}, offset {offset}) must equal the single statement"
        );
    }

    // …but only within the window cap.
    let err = chunked
        .list_runs(&query(
            Some(visibility.clone()),
            500,
            cognee_database::LIST_RUNS_CHUNKED_WINDOW,
        ))
        .await
        .expect_err("the chunked path refuses a window above the cap");
    assert!(err.to_string().contains("offset + limit"), "{err}");
}

/// Ties on `created_at` are broken by `id DESC`, so OFFSET paging never
/// re-serves or skips a row (Python `get_activity_router.py:164-167`).
async fn tie_break_suite(db: Arc<DatabaseConnection>) {
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let user = Uuid::new_v4();
    let at = Utc::now();
    let mut expected = Vec::new();
    for _ in 0..5 {
        let id = Uuid::new_v4();
        pipeline_run::ActiveModel {
            id: Set(uuid_hex::to_hex(id)),
            created_at: Set(at),
            status: Set(pipeline_run::PipelineRunStatus::Completed),
            pipeline_run_id: Set(uuid_hex::to_hex(Uuid::new_v4())),
            pipeline_name: Set("tie".into()),
            pipeline_id: Set(uuid_hex::to_hex(Uuid::new_v4())),
            dataset_id: Set(None),
            run_info: Set(None),
            user_id: Set(Some(uuid_hex::to_hex(user))),
        }
        .insert(db.as_ref())
        .await
        .unwrap();
        expected.push(id);
    }
    expected.sort_unstable_by(|a, b| b.cmp(a));
    let visibility = PipelineRunVisibility {
        user_ids: vec![user],
        dataset_ids: vec![],
    };
    let mut paged = Vec::new();
    for offset in 0..5 {
        paged.extend(ids(&repo
            .list_runs(&query(Some(visibility.clone()), 1, offset))
            .await
            .unwrap()));
    }
    assert_eq!(paged, expected);
}

/// `log_pipeline_run_entry` stores the triggering user; the legacy
/// `log_pipeline_run` stores NULL; orphan resets keep the user.
async fn user_id_write_suite(db: Arc<DatabaseConnection>) {
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let user = Uuid::new_v4();
    let ds = dataset(&db, user).await;
    let run = Uuid::new_v4();
    repo.log_pipeline_run_entry(PipelineRunEntry {
        pipeline_run_id: run,
        pipeline_id: Uuid::new_v4(),
        pipeline_name: "cognify_pipeline",
        dataset_id: Some(ds),
        status: PipelineRunStatus::Started,
        run_info: None,
        user_id: Some(user),
    })
    .await
    .unwrap();
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "legacy",
        Some(ds),
        PipelineRunStatus::Completed,
        None,
    )
    .await
    .unwrap();

    let latest = repo.get_pipeline_run(run).await.unwrap().unwrap();
    assert_eq!(latest.user_id, Some(user));

    // The Started row is an orphan; its Errored successor keeps the user.
    assert_eq!(repo.reset_orphans("restart").await.unwrap(), 1);
    let rows = repo
        .list_runs(&PipelineRunListQuery {
            dataset_id: Some(ds),
            ..query(None, 50, 0)
        })
        .await
        .unwrap();
    let by_user: Vec<_> = rows.iter().filter(|r| r.user_id == Some(user)).collect();
    assert_eq!(
        by_user.len(),
        2,
        "Started + its Errored successor: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.pipeline_name == "legacy" && r.user_id.is_none())
    );
}

#[tokio::test]
async fn listing_sqlite() {
    listing_suite(sqlite().await).await;
}

#[tokio::test]
async fn tie_break_sqlite() {
    tie_break_suite(sqlite().await).await;
}

#[tokio::test]
async fn user_id_writes_sqlite() {
    user_id_write_suite(sqlite().await).await;
}

/// The migrations from `m20261020_000001_pipeline_runs_user_id` to the end of
/// the chain: how many `Migrator::down` reverts to undo it, and where `up`
/// stops just before it. Counted, not assumed to be one: later migrations
/// (`dataset_database`, SDK-1162) sit after it.
fn migrations_from_user_id() -> u32 {
    let names: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let position = names
        .iter()
        .position(|name| name.contains("pipeline_runs_user_id"))
        .expect("the user_id migration is in the chain");
    (names.len() - position) as u32
}

/// Python's alembic `a7f3c9e1b5d2` may add `user_id` and its index to a shared
/// SQLite file first; the Rust migration must skip both instead of failing
/// with "duplicate column name". Then `down` and `up` round-trip.
#[tokio::test]
async fn user_id_migration_tolerates_python_first_and_round_trips() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let url = format!(
        "sqlite://{}?mode=rwc",
        tmp.path().join("py_first.db").display()
    );
    let db = connect(&url).await.expect("connect");
    let chain = Migrator::migrations().len() as u32;
    Migrator::up(&db, Some(chain - migrations_from_user_id()))
        .await
        .expect("everything before user_id");
    for sql in [
        "ALTER TABLE pipeline_runs ADD COLUMN user_id CHAR(32)",
        "CREATE INDEX ix_pipeline_runs_user_id ON pipeline_runs (user_id)",
    ] {
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            sql.to_string(),
        ))
        .await
        .expect("stand in for alembic");
    }
    Migrator::up(&db, None)
        .await
        .expect("the user_id migration must skip what Python already created");

    let has_column = |db: &DatabaseConnection| {
        let db = db.clone();
        async move {
            db.query_one(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT COUNT(*) AS cnt FROM pragma_table_info('pipeline_runs') WHERE name = 'user_id'"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "cnt")
            .unwrap()
        }
    };
    assert_eq!(has_column(&db).await, 1);
    Migrator::down(&db, Some(migrations_from_user_id()))
        .await
        .expect("down");
    assert_eq!(has_column(&db).await, 0, "down drops the column");
    Migrator::up(&db, None).await.expect("up again");
    assert_eq!(has_column(&db).await, 1);
}

/// The same suites on Postgres. Skipped when `TEST_POSTGRES_URL` is unset;
/// each run gets its own throwaway database.
#[tokio::test]
async fn listing_and_user_id_postgres() {
    let Some(base_url) = cognee_test_utils::test_postgres_url() else {
        eprintln!("TEST_POSTGRES_URL not set — skipping listing_and_user_id_postgres");
        return;
    };
    let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
        .await
        .expect("create temp Postgres database");
    let db = connect(tmp.url()).await.expect("connect to temp Postgres");
    initialize(&db).await.expect("migrate");
    let db = Arc::new(db);
    listing_suite(Arc::clone(&db)).await;
    tie_break_suite(Arc::clone(&db)).await;
    user_id_write_suite(Arc::clone(&db)).await;

    // down / up of the user_id migration on Postgres.
    Migrator::down(db.as_ref(), Some(migrations_from_user_id()))
        .await
        .expect("down");
    Migrator::up(db.as_ref(), None).await.expect("up again");
    drop(db);
    tmp.cleanup().await;
}
