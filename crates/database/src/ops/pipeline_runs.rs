use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
};
use uuid::Uuid;

use crate::conversions::{domain_status_to_entity, map_sea_err};
use crate::entities::pipeline_run;
use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};
use crate::uuid_hex;

pub async fn create_pipeline_run(
    db: &DatabaseConnection,
    run: PipelineRun,
) -> Result<PipelineRun, DatabaseError> {
    pipeline_run::ActiveModel::from(&run)
        .insert(db)
        .await
        .map_err(map_sea_err)?;
    Ok(run)
}

pub async fn update_pipeline_run_status(
    db: &DatabaseConnection,
    id: Uuid,
    status: PipelineRunStatus,
) -> Result<(), DatabaseError> {
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

pub async fn get_pipeline_run(
    db: &DatabaseConnection,
    id: Uuid,
) -> Result<Option<PipelineRun>, DatabaseError> {
    pipeline_run::Entity::find_by_id(uuid_hex::to_hex(id))
        .one(db)
        .await
        .map_err(map_sea_err)
        .map(|opt| opt.map(PipelineRun::from))
}

/// Get the latest pipeline run status for a (pipeline_name, dataset_id) pair.
///
/// Queries the `pipeline_runs` table for the most recent entry matching
/// the given `pipeline_name` and `dataset_id`, ordered by `created_at DESC`.
///
/// Returns `None` if no matching run exists.
pub async fn get_latest_pipeline_status(
    db: &DatabaseConnection,
    pipeline_name: &str,
    dataset_id: Uuid,
) -> Result<Option<PipelineRunStatus>, DatabaseError> {
    let run = pipeline_run::Entity::find()
        .filter(pipeline_run::Column::PipelineName.eq(pipeline_name))
        .filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_desc(pipeline_run::Column::CreatedAt)
        .one(db)
        .await
        .map_err(map_sea_err)?;

    Ok(run.map(|m| PipelineRun::from(m).status))
}
