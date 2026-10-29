//! Fold a user's rated turns and stated preferences into their preference
//! subgraph — the `update_user_preferences` stage.
//!
//! Port of Python's `cognee/modules/user_preferences/update.py`. Weights and
//! text are two independent tracks: a user can rate turns without stating a
//! preference, and state one without rating anything. The clock, the prune
//! pass and the text refresh run on every call.
//!
//! - `feedback_score` is only read (it also gates `feedback_weights`).
//! - Only `used_graph_element_ids.node_ids` become `prefers` targets: an edge
//!   cannot be the endpoint of an edge.
//! - Decay is never written: it is computed on read from a turn counter that
//!   advances on every turn, rated or not.
//!
//! Re-running is safe: each turn is counted once and its rating spent once
//! (`memify_metadata` flags), and each text row is folded once (watermark).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cognee_cognify::memify::{normalize_feedback_score, stream_update_weight};
use cognee_graph::GraphDBTrait;
use cognee_search::user_preferences::{
    MAX_PREFERENCE_TEXT_CHARS, NEUTRAL_WEIGHT, PREFERENCE_DELETE_THRESHOLD,
    PREFERENCE_TURN_COUNTED_KEY, PREFERENCE_WEIGHTS_APPLIED_KEY, PreferenceConfig,
    StoredPreferenceWeight, delete_prefers_edges, effective_weight, load_preference_state,
    node_text, node_turn_counter, upsert_preference_node, write_prefers_edges,
};
use cognee_session::{
    FEEDBACK_ENTRY_KIND, MIN_CANDIDATE_CONFIDENCE, SessionContextEntry, SessionContextRow,
    SessionManager, SessionQAEntry, SessionQAUpdate, coerce_active_context_entries,
};
use serde_json::Value;
use tracing::warn;

/// `status` of a run that changed something.
pub const STATUS_COMPLETED: &str = "completed";
/// `status` (and the stage's reason) of a run with nothing to change.
pub const REASON_NO_CHANGES: &str = "no_changes";

/// Outcome of one preference update for a (user, dataset) pair (Python's
/// `PreferenceUpdateResult`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreferenceUpdateResult {
    /// [`STATUS_COMPLETED`] or [`REASON_NO_CHANGES`].
    pub status: &'static str,
    pub turns_applied: usize,
    pub edges_written: usize,
    pub edges_pruned: usize,
    pub text_lines_added: usize,
}

/// A 1–5 rating, from a JSON integer (a bool or a float is no rating).
fn valid_rating(value: Option<&Value>) -> Option<i32> {
    let rating = value?.as_i64()?;
    (1..=5)
        .contains(&rating)
        .then(|| i32::try_from(rating).ok())
        .flatten()
}

/// qa id → the rating feedback rows inferred for it, the latest row winning
/// (`rows` oldest first; Python's `build_rating_map`).
pub fn build_rating_map(feedback_rows: &[SessionContextRow]) -> HashMap<String, i32> {
    let mut ratings = HashMap::new();
    for row in feedback_rows {
        let Some(rating) = valid_rating(row.get("referenced_qa_rating")) else {
            continue;
        };
        for qa_id in row
            .get("referenced_qa_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            ratings.insert(qa_id.to_string(), rating);
        }
    }
    ratings
}

/// The turn's rating: explicit human feedback wins, else the inferred one.
pub fn resolve_turn_rating(entry: &SessionQAEntry, ratings: &HashMap<String, i32>) -> Option<i32> {
    match entry.feedback_score {
        Some(score) if (1..=5).contains(&score) => Some(score),
        _ => ratings.get(&entry.id.to_string()).copied(),
    }
}

/// The node ids a turn's answer was built from, sorted and deduplicated.
pub fn extract_node_ids(entry: &SessionQAEntry) -> Vec<String> {
    entry
        .used_graph_element_ids
        .as_ref()
        .map(|ids| {
            ids.node_ids
                .iter()
                .filter(|id| !id.is_empty())
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        })
        .unwrap_or_default()
}

/// Cap `text` at `max_chars` characters without cutting a line: whole
/// trailing lines are dropped; a single over-long line falls back to a
/// plain cut.
pub fn truncate_to_whole_lines(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    match truncated.rfind('\n') {
        Some(cut) => truncated[..cut].to_string(),
        None => truncated,
    }
}

/// Serving/preferences gate: never rated harmful, confidence clears 0.75.
fn is_usable(entry: &SessionContextEntry) -> bool {
    entry.harmful_count == 0 && entry.confidence >= MIN_CANDIDATE_CONFIDENCE
}

/// Fold new `preferences` entries into the node text; returns
/// `(text, watermark, lines added)` (Python's `refresh_preference_text`).
///
/// Only usable entries strictly newer than `text_watermark` are folded,
/// newest first, ahead of the existing text, with no content dedup (a
/// preference stated three times is three pieces of evidence); the result is
/// capped at [`MAX_PREFERENCE_TEXT_CHARS`] on line boundaries, so the oldest
/// lines fade.
pub fn refresh_preference_text(
    existing_text: &str,
    text_watermark: &str,
    entries: &[SessionContextEntry],
) -> (String, String, usize) {
    let mut gated: Vec<&SessionContextEntry> = entries
        .iter()
        .filter(|entry| {
            entry.section == "preferences"
                && is_usable(entry)
                && entry.created_at.as_str() > text_watermark
        })
        .collect();
    if gated.is_empty() {
        return (existing_text.to_string(), text_watermark.to_string(), 0);
    }
    // Stable, like Python's `sort(reverse=True)`.
    gated.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let mut combined = gated
        .iter()
        .map(|entry| entry.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !existing_text.is_empty() {
        combined = format!("{combined}\n{existing_text}");
    }
    let watermark = gated
        .iter()
        .map(|entry| entry.created_at.as_str())
        .max()
        .unwrap_or(text_watermark)
        .to_string();
    (
        truncate_to_whole_lines(&combined, MAX_PREFERENCE_TEXT_CHARS),
        watermark,
        gated.len(),
    )
}

fn flag(entry: &SessionQAEntry, key: &str) -> Option<bool> {
    entry.memify_metadata.as_ref()?.get(key).copied()
}

/// The sessions' Q&A turns (oldest first, across sessions), feedback rows
/// (oldest first) and other context rows.
async fn load_session_rows(
    manager: &SessionManager,
    user_id: &str,
    session_ids: &[String],
) -> Result<
    (
        Vec<(String, SessionQAEntry)>,
        Vec<SessionContextRow>,
        Vec<SessionContextRow>,
    ),
    String,
> {
    let mut qa_rows = Vec::new();
    let mut feedback_rows = Vec::new();
    let mut context_rows = Vec::new();
    for session_id in session_ids {
        let entries = manager
            .get_session(Some(session_id), Some(user_id))
            .await
            .map_err(|e| format!("could not read the Q&A of session {session_id}: {e}"))?;
        qa_rows.extend(entries.into_iter().map(|entry| (session_id.clone(), entry)));
        let rows = manager
            .get_session_context_entries(Some(session_id), Some(user_id))
            .await
            .map_err(|e| format!("could not read the context of session {session_id}: {e}"))?;
        for row in rows {
            match row.get("kind").and_then(Value::as_str) {
                Some(FEEDBACK_ENTRY_KIND) => feedback_rows.push(row),
                _ => context_rows.push(row),
            }
        }
    }
    // Oldest first: the update is order-sensitive, so recent evidence wins.
    qa_rows.sort_by_key(|(_, entry)| entry.created_at);
    let created_at = |row: &SessionContextRow| {
        row.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    feedback_rows.sort_by_key(created_at);
    Ok((qa_rows, feedback_rows, context_rows))
}

/// Fold `session_ids` into the preference subgraph of `user_id` in
/// `dataset_id`. The caller runs it in the dataset's storage scope.
///
/// Errors are the reads and the node write; a failed `prefers` edge write is
/// logged and leaves the turns retryable (`preference_weights_applied:
/// false`), as in Python.
pub async fn update_user_preferences(
    graph_db: &dyn GraphDBTrait,
    manager: &SessionManager,
    config: &PreferenceConfig,
    user_id: &str,
    dataset_id: &str,
    session_ids: &[String],
) -> Result<PreferenceUpdateResult, String> {
    if session_ids.is_empty() {
        return Ok(PreferenceUpdateResult {
            status: REASON_NO_CHANGES,
            ..Default::default()
        });
    }
    let (qa_rows, feedback_rows, context_rows) =
        load_session_rows(manager, user_id, session_ids).await?;
    let ratings = build_rating_map(&feedback_rows);

    let state = load_preference_state(graph_db, user_id, dataset_id)
        .await
        .map_err(|e| format!("could not read the preference node: {e}"))?;
    let (node, stored) = match state {
        Some((node, stored)) => (Some(node), stored),
        None => (None, HashMap::new()),
    };
    let mut turn_counter = node.as_ref().map_or(0, node_turn_counter);
    let existing_text = node
        .as_ref()
        .map(|node| node_text(node, "text"))
        .unwrap_or_default();
    let text_watermark = node
        .as_ref()
        .map(|node| node_text(node, "text_watermark"))
        .unwrap_or_default();

    // Step 1: advance the clock once for every not-yet-counted turn.
    let new_turns: Vec<&(String, SessionQAEntry)> = qa_rows
        .iter()
        .filter(|(_, entry)| flag(entry, PREFERENCE_TURN_COUNTED_KEY) != Some(true))
        .collect();
    turn_counter += i64::try_from(new_turns.len()).unwrap_or(i64::MAX);
    let counter_moved = !new_turns.is_empty();

    // Steps 2-4: resolve a rating per turn and move weights toward its target.
    let mut weights_state = stored.clone();
    let mut pending_writes: BTreeMap<String, f64> = BTreeMap::new();
    let mut applied_turns: Vec<&(String, SessionQAEntry)> = Vec::new();
    let mut unapplied_turns: Vec<&(String, SessionQAEntry)> = Vec::new();
    for turn in &qa_rows {
        let entry = &turn.1;
        if flag(entry, PREFERENCE_WEIGHTS_APPLIED_KEY) == Some(true) {
            continue;
        }
        // 3 is a no-op by construction (its target is exactly neutral).
        let Some(rating) = resolve_turn_rating(entry, &ratings).filter(|r| *r != 3) else {
            continue;
        };
        let node_ids = extract_node_ids(entry);
        if node_ids.is_empty() {
            // A rating with nothing to credit: retried on the next run.
            unapplied_turns.push(turn);
            continue;
        }
        let target = normalize_feedback_score(rating).map_err(|e| e.to_string())?;
        for target_id in node_ids {
            let current = weights_state.get(&target_id).map_or(NEUTRAL_WEIGHT, |s| {
                effective_weight(s.weight, s.updated_at_turn, turn_counter, config.beta)
            });
            let updated =
                stream_update_weight(current, target, config.alpha).map_err(|e| e.to_string())?;
            weights_state.insert(
                target_id.clone(),
                StoredPreferenceWeight {
                    weight: updated,
                    updated_at_turn: turn_counter,
                },
            );
            pending_writes.insert(target_id, updated);
        }
        applied_turns.push(turn);
    }

    // Step 7, computed first so the node is written exactly once.
    let context_entries =
        coerce_active_context_entries(&context_rows).map_err(|e| e.to_string())?;
    let (text, new_watermark, text_lines_added) =
        refresh_preference_text(&existing_text, &text_watermark, &context_entries);

    // The node is created only when there is something to store: turns with
    // no rating and no stated preference leave none behind.
    let node_dirty =
        !pending_writes.is_empty() || text_lines_added > 0 || (node.is_some() && counter_moved);
    if node_dirty {
        upsert_preference_node(
            graph_db,
            user_id,
            dataset_id,
            &text,
            turn_counter,
            &new_watermark,
        )
        .await
        .map_err(|e| format!("could not write the preference node: {e}"))?;
    }

    // Step 4's write: weight and updated_at_turn land together.
    let mut write_ok = true;
    let mut edges_written = 0;
    if !pending_writes.is_empty() {
        match write_prefers_edges(graph_db, user_id, dataset_id, &pending_writes, turn_counter)
            .await
        {
            Ok(()) => edges_written = pending_writes.len(),
            Err(error) => {
                warn!(%error, "preference update: prefers-edge write failed");
                write_ok = false;
            }
        }
    }

    // Step 6: prune whenever the clock moved — an edge can decay out of
    // existence through unrated turns. Edges already at exactly neutral (the
    // rewrite a retired edge becomes) are skipped, so they are not
    // rewritten on every later run.
    let mut edges_pruned = 0;
    if counter_moved {
        let state_for_prune = if write_ok { &weights_state } else { &stored };
        let mut prune_ids: Vec<String> = state_for_prune
            .iter()
            .filter(|(_, s)| {
                s.weight != NEUTRAL_WEIGHT
                    && (effective_weight(s.weight, s.updated_at_turn, turn_counter, config.beta)
                        - NEUTRAL_WEIGHT)
                        .abs()
                        <= PREFERENCE_DELETE_THRESHOLD
            })
            .map(|(id, _)| id.clone())
            .collect();
        prune_ids.sort();
        if !prune_ids.is_empty() {
            delete_prefers_edges(graph_db, user_id, dataset_id, &prune_ids)
                .await
                .map_err(|e| format!("could not retire pruned prefers edges: {e}"))?;
            edges_pruned = prune_ids.len();
        }
    }

    // Step 5: mark the turns. The stores merge `memify_metadata`, so only
    // this stage's keys are sent; the applied flag records the outcome
    // (`false` is retried on the next run).
    let mut metadata_updates: BTreeMap<(String, String), HashMap<String, bool>> = BTreeMap::new();
    let key =
        |(session_id, entry): &(String, SessionQAEntry)| (session_id.clone(), entry.id.to_string());
    for turn in &new_turns {
        metadata_updates
            .entry(key(turn))
            .or_default()
            .insert(PREFERENCE_TURN_COUNTED_KEY.to_string(), true);
    }
    for turn in &applied_turns {
        metadata_updates
            .entry(key(turn))
            .or_default()
            .insert(PREFERENCE_WEIGHTS_APPLIED_KEY.to_string(), write_ok);
    }
    for turn in &unapplied_turns {
        metadata_updates
            .entry(key(turn))
            .or_default()
            .insert(PREFERENCE_WEIGHTS_APPLIED_KEY.to_string(), false);
    }
    for ((session_id, qa_id), metadata) in metadata_updates {
        manager
            .update_qa(
                Some(&session_id),
                Some(user_id),
                &qa_id,
                SessionQAUpdate {
                    memify_metadata: Some(Some(metadata)),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("could not mark turn {qa_id}: {e}"))?;
    }

    let changed = counter_moved || edges_written > 0 || edges_pruned > 0 || text_lines_added > 0;
    Ok(PreferenceUpdateResult {
        status: if changed {
            STATUS_COMPLETED
        } else {
            REASON_NO_CHANGES
        },
        turns_applied: if write_ok { applied_turns.len() } else { 0 },
        edges_written,
        edges_pruned,
        text_lines_added,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Arc;

    use cognee_graph::MockGraphDB;
    use cognee_search::user_preferences::preference_node_id;
    use cognee_session::{FsSessionStore, UsedGraphElementIds};
    use serde_json::json;

    use super::*;

    const ALPHA: f64 = 0.3;
    const BETA: f64 = 0.02;
    const USER: &str = "user-1";
    const DATASET: &str = "dataset-1";

    fn config() -> PreferenceConfig {
        PreferenceConfig {
            enabled: true,
            alpha: ALPHA,
            beta: BETA,
            influence: 0.3,
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        manager: SessionManager,
        graph: MockGraphDB,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(Arc::new(FsSessionStore::new(dir.path())));
        let graph = MockGraphDB::new();
        for id in ["n1", "n2", "n-old", "n-strong"] {
            graph
                .add_node_raw(json!({"id": id, "type": "Entity", "name": id}))
                .await
                .unwrap();
        }
        Fixture {
            _dir: dir,
            manager,
            graph,
        }
    }

    impl Fixture {
        /// Store a turn; returns its qa id.
        async fn qa(&self, session: &str, score: Option<i32>, node_ids: Option<&[&str]>) -> String {
            let qa_id = self
                .manager
                .save_qa(
                    Some(session),
                    Some(USER),
                    "q",
                    "a",
                    None,
                    node_ids.map(|ids| UsedGraphElementIds {
                        node_ids: ids.iter().map(|id| id.to_string()).collect(),
                        edge_ids: vec!["edge-x".to_string()],
                    }),
                )
                .await
                .unwrap();
            if let Some(score) = score {
                self.manager
                    .update_qa(
                        Some(session),
                        Some(USER),
                        &qa_id,
                        SessionQAUpdate {
                            feedback_score: Some(Some(score)),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
            }
            qa_id
        }

        async fn row(&self, session: &str, row: Value) {
            self.manager
                .create_session_context_entry(
                    Some(session),
                    Some(USER),
                    row.as_object().unwrap().clone(),
                )
                .await
                .unwrap();
        }

        async fn metadata(&self, session: &str) -> Vec<HashMap<String, bool>> {
            self.manager
                .get_session(Some(session), Some(USER))
                .await
                .unwrap()
                .into_iter()
                .map(|entry| entry.memify_metadata.unwrap_or_default())
                .collect()
        }

        async fn run(&self, sessions: &[&str]) -> PreferenceUpdateResult {
            let sessions: Vec<String> = sessions.iter().map(|s| s.to_string()).collect();
            update_user_preferences(
                &self.graph,
                &self.manager,
                &config(),
                USER,
                DATASET,
                &sessions,
            )
            .await
            .unwrap()
        }

        async fn state(&self) -> Option<(i64, String, HashMap<String, StoredPreferenceWeight>)> {
            load_preference_state(&self.graph, USER, DATASET)
                .await
                .unwrap()
                .map(|(node, weights)| {
                    (node_turn_counter(&node), node_text(&node, "text"), weights)
                })
        }

        async fn seed_node(&self, turn_counter: i64, weights: &[(&str, f64, i64)]) {
            upsert_preference_node(&self.graph, USER, DATASET, "", turn_counter, "")
                .await
                .unwrap();
            for (target, weight, turn) in weights {
                write_prefers_edges(
                    &self.graph,
                    USER,
                    DATASET,
                    &BTreeMap::from([(target.to_string(), *weight)]),
                    *turn,
                )
                .await
                .unwrap();
            }
        }
    }

    fn counted_and_applied(applied: bool) -> HashMap<String, bool> {
        HashMap::from([
            (PREFERENCE_TURN_COUNTED_KEY.to_string(), true),
            (PREFERENCE_WEIGHTS_APPLIED_KEY.to_string(), applied),
        ])
    }

    // Python `TestRatingResolution`.
    #[test]
    fn explicit_feedback_wins_and_the_latest_inferred_rating_counts() {
        let row = |rating: Value, ids: Value| -> SessionContextRow {
            json!({"referenced_qa_rating": rating, "referenced_qa_ids": ids})
                .as_object()
                .unwrap()
                .clone()
        };
        assert_eq!(
            build_rating_map(&[
                row(json!(5), json!(["qa-1"])),
                row(json!(2), json!(["qa-1"]))
            ]),
            HashMap::from([("qa-1".to_string(), 2)])
        );
        assert!(
            build_rating_map(&[
                row(json!(9), json!(["qa-1"])),
                row(json!(null), json!(["qa-2"])),
                row(json!(true), json!(["qa-3"])),
                row(json!(2.5), json!(["qa-4"])),
            ])
            .is_empty()
        );

        let mut entry: SessionQAEntry = serde_json::from_value(json!({
            "id": uuid::Uuid::nil(), "session_id": "s", "user_id": null, "question": "q",
            "answer": "a", "context": null, "created_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        let qa_id = entry.id.to_string();
        let ratings = HashMap::from([(qa_id, 5)]);
        assert_eq!(resolve_turn_rating(&entry, &ratings), Some(5));
        entry.feedback_score = Some(1);
        assert_eq!(resolve_turn_rating(&entry, &ratings), Some(1));
        entry.feedback_score = None;
        assert_eq!(resolve_turn_rating(&entry, &HashMap::new()), None);
    }

    // Python `test_rated_turn_writes_edges_and_marks_applied`.
    #[tokio::test]
    async fn a_rated_turn_writes_edges_and_marks_itself_applied() {
        let f = fixture().await;
        f.qa("s1", Some(5), Some(&["n1", "n2"])).await;
        let result = f.run(&["s1"]).await;
        assert_eq!(result.status, STATUS_COMPLETED);
        assert_eq!((result.turns_applied, result.edges_written), (1, 2));
        let (turn_counter, _, weights) = f.state().await.unwrap();
        assert_eq!(turn_counter, 1);
        for target in ["n1", "n2"] {
            assert_eq!(
                weights[target],
                StoredPreferenceWeight {
                    weight: 0.65,
                    updated_at_turn: 1
                }
            );
        }
        assert_eq!(f.metadata("s1").await, [counted_and_applied(true)]);
    }

    // Python `test_rating_three_is_counted_but_never_applied`.
    #[tokio::test]
    async fn a_neutral_rating_is_counted_but_never_applied() {
        let f = fixture().await;
        f.qa("s1", Some(3), Some(&["n1"])).await;
        let result = f.run(&["s1"]).await;
        assert_eq!((result.status, result.turns_applied), (STATUS_COMPLETED, 0));
        assert!(f.state().await.is_none(), "no node without evidence");
        assert_eq!(
            f.metadata("s1").await,
            [HashMap::from([(
                PREFERENCE_TURN_COUNTED_KEY.to_string(),
                true
            )])]
        );
    }

    // Python `test_memify_metadata_passes_only_own_keys_as_real_bools`: the
    // feedback stage's flag survives because only this stage's keys merge in.
    #[tokio::test]
    async fn the_feedback_stage_flag_survives_the_marking() {
        let f = fixture().await;
        let qa_id = f.qa("s1", Some(5), Some(&["n1"])).await;
        f.manager
            .update_qa(
                Some("s1"),
                Some(USER),
                &qa_id,
                SessionQAUpdate {
                    memify_metadata: Some(Some(HashMap::from([(
                        "feedback_weights_applied".to_string(),
                        true,
                    )]))),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        f.run(&["s1"]).await;
        let mut expected = counted_and_applied(true);
        expected.insert("feedback_weights_applied".to_string(), true);
        assert_eq!(f.metadata("s1").await, [expected]);
    }

    // Python `test_inferred_rating_from_feedback_entry`.
    #[tokio::test]
    async fn a_feedback_row_rating_is_applied() {
        let f = fixture().await;
        let qa_id = f.qa("s1", None, Some(&["n1"])).await;
        f.row(
            "s1",
            json!({"kind": "feedback", "id": "fb-1", "created_at": "t2",
                   "referenced_qa_ids": [qa_id], "referenced_qa_rating": 1}),
        )
        .await;
        let result = f.run(&["s1"]).await;
        assert_eq!(result.turns_applied, 1);
        // 0.5 + 0.3 * (0 - 0.5)
        assert_eq!(f.state().await.unwrap().2["n1"].weight, 0.35);
    }

    // Python `test_already_processed_turn_is_a_no_op`.
    #[tokio::test]
    async fn an_already_processed_turn_is_a_no_op() {
        let f = fixture().await;
        f.qa("s1", Some(5), Some(&["n1"])).await;
        f.run(&["s1"]).await;
        let before = f.state().await;
        let result = f.run(&["s1"]).await;
        assert_eq!(
            result,
            PreferenceUpdateResult {
                status: REASON_NO_CHANGES,
                ..Default::default()
            }
        );
        assert_eq!(f.state().await, before);
    }

    // Python `test_unrated_turns_advance_clock_and_prune_neutral_edges` and
    // `test_neutralized_edge_is_not_re_pruned_on_the_next_run`.
    #[tokio::test]
    async fn unrated_turns_advance_the_clock_and_prune_a_faded_edge_once() {
        let f = fixture().await;
        f.seed_node(5, &[("n-old", 0.505, 0)]).await;
        for _ in 0..10 {
            f.qa("s1", None, None).await;
        }
        let first = f.run(&["s1"]).await;
        assert_eq!((first.edges_pruned, first.edges_written), (1, 0));
        let (turn_counter, _, weights) = f.state().await.unwrap();
        assert_eq!(turn_counter, 15);
        // The edge is retired as exactly neutral.
        assert_eq!(
            weights["n-old"],
            StoredPreferenceWeight {
                weight: NEUTRAL_WEIGHT,
                updated_at_turn: 0
            }
        );
        assert_eq!(f.metadata("s1").await.len(), 10);

        f.qa("s2", None, None).await;
        let second = f.run(&["s2"]).await;
        assert_eq!((second.edges_pruned, second.edges_written), (0, 0));
        assert_eq!(f.state().await.unwrap().0, 16);
    }

    // Python `test_strong_edge_survives_the_prune_pass`.
    #[tokio::test]
    async fn a_strong_edge_survives_the_prune_pass() {
        let f = fixture().await;
        f.seed_node(0, &[("n-strong", 0.9, 0)]).await;
        f.qa("s1", None, None).await;
        assert_eq!(f.run(&["s1"]).await.edges_pruned, 0);
        assert_eq!(f.state().await.unwrap().2["n-strong"].weight, 0.9);
    }

    // Python `test_empty_sessions_create_nothing` and
    // `test_unrated_turns_without_existing_node_do_not_create_one`.
    #[tokio::test]
    async fn turns_without_evidence_create_no_node_but_are_counted() {
        let f = fixture().await;
        assert_eq!(f.run(&["empty"]).await.status, REASON_NO_CHANGES);
        assert!(f.state().await.is_none());

        f.qa("s1", None, None).await;
        assert_eq!(f.run(&["s1"]).await.status, STATUS_COMPLETED);
        assert!(f.state().await.is_none());
        assert_eq!(
            f.metadata("s1").await,
            [HashMap::from([(
                PREFERENCE_TURN_COUNTED_KEY.to_string(),
                true
            )])]
        );
    }

    #[tokio::test]
    async fn a_rating_with_no_nodes_is_retried_later() {
        let f = fixture().await;
        f.qa("s1", Some(5), None).await;
        let result = f.run(&["s1"]).await;
        assert_eq!(result.turns_applied, 0);
        assert_eq!(f.metadata("s1").await, [counted_and_applied(false)]);
    }

    // Python `test_preference_text_folds_without_any_ratings`.
    #[tokio::test]
    async fn preference_text_folds_without_any_ratings() {
        let f = fixture().await;
        f.row(
            "s1",
            json!({"kind": "context", "id": "ctx-1", "section": "preferences",
                   "content": "Keep answers concise.", "confidence": 0.9,
                   "created_at": "2026-01-01T00:00:00+00:00"}),
        )
        .await;
        let result = f.run(&["s1"]).await;
        assert_eq!(
            (result.status, result.text_lines_added),
            (STATUS_COMPLETED, 1)
        );
        let (node, _) = load_preference_state(&f.graph, USER, DATASET)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(node_text(&node, "text"), "Keep answers concise.");
        assert_eq!(
            node_text(&node, "text_watermark"),
            "2026-01-01T00:00:00+00:00"
        );
        assert_eq!(node.get("is_internal"), Some(&json!(true)));
        assert_eq!(
            node.get("id").and_then(Value::as_str),
            Some(preference_node_id(USER, DATASET).to_string().as_str())
        );
        // Folded once: a re-run adds nothing.
        assert_eq!(f.run(&["s1"]).await.text_lines_added, 0);
    }

    fn entry(id: &str, content: &str, created_at: &str) -> SessionContextEntry {
        SessionContextEntry::from_row(
            json!({"id": id, "section": "preferences", "content": content,
                   "confidence": 0.9, "created_at": created_at})
            .as_object()
            .unwrap(),
        )
        .unwrap()
    }

    // Python `TestRefreshPreferenceText`.
    #[test]
    fn refresh_preference_text_matches_python() {
        let mut goal = entry("e1", "a goal", "t1");
        goal.section = "goals".to_string();
        let mut harmful = entry("e2", "harmful", "t2");
        harmful.harmful_count = 1;
        let mut weak = entry("e3", "weak", "t3");
        weak.confidence = 0.5;
        assert_eq!(
            refresh_preference_text("", "", &[goal, harmful, weak]),
            (String::new(), String::new(), 0)
        );

        let entries = [
            entry("e1", "prefers tables", "2026-01-01T00:00:00"),
            entry("e2", "prefers tables", "2026-01-03T00:00:00"),
            entry("e3", "short answers", "2026-01-02T00:00:00"),
        ];
        let (text, watermark, added) = refresh_preference_text("", "", &entries);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            ["prefers tables", "short answers", "prefers tables"]
        );
        assert_eq!((watermark.as_str(), added), ("2026-01-03T00:00:00", 3));
        // A re-run with no new rows is byte-identical.
        assert_eq!(
            refresh_preference_text(&text, &watermark, &entries),
            (text.clone(), watermark.clone(), 0)
        );

        let (text, watermark, added) =
            refresh_preference_text("existing", "2026-01-02T00:00:00", &entries);
        assert_eq!(text, "prefers tables\nexisting");
        assert_eq!((watermark.as_str(), added), ("2026-01-03T00:00:00", 1));
    }

    // Python `test_cap_drops_the_oldest_lines` and
    // `test_single_over_length_line_still_yields_text`.
    #[test]
    fn the_cap_drops_the_oldest_whole_lines() {
        let old_lines: Vec<String> = (0..200).map(|i| format!("old line {i:03}")).collect();
        let old_text = old_lines.join("\n");
        let (text, _, added) = refresh_preference_text(
            &old_text,
            "",
            &[entry("e1", "newest line", "2026-01-05T00:00:00")],
        );
        assert!(text.chars().count() <= MAX_PREFERENCE_TEXT_CHARS);
        assert_eq!(added, 1);
        let kept: Vec<&str> = text.lines().collect();
        assert_eq!(kept[0], "newest line");
        assert_eq!(kept[1..], old_lines[..kept.len() - 1]);

        let long_line = "x".repeat(MAX_PREFERENCE_TEXT_CHARS + 100);
        assert_eq!(
            truncate_to_whole_lines(&long_line, MAX_PREFERENCE_TEXT_CHARS),
            "x".repeat(MAX_PREFERENCE_TEXT_CHARS)
        );
    }
}
