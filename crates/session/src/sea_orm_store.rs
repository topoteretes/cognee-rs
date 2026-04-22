use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

use crate::error::SessionError;
use crate::migrator::SessionMigrator;
use crate::sea_orm_backend::{entity, ops};
use crate::session_store::{SessionQAUpdate, SessionStore};
use crate::types::{SessionQAEntry, UsedGraphElementIds};

/// SeaORM-backed session store using the `session_qa_entries` table.
///
/// Works with any SeaORM-supported database (SQLite, PostgreSQL, MySQL).
/// Runs its own schema migration on creation so the table is only created when
/// this backend is actually used (not as part of the generic database init).
pub struct SeaOrmSessionStore {
    db: Arc<DatabaseConnection>,
}

impl SeaOrmSessionStore {
    /// Create a new store and run the session schema migration.
    pub async fn new(db: Arc<DatabaseConnection>) -> Result<Self, SessionError> {
        SessionMigrator::up(db.as_ref(), None)
            .await
            .map_err(|e| SessionError::StoreError(format!("session migration failed: {e}")))?;
        Ok(Self { db })
    }
}

fn model_to_entry(m: entity::Model) -> SessionQAEntry {
    let used_graph_element_ids = m
        .used_graph_element_ids
        .as_deref()
        .and_then(|s| serde_json::from_str::<UsedGraphElementIds>(s).ok());
    let memify_metadata = m
        .memify_metadata
        .as_deref()
        .and_then(|s| serde_json::from_str::<HashMap<String, bool>>(s).ok());

    SessionQAEntry {
        id: Uuid::parse_str(&m.id).unwrap_or_default(),
        session_id: m.session_id,
        user_id: m.user_id,
        question: m.question,
        answer: m.answer,
        context: m.context,
        created_at: m.created_at,
        feedback_text: m.feedback_text,
        feedback_score: m.feedback_score,
        used_graph_element_ids,
        memify_metadata,
    }
}

#[async_trait]
impl SessionStore for SeaOrmSessionStore {
    async fn create_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<String, SessionError> {
        let id = Uuid::new_v4();
        ops::create_qa_entry(&self.db, id, session_id, user_id, question, answer, context).await?;
        Ok(id.simple().to_string())
    }

    async fn get_latest_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        last_n: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let models = ops::get_latest_entries(&self.db, session_id, user_id, last_n).await?;
        Ok(models.into_iter().map(model_to_entry).collect())
    }

    async fn get_all_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let models = ops::get_all_entries(&self.db, session_id, user_id).await?;
        Ok(models.into_iter().map(model_to_entry).collect())
    }

    async fn delete_session(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<bool, SessionError> {
        let rows = ops::delete_session(&self.db, session_id, user_id).await?;
        Ok(rows > 0)
    }

    async fn delete_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, SessionError> {
        let deleted = ops::delete_qa_entry(&self.db, session_id, user_id, qa_id).await?;
        Ok(deleted)
    }

    async fn prune(&self) -> Result<(), SessionError> {
        ops::delete_all(&self.db).await
    }

    async fn update_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
        updates: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        ops::update_qa_entry(&self.db, session_id, user_id, qa_id, updates).await
    }

    async fn get_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        ops::get_graph_context(&self.db, session_id, user_id).await
    }

    async fn set_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), SessionError> {
        ops::set_graph_context(&self.db, session_id, user_id, context).await
    }
}
