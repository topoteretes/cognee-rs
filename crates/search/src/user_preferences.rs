//! Per-user preference storage inside a dataset's graph, and its read at
//! retrieval time.
//!
//! Port of Python's `cognee/modules/user_preferences/` (`constants.py`,
//! `weights.py`, `store.py`, `lookup.py`): one [`UserPreference`] node per
//! (user, dataset) and weighted `prefers` edges from it to content nodes.
//! Everything goes straight through the graph engine — never embedded — and
//! the node is internal, so it never comes back as a search result.
//!
//! The update that writes them is `improve()`'s `update_user_preferences`
//! stage (`cognee::api::improve`); the retrievers read them through
//! [`load_active_preferences`]. Decay is never written: it is applied on read
//! by [`effective_weight`], from the node's turn counter.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use cognee_graph::{EdgeData, GraphDBTrait, NodeData};
use cognee_models::{DataPoint, UserPreference};
use cognee_utils::id_generation::generate_node_id;
use serde_json::{Value, json};
use uuid::Uuid;

/// Relationship of the weighted edges from a preference node to content nodes.
pub const PREFERS_RELATIONSHIP: &str = "prefers";
/// The node set grouping every preference node.
pub const PREFERENCE_NODE_SET: &str = "user_preferences";
/// A `prefers` weight that carries no signal.
pub const NEUTRAL_WEIGHT: f64 = 0.5;
/// Distance from neutral at or below which a `prefers` edge is pruned.
pub const PREFERENCE_DELETE_THRESHOLD: f64 = 0.01;
/// `memify_metadata` key: the turn advanced the preference clock.
pub const PREFERENCE_TURN_COUNTED_KEY: &str = "preference_turn_counted";
/// `memify_metadata` key: the turn's rating moved the `prefers` weights.
pub const PREFERENCE_WEIGHTS_APPLIED_KEY: &str = "preference_weights_applied";
/// Cap on the preference node's text; the oldest lines fade first.
pub const MAX_PREFERENCE_TEXT_CHARS: usize = 2000;
/// Prepended to the preference text at render time, never stored.
pub const PREFERENCE_RENDER_HEADER: &str = "## What this user prefers\n\
     Most recent first. When two lines conflict, follow the one nearer the top.";

/// The personalization settings (Python's `BaseConfig` knobs), as one value
/// the retrievers and `improve()` are handed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreferenceConfig {
    /// Master switch (`personalization_enabled`). Off: nothing is written and
    /// nothing is read.
    pub enabled: bool,
    /// How far one rating pulls a weight toward its target, in (0, 1]
    /// (`preference_alpha`).
    pub alpha: f64,
    /// How much an untouched weight fades toward neutral per turn, in [0, 1)
    /// (`preference_beta`).
    pub beta: f64,
    /// The most personalization may move a ranking score, in [0, 1]
    /// (`personalization_influence`).
    pub influence: f64,
}

impl Default for PreferenceConfig {
    /// Python's defaults: off, alpha 0.3, beta 0.02, influence 0.3.
    fn default() -> Self {
        Self {
            enabled: false,
            alpha: 0.3,
            beta: 0.02,
            influence: 0.3,
        }
    }
}

/// A stored weight pulled back toward neutral once per idle turn:
/// `0.5 + (w - 0.5) * (1 - beta)^(turn_counter - updated_at_turn)`.
pub fn effective_weight(weight: f64, updated_at_turn: i64, turn_counter: i64, beta: f64) -> f64 {
    let idle = turn_counter.saturating_sub(updated_at_turn).max(0);
    let idle = i32::try_from(idle).unwrap_or(i32::MAX);
    NEUTRAL_WEIGHT + (weight - NEUTRAL_WEIGHT) * (1.0 - beta).powi(idle)
}

/// The multiplicative ranking factor of one personal weight: exactly 1.0 at
/// a neutral weight and at zero influence. In distance space (lower is
/// better) a preferred item's distance shrinks; in score space (higher is
/// better) its score grows.
pub fn personal_factor(weight: f64, influence: f64, distance_space: bool) -> f64 {
    let signal = influence * (2.0 * weight - 1.0);
    if distance_space {
        1.0 - signal
    } else {
        1.0 + signal
    }
}

/// Id of the one preference node of (`user_id`, `dataset_id`):
/// `generate_node_id("user_preference:{user}:{dataset}")`.
pub fn preference_node_id(user_id: &str, dataset_id: &str) -> Uuid {
    generate_node_id(&format!("user_preference:{user_id}:{dataset_id}"))
}

/// Id of the `user_preferences` node set (`NodeSet.id_for`).
fn preference_node_set_id() -> Uuid {
    generate_node_id(&format!("NodeSet:{PREFERENCE_NODE_SET}"))
}

/// One stored `prefers` edge: its weight and the turn it was written at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StoredPreferenceWeight {
    pub weight: f64,
    pub updated_at_turn: i64,
}

/// The preference node's properties and its stored (undecayed) `prefers`
/// weights by target node id; `None` when the node does not exist.
pub type PreferenceState = Option<(NodeData, HashMap<String, StoredPreferenceWeight>)>;

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Read the preference node and its `prefers` edges (Python's
/// `load_preference_state`). Only edges leaving the node with the `prefers`
/// relationship count.
pub async fn load_preference_state(
    graph_db: &dyn GraphDBTrait,
    user_id: &str,
    dataset_id: &str,
) -> cognee_graph::GraphDBResult<PreferenceState> {
    let pref_id = preference_node_id(user_id, dataset_id).to_string();
    let Some(node) = graph_db.get_node(&pref_id).await? else {
        return Ok(None);
    };
    let weights = graph_db
        .get_edges(&pref_id)
        .await?
        .into_iter()
        .filter(|(source, _, relationship, _)| {
            *source == pref_id && relationship == PREFERS_RELATIONSHIP
        })
        .map(|(_, target, _, properties)| {
            let weight = number(properties.get("weight")).unwrap_or(NEUTRAL_WEIGHT);
            let updated_at_turn = number(properties.get("updated_at_turn")).unwrap_or(0.0) as i64;
            (
                target,
                StoredPreferenceWeight {
                    weight,
                    updated_at_turn,
                },
            )
        })
        .collect();
    Ok(Some((node, weights)))
}

/// The node's `turn_counter` property (0 when missing or malformed).
pub fn node_turn_counter(node: &NodeData) -> i64 {
    number(node.get("turn_counter")).unwrap_or(0.0) as i64
}

/// A string property of the node (empty when missing).
pub fn node_text(node: &NodeData, key: &str) -> String {
    node.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Write the preference node of (`user_id`, `dataset_id`) in full and link it
/// to the `user_preferences` node set (Python's `upsert_preference_node`).
/// Every adapter's node and edge writes are upserts, so this is safe as an
/// update too; returns the node id.
pub async fn upsert_preference_node(
    graph_db: &dyn GraphDBTrait,
    user_id: &str,
    dataset_id: &str,
    text: &str,
    turn_counter: i64,
    text_watermark: &str,
) -> cognee_graph::GraphDBResult<String> {
    let pref_id = preference_node_id(user_id, dataset_id);
    let preference = UserPreference::new(
        pref_id,
        user_id,
        dataset_id,
        text,
        turn_counter,
        text_watermark,
    );
    let mut node_set_base = DataPoint::with_metadata(
        "NodeSet",
        None,
        HashMap::from([("index_fields".to_string(), json!([]))]),
    );
    node_set_base.id = preference_node_set_id();
    let mut node_set = serde_json::to_value(&node_set_base)
        .map_err(|e| cognee_graph::GraphDBError::QueryError(e.to_string()))?;
    node_set["name"] = json!(PREFERENCE_NODE_SET);
    let preference = serde_json::to_value(&preference)
        .map_err(|e| cognee_graph::GraphDBError::QueryError(e.to_string()))?;
    graph_db.add_nodes_raw(vec![preference, node_set]).await?;

    let pref_id = pref_id.to_string();
    let node_set_id = preference_node_set_id().to_string();
    graph_db
        .add_edges(&[edge(&pref_id, &node_set_id, "belongs_to_set", [])])
        .await?;
    Ok(pref_id)
}

fn edge<const N: usize>(
    source: &str,
    target: &str,
    relationship: &str,
    extra: [(&'static str, Value); N],
) -> EdgeData {
    let mut properties: HashMap<Cow<'static, str>, Value> = HashMap::from([
        (Cow::Borrowed("relationship_name"), json!(relationship)),
        (Cow::Borrowed("source_node_id"), json!(source)),
        (Cow::Borrowed("target_node_id"), json!(target)),
    ]);
    properties.extend(extra.map(|(key, value)| (Cow::Borrowed(key), value)));
    (
        source.to_string(),
        target.to_string(),
        relationship.to_string(),
        properties,
    )
}

/// Upsert the `prefers` edges to `weights_by_target`, each written with its
/// `updated_at_turn` (Python's `write_prefers_edges`): an edge write replaces
/// the property blob, so the two always land together.
pub async fn write_prefers_edges(
    graph_db: &dyn GraphDBTrait,
    user_id: &str,
    dataset_id: &str,
    weights_by_target: &BTreeMap<String, f64>,
    updated_at_turn: i64,
) -> cognee_graph::GraphDBResult<()> {
    if weights_by_target.is_empty() {
        return Ok(());
    }
    let source = preference_node_id(user_id, dataset_id).to_string();
    let edges: Vec<EdgeData> = weights_by_target
        .iter()
        .map(|(target, weight)| {
            edge(
                &source,
                target,
                PREFERS_RELATIONSHIP,
                [
                    ("weight", json!(weight)),
                    ("updated_at_turn", json!(updated_at_turn)),
                ],
            )
        })
        .collect();
    graph_db.add_edges(&edges).await
}

/// Retire the `prefers` edges to `target_ids` (Python's
/// `delete_prefers_edges`). The graph trait has no edge delete, so this is
/// Python's portable fallback: each edge is rewritten as neutral
/// (weight 0.5, turn 0), which carries no ranking signal and never decays
/// away from neutral. The update skips edges already at exactly neutral, so
/// the rewrite happens once.
pub async fn delete_prefers_edges(
    graph_db: &dyn GraphDBTrait,
    user_id: &str,
    dataset_id: &str,
    target_ids: &[String],
) -> cognee_graph::GraphDBResult<()> {
    if target_ids.is_empty() {
        return Ok(());
    }
    let neutral: BTreeMap<String, f64> = target_ids
        .iter()
        .map(|target| (target.clone(), NEUTRAL_WEIGHT))
        .collect();
    write_prefers_edges(graph_db, user_id, dataset_id, &neutral, 0).await
}

/// The active user's preferences as retrieval reads them: the stated
/// preference lines and the already-decayed `prefers` weights.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActivePreferences {
    /// The node's preference lines, newest first (no header).
    pub text: String,
    /// Target node id → effective (decayed) weight.
    pub weights: HashMap<String, f64>,
}

impl ActivePreferences {
    /// The guidance block a completion prompt carries: the render header and
    /// the lines, or `""` when there are none (Python's
    /// `load_preference_text`).
    pub fn rendered_text(&self) -> String {
        if self.text.trim().is_empty() {
            String::new()
        } else {
            format!("{PREFERENCE_RENDER_HEADER}\n{}", self.text)
        }
    }

    /// The stated lines, newest first (Python's
    /// `load_active_preference_lines`).
    pub fn lines(&self) -> Vec<&str> {
        self.text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect()
    }
}

/// Load the preferences of `user_id` in `dataset_id` for ranking and
/// guidance (Python's `lookup.py`).
///
/// Fails open everywhere: personalization off, no user or no single dataset,
/// no node, or any read error yields the empty value, so a search is never
/// failed by a missing or broken preference node.
pub async fn load_active_preferences(
    graph_db: &dyn GraphDBTrait,
    config: &PreferenceConfig,
    user_id: Option<Uuid>,
    dataset_id: Option<Uuid>,
) -> ActivePreferences {
    if !config.enabled {
        return ActivePreferences::default();
    }
    let (Some(user_id), Some(dataset_id)) = (user_id, dataset_id) else {
        tracing::debug!("preference lookup skipped: no user or no single dataset in scope");
        return ActivePreferences::default();
    };
    match load_preference_state(graph_db, &user_id.to_string(), &dataset_id.to_string()).await {
        Ok(Some((node, stored))) => {
            let turn_counter = node_turn_counter(&node);
            ActivePreferences {
                text: node_text(&node, "text"),
                weights: stored
                    .into_iter()
                    .map(|(target, s)| {
                        (
                            target,
                            effective_weight(
                                s.weight,
                                s.updated_at_turn,
                                turn_counter,
                                config.beta,
                            ),
                        )
                    })
                    .collect(),
            }
        }
        Ok(None) => ActivePreferences::default(),
        Err(error) => {
            tracing::debug!(%error, "preference lookup failed open");
            ActivePreferences::default()
        }
    }
}

/// [`load_active_preferences`] for a search: the request's user, in the
/// request's single dataset — or, under per-dataset storage, the dataset the
/// search runs in. Empty unless the orchestrator stamped
/// [`SearchParams::personalization`](crate::types::SearchParams::personalization).
pub async fn load_for_search(
    graph_db: &dyn GraphDBTrait,
    params: &crate::types::SearchParams,
) -> ActivePreferences {
    let Some(config) = params.personalization.as_ref() else {
        return ActivePreferences::default();
    };
    let dataset_id = params
        .dataset_id
        .or_else(cognee_models::dataset_scope::current_dataset);
    load_active_preferences(graph_db, config, params.user_id, dataset_id).await
}

/// Put the preference guidance block after the question and context, as
/// Python's `build_completion_prompts` places `SessionPrompt.guidance` last
/// in the user prompt. An empty block adds nothing.
pub fn append_preference_guidance(messages: &mut [cognee_llm::Message], guidance: &str) {
    if guidance.is_empty() {
        return;
    }
    if let Some(user) = messages
        .iter_mut()
        .rev()
        .find(|message| message.role == cognee_llm::MessageRole::User)
    {
        user.content = format!("{}\n\n{guidance}", user.content);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use cognee_graph::MockGraphDB;

    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // Python `TestEffectiveWeightDecay` (test_preference_update.py): a
    // three-fives edge (0.8285) under beta 0.02.
    #[test]
    fn decay_matches_python() {
        const BETA: f64 = 0.02;
        let three_fives = 0.8285;
        assert!(close(
            effective_weight(three_fives, 10, 10, BETA),
            three_fives
        ));
        // 34 idle turns halve the distance from neutral.
        assert!((effective_weight(three_fives, 0, 34, BETA) - 0.665).abs() <= 0.005);
        // 173 idle turns fall under the delete threshold, 172 do not.
        assert!(
            (effective_weight(three_fives, 0, 173, BETA) - 0.5).abs()
                <= PREFERENCE_DELETE_THRESHOLD
        );
        assert!(
            (effective_weight(three_fives, 0, 172, BETA) - 0.5).abs() > PREFERENCE_DELETE_THRESHOLD
        );
        // A turn stamp ahead of the counter never amplifies.
        assert!(close(effective_weight(0.9, 10, 5, BETA), 0.9));
    }

    // Python `TestPersonalFactor` (test_retrieval_foundation.py).
    #[test]
    fn the_personal_factor_is_one_at_neutral_and_mirrors_across_spaces() {
        for distance_space in [true, false] {
            assert_eq!(personal_factor(0.5, 0.3, distance_space), 1.0);
            for w in [0.0, 0.3, 1.0] {
                assert_eq!(personal_factor(w, 0.0, distance_space), 1.0);
            }
        }
        assert!(personal_factor(0.9, 0.3, true) < 1.0);
        assert!(personal_factor(0.9, 0.3, false) > 1.0);
        assert!(close(personal_factor(1.0, 0.3, true), 0.7));
        assert!(close(personal_factor(0.0, 0.3, false), 0.7));
    }

    // Python `TestPreferenceNodeId`.
    #[test]
    fn the_node_id_is_deterministic_and_per_user_and_dataset() {
        assert_eq!(preference_node_id("u", "d"), preference_node_id("u", "d"));
        assert_ne!(preference_node_id("u", "d"), preference_node_id("u2", "d"));
        assert_ne!(preference_node_id("u", "d"), preference_node_id("u", "d2"));
    }

    async fn content_node(graph: &MockGraphDB, id: &str) {
        graph
            .add_node_raw(json!({"id": id, "type": "Entity", "name": id}))
            .await
            .unwrap();
    }

    // Python `test_node_with_no_prefers_edges_still_reads_back` and
    // `test_read_traverses_only_prefers_edges`.
    #[tokio::test]
    async fn the_state_reads_the_node_and_only_its_prefers_edges() {
        let graph = MockGraphDB::new();
        assert!(
            load_preference_state(&graph, "u", "d")
                .await
                .unwrap()
                .is_none()
        );

        upsert_preference_node(&graph, "u", "d", "Be brief", 4, "w")
            .await
            .unwrap();
        let (node, weights) = load_preference_state(&graph, "u", "d")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(node_text(&node, "text"), "Be brief");
        assert_eq!(node_turn_counter(&node), 4);
        assert!(weights.is_empty(), "belongs_to_set is not a prefers edge");

        content_node(&graph, "n1").await;
        content_node(&graph, "n2").await;
        write_prefers_edges(
            &graph,
            "u",
            "d",
            &BTreeMap::from([("n1".to_string(), 0.8)]),
            4,
        )
        .await
        .unwrap();
        let mut other = edge("n1", "n2", "related_to", []);
        other.3.insert(Cow::Borrowed("weight"), json!(0.1));
        graph.add_edges(&[other]).await.unwrap();
        let (_, weights) = load_preference_state(&graph, "u", "d")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            weights,
            HashMap::from([(
                "n1".to_string(),
                StoredPreferenceWeight {
                    weight: 0.8,
                    updated_at_turn: 4
                }
            )])
        );
    }

    // Python `test_delete_unsupported_neutralizes_edges` and
    // `test_empty_targets_touch_nothing`.
    #[tokio::test]
    async fn retiring_an_edge_rewrites_it_as_neutral() {
        let graph = MockGraphDB::new();
        upsert_preference_node(&graph, "u", "d", "", 1, "")
            .await
            .unwrap();
        content_node(&graph, "n1").await;
        write_prefers_edges(
            &graph,
            "u",
            "d",
            &BTreeMap::from([("n1".to_string(), 0.9)]),
            1,
        )
        .await
        .unwrap();
        delete_prefers_edges(&graph, "u", "d", &[]).await.unwrap();
        delete_prefers_edges(&graph, "u", "d", &["n1".to_string()])
            .await
            .unwrap();
        let (_, weights) = load_preference_state(&graph, "u", "d")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(weights["n1"].weight, NEUTRAL_WEIGHT);
        assert_eq!(weights["n1"].updated_at_turn, 0);
    }

    fn enabled() -> PreferenceConfig {
        PreferenceConfig {
            enabled: true,
            ..PreferenceConfig::default()
        }
    }

    // Python `TestLoadRawPreferences` (test_retrieval_foundation.py).
    #[tokio::test]
    async fn the_lookup_fails_open_and_decays_on_read() {
        let graph = MockGraphDB::new();
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        let (u, d) = (user.to_string(), dataset.to_string());
        upsert_preference_node(&graph, &u, &d, "Prefer tables\nBe brief", 10, "w")
            .await
            .unwrap();
        content_node(&graph, "n1").await;
        content_node(&graph, "n2").await;
        write_prefers_edges(
            &graph,
            &u,
            &d,
            &BTreeMap::from([("n1".to_string(), 0.9)]),
            0,
        )
        .await
        .unwrap();
        write_prefers_edges(
            &graph,
            &u,
            &d,
            &BTreeMap::from([("n2".to_string(), 0.2)]),
            10,
        )
        .await
        .unwrap();

        // Flag off, or no identity: empty without a read.
        let off = PreferenceConfig::default();
        assert_eq!(
            load_active_preferences(&graph, &off, Some(user), Some(dataset)).await,
            ActivePreferences::default()
        );
        assert_eq!(
            load_active_preferences(&graph, &enabled(), None, Some(dataset)).await,
            ActivePreferences::default()
        );
        assert_eq!(
            load_active_preferences(&graph, &enabled(), Some(user), Some(Uuid::new_v4())).await,
            ActivePreferences::default()
        );

        let active = load_active_preferences(&graph, &enabled(), Some(user), Some(dataset)).await;
        // n1 sat idle 10 turns; n2 was written at the current turn.
        assert!(close(
            active.weights["n1"],
            effective_weight(0.9, 0, 10, 0.02)
        ));
        assert!(active.weights["n1"] < 0.9);
        assert_eq!(active.weights["n2"], 0.2);
        assert_eq!(active.lines(), ["Prefer tables", "Be brief"]);
        assert_eq!(
            active.rendered_text(),
            format!("{PREFERENCE_RENDER_HEADER}\nPrefer tables\nBe brief")
        );
        assert_eq!(ActivePreferences::default().rendered_text(), "");
    }

    #[test]
    fn the_preference_node_is_internal() {
        let node = UserPreference::new(preference_node_id("u", "d"), "u", "d", "", 0, "");
        let value = serde_json::to_value(node).unwrap();
        assert!(cognee_models::is_internal_node(value.as_object().unwrap()));
    }
}
