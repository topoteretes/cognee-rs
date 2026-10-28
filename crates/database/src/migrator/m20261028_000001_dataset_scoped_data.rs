//! Dataset-scoped `data` rows; retires the `dataset_data` membership table.
//!
//! Python made `data` rows dataset-scoped (`Data.dataset_id`, `Data.legacy_id`)
//! and dropped `dataset_data` in alembic `d6e8f0a2b4c6`
//! (`backfill_dataset_scoped_data_drop_dataset_data`). This migration does the
//! same to a Rust database, so both SDKs read one membership model:
//!
//! 1. Adds `data.dataset_id` and `data.legacy_id` (nullable, indexed) and
//!    Python's dedup index `data_dataset_content_lookup`. A database Python
//!    migrated already has them, so each is added only when missing.
//! 2. Backfills every row whose `dataset_id` is still `NULL` from its
//!    `dataset_data` memberships, exactly as Python does:
//!    - **one membership:** the row keeps its id and is stamped with that
//!      dataset (one set-based `UPDATE`);
//!    - **several memberships:** the **oldest** membership (by
//!      `dataset_data.created_at`, ties by dataset id) keeps the id; every
//!      other dataset gets a copy of the row with a fresh `uuid4` id, its own
//!      `dataset_id`, and `legacy_id` = the original id. The relational
//!      provenance ledger is repointed for those datasets: `nodes.data_id`
//!      **and** `edges.data_id` (Python's ledger has only `nodes`; Rust's
//!      also records edges per data item, and its delete path reads both).
//!      The graph and vector stores still carry the original id in their
//!      document nodes until the item is re-cognified;
//!    - **no membership:** an unreachable orphan, left untouched.
//! 3. Drops `dataset_data`. Membership is now exactly `data.dataset_id`.
//!
//! The backfill and the drop run in one transaction, so a crash rolls back
//! cleanly and the migration simply reruns. On PostgreSQL sea-orm already
//! wraps the migration in a transaction (the `begin` below is a savepoint);
//! on SQLite it does not, so the migration opens its own — the same reason
//! Python's revision does. The column and index additions before it are
//! idempotent.
//!
//! Like Python's, this is not an online migration: a process of the previous
//! version still writes `dataset_data`. Stop every writer, upgrade, restart.
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait, Value};
use sea_orm_migration::prelude::*;
use tracing::info;
use uuid::Uuid;

use super::helpers::{add_column_if_missing, drop_column_if_present};
use crate::uuid_hex;

#[derive(DeriveMigrationName)]
pub struct Migration;

const DATA: &str = "data";
const DATASET_DATA: &str = "dataset_data";
const DATASET_ID: &str = "dataset_id";
const LEGACY_ID: &str = "legacy_id";
/// The provenance-ledger tables whose `data_id` follows a forked row.
const LEDGER_TABLES: [&str; 2] = ["nodes", "edges"];

/// SQLAlchemy's default names for Python's `index=True` columns, and the
/// composite dedup index Python declares in `Data.__table_args__`.
const IX_DATASET_ID: &str = "ix_data_dataset_id";
const IX_LEGACY_ID: &str = "ix_data_legacy_id";
const IX_CONTENT_LOOKUP: &str = "data_dataset_content_lookup";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // `text` holding `uuid_hex`, like every other id column here.
        for column in [DATASET_ID, LEGACY_ID] {
            let def = ColumnDef::new(Alias::new(column)).text().null().to_owned();
            add_column_if_missing(manager, DATA, def).await?;
        }
        for (name, columns) in [
            (IX_DATASET_ID, &[DATASET_ID][..]),
            (IX_LEGACY_ID, &[LEGACY_ID][..]),
            (
                IX_CONTENT_LOOKUP,
                &[DATASET_ID, "owner_id", "content_hash"][..],
            ),
        ] {
            let mut index = Index::create();
            index.if_not_exists().name(name).table(Alias::new(DATA));
            for column in columns {
                index.col(Alias::new(*column));
            }
            manager.create_index(index.to_owned()).await?;
        }

        // `SchemaManager::has_table` is unusable here for the reason
        // `helpers::has_column` gives, hence the probe.
        if existing_tables(manager.get_connection(), &[DATASET_DATA])
            .await?
            .is_empty()
        {
            return Ok(());
        }

        let txn = manager.get_connection().begin().await?;
        backfill(&txn).await?;
        txn.execute_unprepared(&format!("DROP TABLE {DATASET_DATA}"))
            .await?;
        txn.commit().await
    }

    /// Python's downgrade: recreate `dataset_data` with one membership per
    /// scoped row (data-preserving, never id-merging) and drop `legacy_id`.
    /// `data.dataset_id` stays, as in Python (it predates the revision there),
    /// so a re-run of `up` finds every row already stamped. Fork lineage is
    /// lost with `legacy_id`.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if existing_tables(manager.get_connection(), &[DATASET_DATA])
            .await?
            .is_empty()
        {
            manager
                .create_table(
                    Table::create()
                        .table(DatasetData::Table)
                        .col(ColumnDef::new(DatasetData::DatasetId).text().not_null())
                        .col(ColumnDef::new(DatasetData::DataId).text().not_null())
                        .col(
                            ColumnDef::new(DatasetData::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .primary_key(
                            Index::create()
                                .col(DatasetData::DatasetId)
                                .col(DatasetData::DataId),
                        )
                        .to_owned(),
                )
                .await?;
            manager
                .get_connection()
                .execute_unprepared(
                    "INSERT INTO dataset_data (dataset_id, data_id, created_at) \
                     SELECT dataset_id, id, created_at FROM data WHERE dataset_id IS NOT NULL",
                )
                .await?;
        }
        for name in [IX_CONTENT_LOOKUP, IX_LEGACY_ID] {
            let index = Index::drop()
                .if_exists()
                .name(name)
                .table(Alias::new(DATA))
                .to_owned();
            manager.drop_index(index).await?;
        }
        drop_column_if_present(manager, DATA, LEGACY_ID).await
    }
}

/// Step 2 of the module docs, inside the caller's transaction.
async fn backfill(conn: &impl ConnectionTrait) -> Result<(), DbErr> {
    let backend = conn.get_database_backend();

    // Sole memberships: one set-based UPDATE. Already-scoped rows (a Python
    // database, or a re-run) are excluded by the `dataset_id IS NULL` guard.
    let stamped = conn
        .execute_unprepared(
            "UPDATE data SET dataset_id = (\
                 SELECT dd.dataset_id FROM dataset_data dd WHERE dd.data_id = data.id\
             ) \
             WHERE dataset_id IS NULL AND id IN (\
                 SELECT data_id FROM dataset_data GROUP BY data_id HAVING COUNT(*) = 1\
             )",
        )
        .await?
        .rows_affected();

    // Shared rows (rare): the oldest membership keeps the id, the others fork.
    let shared: Vec<String> = conn
        .query_all(Statement::from_string(
            backend,
            "SELECT data_id FROM dataset_data GROUP BY data_id HAVING COUNT(*) > 1 \
             ORDER BY data_id",
        ))
        .await?
        .iter()
        .map(|row| row.try_get::<String>("", "data_id"))
        .collect::<Result<_, _>>()?;

    let mut forks = 0usize;
    if !shared.is_empty() {
        let columns = data_columns(conn).await?;
        let ledger_tables = existing_tables(conn, &LEDGER_TABLES).await?;
        for data_id in &shared {
            forks += split_shared_row(conn, data_id, &columns, &ledger_tables).await?;
        }
    }

    info!(
        stamped,
        shared = shared.len(),
        forks,
        "dataset-scoping backfill: stamped sole-membership rows, split shared rows"
    );
    Ok(())
}

/// Split one shared legacy row; returns the number of forks created.
async fn split_shared_row(
    conn: &impl ConnectionTrait,
    data_id: &str,
    columns: &[String],
    ledger_tables: &[&str],
) -> Result<usize, DbErr> {
    let backend = conn.get_database_backend();

    // A missing row, or one already scoped, is skipped BEFORE any ledger
    // repoint: a membership whose row is gone must not repoint ledger rows at
    // a fork that is never created (Python's guard, same reason).
    let row = conn
        .query_one(statement(
            backend,
            "SELECT dataset_id FROM data WHERE id = {}",
            [data_id.into()],
        ))
        .await?;
    match row {
        None => return Ok(0),
        Some(row) if row.try_get::<Option<String>>("", DATASET_ID)?.is_some() => return Ok(0),
        Some(_) => {}
    }

    let datasets: Vec<String> = conn
        .query_all(statement(
            backend,
            "SELECT dataset_id FROM dataset_data WHERE data_id = {} \
             ORDER BY created_at, dataset_id",
            [data_id.into()],
        ))
        .await?
        .iter()
        .map(|row| row.try_get::<String>("", DATASET_ID))
        .collect::<Result<_, _>>()?;
    let Some((keeper, others)) = datasets.split_first() else {
        return Ok(0);
    };

    conn.execute(statement(
        backend,
        "UPDATE data SET dataset_id = {} WHERE id = {} AND dataset_id IS NULL",
        [keeper.as_str().into(), data_id.into()],
    ))
    .await?;

    // Every column is copied server-side except the three that identify the
    // fork. The column list is read from the table, so columns another writer
    // added (Python's `system_metadata`) are copied too.
    let select_list = columns
        .iter()
        .map(|column| match column.as_str() {
            "id" | DATASET_ID => "{}".to_string(),
            LEGACY_ID => "id".to_string(),
            other => quote(other),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let insert_list = columns
        .iter()
        .map(|column| quote(column))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql =
        format!("INSERT INTO data ({insert_list}) SELECT {select_list} FROM data WHERE id = {{}}");

    for fork_dataset in others {
        let new_id = uuid_hex::to_hex(Uuid::new_v4());
        // Bind values in placeholder order: `id` and `dataset_id` as they
        // appear in the column list, then the source row id.
        let mut values: Vec<Value> = columns
            .iter()
            .filter_map(|column| match column.as_str() {
                "id" => Some(new_id.as_str().into()),
                DATASET_ID => Some(fork_dataset.as_str().into()),
                _ => None,
            })
            .collect();
        values.push(data_id.into());
        conn.execute(statement(backend, &insert_sql, values))
            .await?;

        for table in ledger_tables {
            conn.execute(statement(
                backend,
                &format!(
                    "UPDATE {table} SET data_id = {{}} WHERE data_id = {{}} AND dataset_id = {{}}"
                ),
                [
                    new_id.as_str().into(),
                    data_id.into(),
                    fork_dataset.as_str().into(),
                ],
            ))
            .await?;
        }
    }
    Ok(others.len())
}

/// `data`'s column names, in table order.
async fn data_columns(conn: &impl ConnectionTrait) -> Result<Vec<String>, DbErr> {
    let backend = conn.get_database_backend();
    let sql = match backend {
        DbBackend::Sqlite => "SELECT name FROM pragma_table_info('data') ORDER BY cid",
        DbBackend::Postgres => {
            "SELECT column_name AS name FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = 'data' \
             ORDER BY ordinal_position"
        }
        other => {
            return Err(DbErr::Custom(format!(
                "dataset-scoping backfill does not support the {other:?} backend"
            )));
        }
    };
    conn.query_all(Statement::from_string(backend, sql))
        .await?
        .iter()
        .map(|row| row.try_get::<String>("", "name"))
        .collect()
}

/// The tables among `candidates` that exist.
async fn existing_tables<'a>(
    conn: &impl ConnectionTrait,
    candidates: &[&'a str],
) -> Result<Vec<&'a str>, DbErr> {
    let backend = conn.get_database_backend();
    let sql = match backend {
        DbBackend::Sqlite => {
            "SELECT COUNT(*) AS cnt FROM sqlite_master WHERE type = 'table' AND name = {}"
        }
        DbBackend::Postgres => {
            "SELECT COUNT(*) AS cnt FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = {}"
        }
        other => {
            return Err(DbErr::Custom(format!(
                "dataset-scoping backfill does not support the {other:?} backend"
            )));
        }
    };
    let mut found = Vec::new();
    for table in candidates {
        let exists = conn
            .query_one(statement(backend, sql, [(*table).into()]))
            .await?
            .map(|row| row.try_get::<i64>("", "cnt"))
            .transpose()?
            .unwrap_or(0)
            > 0;
        if exists {
            found.push(*table);
        }
    }
    Ok(found)
}

/// Build a statement from `sql`, replacing each `{}` with the backend's
/// positional placeholder (`?` on SQLite, `$n` on PostgreSQL).
fn statement(backend: DbBackend, sql: &str, values: impl IntoIterator<Item = Value>) -> Statement {
    let mut out = String::with_capacity(sql.len());
    let mut parts = sql.split("{}").peekable();
    let mut n = 0;
    while let Some(part) = parts.next() {
        out.push_str(part);
        if parts.peek().is_some() {
            n += 1;
            match backend {
                DbBackend::Postgres => out.push_str(&format!("${n}")),
                _ => out.push('?'),
            }
        }
    }
    Statement::from_sql_and_values(backend, out, values)
}

/// A double-quoted SQL identifier (valid on SQLite and PostgreSQL).
fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[derive(DeriveIden)]
enum DatasetData {
    Table,
    DatasetId,
    DataId,
    CreatedAt,
}
