//! Sequential preparation for one session-aware completion turn.

use chrono::Utc;
use cognee_llm::{Llm, Message, generate_json_schema};
use cognee_session::{
    CONTEXT_PROFILE_QA, CandidateContextUpdate as StoredCandidateContextUpdate, SessionContextRow,
    SessionManager, SessionQAEntry, apply_candidate_updates_for_user,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use uuid::Uuid;

const SYSTEM_PROMPT: &str = include_str!("../utils/prompts/session_turn_analysis_system.txt");
const RATING_PROMPT: &str = include_str!("../utils/prompts/session_turn_analysis_rating.txt");
const DEFAULT_ACK: &str = "Got it.";

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ServedContextRating {
    pub entry_id: String,
    pub rating: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CandidateContextUpdate {
    pub section: String,
    pub content: String,
    #[serde(default)]
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SessionTurnAnalysis {
    pub response_to_user: Option<String>,
    pub query_to_answer: Option<String>,
    #[serde(default)]
    pub served_context_ratings: Vec<ServedContextRating>,
    #[serde(default)]
    pub candidate_context_updates: Vec<CandidateContextUpdate>,
    pub previous_answer_rating: Option<i32>,
}

impl SessionTurnAnalysis {
    fn normalized(mut self) -> Self {
        self.response_to_user = clean(self.response_to_user);
        self.query_to_answer = clean(self.query_to_answer);
        self.served_context_ratings.truncate(3);
        self.candidate_context_updates.truncate(3);
        self.previous_answer_rating = self.previous_answer_rating.filter(|r| (1..=5).contains(r));
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionTurnPreparation {
    pub should_answer: bool,
    pub response_to_user: Option<String>,
    pub effective_query: String,
    pub analysis: Option<SessionTurnAnalysis>,
    pub accepted_context_ids: Vec<String>,
    pub previous_qa_id: Option<String>,
}

impl SessionTurnPreparation {
    pub fn pass_through(query: &str) -> Self {
        Self {
            should_answer: true,
            response_to_user: None,
            effective_query: query.into(),
            analysis: None,
            accepted_context_ids: vec![],
            previous_qa_id: None,
        }
    }
}

fn clean(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let v = v.trim().to_owned();
        (!v.is_empty()).then_some(v)
    })
}

pub fn should_answer_turn(a: &SessionTurnAnalysis, has_previous: bool) -> bool {
    let signal = a.query_to_answer.is_some()
        || a.response_to_user.is_some()
        || !a.candidate_context_updates.is_empty()
        || !a.served_context_ratings.is_empty();
    a.query_to_answer.is_some() || !signal || !has_previous
}

async fn analyze(
    llm: &dyn Llm,
    query: &str,
    previous: Option<(&str, &str)>,
    served: &[(String, String)],
) -> SessionTurnAnalysis {
    let mut input = format!("CURRENT USER MESSAGE:\n{}", query.trim());
    if let Some((question, answer)) = previous {
        if !question.trim().is_empty() {
            input.push_str("\n\nPREVIOUS QUESTION:\n");
            input.push_str(question.trim());
        }
        if !answer.trim().is_empty() {
            input.push_str("\n\nPREVIOUS ANSWER:\n");
            input.push_str(answer.trim());
        }
    }
    if !served.is_empty() {
        input
            .push_str("\n\nSESSION CONTEXT ENTRIES SERVED TO THE PREVIOUS ANSWER (id: content):\n");
        for (id, content) in served {
            input.push_str(&format!("{id}: {}\n", content.trim()));
        }
    }
    let schema = generate_json_schema::<SessionTurnAnalysis>();
    llm.create_structured_output_with_messages_raw(
        vec![
            Message::system(format!("{SYSTEM_PROMPT}\n\n{RATING_PROMPT}")),
            Message::user(input),
        ],
        &schema,
        None,
    )
    .await
    .ok()
    .and_then(|v| serde_json::from_value::<SessionTurnAnalysis>(v).ok())
    .unwrap_or_default()
    .normalized()
}

/// Analyze and persist one turn before retrieval. LLM/storage failures are
/// fail-open and preserve the caller's raw query.
pub async fn prepare_session_turn(
    manager: &Arc<SessionManager>,
    llm: &dyn Llm,
    session_id: &str,
    user_id: Option<&str>,
    query: &str,
) -> SessionTurnPreparation {
    if query.trim().is_empty() {
        return SessionTurnPreparation::pass_through(query);
    }
    let entries = match manager.get_session(Some(session_id), user_id).await {
        Ok(v) => v,
        Err(error) => {
            tracing::warn!(%error, "session turn preparation failed open");
            return SessionTurnPreparation::pass_through(query);
        }
    };
    let rows = manager
        .get_session_context_entries(Some(session_id), user_id)
        .await
        .unwrap_or_default();
    prepare_session_turn_from_snapshot(manager, llm, session_id, user_id, query, &entries, &rows)
        .await
}

/// Analyze and apply a turn from an immutable cache snapshot.  The concurrent
/// runner uses this entry point so its answer and analysis lanes cannot observe
/// different session reads.
pub async fn prepare_session_turn_from_snapshot(
    manager: &Arc<SessionManager>,
    llm: &dyn Llm,
    session_id: &str,
    user_id: Option<&str>,
    query: &str,
    entries: &[SessionQAEntry],
    rows: &[SessionContextRow],
) -> SessionTurnPreparation {
    if query.trim().is_empty() {
        return SessionTurnPreparation::pass_through(query);
    }
    let previous = entries.last();
    let previous_qa_id = previous.map(|e| e.id.to_string());
    // Only context actually stamped on the previous answer may be rated. This
    // prevents an LLM from incrementing arbitrary session-context rows and
    // matches Python's cross-turn lineage contract.
    let served_ids = previous
        .and_then(|entry| entry.used_session_context_ids.clone())
        .unwrap_or_default();
    let served = rows
        .iter()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            served_ids.iter().any(|candidate| candidate == id).then(|| {
                (
                    id.into(),
                    row.get("content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                )
            })
        })
        .collect::<Vec<_>>();
    let analysis = analyze(
        llm,
        query,
        previous.map(|e| (e.question.as_str(), e.answer.as_str())),
        &served,
    )
    .await;

    if let (Some(qa_id), Some(rating)) = (&previous_qa_id, analysis.previous_answer_rating)
        && let Err(error) = manager
            .add_feedback(Some(session_id), user_id, qa_id, Some(query), Some(rating))
            .await
    {
        tracing::warn!(%error, "session rating persistence failed open");
    }

    let has_evidence = !analysis.candidate_context_updates.is_empty()
        || !analysis.served_context_ratings.is_empty()
        || analysis.previous_answer_rating.is_some();
    let mut accepted = vec![];
    if has_evidence {
        let feedback_id = Uuid::new_v4().to_string();
        let mut feedback = Map::new();
        for (key, value) in [
            ("id", json!(feedback_id)),
            ("kind", json!("feedback")),
            ("created_at", json!(Utc::now().to_rfc3339())),
            ("raw_text", json!(query)),
            (
                "referenced_qa_ids",
                json!(previous_qa_id.iter().cloned().collect::<Vec<_>>()),
            ),
            (
                "referenced_qa_rating",
                json!(analysis.previous_answer_rating),
            ),
            ("influencing_context_ids", json!(served_ids)),
            (
                "candidate_context_entries",
                json!(analysis.candidate_context_updates),
            ),
        ] {
            feedback.insert(key.into(), value);
        }
        let _ = manager
            .create_session_context_entry(Some(session_id), user_id, feedback)
            .await;

        for rating in &analysis.served_context_ratings {
            // The model may only rate context proven to have been served to
            // the previous answer. Ignore invented or stale IDs.
            if !served_ids.iter().any(|id| id == &rating.entry_id) {
                continue;
            }
            let Some(row) = rows
                .iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(&rating.entry_id))
            else {
                continue;
            };
            let field = match rating.rating.trim().to_ascii_lowercase().as_str() {
                "helpful" => "helpful_count",
                "harmful" => "harmful_count",
                _ => continue,
            };
            let mut merge = Map::new();
            merge.insert(
                field.into(),
                json!(row.get(field).and_then(Value::as_u64).unwrap_or(0) + 1),
            );
            let _ = manager
                .update_session_context_entry(Some(session_id), user_id, &rating.entry_id, merge)
                .await;
        }
        let candidates = analysis
            .candidate_context_updates
            .iter()
            .map(|candidate| {
                StoredCandidateContextUpdate::new(
                    CONTEXT_PROFILE_QA,
                    &candidate.section,
                    &candidate.content,
                    candidate.confidence,
                )
            })
            .collect::<Vec<_>>();
        accepted = apply_candidate_updates_for_user(
            manager,
            session_id,
            user_id,
            &feedback_id,
            &candidates,
        )
        .await;
    }
    let should_answer = should_answer_turn(&analysis, previous_qa_id.is_some());
    SessionTurnPreparation {
        should_answer,
        response_to_user: if should_answer {
            analysis.response_to_user.clone()
        } else {
            Some(
                analysis
                    .response_to_user
                    .clone()
                    .unwrap_or_else(|| DEFAULT_ACK.into()),
            )
        },
        effective_query: analysis
            .query_to_answer
            .clone()
            .unwrap_or_else(|| query.into()),
        analysis: Some(analysis),
        accepted_context_ids: accepted,
        previous_qa_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_golden_routing() {
        assert!(should_answer_turn(&SessionTurnAnalysis::default(), true));
        assert!(!should_answer_turn(
            &SessionTurnAnalysis {
                response_to_user: Some("Thanks".into()),
                ..Default::default()
            },
            true
        ));
        assert!(should_answer_turn(
            &SessionTurnAnalysis {
                query_to_answer: Some("restated".into()),
                ..Default::default()
            },
            true
        ));
        assert!(should_answer_turn(
            &SessionTurnAnalysis {
                response_to_user: Some("Thanks".into()),
                ..Default::default()
            },
            false
        ));
    }
    #[test]
    fn normalization_is_non_blocking() {
        let a = SessionTurnAnalysis {
            response_to_user: Some("  ".into()),
            previous_answer_rating: Some(7),
            served_context_ratings: vec![Default::default(); 5],
            candidate_context_updates: vec![Default::default(); 5],
            ..Default::default()
        }
        .normalized();
        assert_eq!(
            (
                a.served_context_ratings.len(),
                a.candidate_context_updates.len(),
                a.previous_answer_rating,
                a.response_to_user
            ),
            (3, 3, None, None)
        );
    }
}
