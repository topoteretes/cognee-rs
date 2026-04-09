use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use uuid::Uuid;

use super::entity;
use crate::error::SessionError;

fn map_db_err(e: sea_orm::DbErr) -> SessionError {
    SessionError::StoreError(e.to_string())
}

pub async fn create_qa_entry(
    db: &DatabaseConnection,
    id: Uuid,
    session_id: &str,
    user_id: Option<&str>,
    question: &str,
    answer: &str,
    context: Option<&str>,
) -> Result<(), SessionError> {
    let model = entity::ActiveModel {
        id: Set(id.simple().to_string()),
        session_id: Set(session_id.to_string()),
        user_id: Set(user_id.map(|s| s.to_string())),
        question: Set(question.to_string()),
        answer: Set(answer.to_string()),
        context: Set(context.map(|s| s.to_string())),
        created_at: Set(Utc::now()),
    };
    model.insert(db).await.map_err(map_db_err)?;
    Ok(())
}

pub async fn get_latest_entries(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
    limit: usize,
) -> Result<Vec<entity::Model>, SessionError> {
    // To get the last N entries ordered oldest-first, we query DESC with limit,
    // then reverse in memory.
    let mut query = entity::Entity::find().filter(entity::Column::SessionId.eq(session_id));

    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    let mut entries = query
        .order_by_desc(entity::Column::CreatedAt)
        .limit(limit as u64)
        .all(db)
        .await
        .map_err(map_db_err)?;

    entries.reverse();
    Ok(entries)
}

pub async fn get_all_entries(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
) -> Result<Vec<entity::Model>, SessionError> {
    let mut query = entity::Entity::find().filter(entity::Column::SessionId.eq(session_id));

    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    query
        .order_by_asc(entity::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_db_err)
}

pub async fn delete_session(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
) -> Result<u64, SessionError> {
    let mut delete = entity::Entity::delete_many().filter(entity::Column::SessionId.eq(session_id));

    if let Some(uid) = user_id {
        delete = delete.filter(entity::Column::UserId.eq(uid));
    }

    let result = delete.exec(db).await.map_err(map_db_err)?;
    Ok(result.rows_affected)
}

pub async fn delete_qa_entry(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
    qa_id: &str,
) -> Result<bool, SessionError> {
    let mut delete = entity::Entity::delete_many()
        .filter(entity::Column::Id.eq(qa_id))
        .filter(entity::Column::SessionId.eq(session_id));

    if let Some(uid) = user_id {
        delete = delete.filter(entity::Column::UserId.eq(uid));
    }

    let result = delete.exec(db).await.map_err(map_db_err)?;
    Ok(result.rows_affected > 0)
}
