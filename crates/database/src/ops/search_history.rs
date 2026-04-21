use chrono::Utc;
use sea_orm::PaginatorTrait;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use uuid::Uuid;

use crate::conversions::{map_sea_err, query_model_to_history, result_model_to_history};
use crate::entities::{query, result_log};
use crate::types::{DatabaseError, SearchHistoryEntry};
use crate::uuid_hex;

pub async fn log_query(
    db: &DatabaseConnection,
    query_text: &str,
    query_type: &str,
    user_id: Option<Uuid>,
) -> Result<Uuid, DatabaseError> {
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

pub async fn log_result(
    db: &DatabaseConnection,
    query_id: Uuid,
    serialized_result: &str,
    user_id: Option<Uuid>,
) -> Result<Uuid, DatabaseError> {
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

pub async fn get_history(
    db: &DatabaseConnection,
    user_id: Option<Uuid>,
    limit: Option<usize>,
) -> Result<Vec<SearchHistoryEntry>, DatabaseError> {
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
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if let Some(n) = limit {
        entries.truncate(n);
    }
    Ok(entries)
}

/// Delete all query rows for a specific user.
///
/// The FK CASCADE on `results.query_id` automatically removes corresponding
/// result rows. Returns the number of deleted query rows.
pub async fn delete_queries_by_user(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<u64, DatabaseError> {
    let result = query::Entity::delete_many()
        .filter(query::Column::UserId.eq(uuid_hex::to_hex(user_id)))
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(result.rows_affected)
}

/// Delete all query rows from the `queries` table.
///
/// The FK CASCADE on `results.query_id` automatically removes all result rows.
/// Returns the number of deleted query rows.
pub async fn delete_all_queries(db: &DatabaseConnection) -> Result<u64, DatabaseError> {
    let result = query::Entity::delete_many()
        .exec(db)
        .await
        .map_err(map_sea_err)?;
    Ok(result.rows_affected)
}

/// Count query rows for a specific user.
pub async fn count_queries_by_user(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<u64, DatabaseError> {
    let count = query::Entity::find()
        .filter(query::Column::UserId.eq(uuid_hex::to_hex(user_id)))
        .count(db)
        .await
        .map_err(map_sea_err)?;
    Ok(count)
}

/// Count all query rows in the `queries` table.
pub async fn count_all_queries(db: &DatabaseConnection) -> Result<u64, DatabaseError> {
    let count = query::Entity::find().count(db).await.map_err(map_sea_err)?;
    Ok(count)
}
