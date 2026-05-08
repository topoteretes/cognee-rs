use cognee_utils::tracing_keys::COGNEE_DB_SYSTEM;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::task_run;
use crate::types::{DatabaseError, TaskRun};
use crate::uuid_hex;

#[instrument(
    name = "cognee.db.relational.task_runs.create_task_run",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn create_task_run(
    db: &DatabaseConnection,
    run: TaskRun,
) -> Result<TaskRun, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    task_run::ActiveModel::from(&run)
        .insert(db)
        .await
        .map_err(map_sea_err)?;
    Ok(run)
}

#[instrument(
    name = "cognee.db.relational.task_runs.update_task_run_status",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn update_task_run_status(
    db: &DatabaseConnection,
    id: Uuid,
    status: &str,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    task_run::Entity::update_many()
        .col_expr(
            task_run::Column::Status,
            sea_orm::sea_query::Expr::value(status),
        )
        .filter(task_run::Column::Id.eq(uuid_hex::to_hex(id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}
