use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_database::{DatabaseError, SessionQaDb, SessionQaPatch, SessionQaRow, SessionTraceRow};
use uuid::Uuid;

use crate::context_entry::SessionContextRow;
use crate::error::SessionError;
use crate::session_store::{SessionQAUpdate, SessionStore};
use crate::types::{SessionQAEntry, SessionTraceStep, UsedGraphElementIds};

/// Session store on the relational database (`session_qa_entries`,
/// `session_graph_context`, `session_trace_steps`, `session_context_entries`),
/// through any
/// [`SessionQaDb`] — the built-in SeaORM connection (SQLite, Postgres) or
/// another relational backend.
///
/// Creates the session tables on construction, so they exist only when this
/// store is actually used (not as part of the generic database init).
pub struct RelationalSessionStore {
    db: Arc<dyn SessionQaDb>,
}

/// Former name of [`RelationalSessionStore`].
#[deprecated(note = "renamed to RelationalSessionStore; it takes any SessionQaDb")]
pub type SeaOrmSessionStore = RelationalSessionStore;

impl RelationalSessionStore {
    /// Create a new store and ensure the session schema exists.
    pub async fn new(db: Arc<dyn SessionQaDb>) -> Result<Self, SessionError> {
        db.ensure_session_schema().await.map_err(store_err)?;
        Ok(Self { db })
    }
}

fn store_err(e: DatabaseError) -> SessionError {
    SessionError::StoreError(e.to_string())
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, SessionError> {
    serde_json::to_string(value).map_err(|e| SessionError::StoreError(format!("json error: {e}")))
}

fn row_to_entry(m: SessionQaRow) -> SessionQAEntry {
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

fn update_to_patch(updates: SessionQAUpdate) -> Result<SessionQaPatch, SessionError> {
    Ok(SessionQaPatch {
        question: updates.question,
        answer: updates.answer,
        context: updates.context,
        feedback_text: updates.feedback_text,
        feedback_score: updates.feedback_score,
        used_graph_element_ids: updates
            .used_graph_element_ids
            .map(|ids| ids.as_ref().map(to_json).transpose())
            .transpose()?,
        memify_metadata: updates
            .memify_metadata
            .map(|mm| mm.as_ref().map(to_json).transpose())
            .transpose()?,
    })
}

fn step_to_row(step: SessionTraceStep) -> Result<SessionTraceRow, SessionError> {
    Ok(SessionTraceRow {
        method_params: to_json(&step.method_params)?,
        method_return_value: step.method_return_value.as_ref().map(to_json).transpose()?,
        trace_id: step.trace_id,
        origin_function: step.origin_function,
        status: step.status,
        memory_query: step.memory_query,
        memory_context: step.memory_context,
        error_message: step.error_message,
        session_feedback: step.session_feedback,
    })
}

fn row_to_step(m: SessionTraceRow) -> Result<SessionTraceStep, SessionError> {
    let parse = |s: &str| {
        serde_json::from_str::<serde_json::Value>(s)
            .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))
    };
    Ok(SessionTraceStep {
        method_params: parse(&m.method_params)?,
        method_return_value: m.method_return_value.as_deref().map(parse).transpose()?,
        trace_id: m.trace_id,
        origin_function: m.origin_function,
        status: m.status,
        memory_query: m.memory_query,
        memory_context: m.memory_context,
        error_message: m.error_message,
        session_feedback: m.session_feedback,
    })
}

#[async_trait]
impl SessionStore for RelationalSessionStore {
    async fn create_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        question: &str,
        answer: &str,
        context: Option<&str>,
    ) -> Result<String, SessionError> {
        let id = Uuid::new_v4();
        self.db
            .create_session_qa_entry(id, session_id, user_id, question, answer, context)
            .await
            .map_err(store_err)?;
        Ok(id.simple().to_string())
    }

    async fn get_latest_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        last_n: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let rows = self
            .db
            .latest_session_qa_entries(session_id, user_id, last_n)
            .await
            .map_err(store_err)?;
        Ok(rows.into_iter().map(row_to_entry).collect())
    }

    async fn get_all_qa_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        let rows = self
            .db
            .all_session_qa_entries(session_id, user_id)
            .await
            .map_err(store_err)?;
        Ok(rows.into_iter().map(row_to_entry).collect())
    }

    async fn delete_session(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<bool, SessionError> {
        let rows = self
            .db
            .delete_session_qa(session_id, user_id)
            .await
            .map_err(store_err)?;
        Ok(rows > 0)
    }

    async fn delete_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, SessionError> {
        self.db
            .delete_session_qa_entry(session_id, user_id, qa_id)
            .await
            .map_err(store_err)
    }

    async fn prune(&self) -> Result<(), SessionError> {
        self.db.delete_all_session_qa().await.map_err(store_err)
    }

    async fn update_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
        updates: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        let patch = update_to_patch(updates)?;
        self.db
            .update_session_qa_entry(session_id, user_id, qa_id, patch)
            .await
            .map_err(store_err)
    }

    async fn get_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        self.db
            .get_session_graph_context(session_id, user_id)
            .await
            .map_err(store_err)
    }

    async fn set_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), SessionError> {
        self.db
            .set_session_graph_context(session_id, user_id, context)
            .await
            .map_err(store_err)
    }

    async fn save_trace_step(
        &self,
        user_id: &str,
        session_id: &str,
        step: SessionTraceStep,
    ) -> Result<String, SessionError> {
        let row = step_to_row(step)?;
        self.db
            .save_session_trace_step(user_id, session_id, row)
            .await
            .map_err(store_err)
    }

    async fn read_trace_steps(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Vec<SessionTraceStep>, SessionError> {
        let rows = self
            .db
            .read_session_trace_steps(user_id, session_id)
            .await
            .map_err(store_err)?;
        rows.into_iter().map(row_to_step).collect()
    }

    async fn create_session_context_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        entry: SessionContextRow,
    ) -> Result<(), SessionError> {
        // The row's "id" is promoted to the key column; an id-less row gets a
        // synthetic key (never targetable by an update), as in Python.
        let entry_id = match entry.get("id") {
            Some(serde_json::Value::String(id)) if !id.is_empty() => id.clone(),
            _ => Uuid::new_v4().to_string(),
        };
        let payload = to_json(&entry)?;
        self.db
            .create_session_context_entry(session_id, user_id, &entry_id, payload)
            .await
            .map_err(store_err)
    }

    async fn get_session_context_entries(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionContextRow>, SessionError> {
        let payloads = self
            .db
            .session_context_entries(session_id, user_id)
            .await
            .map_err(store_err)?;
        payloads
            .iter()
            .map(|p| {
                serde_json::from_str::<SessionContextRow>(p)
                    .map_err(|e| SessionError::StoreError(format!("json parse error: {e}")))
            })
            .collect()
    }

    async fn update_session_context_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        entry_id: &str,
        merge: SessionContextRow,
    ) -> Result<bool, SessionError> {
        self.db
            .update_session_context_entry(session_id, user_id, entry_id, &merge)
            .await
            .map_err(store_err)
    }
}
