use cognee_models::{Data, Dataset};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set,
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
    attach_data_to_dataset_with_external_event(db, dataset_id, data_id, None).await
}

/// Attach data to a dataset together with an optional external idempotency key.
///
/// An identical replay is a no-op. Reusing a key for a different data item is
/// rejected so callers never silently replace the event's original content.
#[instrument(
    name = "cognee.db.relational.datasets.attach_data_to_dataset_with_external_event",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn attach_data_to_dataset_with_external_event(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    data_id: Uuid,
    external_event_id: Option<&str>,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));

    let Some(event_id) = external_event_id else {
        let model = make_dataset_data_active(dataset_id, data_id, None);
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
        return ignore_do_nothing(res);
    };

    let dataset_hex = uuid_hex::to_hex(dataset_id);
    let data_hex = uuid_hex::to_hex(data_id);

    if let Some(existing) = dataset_data::Entity::find()
        .filter(dataset_data::Column::DatasetId.eq(&dataset_hex))
        .filter(dataset_data::Column::ExternalEventId.eq(event_id))
        .one(db)
        .await
        .map_err(map_sea_err)?
    {
        return if existing.data_id == data_hex {
            Ok(())
        } else {
            Err(DatabaseError::UniqueViolation(format!(
                "external event '{event_id}' is already attached to different data in dataset {dataset_id}"
            )))
        };
    }

    // Content-addressed data can already be attached by a legacy call. Claim
    // that null slot atomically rather than creating a duplicate membership.
    if let Some(existing) =
        dataset_data::Entity::find_by_id((dataset_hex.clone(), data_hex.clone()))
            .one(db)
            .await
            .map_err(map_sea_err)?
    {
        match existing.external_event_id.as_deref() {
            Some(existing_event) if existing_event == event_id => return Ok(()),
            Some(existing_event) => {
                return Err(DatabaseError::UniqueViolation(format!(
                    "data {data_id} in dataset {dataset_id} is already attached to external event '{existing_event}'"
                )));
            }
            None => {
                let mut active: dataset_data::ActiveModel = existing.into();
                active.external_event_id = Set(Some(event_id.to_owned()));
                return active.update(db).await.map(|_| ()).map_err(map_sea_err);
            }
        }
    }

    let model = make_dataset_data_active(dataset_id, data_id, Some(event_id));
    let res = dataset_data::Entity::insert(model)
        .exec(db)
        .await
        .map_err(map_sea_err);
    match res {
        Ok(_) => Ok(()),
        Err(DatabaseError::UniqueViolation(_)) => {
            // A concurrent identical writer may have won after our lookup.
            match dataset_data::Entity::find()
                .filter(dataset_data::Column::DatasetId.eq(dataset_hex))
                .filter(dataset_data::Column::ExternalEventId.eq(event_id))
                .one(db)
                .await
                .map_err(map_sea_err)?
            {
                Some(existing) if existing.data_id == data_hex => Ok(()),
                _ => Err(DatabaseError::UniqueViolation(format!(
                    "external event '{event_id}' conflicts in dataset {dataset_id}"
                ))),
            }
        }
        Err(error) => Err(error),
    }
}

/// Return the data item attached under an external event key, if any.
pub async fn get_data_id_for_external_event(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    external_event_id: &str,
) -> Result<Option<Uuid>, DatabaseError> {
    let membership = dataset_data::Entity::find()
        .filter(dataset_data::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .filter(dataset_data::Column::ExternalEventId.eq(external_event_id))
        .one(db)
        .await
        .map_err(map_sea_err)?;
    membership
        .map(|row| {
            uuid_hex::from_hex(&row.data_id)
                .map_err(|error| DatabaseError::QueryError(error.to_string()))
        })
        .transpose()
}

/// Whether a dataset already contains an external event key.
pub async fn contains_external_event(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    external_event_id: &str,
) -> Result<bool, DatabaseError> {
    Ok(
        get_data_id_for_external_event(db, dataset_id, external_event_id)
            .await?
            .is_some(),
    )
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
