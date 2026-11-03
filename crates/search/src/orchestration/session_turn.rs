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
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
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
/// fail-open and preserve the caller's raw query. Like Python's
/// `is_session_available_for_completion`, a turn without a user or over an
/// unavailable session cache makes no analysis call.
pub async fn prepare_session_turn(
    manager: &Arc<SessionManager>,
    llm: &dyn Llm,
    session_id: &str,
    user_id: Option<&str>,
    query: &str,
) -> SessionTurnPreparation {
    if query.trim().is_empty() || user_id.is_none() || !manager.is_available() {
        return SessionTurnPreparation::pass_through(query);
    }
    let entries = match manager
        .get_latest_session_entries(Some(session_id), user_id, 1)
        .await
    {
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
    let mut preparation = analyze_session_turn_from_snapshot(llm, query, entries, rows).await;
    commit_session_turn_analysis(
        manager,
        session_id,
        user_id,
        query,
        &mut preparation,
        entries,
        rows,
    )
    .await;
    if let Some(analysis) = preparation.analysis.as_ref() {
        tracing::debug!(
            session_id,
            has_previous = preparation.previous_qa_id.is_some(),
            should_answer = preparation.should_answer,
            query_rewritten = analysis.query_to_answer.is_some(),
            previous_answer_rated =
                analysis.previous_answer_rating.is_some() && preparation.previous_qa_id.is_some(),
            served_context_ratings = analysis.served_context_ratings.len(),
            candidate_context_updates = analysis.candidate_context_updates.len(),
            accepted_context_updates = preparation.accepted_context_ids.len(),
            "session turn analysis applied"
        );
    }
    preparation
}

/// Analyze a concurrent turn without writing. The caller commits this only
/// after the answer lane has completed successfully.
pub async fn analyze_session_turn_from_snapshot(
    llm: &dyn Llm,
    query: &str,
    entries: &[SessionQAEntry],
    rows: &[SessionContextRow],
) -> SessionTurnPreparation {
    if query.trim().is_empty() {
        return SessionTurnPreparation::pass_through(query);
    }
    let previous = entries.last();
    let previous_qa_id = previous.map(|entry| entry.id.to_string());
    let served_ids = previous
        .and_then(|entry| entry.used_session_context_ids.clone())
        .unwrap_or_default();
    let served = rows
        .iter()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?;
            served_ids.iter().any(|candidate| candidate == id).then(|| {
                (
                    id.to_string(),
                    row.get("content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                )
            })
        })
        .collect::<Vec<_>>();
    let analysis = analyze(
        llm,
        query,
        previous.map(|entry| (entry.question.as_str(), entry.answer.as_str())),
        &served,
    )
    .await;
    let should_answer = should_answer_turn(&analysis, previous.is_some());
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
            .unwrap_or_else(|| query.to_string()),
        analysis: Some(analysis),
        accepted_context_ids: Vec::new(),
        previous_qa_id,
    }
}

/// Persist evidence from a previously analyzed concurrent turn.
pub async fn commit_session_turn_analysis(
    manager: &Arc<SessionManager>,
    session_id: &str,
    user_id: Option<&str>,
    query: &str,
    preparation: &mut SessionTurnPreparation,
    entries: &[SessionQAEntry],
    rows: &[SessionContextRow],
) {
    let Some(analysis) = preparation.analysis.as_ref() else {
        return;
    };
    let served_ids = entries
        .last()
        .and_then(|entry| entry.used_session_context_ids.clone())
        .unwrap_or_default();
    preparation.accepted_context_ids = apply_turn_analysis(
        manager,
        session_id,
        user_id,
        query,
        analysis,
        preparation.previous_qa_id.as_deref(),
        &served_ids,
        rows,
    )
    .await;
}

/// Persist a turn's evidence (Python `apply_session_turn_analysis`): one
/// feedback row, then the candidate guidance updates, then the helpful/harmful
/// counters. A previous-answer rating is evidence only when a previous turn
/// exists for it to refer to. Returns the touched guidance ids.
#[allow(clippy::too_many_arguments)]
async fn apply_turn_analysis(
    manager: &Arc<SessionManager>,
    session_id: &str,
    user_id: Option<&str>,
    query: &str,
    analysis: &SessionTurnAnalysis,
    previous_qa_id: Option<&str>,
    served_ids: &[String],
    rows: &[SessionContextRow],
) -> Vec<String> {
    let previous_answer_rating = analysis
        .previous_answer_rating
        .filter(|_| previous_qa_id.is_some());
    if analysis.candidate_context_updates.is_empty()
        && analysis.served_context_ratings.is_empty()
        && previous_answer_rating.is_none()
    {
        return Vec::new();
    }
    let feedback_id = Uuid::new_v4().to_string();
    let mut feedback = Map::new();
    for (key, value) in [
        ("id", json!(feedback_id)),
        ("kind", json!("feedback")),
        ("created_at", json!(Utc::now().to_rfc3339())),
        ("raw_text", json!(query)),
        (
            "referenced_qa_ids",
            json!(previous_qa_id.into_iter().collect::<Vec<_>>()),
        ),
        ("referenced_qa_rating", json!(previous_answer_rating)),
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
    let accepted =
        apply_candidate_updates_for_user(manager, session_id, user_id, &feedback_id, &candidates)
            .await;

    // Counters accumulate locally, as Python's `apply_served_context_ratings`
    // does, so repeated ratings of one entry in a turn all count.
    let mut counts: HashMap<&str, (u64, u64)> = HashMap::new();
    for rating in &analysis.served_context_ratings {
        // Hardening beyond Python: the model may only rate context proven to
        // have been served to the previous answer. Ignore invented or stale IDs.
        if !served_ids.iter().any(|id| id == &rating.entry_id) {
            continue;
        }
        let Some(row) = rows
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(&rating.entry_id))
        else {
            continue;
        };
        let helpful = match rating.rating.trim().to_ascii_lowercase().as_str() {
            "helpful" => true,
            "harmful" => false,
            _ => continue,
        };
        let count = |field: &str| row.get(field).and_then(Value::as_u64).unwrap_or(0);
        let entry = counts
            .entry(rating.entry_id.as_str())
            .or_insert_with(|| (count("helpful_count"), count("harmful_count")));
        let field = if helpful {
            entry.0 += 1;
            ("helpful_count", entry.0)
        } else {
            entry.1 += 1;
            ("harmful_count", entry.1)
        };
        let mut merge = Map::new();
        merge.insert(field.0.into(), json!(field.1));
        let _ = manager
            .update_session_context_entry(Some(session_id), user_id, &rating.entry_id, merge)
            .await;
    }
    accepted
}

/// Bound the analysis lane of a concurrent turn so an already-completed
/// answer cannot be held indefinitely by a stuck model call. Python applies
/// the same 30-second bound in `session_concurrent_turn.analyze_turn`.
pub async fn prepare_concurrent_session_turn_from_snapshot(
    llm: &dyn Llm,
    session_id: &str,
    query: &str,
    entries: &[SessionQAEntry],
    rows: &[SessionContextRow],
    timeout: Duration,
) -> SessionTurnPreparation {
    match tokio::time::timeout(
        timeout,
        analyze_session_turn_from_snapshot(llm, query, entries, rows),
    )
    .await
    {
        Ok(preparation) => preparation,
        Err(_) => {
            tracing::warn!(
                session_id,
                "concurrent session turn analysis timed out; failing open"
            );
            SessionTurnPreparation::pass_through(query)
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "session-turn fixtures should fail loudly when setup or assertions cannot be read"
)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cognee_llm::{GenerationOptions, GenerationResponse, LlmError, LlmResult};
    use cognee_session::FsSessionStore;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct GoldenTurn {
        name: String,
        has_previous: bool,
        raw_query: String,
        analysis: SessionTurnAnalysis,
        should_answer: bool,
        effective_query: String,
        /// Expected `kind: feedback` rows after the turn is prepared, when the
        /// case pins them. Only fields listed here are compared.
        #[serde(default)]
        expected_context_rows: Option<Vec<Value>>,
    }

    fn golden_turns() -> Vec<GoldenTurn> {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/python_session_turn_parity.json"
        ))
        .expect("Python-derived session-turn fixture must stay valid")
    }

    struct FixtureLlm(Value);

    #[async_trait]
    impl Llm for FixtureLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            Err(LlmError::ConfigError("not used".into()))
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            Ok(self.0.clone())
        }

        fn model(&self) -> &str {
            "python-session-turn-fixture"
        }
    }

    struct FailingLlm;

    #[async_trait]
    impl Llm for FailingLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            Err(LlmError::ConfigError("fixture failure".into()))
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            Err(LlmError::ConfigError("fixture failure".into()))
        }

        fn model(&self) -> &str {
            "failing-session-turn-fixture"
        }
    }

    struct HangingLlm;

    #[async_trait]
    impl Llm for HangingLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            std::future::pending().await
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            std::future::pending().await
        }

        fn model(&self) -> &str {
            "hanging-session-turn-fixture"
        }
    }

    #[test]
    fn python_derived_routing_and_query_golden() {
        for turn in golden_turns() {
            assert_eq!(
                should_answer_turn(&turn.analysis, turn.has_previous),
                turn.should_answer,
                "routing mismatch for {}",
                turn.name
            );
            let effective = turn
                .analysis
                .query_to_answer
                .as_deref()
                .unwrap_or(&turn.raw_query);
            assert_eq!(
                effective, turn.effective_query,
                "query mismatch for {}",
                turn.name
            );
        }
    }

    #[tokio::test]
    async fn python_derived_first_turn_rating_evidence_golden() {
        for turn in golden_turns() {
            let Some(expected) = &turn.expected_context_rows else {
                continue;
            };
            let dir = tempfile::tempdir().unwrap();
            let manager = Arc::new(SessionManager::new(Arc::new(FsSessionStore::new(
                dir.path(),
            ))));
            let llm = FixtureLlm(serde_json::to_value(&turn.analysis).unwrap());

            let prepared =
                prepare_session_turn(&manager, &llm, "golden", Some("user"), &turn.raw_query).await;

            assert_eq!(prepared.should_answer, turn.should_answer, "{}", turn.name);
            let rows = manager
                .get_session_context_entries(Some("golden"), Some("user"))
                .await
                .unwrap();
            let feedback: Vec<_> = rows
                .iter()
                .filter(|row| row.get("kind").and_then(Value::as_str) == Some("feedback"))
                .collect();
            assert_eq!(feedback.len(), expected.len(), "{}", turn.name);
            for (row, want) in feedback.iter().zip(expected) {
                for (key, value) in want.as_object().unwrap() {
                    assert_eq!(row.get(key), Some(value), "{}: {key}", turn.name);
                }
            }
            if expected.is_empty() {
                assert!(rows.is_empty(), "{}: no rows expected", turn.name);
            }
        }
    }

    /// Seed one active guidance row and return its id and section.
    async fn seed_context_row(manager: &SessionManager, session: &str) -> (String, String) {
        let section = cognee_session::QA_CONTEXT_SECTIONS[0].to_string();
        let entry = cognee_session::SessionContextEntry {
            id: Uuid::new_v4().to_string(),
            section: section.clone(),
            context_profile: CONTEXT_PROFILE_QA.to_string(),
            content: "Prefer terse answers.".into(),
            normalized_content: cognee_session::normalize_content("Prefer terse answers."),
            confidence: 0.9,
            created_at: Utc::now().to_rfc3339(),
            source_feedback_ids: Vec::new(),
            source_trace_ids: Vec::new(),
            helpful_count: 0,
            harmful_count: 0,
            priority: 0,
            last_served_at: None,
            embedding: None,
            kind: cognee_session::CONTEXT_ENTRY_KIND.to_string(),
        };
        manager
            .create_session_context_entry(Some(session), None, entry.to_row().unwrap())
            .await
            .unwrap();
        (entry.id, section)
    }

    #[tokio::test]
    async fn repeated_served_context_ratings_accumulate_within_a_turn() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(SessionManager::new(Arc::new(FsSessionStore::new(
            dir.path(),
        ))));
        let (id, _) = seed_context_row(&manager, "ratings").await;
        let rows = manager
            .get_session_context_entries(Some("ratings"), None)
            .await
            .unwrap();
        let rate = |rating: &str| ServedContextRating {
            entry_id: id.clone(),
            rating: rating.into(),
        };
        let analysis = SessionTurnAnalysis {
            served_context_ratings: vec![rate("helpful"), rate("helpful"), rate("harmful")],
            ..Default::default()
        };

        apply_turn_analysis(
            &manager,
            "ratings",
            None,
            "q",
            &analysis,
            None,
            std::slice::from_ref(&id),
            &rows,
        )
        .await;

        let rows = manager
            .get_session_context_entries(Some("ratings"), None)
            .await
            .unwrap();
        let row = rows
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .unwrap();
        assert_eq!(row.get("helpful_count").and_then(Value::as_u64), Some(2));
        assert_eq!(row.get("harmful_count").and_then(Value::as_u64), Some(1));
    }

    #[tokio::test]
    async fn candidate_update_and_rating_on_one_entry_both_persist() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(SessionManager::new(Arc::new(FsSessionStore::new(
            dir.path(),
        ))));
        let (id, section) = seed_context_row(&manager, "both").await;
        let rows = manager
            .get_session_context_entries(Some("both"), None)
            .await
            .unwrap();
        let analysis = SessionTurnAnalysis {
            served_context_ratings: vec![ServedContextRating {
                entry_id: id.clone(),
                rating: "helpful".into(),
            }],
            candidate_context_updates: vec![CandidateContextUpdate {
                section,
                content: "prefer  TERSE answers.".into(),
                confidence: 0.95,
            }],
            ..Default::default()
        };

        let accepted = apply_turn_analysis(
            &manager,
            "both",
            None,
            "q",
            &analysis,
            None,
            std::slice::from_ref(&id),
            &rows,
        )
        .await;

        assert_eq!(
            accepted,
            vec![id.clone()],
            "duplicate candidate reuses the row"
        );
        let rows = manager
            .get_session_context_entries(Some("both"), None)
            .await
            .unwrap();
        let row = rows
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .unwrap();
        assert_eq!(row.get("helpful_count").and_then(Value::as_u64), Some(1));
        assert_eq!(
            row.get("source_feedback_ids")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    #[tokio::test]
    async fn first_turn_candidate_is_persisted_but_never_suppresses_answer() {
        let turn = golden_turns().remove(0);
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(SessionManager::new(Arc::new(FsSessionStore::new(
            dir.path(),
        ))));
        let llm = FixtureLlm(serde_json::to_value(&turn.analysis).unwrap());

        let prepared =
            prepare_session_turn(&manager, &llm, "first", Some("user"), &turn.raw_query).await;

        assert!(prepared.should_answer);
        assert_eq!(prepared.effective_query, turn.raw_query);
        assert_eq!(prepared.accepted_context_ids.len(), 1);
        let rows = manager
            .get_session_context_entries(Some("first"), Some("user"))
            .await
            .unwrap();
        assert!(rows.iter().any(|row| {
            row.get("kind").and_then(Value::as_str) == Some("context")
                && row.get("content").and_then(Value::as_str)
                    == Some("Answer in terse bullet points.")
        }));
    }

    #[tokio::test]
    async fn feedback_follow_up_records_evidence_and_rewrites_effective_query() {
        let turn = golden_turns().remove(2);
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(SessionManager::new(Arc::new(FsSessionStore::new(
            dir.path(),
        ))));
        manager
            .save_qa(
                Some("follow-up"),
                Some("user"),
                "previous",
                "answer",
                None,
                None,
            )
            .await
            .unwrap();
        let llm = FixtureLlm(serde_json::to_value(&turn.analysis).unwrap());

        let prepared =
            prepare_session_turn(&manager, &llm, "follow-up", Some("user"), &turn.raw_query).await;

        assert!(prepared.should_answer);
        assert_eq!(prepared.effective_query, turn.effective_query);
        let entries = manager
            .get_session(Some("follow-up"), Some("user"))
            .await
            .unwrap();
        assert_eq!(entries[0].feedback_score, None);
        assert_eq!(entries[0].feedback_text, None);
        let rows = manager
            .get_session_context_entries(Some("follow-up"), Some("user"))
            .await
            .unwrap();
        assert!(
            rows.iter().any(|row| {
                row.get("kind").and_then(Value::as_str) == Some("feedback")
                    && row.get("referenced_qa_rating").and_then(Value::as_i64) == Some(5)
                    && row
                        .get("referenced_qa_ids")
                        .and_then(Value::as_array)
                        .is_some_and(|ids| {
                            ids.iter()
                                .any(|id| id.as_str() == Some(&entries[0].id.to_string()))
                        })
            }),
            "update_user_preferences must be able to infer the rating from the feedback row"
        );
    }

    #[tokio::test]
    async fn analysis_failure_is_fail_open_and_preserves_raw_query() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(SessionManager::new(Arc::new(FsSessionStore::new(
            dir.path(),
        ))));
        manager
            .save_qa(
                Some("failure"),
                Some("user"),
                "previous",
                "answer",
                None,
                None,
            )
            .await
            .unwrap();

        let prepared =
            prepare_session_turn(&manager, &FailingLlm, "failure", Some("user"), "raw query").await;

        assert!(prepared.should_answer);
        assert_eq!(prepared.effective_query, "raw query");
        assert!(prepared.accepted_context_ids.is_empty());
        let entries = manager
            .get_session(Some("failure"), Some("user"))
            .await
            .unwrap();
        assert!(entries[0].feedback_score.is_none());
    }

    #[tokio::test]
    async fn concurrent_analysis_timeout_is_fail_open_and_preserves_raw_query() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(SessionManager::new(Arc::new(FsSessionStore::new(
            dir.path(),
        ))));

        let prepared = prepare_concurrent_session_turn_from_snapshot(
            &HangingLlm,
            "timeout",
            "raw query",
            &[],
            &[],
            Duration::from_millis(1),
        )
        .await;

        assert!(prepared.should_answer);
        assert_eq!(prepared.effective_query, "raw query");
        assert!(prepared.analysis.is_none());
        assert!(
            manager
                .get_session_context_entries(Some("timeout"), None)
                .await
                .unwrap()
                .is_empty(),
            "a timed-out analysis must not persist partial evidence"
        );
    }
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
