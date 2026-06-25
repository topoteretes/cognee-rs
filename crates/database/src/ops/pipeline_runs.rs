use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::{domain_status_to_entity, map_sea_err};
use crate::database_system_label;
use crate::entities::pipeline_run;
use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};
use crate::uuid_hex;

#[instrument(
    name = "cognee.db.relational.pipeline_runs.create_pipeline_run",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn create_pipeline_run(
    db: &DatabaseConnection,
    run: PipelineRun,
) -> Result<PipelineRun, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    pipeline_run::ActiveModel::from(&run)
        .insert(db)
        .await
        .map_err(map_sea_err)?;
    Ok(run)
}

#[instrument(
    name = "cognee.db.relational.pipeline_runs.update_pipeline_run_status",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn update_pipeline_run_status(
    db: &DatabaseConnection,
    id: Uuid,
    status: PipelineRunStatus,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    pipeline_run::Entity::update_many()
        .col_expr(
            pipeline_run::Column::Status,
            sea_orm::sea_query::Expr::value(domain_status_to_entity(status)),
        )
        .filter(pipeline_run::Column::Id.eq(uuid_hex::to_hex(id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

#[instrument(
    name = "cognee.db.relational.pipeline_runs.get_pipeline_run",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_pipeline_run(
    db: &DatabaseConnection,
    id: Uuid,
) -> Result<Option<PipelineRun>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let result = pipeline_run::Entity::find_by_id(uuid_hex::to_hex(id))
        .one(db)
        .await
        .map_err(map_sea_err)
        .map(|opt| opt.map(PipelineRun::from))?;
    Span::current().record(
        COGNEE_DB_ROW_COUNT,
        if result.is_some() { 1i64 } else { 0i64 },
    );
    Ok(result)
}

/// Delete all `pipeline_runs` rows for a given `dataset_id`.
///
/// This is needed for data-scoped deletion where the dataset itself is not
/// deleted (so the FK cascade does not fire), but we still want to invalidate
/// the pipeline cache so the dataset can be re-cognified after data changes.
///
/// Returns the count of deleted rows.
#[instrument(
    name = "cognee.db.relational.pipeline_runs.delete_pipeline_runs_by_dataset",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn delete_pipeline_runs_by_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let result = pipeline_run::Entity::delete_many()
        .filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Span::current().record(COGNEE_DB_ROW_COUNT, result.rows_affected as i64);
    Ok(result.rows_affected)
}

#[instrument(
    name = "cognee.db.relational.pipeline_runs.get_latest_pipeline_status",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_latest_pipeline_status(
    db: &DatabaseConnection,
    pipeline_name: &str,
    dataset_id: Uuid,
) -> Result<Option<PipelineRunStatus>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let run = pipeline_run::Entity::find()
        .filter(pipeline_run::Column::PipelineName.eq(pipeline_name))
        .filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_desc(pipeline_run::Column::CreatedAt)
        .one(db)
        .await
        .map_err(map_sea_err)?;

    let result = run.map(|m| PipelineRun::from(m).status);
    Span::current().record(
        COGNEE_DB_ROW_COUNT,
        if result.is_some() { 1i64 } else { 0i64 },
    );
    Ok(result)
}
