use chrono::{DateTime, Utc};
use cognee_models::{Data, Dataset};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    IntoActiveModel, PaginatorTrait, QueryFilter,
};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::{data, dataset, dataset_data};
use crate::types::DatabaseError;
use crate::uuid_hex;

#[instrument(
    name = "cognee.db.relational.data.create_data",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn create_data(db: &DatabaseConnection, d: Data) -> Result<Data, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    data::ActiveModel::from(&d)
        .insert(db)
        .await
        .map_err(map_sea_err)?;
    Ok(d)
}

#[instrument(
    name = "cognee.db.relational.data.get_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_data(db: &DatabaseConnection, id: Uuid) -> Result<Option<Data>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let result = data::Entity::find_by_id(uuid_hex::to_hex(id))
        .one(db)
        .await
        .map_err(map_sea_err)
        .map(|opt| opt.map(Data::from))?;
    Span::current().record(
        COGNEE_DB_ROW_COUNT,
        if result.is_some() { 1i64 } else { 0i64 },
    );
    Ok(result)
}

#[instrument(
    name = "cognee.db.relational.data.delete_data",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn delete_data(db: &DatabaseConnection, id: Uuid) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    data::Entity::delete_by_id(uuid_hex::to_hex(id))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

#[instrument(
    name = "cognee.db.relational.data.update_data",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn update_data(db: &DatabaseConnection, d: Data) -> Result<Data, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let mut model = data::ActiveModel::from(&d);
    model.updated_at = Set(Some(Utc::now()));
    model.update(db).await.map_err(map_sea_err)?;
    Ok(d)
}

#[instrument(
    name = "cognee.db.relational.data.count_data_dataset_links",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn count_data_dataset_links(
    db: &DatabaseConnection,
    data_id: Uuid,
) -> Result<usize, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let count: u64 = dataset_data::Entity::find()
        .filter(dataset_data::Column::DataId.eq(uuid_hex::to_hex(data_id)))
        .count(db)
        .await
        .map_err(map_sea_err)?;
    Span::current().record(COGNEE_DB_ROW_COUNT, count as i64);
    Ok(count as usize)
}

/// Update only the `token_count` column for a Data record.
///
/// Mirrors the Python `update_document_token_count()` in
/// `cognee/tasks/documents/extract_chunks_from_documents.py`.
#[instrument(
    name = "cognee.db.relational.data.update_data_token_count",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn update_data_token_count(
    db: &DatabaseConnection,
    data_id: Uuid,
    token_count: i64,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let model = data::Entity::find_by_id(uuid_hex::to_hex(data_id))
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

/// Update `last_accessed` for a batch of Data records identified by their IDs.
///
/// This is a no-op when `data_ids` is empty.
#[instrument(
    name = "cognee.db.relational.data.update_last_accessed",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn update_last_accessed(
    db: &DatabaseConnection,
    data_ids: &[Uuid],
    timestamp: DateTime<Utc>,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    if data_ids.is_empty() {
        return Ok(());
    }

    for id in data_ids {
        let model = data::Entity::find_by_id(uuid_hex::to_hex(*id))
            .one(db)
            .await
            .map_err(map_sea_err)?;

        if let Some(m) = model {
            let mut active = m.into_active_model();
            active.last_accessed = Set(Some(timestamp));
            active.update(db).await.map_err(map_sea_err)?;
        }
    }

    Ok(())
}

/// Clear `pipeline_status` JSON entries keyed by the given `dataset_id`
/// from all `Data` records linked to that dataset via the `dataset_data`
/// junction table.
///
/// This mirrors the Python cleanup in `delete_dataset.py` lines 33-54.
/// Must be called **before** the junction rows are removed (before
/// `detach_data_from_dataset` or `delete_dataset`), since the junction is
/// needed to find related `Data` records.
///
/// Returns the number of `Data` records whose `pipeline_status` was modified.
#[instrument(
    name = "cognee.db.relational.data.clear_pipeline_status_for_dataset",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn clear_pipeline_status_for_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<usize, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    // Find all data IDs linked to this dataset via the junction table
    let junction_rows = dataset_data::Entity::find()
        .filter(dataset_data::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .all(db)
        .await
        .map_err(map_sea_err)?;

    let data_ids: Vec<String> = junction_rows.into_iter().map(|j| j.data_id).collect();
    if data_ids.is_empty() {
        Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
        return Ok(0);
    }

    let dataset_id_str = uuid_hex::to_hex(dataset_id);
    let mut updated_count = 0usize;

    for data_hex_id in &data_ids {
        let model = data::Entity::find_by_id(data_hex_id.clone())
            .one(db)
            .await
            .map_err(map_sea_err)?;

        let Some(model) = model else { continue };

        let Some(ref status_json) = model.pipeline_status else {
            continue;
        };

        let mut parsed: serde_json::Value = serde_json::from_str(status_json)
            .unwrap_or(serde_json::Value::Object(Default::default()));

        let serde_json::Value::Object(ref mut top_map) = parsed else {
            continue;
        };

        let mut modified = false;
        for (_pipeline_name, inner) in top_map.iter_mut() {
            if let serde_json::Value::Object(inner_map) = inner
                && inner_map.remove(&dataset_id_str).is_some()
            {
                modified = true;
            }
        }

        if !modified {
            continue;
        }

        // Remove pipeline entries whose inner map is now empty
        top_map.retain(|_, v| !matches!(v, serde_json::Value::Object(m) if m.is_empty()));

        let new_status = if top_map.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&parsed).map_err(|e| {
                DatabaseError::QueryError(format!("Failed to serialize pipeline_status: {e}"))
            })?)
        };

        let mut active = model.into_active_model();
        active.pipeline_status = Set(new_status);
        active.updated_at = Set(Some(Utc::now()));
        active.update(db).await.map_err(map_sea_err)?;
        updated_count += 1;
    }

    Span::current().record(COGNEE_DB_ROW_COUNT, updated_count as i64);
    Ok(updated_count)
}

#[instrument(
    name = "cognee.db.relational.data.list_datasets_for_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn list_datasets_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
) -> Result<Vec<Dataset>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let pairs = data::Entity::find_by_id(uuid_hex::to_hex(data_id))
        .find_with_related(dataset::Entity)
        .all(db)
        .await
        .map_err(map_sea_err)?;
    let datasets: Vec<Dataset> = pairs
        .into_iter()
        .flat_map(|(_, ds_list)| ds_list)
        .map(Dataset::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, datasets.len() as i64);
    Ok(datasets)
}
