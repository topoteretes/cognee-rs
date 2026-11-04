use cognee_models::{Data, Dataset};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder,
};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::{data, dataset};
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

/// The dataset named `name` owned by `owner_id` **in `tenant_id`**, with
/// strict tenant equality: `None` matches only a row whose `tenant_id` is
/// NULL. Python resolves names exactly so — the owner's datasets, filtered on
/// name, then `dataset.tenant_id == user.tenant_id`
/// (`cognee/modules/data/methods/get_dataset_ids.py:30-34@37f788bd2`).
///
/// This is the only by-name dataset lookup: there is no tenant-loose variant
/// (removed in SDK-936), so a name never resolves across tenants. It is never
/// ambiguous: `(name, owner, tenant)` determines the deterministic dataset id.
#[instrument(
    name = "cognee.db.relational.datasets.get_dataset_by_name_in_tenant",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_dataset_by_name_in_tenant(
    db: &DatabaseConnection,
    name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<Option<Dataset>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let q = dataset::Entity::find().filter(
        dataset::Column::Name
            .eq(name)
            .and(dataset::Column::OwnerId.eq(uuid_hex::to_hex(owner_id))),
    );
    let q = match tenant_id {
        Some(tid) => q.filter(dataset::Column::TenantId.eq(uuid_hex::to_hex(tid))),
        None => q.filter(dataset::Column::TenantId.is_null()),
    };
    let result = q
        .order_by_asc(dataset::Column::CreatedAt)
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

/// The datasets among `ids` that exist, in one `IN (...)` query per 500 ids
/// (the driver's bound-variable cap). Missing ids are skipped; the result is
/// in `ids` order, without duplicates.
#[instrument(
    name = "cognee.db.relational.datasets.get_datasets_by_ids",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_datasets_by_ids(
    db: &DatabaseConnection,
    ids: &[Uuid],
) -> Result<Vec<Dataset>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    const CHUNK: usize = 500;
    let mut found: std::collections::HashMap<Uuid, Dataset> =
        std::collections::HashMap::with_capacity(ids.len());
    let hex_ids: Vec<String> = ids.iter().map(|id| uuid_hex::to_hex(*id)).collect();
    for chunk in hex_ids.chunks(CHUNK) {
        let rows = dataset::Entity::find()
            .filter(dataset::Column::Id.is_in(chunk.to_vec()))
            .all(db)
            .await
            .map_err(map_sea_err)?;
        for row in rows {
            let ds = Dataset::from(row);
            found.insert(ds.id, ds);
        }
    }
    let mut seen = std::collections::HashSet::with_capacity(ids.len());
    let rows: Vec<Dataset> = ids
        .iter()
        .filter(|id| seen.insert(**id))
        .filter_map(|id| found.remove(id))
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
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

/// Make `dataset_id` the owning dataset of the row `data_id`
/// (`data.dataset_id`).
///
/// Rows are dataset-scoped, as in Python: a row belongs to exactly one
/// dataset. Stamping a row that already belongs to `dataset_id` is a no-op;
/// a row of **another** dataset is refused with [`DatabaseError::QueryError`]
/// (the same content in a second dataset is a second row, which the ingest
/// pipeline creates). A missing row is [`DatabaseError::NotFound`].
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
    let dataset_hex = uuid_hex::to_hex(dataset_id);
    let result = data::Entity::update_many()
        .col_expr(data::Column::DatasetId, Expr::value(dataset_hex.clone()))
        .filter(data::Column::Id.eq(uuid_hex::to_hex(data_id)))
        .filter(data::Column::DatasetId.is_null())
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    if result.rows_affected > 0 {
        return Ok(());
    }
    match data::Entity::find_by_id(uuid_hex::to_hex(data_id))
        .one(db)
        .await
        .map_err(map_sea_err)?
    {
        None => Err(DatabaseError::NotFound(format!("Data {data_id} not found"))),
        Some(row) if row.dataset_id.as_deref() == Some(dataset_hex.as_str()) => Ok(()),
        Some(_) => Err(DatabaseError::QueryError(format!(
            "Data {data_id} belongs to another dataset; rows are dataset-scoped"
        ))),
    }
}

/// Detach the row `data_id` from `dataset_id`: clear its `data.dataset_id`
/// when it belongs to that dataset, leaving the row in place for the delete
/// path to remove (it removes a row once no dataset holds it). A row of
/// another dataset, or a missing row, is left alone.
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
    data::Entity::update_many()
        .col_expr(data::Column::DatasetId, Expr::value(Option::<String>::None))
        .filter(data::Column::Id.eq(uuid_hex::to_hex(data_id)))
        .filter(data::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(())
}

/// Count the number of data items linked to a dataset without loading them.
///
/// Uses `SELECT COUNT(*)` on the indexed `data.dataset_id` for efficiency.
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
    let count: u64 = data::Entity::find()
        .filter(data::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
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
    // Oldest first (ties by id), so listings are stable across calls.
    let rows: Vec<Data> = data::Entity::find()
        .filter(data::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_asc(data::Column::CreatedAt)
        .order_by_asc(data::Column::Id)
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(Data::from)
        .collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}
