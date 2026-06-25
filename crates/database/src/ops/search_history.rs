use chrono::Utc;
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::PaginatorTrait;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::{map_sea_err, query_model_to_history, result_model_to_history};
use crate::database_system_label;
use crate::entities::{query, result_log};
use crate::types::{DatabaseError, SearchHistoryEntry};
use crate::uuid_hex;

#[instrument(
    name = "cognee.db.relational.search_history.log_query",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn log_query(
    db: &DatabaseConnection,
    query_text: &str,
    query_type: &str,
    user_id: Option<Uuid>,
) -> Result<Uuid, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let id = Uuid::new_v4();
    let model = query::ActiveModel {
        id: Set(uuid_hex::to_hex(id)),
        query_text: Set(query_text.to_string()),
        query_type: Set(query_type.to_string()),
        user_id: Set(uuid_hex::to_hex_opt(user_id)),
        created_at: Set(Utc::now()),
    };
    model.insert(db).await.map_err(map_sea_err)?;
    Ok(id)
}

#[instrument(
    name = "cognee.db.relational.search_history.log_result",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn log_result(
    db: &DatabaseConnection,
    query_id: Uuid,
    serialized_result: &str,
    user_id: Option<Uuid>,
) -> Result<Uuid, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let id = Uuid::new_v4();
    let model = result_log::ActiveModel {
        id: Set(uuid_hex::to_hex(id)),
        query_id: Set(uuid_hex::to_hex(query_id)),
        serialized_result: Set(serialized_result.to_string()),
        user_id: Set(uuid_hex::to_hex_opt(user_id)),
        created_at: Set(Utc::now()),
    };
    model.insert(db).await.map_err(map_sea_err)?;
    Ok(id)
}

#[instrument(
    name = "cognee.db.relational.search_history.get_history",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn get_history(
    db: &DatabaseConnection,
    user_id: Option<Uuid>,
    limit: Option<usize>,
) -> Result<Vec<SearchHistoryEntry>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let mut q_query = query::Entity::find();
    if let Some(uid) = user_id {
        q_query = q_query.filter(query::Column::UserId.eq(uuid_hex::to_hex(uid)));
    }
    let queries = q_query
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(query_model_to_history);

    let mut r_query = result_log::Entity::find();
    if let Some(uid) = user_id {
        r_query = r_query.filter(result_log::Column::UserId.eq(uuid_hex::to_hex(uid)));
    }
    let results = r_query
        .all(db)
        .await
        .map_err(map_sea_err)?
        .into_iter()
        .map(result_model_to_history);

    let mut entries: Vec<SearchHistoryEntry> = queries.chain(results).collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.created_at));
    if let Some(n) = limit {
        entries.truncate(n);
    }
    Span::current().record(COGNEE_DB_ROW_COUNT, entries.len() as i64);
    Ok(entries)
}

/// Delete all query rows for a specific user.
///
/// The FK CASCADE on `results.query_id` automatically removes corresponding
/// result rows. Returns the number of deleted query rows.
#[instrument(
    name = "cognee.db.relational.search_history.delete_queries_by_user",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn delete_queries_by_user(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let result = query::Entity::delete_many()
        .filter(query::Column::UserId.eq(uuid_hex::to_hex(user_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Span::current().record(COGNEE_DB_ROW_COUNT, result.rows_affected as i64);
    Ok(result.rows_affected)
}

/// Delete all query rows from the `queries` table.
///
/// The FK CASCADE on `results.query_id` automatically removes all result rows.
/// Returns the number of deleted query rows.
#[instrument(
    name = "cognee.db.relational.search_history.delete_all_queries",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn delete_all_queries(db: &DatabaseConnection) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let result = query::Entity::delete_many()
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Span::current().record(COGNEE_DB_ROW_COUNT, result.rows_affected as i64);
    Ok(result.rows_affected)
}

/// Count query rows for a specific user.
#[instrument(
    name = "cognee.db.relational.search_history.count_queries_by_user",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn count_queries_by_user(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let count = query::Entity::find()
        .filter(query::Column::UserId.eq(uuid_hex::to_hex(user_id)))
        .count(db)
        .await
        .map_err(map_sea_err)?;
    Span::current().record(COGNEE_DB_ROW_COUNT, count as i64);
    Ok(count)
}

/// Count all query rows in the `queries` table.
#[instrument(
    name = "cognee.db.relational.search_history.count_all_queries",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn count_all_queries(db: &DatabaseConnection) -> Result<u64, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let count = query::Entity::find().count(db).await.map_err(map_sea_err)?;
    Span::current().record(COGNEE_DB_ROW_COUNT, count as i64);
    Ok(count)
}
