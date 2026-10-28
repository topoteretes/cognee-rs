//! One watermark implementation for the self-improvement stages.
//!
//! Several improve stages read a session, do expensive work over it and must
//! not repeat that work on the next run. Each keeps a *watermark*: a small
//! record of how far it got, stored as an internal session-context row per
//! (user, session). The row's `kind` is never `"context"`, so
//! [`crate::coerce_active_context_entries`] drops it and it never reaches a
//! prompt.
//!
//! Port of Python's `cognee/infrastructure/session/session_persist_watermark.py`.
//! Rows are `{"id", "kind", <field>, "updated_at"}`, byte-compatible with the
//! rows Python writes. Two policies apply to every count watermark:
//!
//! 1. A watermark above the current total means the session was cleared and
//!    rebuilt; it is stale and the stage starts over
//!    ([`StateRowWatermark::resolve_effective`]).
//! 2. A retried window is harmless: the watermark is read before the work and
//!    advanced only after the window was committed.

use std::borrow::Cow;

use chrono::Utc;
use serde_json::Value;

use crate::context_entry::SessionContextRow;
use crate::error::SessionError;
use crate::session_manager::SessionManager;

/// Row id of the Q&A persist watermark (`persist_session_qa`).
pub const SESSION_PERSIST_STATE_ID: &str = "session_persist_watermark";
/// Row kind of the Q&A persist watermark.
pub const SESSION_PERSIST_STATE_KIND: &str = "session_persist_watermark_state";

/// Row id of the agent-trace persist watermark (`persist_agent_traces`).
pub const TRACE_PERSIST_STATE_ID: &str = "agent_trace_persist_watermark";
/// Row kind of the agent-trace persist watermark.
pub const TRACE_PERSIST_STATE_KIND: &str = "agent_trace_persist_watermark_state";

/// Row id of the trace-extraction watermark (`extract_agent_context`).
pub const TRACE_EXTRACTION_STATE_ID: &str = "__agent_context_extraction_state__";
/// Row kind of the trace-extraction watermark.
pub const TRACE_EXTRACTION_STATE_KIND: &str = "agent_context_extraction_state";

/// Row id prefix of the per-dataset distillation watermark (`distill_sessions`).
pub const SESSION_DISTILL_STATE_ID_PREFIX: &str = "session_distill_watermark";
/// Row kind of the per-dataset distillation watermark.
pub const SESSION_DISTILL_STATE_KIND: &str = "session_distill_watermark_state";
/// Field of the distillation watermark: the distilled entry ids.
pub const SESSION_DISTILL_FIELD: &str = "processed_entry_ids";

/// Persisted session Q&A entry count. Per (user, session), not per dataset.
pub const SESSION_PERSIST_WATERMARK: StateRowWatermark = StateRowWatermark::new(
    SESSION_PERSIST_STATE_ID,
    SESSION_PERSIST_STATE_KIND,
    "persisted_qa_count",
);

/// Persisted agent-trace step count. Per (user, session), not per dataset.
pub const TRACE_PERSIST_WATERMARK: StateRowWatermark = StateRowWatermark::new(
    TRACE_PERSIST_STATE_ID,
    TRACE_PERSIST_STATE_KIND,
    "persisted_trace_count",
);

/// Trace steps already turned into agent lessons.
pub const TRACE_EXTRACTION_WATERMARK: StateRowWatermark = StateRowWatermark::new(
    TRACE_EXTRACTION_STATE_ID,
    TRACE_EXTRACTION_STATE_KIND,
    "processed_trace_count",
);

/// A watermark stored as one internal session-context row per (user, session).
///
/// `state_id` is the row's `id` (one row per watermark per session, updated
/// in place); `state_kind` is the row's `kind`, which keeps it out of every
/// rendered context; `field` is the key holding the value. A value is either
/// a count ([`Self::read_count`] / [`Self::write_count`]) or any JSON value
/// ([`Self::read_value`] / [`Self::write_value`]).
///
/// `match_kind` controls the lookup fallback: when `true` a row is found by
/// `id` or, failing that, by `kind`. A watermark whose id carries a scope
/// suffix (one row per dataset) sets it `false`, so a sibling row of the
/// same kind is never mistaken for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRowWatermark {
    pub state_id: Cow<'static, str>,
    pub state_kind: &'static str,
    pub field: &'static str,
    pub match_kind: bool,
}

impl StateRowWatermark {
    /// A watermark found by `id`, falling back to `kind`.
    pub const fn new(
        state_id: &'static str,
        state_kind: &'static str,
        field: &'static str,
    ) -> Self {
        Self {
            state_id: Cow::Borrowed(state_id),
            state_kind,
            field,
            match_kind: true,
        }
    }

    /// A watermark with a scoped (runtime) id, found by `id` only.
    pub fn scoped(state_id: String, state_kind: &'static str, field: &'static str) -> Self {
        Self {
            state_id: Cow::Owned(state_id),
            state_kind,
            field,
            match_kind: false,
        }
    }

    // -- lookup ------------------------------------------------------------

    /// This watermark's state row among already-loaded context rows.
    pub fn find_row<'a>(&self, rows: &'a [SessionContextRow]) -> Option<&'a SessionContextRow> {
        let field_is = |row: &SessionContextRow, key: &str, want: &str| {
            row.get(key).and_then(Value::as_str) == Some(want)
        };
        rows.iter()
            .find(|row| field_is(row, "id", &self.state_id))
            .or_else(|| {
                self.match_kind
                    .then(|| {
                        rows.iter()
                            .find(|row| field_is(row, "kind", self.state_kind))
                    })
                    .flatten()
            })
    }

    /// The count in already-loaded rows; missing or malformed reads as 0.
    ///
    /// Lenient like Python's `max(0, int(row.get(field) or 0))`: integers,
    /// floats (truncated) and integer strings are accepted, negatives clamp
    /// to 0.
    pub fn count_from_rows(&self, rows: &[SessionContextRow]) -> u64 {
        let Some(value) = self.find_row(rows).and_then(|row| row.get(self.field)) else {
            return 0;
        };
        let count = match value {
            Value::Number(n) => n
                .as_i64()
                .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
                .unwrap_or(0),
            Value::String(s) => s.trim().parse::<i64>().unwrap_or(0),
            Value::Bool(b) => i64::from(*b),
            _ => 0,
        };
        u64::try_from(count).unwrap_or(0)
    }

    /// The raw stored value; `None` when the row (or its field) is missing.
    pub fn value_from_rows(&self, rows: &[SessionContextRow]) -> Option<Value> {
        self.find_row(rows)?.get(self.field).cloned()
    }

    // -- policies ----------------------------------------------------------

    /// The stale-watermark policy: a stored value above the current total
    /// means the session was cleared and rebuilt, so start over from 0.
    pub fn resolve_effective(stored: u64, current_total: u64, session_id: &str) -> u64 {
        if stored > current_total {
            tracing::warn!(
                session_id,
                current_total,
                stored,
                "session has fewer entries than its watermark; treating the watermark as stale and starting from the beginning"
            );
            return 0;
        }
        stored
    }

    // -- storage -----------------------------------------------------------

    /// Read the count watermark. Missing or malformed state reads as 0.
    pub async fn read_count(
        &self,
        manager: &SessionManager,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<u64, SessionError> {
        let rows = manager
            .get_session_context_entries(session_id, user_id)
            .await?;
        Ok(self.count_from_rows(&rows))
    }

    /// Read the raw stored value; `None` when no row exists.
    pub async fn read_value(
        &self,
        manager: &SessionManager,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<Option<Value>, SessionError> {
        let rows = manager
            .get_session_context_entries(session_id, user_id)
            .await?;
        Ok(self.value_from_rows(&rows))
    }

    /// Store `value` under `field`, updating the existing row or creating it.
    pub async fn write_value(
        &self,
        manager: &SessionManager,
        session_id: Option<&str>,
        user_id: Option<&str>,
        value: Value,
    ) -> Result<(), SessionError> {
        let mut payload = SessionContextRow::new();
        payload.insert("id".into(), Value::from(self.state_id.as_ref()));
        payload.insert("kind".into(), Value::from(self.state_kind));
        payload.insert(self.field.into(), value);
        payload.insert("updated_at".into(), Value::from(Utc::now().to_rfc3339()));
        let updated = manager
            .update_session_context_entry(session_id, user_id, &self.state_id, payload.clone())
            .await?;
        if !updated {
            manager
                .create_session_context_entry(session_id, user_id, payload)
                .await?;
        }
        Ok(())
    }

    /// Store a count watermark (clamped at zero).
    pub async fn write_count(
        &self,
        manager: &SessionManager,
        session_id: Option<&str>,
        user_id: Option<&str>,
        count: i64,
    ) -> Result<(), SessionError> {
        self.write_value(manager, session_id, user_id, Value::from(count.max(0)))
            .await
    }
}

/// The distillation watermark for one target dataset.
///
/// Gated context entries are not append-only, so a count cannot describe
/// them: the row stores the ids of the entries already distilled into
/// `dataset_id`. The id carries the dataset, so one session distilled into
/// two datasets keeps two rows (`match_kind` is `false`).
pub fn distill_watermark(dataset_id: &str) -> StateRowWatermark {
    StateRowWatermark::scoped(
        format!("{SESSION_DISTILL_STATE_ID_PREFIX}:{dataset_id}"),
        SESSION_DISTILL_STATE_KIND,
        SESSION_DISTILL_FIELD,
    )
}

/// Ids of the gated context entries already distilled into `dataset_id`; a
/// malformed stored value reads as none.
pub async fn get_distilled_entry_ids(
    manager: &SessionManager,
    session_id: Option<&str>,
    user_id: Option<&str>,
    dataset_id: &str,
) -> Result<std::collections::BTreeSet<String>, SessionError> {
    let value = distill_watermark(dataset_id)
        .read_value(manager, session_id, user_id)
        .await?;
    let Some(Value::Array(items)) = value else {
        return Ok(Default::default());
    };
    Ok(items
        .into_iter()
        .filter_map(|item| match item {
            Value::String(s) if !s.is_empty() => Some(s),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .collect())
}

/// Record the gated entry ids covered by a finished distillation run. The
/// stored set is replaced, not appended to, so ids of entries deleted since
/// drop out and the row stays bounded.
pub async fn save_distilled_entry_ids(
    manager: &SessionManager,
    session_id: Option<&str>,
    user_id: Option<&str>,
    dataset_id: &str,
    entry_ids: impl IntoIterator<Item = String>,
) -> Result<(), SessionError> {
    let ids: std::collections::BTreeSet<String> = entry_ids.into_iter().collect();
    distill_watermark(dataset_id)
        .write_value(
            manager,
            session_id,
            user_id,
            Value::Array(ids.into_iter().map(Value::String).collect()),
        )
        .await
}
