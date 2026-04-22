use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use uuid::Uuid;

use super::entity;
use crate::error::SessionError;
use crate::session_store::SessionQAUpdate;

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
        feedback_text: Set(None),
        feedback_score: Set(None),
        used_graph_element_ids: Set(None),
        memify_metadata: Set(None),
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

    // Also delete graph context for this session
    let mut gc_delete = entity::graph_context::Entity::delete_many()
        .filter(entity::graph_context::Column::SessionId.eq(session_id));
    if let Some(uid) = user_id {
        gc_delete = gc_delete.filter(entity::graph_context::Column::UserId.eq(uid));
    }
    let _ = gc_delete.exec(db).await.map_err(map_db_err)?;

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

/// Delete all rows from the session_qa_entries and session_graph_context tables (prune).
pub async fn delete_all(db: &DatabaseConnection) -> Result<(), SessionError> {
    entity::Entity::delete_many()
        .exec(db)
        .await
        .map_err(map_db_err)?;
    entity::graph_context::Entity::delete_many()
        .exec(db)
        .await
        .map_err(map_db_err)?;
    Ok(())
}

/// Update fields on a QA entry. Returns true if the entry was found and updated.
pub async fn update_qa_entry(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
    qa_id: &str,
    updates: SessionQAUpdate,
) -> Result<bool, SessionError> {
    // First find the existing entry
    let mut query = entity::Entity::find()
        .filter(entity::Column::Id.eq(qa_id))
        .filter(entity::Column::SessionId.eq(session_id));

    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    let existing = query.one(db).await.map_err(map_db_err)?;
    let Some(existing) = existing else {
        return Ok(false);
    };

    // Build an ActiveModel with only the fields that need updating
    let mut active: entity::ActiveModel = existing.into();

    if let Some(ref q) = updates.question {
        active.question = Set(q.clone());
    }
    if let Some(ref a) = updates.answer {
        active.answer = Set(a.clone());
    }
    if let Some(ref ctx) = updates.context {
        active.context = Set(ctx.clone());
    }
    if let Some(ref ft) = updates.feedback_text {
        active.feedback_text = Set(ft.clone());
    }
    if let Some(ref fs) = updates.feedback_score {
        active.feedback_score = Set(*fs);
    }
    if let Some(ref ids) = updates.used_graph_element_ids {
        active.used_graph_element_ids = Set(ids
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default()));
    }
    if let Some(ref mm) = updates.memify_metadata {
        active.memify_metadata = Set(mm
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default()));
    }

    // The `id` field is already `Unchanged` from the `.into()` conversion,
    // which SeaORM uses as the WHERE clause for the UPDATE statement.
    active.update(db).await.map_err(map_db_err)?;
    Ok(true)
}

fn graph_context_id(user_id: Option<&str>, session_id: &str) -> String {
    let uid = user_id.unwrap_or("default");
    format!("{uid}:{session_id}")
}

/// Retrieve the graph context for a session.
pub async fn get_graph_context(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
) -> Result<Option<String>, SessionError> {
    let id = graph_context_id(user_id, session_id);
    let model = entity::graph_context::Entity::find_by_id(&id)
        .one(db)
        .await
        .map_err(map_db_err)?;
    Ok(model.map(|m| m.context))
}

/// Store (or overwrite) the graph context for a session.
pub async fn set_graph_context(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
    context: &str,
) -> Result<(), SessionError> {
    let id = graph_context_id(user_id, session_id);

    // Try to find existing, then update or insert
    let existing = entity::graph_context::Entity::find_by_id(&id)
        .one(db)
        .await
        .map_err(map_db_err)?;

    if let Some(existing) = existing {
        let mut active: entity::graph_context::ActiveModel = existing.into();
        active.context = Set(context.to_string());
        active.updated_at = Set(Utc::now());
        active.update(db).await.map_err(map_db_err)?;
    } else {
        let model = entity::graph_context::ActiveModel {
            id: Set(id),
            session_id: Set(session_id.to_string()),
            user_id: Set(user_id.map(|s| s.to_string())),
            context: Set(context.to_string()),
            updated_at: Set(Utc::now()),
        };
        model.insert(db).await.map_err(map_db_err)?;
    }

    Ok(())
}
