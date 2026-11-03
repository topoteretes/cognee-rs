//! Moves the improve stage watermarks out of `graph_sync_checkpoints` into the
//! operation rows' `run_info`, and drops that table.
//!
//! Python keeps a stage watermark on the `improve` operation's `pipeline_runs`
//! row, in `run_info[<stage>] = {"status", "started_at"}`
//! (`cognee/modules/improve/graph_changes.py:46-48`, `:66-97`); it has no
//! checkpoint table. Rust kept the same timestamps under the keys
//! `improve_stage:{stage}:{dataset_id}`. Each such row becomes one `improve`
//! row on its dataset (`Completed`, created at the stamped start time, so it
//! counts as no change after itself), carrying the stamp under the stage name.
//! A stage whose stamp is migrated is not rerun by the next improve.
//!
//! Rows of other keys, an unparseable key, and a stamp of a dataset that no
//! longer exists carry nothing a stage can read and are dropped with the
//! table. `down` recreates the (empty) table; migrated rows stay.
use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement, Value};
use sea_orm_migration::prelude::*;

use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(DeriveMigrationName)]
pub struct Migration;

const KEY_PREFIX: &str = "improve_stage:";
/// The `pipeline_runs` name of an `improve` operation row (Python's
/// `IMPROVE_OPERATION_NAME`, `graph_changes.py:31`).
const IMPROVE_NAME: &str = "improve";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        if !table_exists(conn, backend, "graph_sync_checkpoints").await? {
            return Ok(());
        }
        if table_exists(conn, backend, "pipeline_runs").await?
            && table_exists(conn, backend, "datasets").await?
        {
            let rows = conn
                .query_all(Statement::from_string(
                    backend,
                    "SELECT key, ts FROM graph_sync_checkpoints ORDER BY key",
                ))
                .await?;
            for row in rows {
                move_stamp(conn, backend, &row).await?;
            }
        }
        manager
            .drop_table(
                Table::drop()
                    .table(GraphSyncCheckpoints::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(GraphSyncCheckpoints::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GraphSyncCheckpoints::Key)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(GraphSyncCheckpoints::Ts)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }
}

/// `improve_stage:{stage}:{dataset_id}` -> `(stage, dataset_id)`.
fn parse_key(key: &str) -> Option<(&str, Uuid)> {
    let (stage, dataset) = key.strip_prefix(KEY_PREFIX)?.rsplit_once(':')?;
    (!stage.is_empty()).then_some(())?;
    Some((stage, Uuid::parse_str(dataset).ok()?))
}

async fn move_stamp(
    conn: &impl ConnectionTrait,
    backend: DbBackend,
    row: &QueryResult,
) -> Result<(), DbErr> {
    let key: String = row.try_get("", "key")?;
    let Some((stage, dataset_id)) = parse_key(&key) else {
        return Ok(());
    };
    let ts: DateTime<Utc> = row.try_get("", "ts")?;
    let dataset_hex = dataset_id.simple().to_string();
    let exists = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            match backend {
                DbBackend::Postgres => "SELECT COUNT(*) AS cnt FROM datasets WHERE id = $1",
                _ => "SELECT COUNT(*) AS cnt FROM datasets WHERE id = ?",
            },
            [dataset_hex.clone().into()],
        ))
        .await?
        .map(|r| r.try_get::<i64>("", "cnt"))
        .transpose()?
        .unwrap_or(0);
    if exists == 0 {
        return Ok(());
    }
    // `watermark_stamp` of Python (`graph_changes.py:46-48`): the stage start,
    // as an ISO timestamp.
    let run_info = serde_json::json!({
        stage: { "status": "completed", "started_at": ts.to_rfc3339() }
    });
    let insert = Query::insert()
        .into_table(PipelineRuns::Table)
        .columns([
            PipelineRuns::Id,
            PipelineRuns::CreatedAt,
            PipelineRuns::Status,
            PipelineRuns::PipelineRunId,
            PipelineRuns::PipelineName,
            PipelineRuns::PipelineId,
            PipelineRuns::DatasetId,
            PipelineRuns::RunInfo,
        ])
        .values_panic([
            Value::from(Uuid::new_v4().simple().to_string()).into(),
            Value::from(ts).into(),
            Value::from("DATASET_PROCESSING_COMPLETED").into(),
            Value::from(Uuid::new_v4().simple().to_string()).into(),
            Value::from(IMPROVE_NAME).into(),
            Value::from(
                Uuid::new_v5(&Uuid::NAMESPACE_OID, IMPROVE_NAME.as_bytes())
                    .simple()
                    .to_string(),
            )
            .into(),
            Value::from(dataset_hex).into(),
            Value::Json(Some(Box::new(run_info))).into(),
        ])
        .to_owned();
    conn.execute(backend.build(&insert)).await?;
    Ok(())
}

/// Whether `table` exists. Not `SchemaManager::has_table`, which needs a
/// sea-schema backend feature this crate does not enable.
async fn table_exists(
    conn: &impl ConnectionTrait,
    backend: DbBackend,
    table: &str,
) -> Result<bool, DbErr> {
    let (sql, bind) = match backend {
        DbBackend::Sqlite => (
            "SELECT COUNT(*) AS cnt FROM sqlite_master WHERE type = 'table' AND name = ?",
            table,
        ),
        DbBackend::Postgres => (
            "SELECT COUNT(*) AS cnt FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = $1",
            table,
        ),
        other => {
            return Err(DbErr::Custom(format!(
                "moving the improve stamps does not support the {other:?} backend"
            )));
        }
    };
    Ok(conn
        .query_one(Statement::from_sql_and_values(backend, sql, [bind.into()]))
        .await?
        .map(|row| row.try_get::<i64>("", "cnt"))
        .transpose()?
        .unwrap_or(0)
        > 0)
}

#[derive(DeriveIden)]
enum GraphSyncCheckpoints {
    Table,
    Key,
    Ts,
}

#[derive(DeriveIden)]
enum PipelineRuns {
    Table,
    Id,
    CreatedAt,
    Status,
    PipelineRunId,
    PipelineName,
    PipelineId,
    DatasetId,
    RunInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stage_key_names_its_stage_and_dataset() {
        let id = Uuid::new_v4();
        let key = format!("improve_stage:global_context_index:{id}");
        assert_eq!(parse_key(&key), Some(("global_context_index", id)));
        assert_eq!(parse_key("improve_stage::0"), None);
        assert_eq!(parse_key(&format!("other:{id}")), None);
        assert_eq!(parse_key("improve_stage:stage:not-a-uuid"), None);
    }
}
