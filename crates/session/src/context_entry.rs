//! Session-context rows: the per-session list of active guidance entries
//! (goals, rules, preferences, lessons) and the internal state rows the
//! improve stages keep next to them.
//!
//! Mirrors Python's `cognee/infrastructure/session/session_context_models.py`
//! and the dict-based cache interface (`CacheDBInterface`'s
//! `create/get/update_session_context_entry`). Rows travel as JSON objects
//! ([`SessionContextRow`]) carrying a `"kind"` field, so the store keeps
//! guidance (`"context"`), feedback (`"feedback"`) and state rows (any other
//! kind, e.g. a [`crate::StateRowWatermark`]) in one list. Only `"context"`
//! rows are rendered; [`coerce_active_context_entries`] is the filter.

use serde::{Deserialize, Serialize};

use crate::error::SessionError;

/// One stored session-context row: a JSON object with a `"kind"` field.
/// Python's rows are plain dicts; the stores keep them as-is.
pub type SessionContextRow = serde_json::Map<String, serde_json::Value>;

/// `kind` of an active guidance entry ([`SessionContextEntry`]).
pub const CONTEXT_ENTRY_KIND: &str = "context";
/// `kind` of a stored feedback record (Python's `SessionFeedbackEntry`).
pub const FEEDBACK_ENTRY_KIND: &str = "feedback";

/// Longest stored entry content, in characters; longer content is truncated.
pub const MAX_CONTEXT_CONTENT_CHARS: usize = 280;

/// Profile of a lesson for Q&A consumers (the default).
pub const CONTEXT_PROFILE_QA: &str = "qa";
/// Profile of a lesson for agent / tool-loop consumers.
pub const CONTEXT_PROFILE_AGENT: &str = "agent";

/// Sections of a [`CONTEXT_PROFILE_QA`] entry (Python's `ContextSection`).
pub const QA_CONTEXT_SECTIONS: &[&str] = &["goals", "rules", "preferences", "lessons_learned"];
/// Sections of a [`CONTEXT_PROFILE_AGENT`] entry (Python's `AgentContextSection`).
pub const AGENT_CONTEXT_SECTIONS: &[&str] = &[
    "tool_rules",
    "workflow_state",
    "success_patterns",
    "failure_lessons",
    "environment_facts",
];

/// Allowed section names for a context profile; empty for an unknown profile.
pub fn valid_sections_for(profile: &str) -> &'static [&'static str] {
    match profile {
        CONTEXT_PROFILE_QA => QA_CONTEXT_SECTIONS,
        CONTEXT_PROFILE_AGENT => AGENT_CONTEXT_SECTIONS,
        _ => &[],
    }
}

/// Deterministic exact-match key: lowercased, whitespace-collapsed, stripped.
pub fn normalize_content(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A stored active session-context lesson, tagged by profile (Python's
/// `SessionContextEntry`). Serialises to the same row shape as Python's
/// `model_dump()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionContextEntry {
    pub id: String,
    pub section: String,
    #[serde(default = "default_profile")]
    pub context_profile: String,
    pub content: String,
    #[serde(default)]
    pub normalized_content: String,
    #[serde(default)]
    pub confidence: f64,
    pub created_at: String,
    #[serde(default)]
    pub source_feedback_ids: Vec<String>,
    #[serde(default)]
    pub source_trace_ids: Vec<String>,
    #[serde(default)]
    pub helpful_count: u64,
    #[serde(default)]
    pub harmful_count: u64,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub last_served_at: Option<String>,
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_profile() -> String {
    CONTEXT_PROFILE_QA.to_string()
}

fn default_kind() -> String {
    CONTEXT_ENTRY_KIND.to_string()
}

fn invalid(msg: impl Into<String>) -> SessionError {
    SessionError::InvalidContextEntry(msg.into())
}

impl SessionContextEntry {
    /// Parse and validate a stored row, applying Python's field validators
    /// (see [`Self::validated`]).
    pub fn from_row(row: &SessionContextRow) -> Result<Self, SessionError> {
        let entry: Self = serde_json::from_value(serde_json::Value::Object(row.clone()))
            .map_err(|e| invalid(e.to_string()))?;
        entry.validated()
    }

    /// The row this entry is stored as.
    pub fn to_row(&self) -> Result<SessionContextRow, SessionError> {
        match serde_json::to_value(self).map_err(|e| invalid(e.to_string()))? {
            serde_json::Value::Object(row) => Ok(row),
            _ => Err(invalid("entry did not serialise to an object")),
        }
    }

    /// Apply Python's validators: `id` and `content` are stripped and must
    /// be non-empty (content is cut to [`MAX_CONTEXT_CONTENT_CHARS`]);
    /// `section` and `context_profile` are trimmed and lowercased, the
    /// profile must be known and the section valid for it; `confidence` is
    /// clamped to `[0, 1]`; empty source ids are dropped; a missing
    /// `normalized_content` is derived from the content; `kind` must be
    /// `"context"`.
    pub fn validated(mut self) -> Result<Self, SessionError> {
        self.id = self.id.trim().to_string();
        if self.id.is_empty() {
            return Err(invalid("id must be a non-empty string"));
        }
        self.section = self.section.trim().to_lowercase();
        self.context_profile = self.context_profile.trim().to_lowercase();
        if valid_sections_for(&self.context_profile).is_empty() {
            return Err(invalid(format!(
                "context_profile must be one of [\"{CONTEXT_PROFILE_AGENT}\", \"{CONTEXT_PROFILE_QA}\"], got {:?}",
                self.context_profile
            )));
        }
        let content = self.content.trim();
        if content.is_empty() {
            return Err(invalid("content must be a non-empty string"));
        }
        self.content = content.chars().take(MAX_CONTEXT_CONTENT_CHARS).collect();
        self.normalized_content = normalize_content(&self.normalized_content);
        if self.normalized_content.is_empty() {
            self.normalized_content = normalize_content(&self.content);
        }
        self.confidence = if self.confidence.is_nan() {
            0.0
        } else {
            self.confidence.clamp(0.0, 1.0)
        };
        for ids in [&mut self.source_feedback_ids, &mut self.source_trace_ids] {
            *ids = ids
                .iter()
                .map(|id| id.trim())
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect();
        }
        if self.kind != CONTEXT_ENTRY_KIND {
            return Err(invalid(format!(
                "kind must be \"{CONTEXT_ENTRY_KIND}\", got {:?}",
                self.kind
            )));
        }
        let allowed = valid_sections_for(&self.context_profile);
        if !allowed.contains(&self.section.as_str()) {
            return Err(invalid(format!(
                "section '{}' is not valid for profile '{}'; allowed: {allowed:?}",
                self.section, self.context_profile
            )));
        }
        Ok(self)
    }
}

/// Validate stored rows and keep only the active guidance entries (Python's
/// `coerce_active_context_entries`).
///
/// A row whose `kind` is anything other than `"context"` (a missing kind
/// counts as `"context"`) is skipped: feedback records and internal state
/// rows such as improve watermarks never reach a rendered context. A
/// `"context"` row that fails validation is an error, as in Python, so
/// malformed stored data is noticed.
pub fn coerce_active_context_entries(
    rows: &[SessionContextRow],
) -> Result<Vec<SessionContextEntry>, SessionError> {
    rows.iter()
        .filter(|row| match row.get("kind") {
            None => true,
            Some(serde_json::Value::String(kind)) => kind == CONTEXT_ENTRY_KIND,
            Some(_) => false,
        })
        .map(SessionContextEntry::from_row)
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row(value: serde_json::Value) -> SessionContextRow {
        match value {
            serde_json::Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    fn minimal() -> serde_json::Value {
        json!({
            "id": " e1 ",
            "section": " Rules ",
            "content": "  Always   CITE sources  ",
            "created_at": "2026-10-28T00:00:00+00:00",
        })
    }

    #[test]
    fn from_row_applies_defaults_and_normalisation() {
        let entry = SessionContextEntry::from_row(&row(minimal())).unwrap();
        assert_eq!(entry.id, "e1");
        assert_eq!(entry.section, "rules");
        assert_eq!(entry.context_profile, CONTEXT_PROFILE_QA);
        assert_eq!(entry.content, "Always   CITE sources");
        assert_eq!(entry.normalized_content, "always cite sources");
        assert_eq!(entry.confidence, 0.0);
        assert_eq!(entry.kind, CONTEXT_ENTRY_KIND);
        assert!(entry.last_served_at.is_none());
    }

    #[test]
    fn content_is_truncated_and_confidence_clamped() {
        let mut value = minimal();
        value["content"] = json!("x".repeat(400));
        value["confidence"] = json!(3.5);
        value["source_trace_ids"] = json!([" t1 ", "", "t2"]);
        let entry = SessionContextEntry::from_row(&row(value)).unwrap();
        assert_eq!(entry.content.chars().count(), MAX_CONTEXT_CONTENT_CHARS);
        assert_eq!(entry.confidence, 1.0);
        assert_eq!(entry.source_trace_ids, vec!["t1", "t2"]);
    }

    #[test]
    fn invalid_rows_are_rejected() {
        for (field, value) in [
            ("id", json!("  ")),
            ("content", json!(" ")),
            ("context_profile", json!("robot")),
            ("section", json!("tool_rules")),
            ("helpful_count", json!(-1)),
            ("kind", json!("feedback")),
        ] {
            let mut bad = minimal();
            bad[field] = value;
            assert!(
                SessionContextEntry::from_row(&row(bad)).is_err(),
                "{field} should be rejected"
            );
        }
    }

    #[test]
    fn agent_profile_accepts_agent_sections() {
        let mut value = minimal();
        value["context_profile"] = json!("AGENT");
        value["section"] = json!("tool_rules");
        let entry = SessionContextEntry::from_row(&row(value)).unwrap();
        assert_eq!(entry.context_profile, CONTEXT_PROFILE_AGENT);
    }

    #[test]
    fn to_row_round_trips_with_python_field_names() {
        let entry = SessionContextEntry::from_row(&row(minimal())).unwrap();
        let stored = entry.to_row().unwrap();
        assert_eq!(stored["kind"], json!("context"));
        assert_eq!(stored["last_served_at"], serde_json::Value::Null);
        assert_eq!(SessionContextEntry::from_row(&stored).unwrap(), entry);
    }

    #[test]
    fn only_context_rows_are_active() {
        let rows = vec![
            row(minimal()),
            row(json!({"id": "f1", "kind": "feedback", "raw_text": "nice"})),
            row(json!({"id": "wm", "kind": "wm_state", "done_count": 3})),
            row(json!({"id": "odd", "kind": null})),
        ];
        let active = coerce_active_context_entries(&rows).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "e1");
    }

    #[test]
    fn a_malformed_context_row_is_an_error() {
        let rows = vec![row(json!({"id": "c", "kind": "context"}))];
        assert!(coerce_active_context_entries(&rows).is_err());
    }
}
