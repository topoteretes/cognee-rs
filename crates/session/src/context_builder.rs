//! The deterministic applier that turns proposed guidance into stored
//! session-context entries.
//!
//! Port of the candidate half of Python's
//! `cognee/infrastructure/session/session_context_builder.py`
//! (`apply_candidate_updates`): validate → confidence ≥ 0.75 → normalize →
//! link an exact duplicate or create a new entry. No LLM, no fuzzy matching,
//! no auto-delete. The ranker and the rendered guidance block are not ported
//! yet: nothing in Rust serves the entries into a prompt.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context_entry::{
    CONTEXT_ENTRY_KIND, CONTEXT_PROFILE_AGENT, CONTEXT_PROFILE_QA, MAX_CONTEXT_CONTENT_CHARS,
    SessionContextEntry, SessionContextRow, normalize_content, valid_sections_for,
};
use crate::error::SessionError;
use crate::session_manager::SessionManager;

/// The one confidence threshold: a candidate below it is not stored
/// (Python's `GATE_CONFIDENCE` / `MIN_CANDIDATE_CONFIDENCE`).
pub const MIN_CANDIDATE_CONFIDENCE: f64 = 0.75;

/// A proposed new guidance entry (Python's `CandidateContextUpdate` and its
/// agent variant). Built by an extractor; [`apply_candidate_updates`] decides
/// whether it is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateContextUpdate {
    /// [`CONTEXT_PROFILE_QA`] or [`CONTEXT_PROFILE_AGENT`].
    #[serde(default = "qa_profile")]
    pub context_profile: String,
    pub section: String,
    pub content: String,
    #[serde(default)]
    pub confidence: f64,
}

fn qa_profile() -> String {
    CONTEXT_PROFILE_QA.to_string()
}

impl CandidateContextUpdate {
    /// A candidate for `profile`, with Python's field validators applied:
    /// the section is trimmed and lowercased, the content stripped and cut to
    /// [`MAX_CONTEXT_CONTENT_CHARS`], the confidence clamped to `[0, 1]`
    /// (`NaN` reads as 0).
    pub fn new(profile: &str, section: &str, content: &str, confidence: f64) -> Self {
        Self {
            context_profile: profile.to_string(),
            section: section.trim().to_lowercase(),
            content: content
                .trim()
                .chars()
                .take(MAX_CONTEXT_CONTENT_CHARS)
                .collect(),
            confidence: if confidence.is_nan() {
                0.0
            } else {
                confidence.clamp(0.0, 1.0)
            },
        }
    }
}

/// Apply candidates to the session's context entries; returns the ids of the
/// entries touched or created, in candidate order.
///
/// For each candidate: its section must be valid for its profile, its
/// content non-empty and at most [`MAX_CONTEXT_CONTENT_CHARS`], its
/// confidence at least [`MIN_CANDIDATE_CONFIDENCE`]. An entry of the same
/// profile and section with identical normalized content is a duplicate: it
/// gains `source_id` (a feedback id for `qa`, a trace id for `agent`; an
/// empty id links nothing) and is returned. Otherwise a new entry is created.
///
/// Fail-open per candidate, as in Python: a store error skips that candidate
/// (logged) and the rest still apply.
pub async fn apply_candidate_updates(
    manager: &SessionManager,
    session_id: &str,
    user_id: &str,
    source_id: &str,
    candidates: &[CandidateContextUpdate],
) -> Vec<String> {
    let mut touched = Vec::new();
    for candidate in candidates {
        match apply_single_candidate(manager, session_id, user_id, source_id, candidate).await {
            Ok(Some(id)) => touched.push(id),
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, "skipping a candidate context update after an error");
            }
        }
    }
    touched
}

async fn apply_single_candidate(
    manager: &SessionManager,
    session_id: &str,
    user_id: &str,
    source_id: &str,
    candidate: &CandidateContextUpdate,
) -> Result<Option<String>, SessionError> {
    let profile = candidate.context_profile.as_str();
    let section = candidate.section.as_str();
    if !valid_sections_for(profile).contains(&section) {
        return Ok(None);
    }
    let content = candidate.content.trim();
    if content.is_empty() || content.chars().count() > MAX_CONTEXT_CONTENT_CHARS {
        return Ok(None);
    }
    if candidate.confidence < MIN_CANDIDATE_CONFIDENCE {
        return Ok(None);
    }

    let normalized = normalize_content(content);
    let source_field = if profile == CONTEXT_PROFILE_AGENT {
        "source_trace_ids"
    } else {
        "source_feedback_ids"
    };

    let rows = manager
        .get_session_context_entries(Some(session_id), Some(user_id))
        .await?;
    if let Some(duplicate) = rows
        .iter()
        .find(|row| is_duplicate(row, profile, section, &normalized))
    {
        let entry_id = duplicate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut source_ids: Vec<Value> = duplicate
            .get(source_field)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !source_id.is_empty() && !source_ids.iter().any(|id| id.as_str() == Some(source_id)) {
            source_ids.push(Value::String(source_id.to_string()));
            let mut merge = SessionContextRow::new();
            merge.insert(source_field.to_string(), Value::Array(source_ids));
            manager
                .update_session_context_entry(Some(session_id), Some(user_id), &entry_id, merge)
                .await?;
        }
        return Ok(Some(entry_id));
    }

    let linked: Vec<String> = if source_id.is_empty() {
        Vec::new()
    } else {
        vec![source_id.to_string()]
    };
    let is_agent = profile == CONTEXT_PROFILE_AGENT;
    let entry = SessionContextEntry {
        id: uuid::Uuid::new_v4().to_string(),
        section: section.to_string(),
        context_profile: profile.to_string(),
        content: content.to_string(),
        normalized_content: normalized,
        confidence: candidate.confidence,
        created_at: Utc::now().to_rfc3339(),
        source_feedback_ids: if is_agent { Vec::new() } else { linked.clone() },
        source_trace_ids: if is_agent { linked } else { Vec::new() },
        helpful_count: 0,
        harmful_count: 0,
        priority: 0,
        last_served_at: None,
        embedding: None,
        kind: CONTEXT_ENTRY_KIND.to_string(),
    };
    manager
        .create_session_context_entry(Some(session_id), Some(user_id), entry.to_row()?)
        .await?;
    Ok(Some(entry.id))
}

/// A stored `"context"` row of `profile`/`section` with this normalized
/// content (rows written by Python may omit `kind` and `context_profile`).
fn is_duplicate(row: &SessionContextRow, profile: &str, section: &str, normalized: &str) -> bool {
    let field = |key: &str, default: &str| -> bool {
        match row.get(key) {
            None => true,
            Some(Value::String(value)) => value == default,
            Some(_) => false,
        }
    };
    let text = |key: &str| row.get(key).and_then(Value::as_str);
    field("kind", CONTEXT_ENTRY_KIND)
        && match row.get("context_profile") {
            None => profile == CONTEXT_PROFILE_QA,
            Some(_) => field("context_profile", profile),
        }
        && text("section") == Some(section)
        && text("normalized_content") == Some(normalized)
}

#[cfg(all(test, feature = "fs"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::FsSessionStore;

    fn manager(dir: &tempfile::TempDir) -> SessionManager {
        SessionManager::new(Arc::new(FsSessionStore::new(dir.path())))
    }

    async fn rows(manager: &SessionManager) -> Vec<SessionContextRow> {
        manager
            .get_session_context_entries(Some("s"), Some("u"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_confident_candidate_is_created_with_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let candidate =
            CandidateContextUpdate::new(CONTEXT_PROFILE_AGENT, "Tool_Rules", "Run uv sync", 0.9);
        let touched = apply_candidate_updates(&manager, "s", "u", "t1", &[candidate]).await;
        assert_eq!(touched.len(), 1);
        let rows = rows(&manager).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["section"], "tool_rules");
        assert_eq!(rows[0]["context_profile"], "agent");
        assert_eq!(rows[0]["source_trace_ids"], serde_json::json!(["t1"]));
        assert_eq!(rows[0]["source_feedback_ids"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn low_confidence_bad_section_and_empty_content_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let candidates = [
            CandidateContextUpdate::new(CONTEXT_PROFILE_AGENT, "tool_rules", "Weak", 0.74),
            CandidateContextUpdate::new(CONTEXT_PROFILE_AGENT, "goals", "Wrong section", 0.9),
            CandidateContextUpdate::new(CONTEXT_PROFILE_QA, "rules", "   ", 0.9),
        ];
        assert!(
            apply_candidate_updates(&manager, "s", "u", "", &candidates)
                .await
                .is_empty()
        );
        assert!(rows(&manager).await.is_empty());
    }

    #[tokio::test]
    async fn an_exact_duplicate_links_the_new_source_instead_of_a_new_entry() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let first = CandidateContextUpdate::new(CONTEXT_PROFILE_QA, "rules", "Cite sources", 0.8);
        let again =
            CandidateContextUpdate::new(CONTEXT_PROFILE_QA, "rules", "  CITE   sources", 0.9);
        let a = apply_candidate_updates(&manager, "s", "u", "f1", &[first]).await;
        let b =
            apply_candidate_updates(&manager, "s", "u", "f2", std::slice::from_ref(&again)).await;
        assert_eq!(a, b);
        // The same source again links nothing new.
        apply_candidate_updates(&manager, "s", "u", "f2", &[again]).await;
        let rows = rows(&manager).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["source_feedback_ids"],
            serde_json::json!(["f1", "f2"])
        );
    }

    #[test]
    fn the_candidate_validators_match_python() {
        let long = "x".repeat(400);
        let candidate =
            CandidateContextUpdate::new(CONTEXT_PROFILE_AGENT, " Failure_Lessons ", &long, 7.0);
        assert_eq!(candidate.section, "failure_lessons");
        assert_eq!(candidate.content.chars().count(), MAX_CONTEXT_CONTENT_CHARS);
        assert_eq!(candidate.confidence, 1.0);
        assert_eq!(
            CandidateContextUpdate::new(CONTEXT_PROFILE_QA, "rules", "x", f64::NAN).confidence,
            0.0
        );
    }
}
