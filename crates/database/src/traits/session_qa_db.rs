use async_trait::async_trait;
use sea_orm::DatabaseConnection;
use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

use crate::session_qa::migrator::SessionMigrator;
use crate::session_qa::{SessionQaPatch, SessionQaRow, SessionTraceRow, ops};
use crate::types::DatabaseError;

/// Session Q&A entries, per-session graph context and agent trace steps kept
/// in the relational database (the `relational` session store).
///
/// `user_id: None` selects rows without a user filter on reads and deletes.
#[async_trait]
pub trait SessionQaDb: Send + Sync {
    /// Create the session tables if they do not exist. Idempotent; called by
    /// the session store before first use.
    async fn ensure_session_schema(&self) -> Result<(), DatabaseError>;

    /// Insert one Q&A entry with the given id.
    async fn create_session_qa_entry(
        &self,
        id: Uuid,
        session_id: &str,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<(), DatabaseError>;

    /// The newest `limit` entries of the session, oldest first.
    async fn latest_session_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SessionQaRow>, DatabaseError>;

    /// Every entry of the session, oldest first.
    async fn all_session_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionQaRow>, DatabaseError>;

    /// Delete the session's entries and graph context; returns the number of
    /// entries removed.
    async fn delete_session_qa(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<u64, DatabaseError>;

    /// Delete one entry; whether it existed.
    async fn delete_session_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, DatabaseError>;

    /// Delete every entry and graph context of every session.
    async fn delete_all_session_qa(&self) -> Result<(), DatabaseError>;

    /// Apply `patch` to one entry; whether it existed.
    async fn update_session_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
        patch: SessionQaPatch,
    ) -> Result<bool, DatabaseError>;

    /// The session's stored graph context.
    async fn get_session_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Option<String>, DatabaseError>;

    /// Store (or overwrite) the session's graph context.
    async fn set_session_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), DatabaseError>;

    /// Append one agent trace step; returns its `trace_id`.
    async fn save_session_trace_step(
        &self,
        user_id: &str,
        session_id: &str,
        step: SessionTraceRow,
    ) -> Result<String, DatabaseError>;

    /// The session's agent trace steps in insertion order.
    async fn read_session_trace_steps(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Vec<SessionTraceRow>, DatabaseError>;
}

#[async_trait]
impl SessionQaDb for DatabaseConnection {
    async fn ensure_session_schema(&self) -> Result<(), DatabaseError> {
        SessionMigrator::up(self, None)
            .await
            .map_err(|e| DatabaseError::QueryError(format!("session migration failed: {e}")))
    }

    async fn create_session_qa_entry(
        &self,
        id: Uuid,
        session_id: &str,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<(), DatabaseError> {
        ops::create_qa_entry(self, id, session_id, user_id, question, answer, context).await
    }

    async fn latest_session_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SessionQaRow>, DatabaseError> {
        ops::get_latest_entries(self, session_id, user_id, limit).await
    }

    async fn all_session_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionQaRow>, DatabaseError> {
        ops::get_all_entries(self, session_id, user_id).await
    }

    async fn delete_session_qa(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<u64, DatabaseError> {
        ops::delete_session(self, session_id, user_id).await
    }

    async fn delete_session_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, DatabaseError> {
        ops::delete_qa_entry(self, session_id, user_id, qa_id).await
    }

    async fn delete_all_session_qa(&self) -> Result<(), DatabaseError> {
        ops::delete_all(self).await
    }

    async fn update_session_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
        patch: SessionQaPatch,
    ) -> Result<bool, DatabaseError> {
        ops::update_qa_entry(self, session_id, user_id, qa_id, patch).await
    }

    async fn get_session_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Option<String>, DatabaseError> {
        ops::get_graph_context(self, session_id, user_id).await
    }

    async fn set_session_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), DatabaseError> {
        ops::set_graph_context(self, session_id, user_id, context).await
    }

    async fn save_session_trace_step(
        &self,
        user_id: &str,
        session_id: &str,
        step: SessionTraceRow,
    ) -> Result<String, DatabaseError> {
        ops::save_trace_step(self, user_id, session_id, step).await
    }

    async fn read_session_trace_steps(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Vec<SessionTraceRow>, DatabaseError> {
        ops::read_trace_steps(self, user_id, session_id).await
    }
}
