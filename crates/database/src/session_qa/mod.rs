//! Session Q&A storage on the relational database: the `session_qa_entries`,
//! `session_graph_context` and `session_trace_steps` tables, behind
//! [`SessionQaDb`](crate::SessionQaDb).
//!
//! The tables have their own migration chain (`seaql_session_migrations`), run
//! by [`SessionQaDb::ensure_session_schema`](crate::SessionQaDb::ensure_session_schema)
//! only when a relational session store is used, not by the generic
//! [`initialize`](crate::initialize).
//!
//! The row types carry JSON-encoded columns as strings; the session store owns
//! their typed form.

use chrono::{DateTime, Utc};

pub(crate) mod entity;
pub(crate) mod migrator;
pub(crate) mod ops;

/// One `session_qa_entries` row.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionQaRow {
    /// Simple-format UUID.
    pub id: String,
    pub session_id: String,
    pub user_id: Option<String>,
    pub question: String,
    pub answer: String,
    pub context: Option<String>,
    pub created_at: DateTime<Utc>,
    pub feedback_text: Option<String>,
    pub feedback_score: Option<i32>,
    /// JSON-encoded used graph element ids.
    pub used_graph_element_ids: Option<String>,
    /// JSON-encoded memify metadata map.
    pub memify_metadata: Option<String>,
}

/// Field updates for one Q&A entry. `None` leaves a field unchanged; for the
/// nullable fields `Some(None)` clears it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionQaPatch {
    pub question: Option<String>,
    pub answer: Option<String>,
    pub context: Option<Option<String>>,
    pub feedback_text: Option<Option<String>>,
    pub feedback_score: Option<Option<i32>>,
    /// JSON-encoded used graph element ids.
    pub used_graph_element_ids: Option<Option<String>>,
    /// JSON-encoded memify metadata map.
    pub memify_metadata: Option<Option<String>>,
}

/// One `session_trace_steps` row (without the store-assigned `seq` and
/// timestamp).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTraceRow {
    pub trace_id: String,
    pub origin_function: String,
    pub status: String,
    pub memory_query: String,
    pub memory_context: String,
    /// JSON-encoded method parameters.
    pub method_params: String,
    /// JSON-encoded return value.
    pub method_return_value: Option<String>,
    pub error_message: String,
    pub session_feedback: String,
}
