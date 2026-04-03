use cognee_models::{Data, Dataset};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
};
use uuid::Uuid;

use crate::conversions::{ignore_do_nothing, make_dataset_data_active, map_sea_err};
use crate::entities::{data, dataset, dataset_data};
use crate::types::DatabaseError;

pub async fn create_dataset(
    db: &DatabaseConnection,
    ds: Dataset,
) -> Result<Dataset, DatabaseError> {
    dataset::ActiveModel::from(&ds)
        .insert(db)
        .await
        .map_err(map_sea_err)?;
    Ok(ds)
}

pub async fn get_dataset(
    db: &DatabaseConnection,
    id: Uuid,
) -> Result<Option<Dataset>, DatabaseError> {
    dataset::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(map_sea_err)
        .map(|opt| opt.map(Dataset::from))
}

pub async fn get_dataset_by_name(
    db: &DatabaseConnection,
    name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<Option<Dataset>, DatabaseError> {
    let mut q = dataset::Entity::find().filter(
        dataset::Column::Name
            .eq(name)
            .and(dataset::Column::OwnerId.eq(owner_id)),
    );
    if let Some(tid) = tenant_id {
        q = q.filter(dataset::Column::TenantId.eq(tid));
    }
    q.one(db)
        .await
        .map_err(map_sea_err)
        .map(|opt| opt.map(Dataset::from))
}

pub async fn list_datasets_by_owner(
    db: &DatabaseConnection,
    owner_id: Uuid,
) -> Result<Vec<Dataset>, DatabaseError> {
    dataset::Entity::find()
        .filter(dataset::Column::OwnerId.eq(owner_id))
        .order_by_asc(dataset::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)
        .map(|v| v.into_iter().map(Dataset::from).collect())
}

pub async fn list_datasets(db: &DatabaseConnection) -> Result<Vec<Dataset>, DatabaseError> {
    dataset::Entity::find()
        .order_by_asc(dataset::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)
        .map(|v| v.into_iter().map(Dataset::from).collect())
}

pub async fn delete_dataset(db: &DatabaseConnection, id: Uuid) -> Result<(), DatabaseError> {
    dataset::Entity::delete_by_id(id)
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

pub async fn attach_data_to_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    data_id: Uuid,
) -> Result<(), DatabaseError> {
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

pub async fn detach_data_from_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    data_id: Uuid,
) -> Result<(), DatabaseError> {
    dataset_data::Entity::delete_many()
        .filter(
            dataset_data::Column::DatasetId
                .eq(dataset_id)
                .and(dataset_data::Column::DataId.eq(data_id)),
        )
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

pub async fn get_dataset_data(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<Vec<Data>, DatabaseError> {
    let pairs = dataset::Entity::find_by_id(dataset_id)
        .find_with_related(data::Entity)
        .all(db)
        .await
        .map_err(map_sea_err)?;
    Ok(pairs
        .into_iter()
        .flat_map(|(_, data_list)| data_list)
        .map(Data::from)
        .collect())
}
