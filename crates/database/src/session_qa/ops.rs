use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
};
use uuid::Uuid;

use super::entity;
use super::{SessionQaPatch, SessionQaRow, SessionTraceRow};
use crate::types::DatabaseError;

fn map_db_err(e: sea_orm::DbErr) -> DatabaseError {
    DatabaseError::QueryError(e.to_string())
}

fn model_to_row(m: entity::Model) -> SessionQaRow {
    SessionQaRow {
        id: m.id,
        session_id: m.session_id,
        user_id: m.user_id,
        question: m.question,
        answer: m.answer,
        context: m.context,
        created_at: m.created_at,
        feedback_text: m.feedback_text,
        feedback_score: m.feedback_score,
        used_graph_element_ids: m.used_graph_element_ids,
        memify_metadata: m.memify_metadata,
    }
}

pub async fn create_qa_entry(
    db: &DatabaseConnection,
    id: Uuid,
    session_id: &str,
    user_id: Option<&str>,
    question: &str,
    answer: &str,
    context: Option<&str>,
) -> Result<(), DatabaseError> {
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
) -> Result<Vec<SessionQaRow>, DatabaseError> {
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
    Ok(entries.into_iter().map(model_to_row).collect())
}

pub async fn get_all_entries(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
) -> Result<Vec<SessionQaRow>, DatabaseError> {
    let mut query = entity::Entity::find().filter(entity::Column::SessionId.eq(session_id));

    if let Some(uid) = user_id {
        query = query.filter(entity::Column::UserId.eq(uid));
    }

    query
        .order_by_asc(entity::Column::CreatedAt)
        .all(db)
        .await
        .map(|rows| rows.into_iter().map(model_to_row).collect())
        .map_err(map_db_err)
}

pub async fn delete_session(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
) -> Result<u64, DatabaseError> {
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

    // And the session-context rows, which count toward "the session existed"
    // as in Python's `delete_session`.
    // Context rows are keyed by user like every other context-row op: a
    // session without a user is the `"default"` bucket, never every user's.
    let ctx_result = entity::context_entry::Entity::delete_many()
        .filter(entity::context_entry::Column::SessionId.eq(session_id))
        .filter(entity::context_entry::Column::UserId.eq(context_user_key(user_id)))
        .exec(db)
        .await
        .map_err(map_db_err)?;

    Ok(result.rows_affected + ctx_result.rows_affected)
}

pub async fn delete_qa_entry(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
    qa_id: &str,
) -> Result<bool, DatabaseError> {
    let mut delete = entity::Entity::delete_many()
        .filter(entity::Column::Id.eq(qa_id))
        .filter(entity::Column::SessionId.eq(session_id));

    if let Some(uid) = user_id {
        delete = delete.filter(entity::Column::UserId.eq(uid));
    }

    let result = delete.exec(db).await.map_err(map_db_err)?;
    Ok(result.rows_affected > 0)
}

/// Delete all rows from the session_qa_entries, session_graph_context and
/// session_context_entries tables (prune).
pub async fn delete_all(db: &DatabaseConnection) -> Result<(), DatabaseError> {
    entity::Entity::delete_many()
        .exec(db)
        .await
        .map_err(map_db_err)?;
    entity::graph_context::Entity::delete_many()
        .exec(db)
        .await
        .map_err(map_db_err)?;
    entity::context_entry::Entity::delete_many()
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
    updates: SessionQaPatch,
) -> Result<bool, DatabaseError> {
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
        active.used_graph_element_ids = Set(ids.clone());
    }
    if let Some(ref mm) = updates.memify_metadata {
        active.memify_metadata = Set(mm.clone());
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
) -> Result<Option<String>, DatabaseError> {
    let id = graph_context_id(user_id, session_id);
    let model = entity::graph_context::Entity::find_by_id(&id)
        .one(db)
        .await
        .map_err(map_db_err)?;
    Ok(model.map(|m| m.context))
}

/// Append one agent-trace step. Returns the persisted `trace_id`.
pub async fn save_trace_step(
    db: &DatabaseConnection,
    user_id: &str,
    session_id: &str,
    step: SessionTraceRow,
) -> Result<String, DatabaseError> {
    // Assign next `seq` for this `(user_id, session_id)` so reads return
    // entries in stable insertion order (independent of timestamp resolution).
    let max_seq: Option<i64> = entity::trace_step::Entity::find()
        .filter(entity::trace_step::Column::UserId.eq(user_id))
        .filter(entity::trace_step::Column::SessionId.eq(session_id))
        .order_by_desc(entity::trace_step::Column::Seq)
        .limit(1)
        .one(db)
        .await
        .map_err(map_db_err)?
        .map(|m| m.seq);
    let next_seq = max_seq.unwrap_or(0) + 1;

    let trace_id = step.trace_id.clone();
    let model = entity::trace_step::ActiveModel {
        trace_id: Set(trace_id.clone()),
        user_id: Set(user_id.to_string()),
        session_id: Set(session_id.to_string()),
        seq: Set(next_seq),
        created_at: Set(Utc::now()),
        origin_function: Set(step.origin_function),
        status: Set(step.status),
        memory_query: Set(step.memory_query),
        memory_context: Set(step.memory_context),
        method_params: Set(step.method_params),
        method_return_value: Set(step.method_return_value),
        error_message: Set(step.error_message),
        session_feedback: Set(step.session_feedback),
    };
    model.insert(db).await.map_err(map_db_err)?;
    Ok(trace_id)
}

/// Read agent-trace steps for `(user_id, session_id)`, ordered oldest-first.
pub async fn read_trace_steps(
    db: &DatabaseConnection,
    user_id: &str,
    session_id: &str,
) -> Result<Vec<SessionTraceRow>, DatabaseError> {
    let models = entity::trace_step::Entity::find()
        .filter(entity::trace_step::Column::UserId.eq(user_id))
        .filter(entity::trace_step::Column::SessionId.eq(session_id))
        .order_by_asc(entity::trace_step::Column::Seq)
        .all(db)
        .await
        .map_err(map_db_err)?;
    Ok(models
        .into_iter()
        .map(|m| SessionTraceRow {
            trace_id: m.trace_id,
            origin_function: m.origin_function,
            status: m.status,
            memory_query: m.memory_query,
            memory_context: m.memory_context,
            method_params: m.method_params,
            method_return_value: m.method_return_value,
            error_message: m.error_message,
            session_feedback: m.session_feedback,
        })
        .collect())
}

/// Store (or overwrite) the graph context for a session.
pub async fn set_graph_context(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
    context: &str,
) -> Result<(), DatabaseError> {
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

// ── session-context rows ───────────────────────────────────────────────────

/// The `user_id` key of a session's context rows: the user, or `"default"`
/// for a session without one — the bucket the file and Redis stores use.
/// Python's cache always has a user, and matches it exactly.
fn context_user_key(user_id: Option<&str>) -> &str {
    user_id.unwrap_or(DEFAULT_CONTEXT_USER)
}

/// [`context_user_key`] of a session without a user.
const DEFAULT_CONTEXT_USER: &str = "default";

/// `session_context_entries` rows of one user's session.
fn context_entry_query(
    session_id: &str,
    user_id: Option<&str>,
) -> sea_orm::Select<entity::context_entry::Entity> {
    entity::context_entry::Entity::find()
        .filter(entity::context_entry::Column::SessionId.eq(session_id))
        .filter(entity::context_entry::Column::UserId.eq(context_user_key(user_id)))
}

/// Create or replace one session-context row keyed by
/// `(user_id, session_id, entry_id)` — Python's `SqlCacheAdapter` upsert:
/// one `INSERT … ON CONFLICT DO UPDATE` on the `uq_session_context_entry`
/// index, so concurrent update-then-create flows converge on one row, last
/// writer wins, on every isolation level. A replaced row keeps its position
/// in the session's order.
pub async fn create_context_entry(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
    entry_id: &str,
    payload: String,
) -> Result<(), DatabaseError> {
    use sea_orm::sea_query::OnConflict;

    let now = Utc::now();
    // A tie with a concurrent insert only orders the two rows by id; a
    // replaced row ignores it.
    let seq = next_context_seq(db, session_id).await?;
    let row = entity::context_entry::ActiveModel {
        id: Set(Uuid::new_v4().to_string()),
        session_id: Set(session_id.to_string()),
        user_id: Set(context_user_key(user_id).to_string()),
        entry_id: Set(entry_id.to_string()),
        seq: Set(seq),
        payload: Set(payload),
        created_at: Set(now),
        updated_at: Set(now),
    };
    entity::context_entry::Entity::insert(row)
        .on_conflict(
            OnConflict::columns([
                entity::context_entry::Column::UserId,
                entity::context_entry::Column::SessionId,
                entity::context_entry::Column::EntryId,
            ])
            .update_columns([
                entity::context_entry::Column::Payload,
                entity::context_entry::Column::UpdatedAt,
            ])
            .to_owned(),
        )
        .exec_without_returning(db)
        .await
        .map_err(map_db_err)?;
    Ok(())
}

/// The next per-session `seq` (insertion order independent of timestamp
/// resolution, like `session_trace_steps`).
async fn next_context_seq<C: ConnectionTrait>(
    db: &C,
    session_id: &str,
) -> Result<i64, DatabaseError> {
    let max_seq = entity::context_entry::Entity::find()
        .filter(entity::context_entry::Column::SessionId.eq(session_id))
        .order_by_desc(entity::context_entry::Column::Seq)
        .limit(1)
        .one(db)
        .await
        .map_err(map_db_err)?
        .map(|m| m.seq);
    Ok(max_seq.unwrap_or(0) + 1)
}

/// The session's context-row payloads, oldest first.
pub async fn get_context_entries(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
) -> Result<Vec<String>, DatabaseError> {
    let models = context_entry_query(session_id, user_id)
        .order_by_asc(entity::context_entry::Column::Seq)
        .order_by_asc(entity::context_entry::Column::Id)
        .all(db)
        .await
        .map_err(map_db_err)?;
    Ok(models.into_iter().map(|m| m.payload).collect())
}

/// Shallow-merge `merge` into the context row holding `entry_id`. Returns
/// `false` when no such row exists.
pub async fn update_context_entry(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Option<&str>,
    entry_id: &str,
    merge: &serde_json::Map<String, serde_json::Value>,
) -> Result<bool, DatabaseError> {
    let txn = db.begin().await.map_err(map_db_err)?;
    let rows = context_entry_query(session_id, user_id)
        .filter(entity::context_entry::Column::EntryId.eq(entry_id))
        .order_by_desc(entity::context_entry::Column::Seq)
        .all(&txn)
        .await
        .map_err(map_db_err)?;
    // The unique index keeps this to one row; Python still resolves
    // duplicates to the newest and writes the merge to every one of them.
    let Some(newest) = rows.first() else {
        return Ok(false);
    };
    let mut merged: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&newest.payload).map_err(|e| {
            DatabaseError::QueryError(format!("session context payload is not an object: {e}"))
        })?;
    merged.extend(merge.clone());
    let payload = serde_json::to_string(&merged)
        .map_err(|e| DatabaseError::QueryError(format!("session context json error: {e}")))?;
    let now = Utc::now();
    for model in rows {
        let mut active: entity::context_entry::ActiveModel = model.into();
        active.payload = Set(payload.clone());
        active.updated_at = Set(now);
        active.update(&txn).await.map_err(map_db_err)?;
    }
    txn.commit().await.map_err(map_db_err)?;
    Ok(true)
}
