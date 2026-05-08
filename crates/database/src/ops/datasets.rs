use cognee_models::{Data, Dataset};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder,
};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::{ignore_do_nothing, make_dataset_data_active, map_sea_err};
use crate::database_system_label;
use crate::entities::{data, dataset, dataset_data};
use crate::types::DatabaseError;
use crate::uuid_hex;

#[instrument(
    name = "cognee.db.relational.datasets.create_dataset",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn create_dataset(
    db: &DatabaseConnection,
    ds: Dataset,
) -> Result<Dataset, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    dataset::ActiveModel::from(&ds)
        .insert(db)
        .await
        .map_err(map_sea_err)?;
    Ok(ds)
}

#[instrument(
    name = "cognee.db.relational.datasets.get_dataset",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_dataset(
    db: &DatabaseConnection,
    id: Uuid,
) -> Result<Option<Dataset>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let result = dataset::Entity::find_by_id(uuid_hex::to_hex(id))
        .one(db)
        .await
        .map_err(map_sea_err)
        .map(|opt| opt.map(Dataset::from))?;
    Span::current().record(
        COGNEE_DB_ROW_COUNT,
        if result.is_some() { 1i64 } else { 0i64 },
    );
    Ok(result)
}

#[instrument(
    name = "cognee.db.relational.datasets.get_dataset_by_name",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_dataset_by_name(
    db: &DatabaseConnection,
    name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<Option<Dataset>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let mut q = dataset::Entity::find().filter(
        dataset::Column::Name
            .eq(name)
            .and(dataset::Column::OwnerId.eq(uuid_hex::to_hex(owner_id))),
    );
    if let Some(tid) = tenant_id {
        q = q.filter(dataset::Column::TenantId.eq(uuid_hex::to_hex(tid)));
    }
    let result = q
        .one(db)
        .await
        .map_err(map_sea_err)
        .map(|opt| opt.map(Dataset::from))?;
    Span::current().record(
        COGNEE_DB_ROW_COUNT,
        if result.is_some() { 1i64 } else { 0i64 },
    );
    Ok(result)
}

#[instrument(
    name = "cognee.db.relational.datasets.list_datasets_by_owner",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn list_datasets_by_owner(
    db: &DatabaseConnection,
    owner_id: Uuid,
) -> Result<Vec<Dataset>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let rows: Vec<Dataset> = dataset::Entity::find()
        .filter(dataset::Column::OwnerId.eq(uuid_hex::to_hex(owner_id)))
        .order_by_asc(dataset::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(Dataset::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

#[instrument(
    name = "cognee.db.relational.datasets.list_datasets",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn list_datasets(db: &DatabaseConnection) -> Result<Vec<Dataset>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let rows: Vec<Dataset> = dataset::Entity::find()
        .order_by_asc(dataset::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(Dataset::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}

#[instrument(
    name = "cognee.db.relational.datasets.delete_dataset",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn delete_dataset(db: &DatabaseConnection, id: Uuid) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    dataset::Entity::delete_by_id(uuid_hex::to_hex(id))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

#[instrument(
    name = "cognee.db.relational.datasets.attach_data_to_dataset",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn attach_data_to_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    data_id: Uuid,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let model = make_dataset_data_active(dataset_id, data_id);
    let res = dataset_data::Entity::insert(model)
        .on_conflict(
            OnConflict::columns([
                dataset_data::Column::DatasetId,
                dataset_data::Column::DataId,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(db)
        .await
        .map_err(map_sea_err)
        .map(|_| ());
    ignore_do_nothing(res)
}

#[instrument(
    name = "cognee.db.relational.datasets.detach_data_from_dataset",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn detach_data_from_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    data_id: Uuid,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    dataset_data::Entity::delete_many()
        .filter(
            dataset_data::Column::DatasetId
                .eq(uuid_hex::to_hex(dataset_id))
                .and(dataset_data::Column::DataId.eq(uuid_hex::to_hex(data_id))),
        )
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

/// Count the number of data items linked to a dataset without loading them.
///
/// Uses `SELECT COUNT(*)` on the `dataset_data` junction table for efficiency.
#[instrument(
    name = "cognee.db.relational.datasets.count_dataset_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn count_dataset_data(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<usize, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let count: u64 = dataset_data::Entity::find()
        .filter(dataset_data::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .count(db)
        .await
        .map_err(map_sea_err)?;
    Span::current().record(COGNEE_DB_ROW_COUNT, count as i64);
    Ok(count as usize)
}

#[instrument(
    name = "cognee.db.relational.datasets.get_dataset_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_dataset_data(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<Vec<Data>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let pairs = dataset::Entity::find_by_id(uuid_hex::to_hex(dataset_id))
        .find_with_related(data::Entity)
        .all(db)
        .await
        .map_err(map_sea_err)?;
    let rows: Vec<Data> = pairs
        .into_iter()
        .flat_map(|(_, data_list)| data_list)
        .map(Data::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}
