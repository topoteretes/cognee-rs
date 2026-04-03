use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::entities::task_run;
use crate::types::{DatabaseError, TaskRun};

pub async fn create_task_run(
    db: &DatabaseConnection,
    run: TaskRun,
) -> Result<TaskRun, DatabaseError> {
    task_run::ActiveModel::from(&run)
        .insert(db)
        .await
        .map_err(map_sea_err)?;
    Ok(run)
}

pub async fn update_task_run_status(
    db: &DatabaseConnection,
    id: Uuid,
    status: &str,
) -> Result<(), DatabaseError> {
    task_run::Entity::update_many()
        .col_expr(
            task_run::Column::Status,
            sea_orm::sea_query::Expr::value(status),
        )
        .filter(task_run::Column::Id.eq(id))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}
