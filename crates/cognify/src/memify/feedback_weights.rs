//! Stage 1 of `improve()` — apply feedback weights from session Q&A entries
//! to graph nodes/edges.
//!
//! Ported from Python cognee's
//! - `cognee/tasks/memify/apply_feedback_weights.py`
//! - `cognee/tasks/memify/extract_feedback_qas.py`
//! - `cognee/tasks/memify/feedback_weights_constants.py`
//! - `cognee/memify_pipelines/apply_feedback_weights.py`
//!
//! The pipeline:
//! 1. For each session, reads the stored feedback rows
//!    ([`cognee_session::build_implicit_rating_map`]: qa id -> inferred
//!    rating, latest row wins) and every Q&A entry. A rated entry whose
//!    `feedback_weights_applied` flag is not `true` becomes a [`FeedbackItem`];
//!    an explicit `feedback_score` beats the implicit rating
//!    ([`resolve_feedback`]).
//! 2. Each item normalizes its score to `[0, 1]` and applies a streaming
//!    update (`w' = w + alpha * (r - w)`, clipped, rounded to 4 dp) to the
//!    graph elements it used, at `alpha * 0.5` for an implicit rating.
//! 3. The row's bookkeeping (`feedback_weights_applied_*`,
//!    `feedback_weights_attempts`, `feedback_weights_pruned_ids`) records which
//!    elements already moved, so a retry moves each element exactly once.
//!    Ids the graph does not have (deleted, or owned by another dataset) keep
//!    the row pending until [`FEEDBACK_WEIGHTS_MAX_ATTEMPTS`] runs; a row
//!    re-rated with another score or source starts over.
//! 4. One row's failure (a graph or session-store error) never aborts the
//!    batch: its bookkeeping is persisted first, the row counts as skipped and
//!    is retried on the next run.
//!
//! A QA entry's id is its `qa_id`. The stores keep Python's `uuid4` strings.
//! A foreign writer's non-UUID `qa_id` reads back as a deterministic v5 UUID
//! of the raw text, and every store matches `update_qa` on that parsed or
//! derived id, so the bookkeeping of such a row (and of an uppercase or
//! non-hyphenated UUID) is saved like any other.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cognee_graph::{EdgeKey, GraphDBTrait};
use cognee_session::{
    SessionManager, SessionQAEntry, SessionQAUpdate, SessionStore, UsedGraphElementIds,
    build_implicit_rating_map, valid_rating,
};
use serde_json::{Value, json};
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

    #[error("session_ids must be a non-empty list of non-empty strings")]
    InvalidSessionIds,

    #[error("Failed to update memify metadata for qa_id={qa_id} in session={session_id}")]
    MetadataNotUpdated { qa_id: String, session_id: String },

    #[error("Session error: {0}")]
    Session(#[from] cognee_session::SessionError),

    #[error("Graph error: {0}")]
    Graph(#[from] cognee_graph::GraphDBError),
}

/// Done marker of a row: `true` once every surviving element moved (or the
/// attempt cap was reached). The only key other code reads.
pub const FEEDBACK_WEIGHTS_APPLIED_KEY: &str = "feedback_weights_applied";
/// Node ids whose weight already moved for the row (list of strings).
pub const FEEDBACK_WEIGHTS_APPLIED_NODE_IDS_KEY: &str = "feedback_weights_applied_node_ids";
/// Edge ids whose weight already moved for the row (list of strings).
pub const FEEDBACK_WEIGHTS_APPLIED_EDGE_IDS_KEY: &str = "feedback_weights_applied_edge_ids";
/// The 1-5 rating those ids were moved with. A different rating or source on
/// the same row starts the row over.
pub const FEEDBACK_WEIGHTS_APPLIED_SCORE_KEY: &str = "feedback_weights_applied_score";
/// The source (`explicit` / `implicit`) of that rating.
pub const FEEDBACK_WEIGHTS_APPLIED_SOURCE_KEY: &str = "feedback_weights_applied_source";
/// Number of runs that tried to write the row's weights (integer).
pub const FEEDBACK_WEIGHTS_ATTEMPTS_KEY: &str = "feedback_weights_attempts";
/// Ids the last run could not find in its graph (observability only).
pub const FEEDBACK_WEIGHTS_PRUNED_IDS_KEY: &str = "feedback_weights_pruned_ids";

/// After this many attempts a row is marked done even if some writes never
/// succeeded or some ids were never found.
pub const FEEDBACK_WEIGHTS_MAX_ATTEMPTS: i64 = 3;

/// An implicit rating (inferred from the user's next turn) moves weights at
/// half the learning rate of an explicit one.
pub const IMPLICIT_FEEDBACK_ALPHA_FACTOR: f64 = 0.5;

/// Number of decimal places to round the streaming update to. Matches
/// Python's `FEEDBACK_WEIGHT_DECIMALS = 4`.
const FEEDBACK_WEIGHT_DECIMALS: i32 = 4;

/// Delimiter used to encode edge-IDs as `"source|||target|||relation"`
/// strings in `used_graph_element_ids.edge_ids`.
pub const EDGE_ID_DELIMITER: &str = "|||";

/// Where a row's rating came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackSource {
    /// The user's own `feedback_score`.
    Explicit,
    /// Inferred from the user's next turn.
    Implicit,
}

impl FeedbackSource {
    /// The value stored under [`FEEDBACK_WEIGHTS_APPLIED_SOURCE_KEY`].
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Implicit => "implicit",
        }
    }
}

/// A rated QA row waiting for its weights to move (Python's `FeedbackItem`).
#[derive(Debug, Clone)]
pub struct FeedbackItem {
    pub session_id: String,
    pub qa_id: String,
    pub feedback_score: i32,
    pub feedback_source: FeedbackSource,
    /// Only its length is ever logged: it is the user's own words.
    pub feedback_text: Option<String>,
    pub used_graph_element_ids: Option<UsedGraphElementIds>,
    pub memify_metadata: HashMap<String, Value>,
}

/// Summary of a Stage 1 run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedbackApplyResult {
    /// Rows whose weights were attempted this run.
    pub processed: usize,
    /// Rows whose surviving elements all moved (no pruned, no failed id).
    pub applied: usize,
    /// Rows left alone: unrated, already done, no usable ids, or errored
    /// (Python's `skipped`; [`errored`](Self::errored) is a subset).
    pub skipped: usize,
    /// Rows marked done this run: applied, sealed by the attempt cap, or
    /// without any usable id.
    pub done: usize,
    /// Rows whose processing raised (a graph or session-store error); they
    /// are counted in `skipped` and retried next run.
    pub errored: usize,
    /// Elements (nodes and edges) whose weight moved this run.
    pub elements_moved: usize,
    /// Elements the graph did not have this run.
    pub elements_pruned: usize,
    /// Elements the graph has but whose write failed this run.
    pub elements_failed: usize,
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

/// Whether the row's done marker is `true` (Python `_is_already_applied`).
fn is_already_applied(metadata: Option<&HashMap<String, Value>>) -> bool {
    metadata
        .and_then(|meta| meta.get(FEEDBACK_WEIGHTS_APPLIED_KEY))
        .and_then(Value::as_bool)
        == Some(true)
}

/// The rating of a QA entry: `(score, source, feedback text)`, or `None` when
/// it carries none. An explicit `feedback_score` always wins; the implicit
/// rating inferred from the user's next turn counts only when there is no
/// explicit one (Python `resolve_feedback`).
pub fn resolve_feedback(
    entry: &SessionQAEntry,
    implicit_ratings: &HashMap<String, (i32, String)>,
) -> Option<(i32, FeedbackSource, Option<String>)> {
    let score = entry.feedback_score.map(i64::from).map(Value::from);
    if let Some(explicit) = valid_rating(score.as_ref()) {
        return Some((
            explicit,
            FeedbackSource::Explicit,
            entry.feedback_text.clone(),
        ));
    }
    let (rating, text) = implicit_ratings.get(&entry.id.to_string())?;
    Some((
        *rating,
        FeedbackSource::Implicit,
        Some(text.clone()).filter(|text| !text.is_empty()),
    ))
}

/// What reading the sessions found.
#[derive(Debug, Default)]
pub struct FeedbackExtraction {
    /// Rated rows not yet done, in session then store order.
    pub items: Vec<FeedbackItem>,
    /// Entries left out: unrated, or already done.
    pub skipped: usize,
}

/// Python's session-id validation: a non-empty list of non-empty strings.
fn validate_session_ids(session_ids: &[String]) -> Result<(), FeedbackError> {
    if session_ids.is_empty() || session_ids.iter().any(String::is_empty) {
        return Err(FeedbackError::InvalidSessionIds);
    }
    Ok(())
}

/// Read `session_ids` and collect the rated QA rows not yet applied to graph
/// weights (Python `extract_feedback_qas`). A row without
/// `used_graph_element_ids` is collected too, so [`apply_feedback_weights`]
/// can mark it done once and it is never rescanned.
pub async fn extract_feedback_qas(
    session_ids: &[String],
    user_id: &str,
    session_store: &dyn SessionStore,
    session_manager: &SessionManager,
) -> Result<FeedbackExtraction, FeedbackError> {
    validate_session_ids(session_ids)?;
    let mut extraction = FeedbackExtraction::default();
    for session_id in session_ids {
        let context_rows = session_manager
            .get_session_context_entries(Some(session_id), Some(user_id))
            .await?;
        let implicit_ratings = build_implicit_rating_map(&context_rows);
        let entries = session_store
            .get_all_qa_entries(session_id, Some(user_id))
            .await?;
        for entry in entries {
            if is_already_applied(entry.memify_metadata.as_ref()) {
                extraction.skipped += 1;
                continue;
            }
            let Some((feedback_score, feedback_source, feedback_text)) =
                resolve_feedback(&entry, &implicit_ratings)
            else {
                extraction.skipped += 1;
                continue;
            };
            extraction.items.push(FeedbackItem {
                session_id: session_id.clone(),
                qa_id: entry.id.to_string(),
                feedback_score,
                feedback_source,
                feedback_text,
                used_graph_element_ids: entry.used_graph_element_ids,
                memify_metadata: entry.memify_metadata.unwrap_or_default(),
            });
        }
    }
    Ok(extraction)
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

/// How one element-update call went (Python's `ElementUpdateOutcome`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElementUpdateOutcome {
    /// Ids whose weight moved in this call.
    pub applied: Vec<String>,
    /// Ids the graph does not have (a malformed edge id counts here).
    ///
    /// "Pruned" conflates two cases: the element was deleted, or it lives
    /// outside this run's dataset-scoped graph (a row's
    /// `used_graph_element_ids` come from a recall that spans every
    /// accessible dataset). Pruned ids keep the row pending until the attempt
    /// cap, so an improve on the dataset that HAS them can consume the row.
    pub pruned: Vec<String>,
    /// Ids the graph has but whose write did not succeed.
    pub failed: Vec<String>,
}

/// Move the weight of each node id once: fetch existing weights, compute new
/// ones via [`stream_update_weight`], write back, and report which moved,
/// which are gone and which failed.
async fn update_node_weights(
    graph_db: &dyn GraphDBTrait,
    ids: &[String],
    normalized_rating: f64,
    alpha: f64,
) -> Result<ElementUpdateOutcome, FeedbackError> {
    let mut outcome = ElementUpdateOutcome::default();
    if ids.is_empty() {
        return Ok(outcome);
    }
    let existing = graph_db.get_node_feedback_weights(ids).await?;
    let mut updates: HashMap<String, f64> = HashMap::new();
    for id in ids {
        match existing.get(id).copied() {
            Some(prev) => {
                updates.insert(
                    id.clone(),
                    stream_update_weight(prev, normalized_rating, alpha)?,
                );
            }
            None => outcome.pruned.push(id.clone()),
        }
    }
    if updates.is_empty() {
        return Ok(outcome);
    }
    let results = graph_db.set_node_feedback_weights(&updates).await?;
    for id in ids.iter().filter(|id| updates.contains_key(*id)) {
        if results.get(id).copied().unwrap_or(false) {
            outcome.applied.push(id.clone());
        } else {
            outcome.failed.push(id.clone());
        }
    }
    Ok(outcome)
}

/// [`update_node_weights`] for `"source|||target|||rel"` edge ids.
async fn update_edge_weights(
    graph_db: &dyn GraphDBTrait,
    edge_ids: &[String],
    normalized_rating: f64,
    alpha: f64,
) -> Result<ElementUpdateOutcome, FeedbackError> {
    let mut outcome = ElementUpdateOutcome::default();
    if edge_ids.is_empty() {
        return Ok(outcome);
    }
    let mut keys: Vec<(String, EdgeKey)> = Vec::with_capacity(edge_ids.len());
    for id in edge_ids {
        match parse_edge_id(id) {
            Some(key) => keys.push((id.clone(), key)),
            None => {
                warn!("feedback_weights: malformed edge id {id:?}, treating it as not found");
                outcome.pruned.push(id.clone());
            }
        }
    }
    if keys.is_empty() {
        return Ok(outcome);
    }
    let edge_keys: Vec<EdgeKey> = keys.iter().map(|(_, key)| key.clone()).collect();
    let existing = graph_db.get_edge_feedback_weights(&edge_keys).await?;
    let mut updates: HashMap<EdgeKey, f64> = HashMap::new();
    for (id, key) in &keys {
        match existing.get(key).copied() {
            Some(prev) => {
                updates.insert(
                    key.clone(),
                    stream_update_weight(prev, normalized_rating, alpha)?,
                );
            }
            None => outcome.pruned.push(id.clone()),
        }
    }
    if updates.is_empty() {
        return Ok(outcome);
    }
    let results = graph_db.set_edge_feedback_weights(&updates).await?;
    for (id, key) in keys.iter().filter(|(_, key)| updates.contains_key(key)) {
        if results.get(key).copied().unwrap_or(false) {
            outcome.applied.push(id.clone());
        } else {
            outcome.failed.push(id.clone());
        }
    }
    Ok(outcome)
}

/// Merge `metadata` into the QA entry's `memify_metadata` via the session
/// manager.
///
/// Callers pass only this stage's keys: `memify_metadata` updates merge keys
/// (Python's `update_qa`), so a stale snapshot of the other keys written back
/// here would clobber a flag another stage (`update_user_preferences`'s
/// `preference_*`) set since it was read.
async fn mark_feedback_processed(
    session_manager: &SessionManager,
    session_id: &str,
    user_id: &str,
    qa_id: &str,
    metadata: HashMap<String, Value>,
) -> Result<(), FeedbackError> {
    let updated = session_manager
        .update_qa(
            Some(session_id),
            Some(user_id),
            qa_id,
            SessionQAUpdate {
                memify_metadata: Some(Some(metadata)),
                ..Default::default()
            },
        )
        .await?;
    if !updated {
        return Err(FeedbackError::MetadataNotUpdated {
            qa_id: qa_id.to_string(),
            session_id: session_id.to_string(),
        });
    }
    Ok(())
}

/// Ids stored under `key` (non-empty strings; anything else reads as none).
fn stored_id_set(metadata: &HashMap<String, Value>, key: &str) -> HashSet<String> {
    metadata
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

/// The stored attempt count (a non-negative integer; anything else reads 0).
fn stored_attempts(metadata: &HashMap<String, Value>) -> i64 {
    metadata
        .get(FEEDBACK_WEIGHTS_ATTEMPTS_KEY)
        .and_then(Value::as_i64)
        .filter(|attempts| *attempts >= 0)
        .unwrap_or(0)
}

fn sorted(ids: &HashSet<String>) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().cloned().collect();
    ids.sort();
    ids
}

/// What one row contributed to the run.
#[derive(Debug, Default, Clone, Copy)]
struct ItemOutcome {
    processed: bool,
    applied: bool,
    done: bool,
    moved: usize,
    pruned: usize,
    failed: usize,
}

async fn process_feedback_item(
    item: &FeedbackItem,
    alpha: f64,
    user_id: &str,
    graph_db: &dyn GraphDBTrait,
    session_manager: &SessionManager,
) -> Result<ItemOutcome, (ItemOutcome, FeedbackError)> {
    let metadata = &item.memify_metadata;
    let qa_id = item.qa_id.as_str();
    let session_id = item.session_id.as_str();

    if is_already_applied(Some(metadata)) {
        info!(
            qa_id,
            "feedback_weights: QA entry already applied on the graph"
        );
        return Ok(ItemOutcome::default());
    }
    let Ok(normalized_rating) = normalize_feedback_score(item.feedback_score) else {
        return Ok(ItemOutcome::default());
    };

    let empty = UsedGraphElementIds::default();
    let used = item.used_graph_element_ids.as_ref().unwrap_or(&empty);
    let node_ids = dedup_sorted(used.node_ids.iter());
    let edge_ids = dedup_sorted(used.edge_ids.iter());

    if node_ids.is_empty() && edge_ids.is_empty() {
        // Nothing to apply, ever: mark the row done so it is never rescanned.
        mark_feedback_processed(
            session_manager,
            session_id,
            user_id,
            qa_id,
            HashMap::from([(FEEDBACK_WEIGHTS_APPLIED_KEY.to_string(), Value::Bool(true))]),
        )
        .await
        .map_err(|error| (ItemOutcome::default(), error))?;
        return Ok(ItemOutcome {
            done: true,
            ..Default::default()
        });
    }

    // A re-rated row (different score OR source than the one already applied)
    // starts over; otherwise only ids not yet applied move, so a retry never
    // compounds a weight. Source matters: an implicit rating applied at half
    // alpha, so an explicit re-rating of the same score still owes the rest.
    let source = item.feedback_source.as_str();
    let applied_score = metadata
        .get(FEEDBACK_WEIGHTS_APPLIED_SCORE_KEY)
        .filter(|value| !value.is_null());
    let source_matches = match metadata
        .get(FEEDBACK_WEIGHTS_APPLIED_SOURCE_KEY)
        .filter(|value| !value.is_null())
    {
        None => true,
        Some(applied_source) => applied_source.as_str() == Some(source),
    };
    let same_rating = applied_score.and_then(Value::as_i64) == Some(i64::from(item.feedback_score))
        && source_matches;
    let (mut applied_nodes, mut applied_edges, mut attempts) =
        if applied_score.is_none() || same_rating {
            (
                stored_id_set(metadata, FEEDBACK_WEIGHTS_APPLIED_NODE_IDS_KEY),
                stored_id_set(metadata, FEEDBACK_WEIGHTS_APPLIED_EDGE_IDS_KEY),
                stored_attempts(metadata),
            )
        } else {
            (HashSet::new(), HashSet::new(), 0)
        };

    let pending_nodes: Vec<String> = node_ids
        .iter()
        .filter(|id| !applied_nodes.contains(*id))
        .cloned()
        .collect();
    let pending_edges: Vec<String> = edge_ids
        .iter()
        .filter(|id| !applied_edges.contains(*id))
        .cloned()
        .collect();
    let effective_alpha = match item.feedback_source {
        FeedbackSource::Implicit => alpha * IMPLICIT_FEEDBACK_ALPHA_FACTOR,
        FeedbackSource::Explicit => alpha,
    };
    attempts += 1;

    // A raised graph call must not lose the bookkeeping of the calls that
    // already succeeded: without persisting applied ids and the attempt count
    // below, the same elements would move again on every retry and the attempt
    // cap would never trip. The error is returned after the metadata write.
    let mut node_outcome = ElementUpdateOutcome::default();
    let mut edge_outcome = ElementUpdateOutcome::default();
    let mut graph_error: Option<FeedbackError> = None;
    match update_node_weights(graph_db, &pending_nodes, normalized_rating, effective_alpha).await {
        Ok(outcome) => {
            node_outcome = outcome;
            match update_edge_weights(graph_db, &pending_edges, normalized_rating, effective_alpha)
                .await
            {
                Ok(outcome) => edge_outcome = outcome,
                Err(error) => graph_error = Some(error),
            }
        }
        Err(error) => graph_error = Some(error),
    }
    if let Some(error) = &graph_error {
        warn!(
            qa_id,
            session_id,
            "feedback_weights: graph call failed mid-row; persisting the bookkeeping first: {error}"
        );
    }

    applied_nodes.extend(node_outcome.applied.iter().cloned());
    applied_edges.extend(edge_outcome.applied.iter().cloned());
    let mut pruned: Vec<String> = node_outcome.pruned.clone();
    pruned.extend(edge_outcome.pruned.iter().cloned());
    let mut failed: Vec<String> = node_outcome.failed.clone();
    failed.extend(edge_outcome.failed.iter().cloned());
    if graph_error.is_some() {
        // Pending ids the error left unattempted count as failed for this run.
        let touched: HashSet<&str> = applied_nodes
            .iter()
            .chain(applied_edges.iter())
            .chain(pruned.iter())
            .chain(failed.iter())
            .map(String::as_str)
            .collect();
        let unattempted: Vec<String> = pending_nodes
            .iter()
            .chain(pending_edges.iter())
            .filter(|id| !touched.contains(id.as_str()))
            .cloned()
            .collect();
        failed.extend(unattempted);
    }

    if !pruned.is_empty() {
        warn!(
            qa_id,
            session_id,
            pruned = pruned.len(),
            "feedback_weights: graph elements not found in this dataset's graph (deleted, or \
             owned by another dataset); the row stays pending until the attempt cap so another \
             dataset's improve can consume them"
        );
    }

    let qa_success = failed.is_empty() && pruned.is_empty();
    let done = qa_success || attempts >= FEEDBACK_WEIGHTS_MAX_ATTEMPTS;
    if !qa_success && done {
        warn!(
            qa_id,
            session_id,
            attempts,
            failed = failed.len(),
            pruned = pruned.len(),
            "feedback_weights: giving up on the row after the attempt cap"
        );
    }

    let pruned_set: HashSet<String> = pruned.iter().cloned().collect();
    let moved = node_outcome.applied.len() + edge_outcome.applied.len();
    let outcome = ItemOutcome {
        processed: true,
        applied: qa_success,
        done,
        moved,
        pruned: pruned.len(),
        failed: failed.len(),
    };
    mark_feedback_processed(
        session_manager,
        session_id,
        user_id,
        qa_id,
        HashMap::from([
            (FEEDBACK_WEIGHTS_APPLIED_KEY.to_string(), json!(done)),
            (
                FEEDBACK_WEIGHTS_APPLIED_NODE_IDS_KEY.to_string(),
                json!(sorted(&applied_nodes)),
            ),
            (
                FEEDBACK_WEIGHTS_APPLIED_EDGE_IDS_KEY.to_string(),
                json!(sorted(&applied_edges)),
            ),
            (
                FEEDBACK_WEIGHTS_APPLIED_SCORE_KEY.to_string(),
                json!(item.feedback_score),
            ),
            (
                FEEDBACK_WEIGHTS_APPLIED_SOURCE_KEY.to_string(),
                json!(source),
            ),
            (FEEDBACK_WEIGHTS_ATTEMPTS_KEY.to_string(), json!(attempts)),
            (
                FEEDBACK_WEIGHTS_PRUNED_IDS_KEY.to_string(),
                json!(sorted(&pruned_set)),
            ),
        ]),
    )
    .await
    .map_err(|error| {
        // Nothing was persisted: the weights moved, but the row is not done.
        let unsaved = ItemOutcome {
            done: false,
            ..outcome
        };
        (unsaved, error)
    })?;

    info!(
        qa_id,
        session_id,
        source,
        alpha = effective_alpha,
        nodes = node_ids.len(),
        edges = edge_ids.len(),
        moved,
        pruned = pruned.len(),
        failed = failed.len(),
        attempt = attempts,
        applied = qa_success,
        // Length only: the text is the user's own words, never log it.
        feedback_text_len = item.feedback_text.as_deref().map_or(0, str::len),
        "feedback_weights: processed QA entry"
    );

    match graph_error {
        // The row's counts are returned with the error so the run totals
        // agree with what was persisted (e.g. a row sealed by the cap).
        Some(error) => Err((outcome, error)),
        None => Ok(outcome),
    }
}

/// Apply feedback-based weight updates for `items` (Python
/// `apply_feedback_weights`). One row's failure is logged and counted as
/// skipped: it never aborts the batch, and its bookkeeping was persisted
/// before the failure surfaced, so its retry never re-moves what moved.
pub async fn apply_feedback_weights(
    items: &[FeedbackItem],
    alpha: f64,
    user_id: &str,
    graph_db: &dyn GraphDBTrait,
    session_manager: &SessionManager,
) -> Result<FeedbackApplyResult, FeedbackError> {
    if !(alpha > 0.0 && alpha <= 1.0) {
        return Err(FeedbackError::InvalidAlpha(alpha));
    }
    let mut result = FeedbackApplyResult::default();
    for item in items {
        match process_feedback_item(item, alpha, user_id, graph_db, session_manager).await {
            Ok(outcome) => {
                if outcome.processed {
                    result.processed += 1;
                } else {
                    result.skipped += 1;
                }
                result.applied += usize::from(outcome.applied);
                result.done += usize::from(outcome.done);
                result.elements_moved += outcome.moved;
                result.elements_pruned += outcome.pruned;
                result.elements_failed += outcome.failed;
            }
            Err((outcome, error)) => {
                result.done += usize::from(outcome.done);
                result.elements_moved += outcome.moved;
                result.elements_pruned += outcome.pruned;
                result.elements_failed += outcome.failed;
                warn!(
                    qa_id = item.qa_id,
                    session_id = item.session_id,
                    "feedback_weights: row failed and will be retried: {error}"
                );
                result.skipped += 1;
                result.errored += 1;
            }
        }
    }
    Ok(result)
}

/// Apply feedback-weight updates for the given sessions: extract the rated
/// rows, then move their weights.
pub async fn apply_feedback_weights_pipeline(
    session_ids: &[String],
    owner_id: Uuid,
    alpha: f64,
    graph_db: &dyn GraphDBTrait,
    session_store: Arc<dyn SessionStore>,
    session_manager: Arc<SessionManager>,
) -> Result<FeedbackApplyResult, FeedbackError> {
    validate_session_ids(session_ids)?;
    if !(alpha > 0.0 && alpha <= 1.0) {
        return Err(FeedbackError::InvalidAlpha(alpha));
    }
    let user_id = owner_id.to_string();
    let extraction =
        extract_feedback_qas(session_ids, &user_id, &*session_store, &session_manager).await?;
    let mut result = apply_feedback_weights(
        &extraction.items,
        alpha,
        &user_id,
        graph_db,
        &session_manager,
    )
    .await?;
    result.skipped += extraction.skipped;

    info!(
        processed = result.processed,
        applied = result.applied,
        skipped = result.skipped,
        done = result.done,
        errored = result.errored,
        moved = result.elements_moved,
        pruned = result.elements_pruned,
        failed = result.elements_failed,
        "feedback_weights: stage complete"
    );
    Ok(result)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
}
