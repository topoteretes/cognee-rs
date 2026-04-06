use chrono::Utc;
use cognee_models::{Data, Dataset};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    IntoActiveModel, PaginatorTrait, QueryFilter,
};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::entities::{data, dataset, dataset_data};
use crate::types::DatabaseError;

pub async fn create_data(db: &DatabaseConnection, d: Data) -> Result<Data, DatabaseError> {
    data::ActiveModel::from(&d)
        .insert(db)
        .await
        .map_err(map_sea_err)?;
    Ok(d)
}

pub async fn get_data(db: &DatabaseConnection, id: Uuid) -> Result<Option<Data>, DatabaseError> {
    data::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(map_sea_err)
        .map(|opt| opt.map(Data::from))
}

pub async fn delete_data(db: &DatabaseConnection, id: Uuid) -> Result<(), DatabaseError> {
    data::Entity::delete_by_id(id)
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

pub async fn update_data(db: &DatabaseConnection, d: Data) -> Result<Data, DatabaseError> {
    let mut model = data::ActiveModel::from(&d);
    model.updated_at = Set(Some(Utc::now()));
    model.update(db).await.map_err(map_sea_err)?;
    Ok(d)
}

pub async fn count_data_dataset_links(
    db: &DatabaseConnection,
    data_id: Uuid,
) -> Result<usize, DatabaseError> {
    let count: u64 = dataset_data::Entity::find()
        .filter(dataset_data::Column::DataId.eq(data_id))
        .count(db)
        .await
        .map_err(map_sea_err)?;
    Ok(count as usize)
}

/// Update only the `token_count` column for a Data record.
///
/// Mirrors the Python `update_document_token_count()` in
/// `cognee/tasks/documents/extract_chunks_from_documents.py`.
pub async fn update_data_token_count(
    db: &DatabaseConnection,
    data_id: Uuid,
    token_count: i64,
) -> Result<(), DatabaseError> {
    let model = data::Entity::find_by_id(data_id)
        .one(db)
        .await
        .map_err(map_sea_err)?
        .ok_or_else(|| DatabaseError::NotFound(format!("Data {data_id} not found")))?;

    let mut active = model.into_active_model();
    active.token_count = Set(token_count);
    active.updated_at = Set(Some(Utc::now()));
    active.update(db).await.map_err(map_sea_err)?;
    Ok(())
}

pub async fn list_datasets_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
) -> Result<Vec<Dataset>, DatabaseError> {
    let pairs = data::Entity::find_by_id(data_id)
        .find_with_related(dataset::Entity)
        .all(db)
        .await
        .map_err(map_sea_err)?;
    Ok(pairs
        .into_iter()
        .flat_map(|(_, ds_list)| ds_list)
        .map(Dataset::from)
        .collect())
}
