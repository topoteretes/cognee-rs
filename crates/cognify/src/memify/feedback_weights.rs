//! Stage 1 of `improve()` — apply feedback weights from session Q&A entries
//! to graph nodes/edges.
//!
//! Ported from:
//! - `/tmp/cognee-python/cognee/tasks/memify/apply_feedback_weights.py`
//! - `/tmp/cognee-python/cognee/tasks/memify/extract_feedback_qas.py`
//!
//! The pipeline:
//! 1. For each session, fetches all Q&A entries via `SessionStore::get_all_qa_entries`.
//! 2. Filters to *eligible* entries: `1 <= feedback_score <= 5`, not already
//!    marked `memify_metadata["feedback_weights_applied"] = true`, and with at
//!    least one node id or edge id in `used_graph_element_ids`.
//! 3. Normalizes the score to `[0, 1]` and applies a streaming update
//!    (`w' = w + alpha * (r - w)`, clipped, rounded to 4 dp).
//! 4. Reads/writes the `feedback_weight` property via the batch methods
//!    on `GraphDBTrait`.
//! 5. Marks the QA entry as processed regardless of whether the graph updates
//!    actually succeeded (`success` flag records the outcome).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cognee_graph::{EdgeKey, GraphDBTrait};
use cognee_session::{
    SessionManager, SessionQAEntry, SessionQAUpdate, SessionStore, UsedGraphElementIds,
};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

/// Error type for Stage 1 (`apply_feedback_weights`).
#[derive(Debug, Error)]
pub enum FeedbackError {
    #[error("Invalid feedback_score {0}: must be in [1, 5]")]
    InvalidScore(i32),

    #[error("Invalid alpha {0}: must be in (0, 1]")]
    InvalidAlpha(f64),

    #[error("Session error: {0}")]
    Session(#[from] cognee_session::SessionError),

    #[error("Graph error: {0}")]
    Graph(#[from] cognee_graph::GraphDBError),
}

/// Key used in `memify_metadata` to mark an entry as processed.
pub const FEEDBACK_WEIGHTS_APPLIED_KEY: &str = "feedback_weights_applied";

/// Number of decimal places to round the streaming update to. Matches
/// Python's `FEEDBACK_WEIGHT_DECIMALS = 4`.
const FEEDBACK_WEIGHT_DECIMALS: i32 = 4;

/// Delimiter used to encode edge-IDs as `"source|||target|||relation"`
/// strings in `used_graph_element_ids.edge_ids`.
pub const EDGE_ID_DELIMITER: &str = "|||";

/// Summary of a Stage 1 run.
#[derive(Debug, Clone, Default)]
pub struct FeedbackApplyResult {
    /// Number of eligible entries that were processed (not skipped).
    pub processed: usize,
    /// Number of entries for which the graph updates were fully applied.
    pub applied: usize,
    /// Number of entries that were skipped (ineligible or already applied).
    pub skipped: usize,
}

/// Normalize a 1..5 feedback score to [0.0, 1.0].
///
/// Matches Python `normalize_feedback_score()` (`apply_feedback_weights.py:43`).
pub fn normalize_feedback_score(score: i32) -> Result<f64, FeedbackError> {
    if !(1..=5).contains(&score) {
        return Err(FeedbackError::InvalidScore(score));
    }
    Ok((score as f64 - 1.0) / 4.0)
}

/// Streaming weight update `w' = w + alpha * (r - w)` clipped to `[0, 1]`
/// and rounded to 4 decimal places.
///
/// Matches Python `stream_update_weight()` (`apply_feedback_weights.py:53`).
pub fn stream_update_weight(
    previous_weight: f64,
    normalized_rating: f64,
    alpha: f64,
) -> Result<f64, FeedbackError> {
    if !(alpha > 0.0 && alpha <= 1.0) {
        return Err(FeedbackError::InvalidAlpha(alpha));
    }
    let updated = previous_weight + alpha * (normalized_rating - previous_weight);
    let clipped = updated.clamp(0.0, 1.0);
    let factor = 10f64.powi(FEEDBACK_WEIGHT_DECIMALS);
    Ok((clipped * factor).round() / factor)
}

/// Eligibility check matching Python `_is_eligible()`
/// (`extract_feedback_qas.py:15-41`).
fn is_eligible(entry: &SessionQAEntry) -> bool {
    let score = match entry.feedback_score {
        Some(s) if (1..=5).contains(&s) => s,
        _ => return false,
    };
    let _ = score;

    if let Some(meta) = &entry.memify_metadata
        && meta.get(FEEDBACK_WEIGHTS_APPLIED_KEY).copied() == Some(true)
    {
        return false;
    }

    match &entry.used_graph_element_ids {
        Some(ids) => {
            let has_nodes = ids.node_ids.iter().any(|s| !s.is_empty());
            let has_edges = ids.edge_ids.iter().any(|s| !s.is_empty());
            has_nodes || has_edges
        }
        None => false,
    }
}

/// De-duplicate and lexically sort string ids, preserving only non-empty
/// entries. Mirrors Python `_extract_ids()`.
fn dedup_sorted<'a>(ids: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let set: HashSet<&str> = ids
        .into_iter()
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    let mut v: Vec<String> = set.into_iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

/// Parse an edge id string `"source|||target|||rel"` into an
/// [`EdgeKey`]. Returns `None` if the id does not have the expected three
/// segments.
fn parse_edge_id(id: &str) -> Option<EdgeKey> {
    let parts: Vec<&str> = id.splitn(3, EDGE_ID_DELIMITER).collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    ))
}

/// Inner per-element update: fetch existing weights, compute new weights
/// via `stream_update_weight`, write back, and report whether every id
/// was found and written.
async fn update_node_weights(
    graph_db: &dyn GraphDBTrait,
    ids: &[String],
    normalized_rating: f64,
    alpha: f64,
) -> Result<bool, FeedbackError> {
    if ids.is_empty() {
        return Ok(true);
    }
    let existing = graph_db.get_node_feedback_weights(ids).await?;
    let mut updates: HashMap<String, f64> = HashMap::new();
    let mut all_found = true;
    for id in ids {
        match existing.get(id).copied() {
            Some(prev) => {
                updates.insert(
                    id.clone(),
                    stream_update_weight(prev, normalized_rating, alpha)?,
                );
            }
            None => {
                all_found = false;
            }
        }
    }
    if updates.is_empty() {
        return Ok(false);
    }
    let results = graph_db.set_node_feedback_weights(&updates).await?;
    let all_written = updates
        .keys()
        .all(|k| results.get(k).copied().unwrap_or(false));
    Ok(all_found && all_written)
}

async fn update_edge_weights(
    graph_db: &dyn GraphDBTrait,
    edge_ids: &[String],
    normalized_rating: f64,
    alpha: f64,
) -> Result<bool, FeedbackError> {
    if edge_ids.is_empty() {
        return Ok(true);
    }
    // Parse "source|||target|||rel" strings; silently skip malformed ones
    // and treat them as "not found" for the all-applied flag.
    let mut keys: Vec<EdgeKey> = Vec::with_capacity(edge_ids.len());
    let mut all_parsed = true;
    for id in edge_ids {
        match parse_edge_id(id) {
            Some(k) => keys.push(k),
            None => {
                warn!("feedback_weights: malformed edge id {id:?}, skipping");
                all_parsed = false;
            }
        }
    }
    if keys.is_empty() {
        return Ok(false);
    }
    let existing = graph_db.get_edge_feedback_weights(&keys).await?;
    let mut updates: HashMap<EdgeKey, f64> = HashMap::new();
    let mut all_found = true;
    for k in &keys {
        match existing.get(k).copied() {
            Some(prev) => {
                updates.insert(
                    k.clone(),
                    stream_update_weight(prev, normalized_rating, alpha)?,
                );
            }
            None => {
                all_found = false;
            }
        }
    }
    if updates.is_empty() {
        return Ok(false);
    }
    let results = graph_db.set_edge_feedback_weights(&updates).await?;
    let all_written = updates
        .keys()
        .all(|k| results.get(k).copied().unwrap_or(false));
    Ok(all_parsed && all_found && all_written)
}

/// Mark the QA entry's `memify_metadata["feedback_weights_applied"] = success`
/// via the session manager.
async fn mark_feedback_processed(
    session_manager: &SessionManager,
    session_id: &str,
    user_id: &str,
    qa_id: &str,
    current_metadata: Option<&HashMap<String, bool>>,
    success: bool,
) -> Result<(), FeedbackError> {
    let mut meta: HashMap<String, bool> = current_metadata.cloned().unwrap_or_default();
    meta.insert(FEEDBACK_WEIGHTS_APPLIED_KEY.to_string(), success);

    session_manager
        .update_qa(
            Some(session_id),
            Some(user_id),
            qa_id,
            SessionQAUpdate {
                memify_metadata: Some(Some(meta)),
                ..Default::default()
            },
        )
        .await?;
    Ok(())
}

/// Apply feedback-weight updates for the given sessions.
#[allow(clippy::too_many_arguments)]
pub async fn apply_feedback_weights_pipeline(
    session_ids: &[String],
    owner_id: Uuid,
    alpha: f64,
    graph_db: &dyn GraphDBTrait,
    session_store: Arc<dyn SessionStore>,
    session_manager: Arc<SessionManager>,
) -> Result<FeedbackApplyResult, FeedbackError> {
    if !(alpha > 0.0 && alpha <= 1.0) {
        return Err(FeedbackError::InvalidAlpha(alpha));
    }

    let user_id_str = owner_id.to_string();
    let mut result = FeedbackApplyResult::default();

    for session_id in session_ids {
        let entries = session_store
            .get_all_qa_entries(session_id, Some(&user_id_str))
            .await?;

        for entry in entries {
            if !is_eligible(&entry) {
                result.skipped += 1;
                continue;
            }

            let score = match entry.feedback_score {
                Some(s) => s,
                None => {
                    // Unreachable: is_eligible requires Some(valid).
                    result.skipped += 1;
                    continue;
                }
            };
            let normalized = match normalize_feedback_score(score) {
                Ok(v) => v,
                Err(_) => {
                    result.skipped += 1;
                    continue;
                }
            };

            let used = entry
                .used_graph_element_ids
                .as_ref()
                .cloned()
                .unwrap_or(UsedGraphElementIds::default());
            let node_ids = dedup_sorted(used.node_ids.iter());
            let edge_ids = dedup_sorted(used.edge_ids.iter());

            if node_ids.is_empty() && edge_ids.is_empty() {
                // Eligible entry with no usable ids — mark as processed
                // (success=false) so we don't revisit.
                mark_feedback_processed(
                    &session_manager,
                    session_id,
                    &user_id_str,
                    &entry.id.to_string(),
                    entry.memify_metadata.as_ref(),
                    false,
                )
                .await?;
                result.skipped += 1;
                continue;
            }

            let node_success = update_node_weights(graph_db, &node_ids, normalized, alpha).await?;
            let edge_success = update_edge_weights(graph_db, &edge_ids, normalized, alpha).await?;
            let success = node_success && edge_success;

            mark_feedback_processed(
                &session_manager,
                session_id,
                &user_id_str,
                &entry.id.to_string(),
                entry.memify_metadata.as_ref(),
                success,
            )
            .await?;

            info!(
                qa_id = %entry.id,
                session_id = session_id,
                nodes = node_ids.len(),
                edges = edge_ids.len(),
                applied = success,
                "feedback_weights: processed QA entry"
            );

            result.processed += 1;
            if success {
                result.applied += 1;
            }
        }
    }

    info!(
        processed = result.processed,
        applied = result.applied,
        skipped = result.skipped,
        "feedback_weights: stage complete"
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_scores_endpoints() {
        assert!((normalize_feedback_score(1).unwrap() - 0.0).abs() < 1e-9);
        assert!((normalize_feedback_score(3).unwrap() - 0.5).abs() < 1e-9);
        assert!((normalize_feedback_score(5).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn normalize_scores_rejects_out_of_range() {
        assert!(normalize_feedback_score(0).is_err());
        assert!(normalize_feedback_score(6).is_err());
        assert!(normalize_feedback_score(-1).is_err());
    }

    #[test]
    fn stream_update_midpoint() {
        let w = stream_update_weight(0.5, 1.0, 0.1).unwrap();
        assert!((w - 0.55).abs() < 1e-9, "got {w}");
    }

    #[test]
    fn stream_update_zero_stays_zero() {
        let w = stream_update_weight(0.0, 0.0, 1.0).unwrap();
        assert!((w - 0.0).abs() < 1e-9);
    }

    #[test]
    fn stream_update_clips_high() {
        // normalized_rating clamped but math says 0.9 + 0.5*(2.0-0.9) = 1.45 -> 1.0
        let w = stream_update_weight(0.9, 2.0, 0.5).unwrap();
        assert!((w - 1.0).abs() < 1e-9);
    }

    #[test]
    fn stream_update_clips_low() {
        let w = stream_update_weight(0.1, -1.0, 0.5).unwrap();
        assert!((w - 0.0).abs() < 1e-9);
    }

    #[test]
    fn stream_update_rejects_bad_alpha() {
        assert!(stream_update_weight(0.5, 0.5, 0.0).is_err());
        assert!(stream_update_weight(0.5, 0.5, 1.1).is_err());
        assert!(stream_update_weight(0.5, 0.5, -0.1).is_err());
    }

    #[test]
    fn stream_update_rounds_to_4dp() {
        // 0.1 + 0.1 * (0.5 - 0.1) = 0.14 exactly, but float leftovers
        let w = stream_update_weight(0.1, 0.5, 0.1).unwrap();
        assert!((w - 0.14).abs() < 1e-12);
    }

    #[test]
    fn parse_edge_id_ok() {
        let k = parse_edge_id("a|||b|||rel").unwrap();
        assert_eq!(k.0, "a");
        assert_eq!(k.1, "b");
        assert_eq!(k.2, "rel");
    }

    #[test]
    fn parse_edge_id_with_extra_delim_in_rel() {
        let k = parse_edge_id("a|||b|||rel|||extra").unwrap();
        assert_eq!(k.2, "rel|||extra");
    }

    #[test]
    fn parse_edge_id_malformed() {
        assert!(parse_edge_id("no_delim").is_none());
        assert!(parse_edge_id("only|||one").is_none());
    }

    #[test]
    fn dedup_sorted_works() {
        let v = [
            "b".to_string(),
            "a".to_string(),
            "".to_string(),
            "a".to_string(),
        ];
        let result = dedup_sorted(v.iter());
        assert_eq!(result, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn is_eligible_valid_node_ids() {
        let entry = SessionQAEntry {
            id: Uuid::new_v4(),
            session_id: "s".into(),
            user_id: None,
            question: "q".into(),
            answer: "a".into(),
            context: None,
            created_at: chrono::Utc::now(),
            feedback_text: None,
            feedback_score: Some(4),
            used_graph_element_ids: Some(UsedGraphElementIds {
                node_ids: vec!["n1".into()],
                edge_ids: vec![],
            }),
            memify_metadata: None,
        };
        assert!(is_eligible(&entry));
    }

    #[test]
    fn is_eligible_rejects_already_applied() {
        let mut meta = HashMap::new();
        meta.insert("feedback_weights_applied".to_string(), true);
        let entry = SessionQAEntry {
            id: Uuid::new_v4(),
            session_id: "s".into(),
            user_id: None,
            question: "q".into(),
            answer: "a".into(),
            context: None,
            created_at: chrono::Utc::now(),
            feedback_text: None,
            feedback_score: Some(4),
            used_graph_element_ids: Some(UsedGraphElementIds {
                node_ids: vec!["n1".into()],
                edge_ids: vec![],
            }),
            memify_metadata: Some(meta),
        };
        assert!(!is_eligible(&entry));
    }

    #[test]
    fn is_eligible_rejects_missing_ids() {
        let entry = SessionQAEntry {
            id: Uuid::new_v4(),
            session_id: "s".into(),
            user_id: None,
            question: "q".into(),
            answer: "a".into(),
            context: None,
            created_at: chrono::Utc::now(),
            feedback_text: None,
            feedback_score: Some(4),
            used_graph_element_ids: None,
            memify_metadata: None,
        };
        assert!(!is_eligible(&entry));
    }

    #[test]
    fn is_eligible_rejects_invalid_score() {
        let entry = SessionQAEntry {
            id: Uuid::new_v4(),
            session_id: "s".into(),
            user_id: None,
            question: "q".into(),
            answer: "a".into(),
            context: None,
            created_at: chrono::Utc::now(),
            feedback_text: None,
            feedback_score: Some(0),
            used_graph_element_ids: Some(UsedGraphElementIds {
                node_ids: vec!["n1".into()],
                edge_ids: vec![],
            }),
            memify_metadata: None,
        };
        assert!(!is_eligible(&entry));
    }
}
