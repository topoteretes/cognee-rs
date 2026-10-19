//! The two graph reads TEMPORAL search makes over `Timestamp` nodes.
//!
//! Port of the temporal half of Python cognee's
//! `cognee/infrastructure/databases/graph/graph_db_interface.py`:
//! `timestamp_overlaps`, `empty_temporal_anchors`, `temporal_anchors_from_rows`
//! and the default `get_timestamps_in_range` / `get_temporal_anchors`.
//!
//! Default cognify writes every date it extracts as a shared `Timestamp` node
//! (`cognee_models::Timestamp`) whose period is the half-open
//! `[time_at, time_until)` in ms since the epoch, UTC. Entities reach it
//! through `*_at` edges, the chunks that name it through `contains` edges, and
//! chunks `contains` the entities they mention. TEMPORAL search asks the graph
//! two questions about a window `[start, end)`:
//!
//! - which `Timestamp` nodes overlap it
//!   ([`GraphDBTrait::get_timestamps_in_range`]), and
//! - which of its candidate chunks and entities are attached to one of those
//!   ([`GraphDBTrait::get_temporal_anchors`]).
//!
//! The functions here are the trait's default implementations, kept as free
//! functions so a backend with a native override can still be checked against
//! them on the same graph (the shared adapter tests do exactly that).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use cognee_models::Timestamp;

use crate::{GraphDBResult, GraphDBTrait, NodeData};

/// The node type an anchored entity has; Python's `e.type = 'Entity'`.
pub(crate) const ENTITY_NODE_TYPE: &str = "Entity";
/// The edge a chunk carries to the entities (and timestamps) it mentions.
pub(crate) const CONTAINS_RELATIONSHIP: &str = "contains";
/// What `time_until` defaults to past `time_at` on a node written before the
/// field existed (the temporal-event pipeline's timestamps): one second.
pub const LEGACY_TIMESTAMP_SPAN_MS: i64 = 1000;

/// A `Timestamp` node inside a window: what
/// [`GraphDBTrait::get_timestamps_in_range`] returns per match.
///
/// Python returns a dict carrying at least `id`, `timestamp_str`, `time_at`
/// and `time_until`; this is that dict typed. `time_until` is already resolved
/// (a node without one gets `time_at + 1000`), so every consumer sees the same
/// period the window test used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTimestamp {
    pub id: String,
    /// The normalized time string the node was built from (`1965-03-18`), when
    /// the node carries one.
    pub timestamp_str: Option<String>,
    /// Inclusive lower bound of the node's period, ms since the epoch (UTC).
    pub time_at: i64,
    /// Exclusive upper bound of the node's period, ms since the epoch (UTC).
    pub time_until: i64,
}

impl WindowTimestamp {
    /// Read a `Timestamp` node's period from its properties; `None` when
    /// `time_at` is missing or not an integer (such a node never matches a
    /// window — Python's `timestamp_overlaps` returns `False` for it).
    pub fn from_properties(id: &str, properties: &NodeData) -> Option<Self> {
        let (time_at, time_until) = timestamp_period(properties)?;
        Some(Self {
            id: id.to_string(),
            timestamp_str: properties
                .get("timestamp_str")
                .and_then(Value::as_str)
                .map(str::to_string),
            time_at,
            time_until,
        })
    }

    /// Whether this period meets the half-open window `[start, end)`.
    pub fn overlaps(&self, start: Option<i64>, end: Option<i64>) -> bool {
        cognee_models::timestamp_overlaps(self.time_at, Some(self.time_until), start, end)
    }
}

/// Which candidates are attached to a `Timestamp` inside a window: what
/// [`GraphDBTrait::get_temporal_anchors`] returns.
///
/// Port of the dict `temporal_anchors_from_rows` builds
/// (`{"timestamp_ids", "chunk_ids", "entity_ids", "chunk_timestamps"}`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemporalAnchors {
    /// The in-window timestamps any candidate reached.
    pub timestamp_ids: HashSet<String>,
    /// The anchored candidate chunks.
    pub chunk_ids: HashSet<String>,
    /// The anchored entities: candidates with a direct edge into the window,
    /// plus the entities through which a candidate chunk reached it.
    pub entity_ids: HashSet<String>,
    /// Per anchored chunk, the in-window timestamp ids it reached, directly or
    /// through an entity it `contains`. The retriever ranks anchored chunks by
    /// how tightly the best of these fits the window.
    pub chunk_timestamps: HashMap<String, HashSet<String>>,
}

impl TemporalAnchors {
    /// Assemble the anchors from the two row shapes every implementation
    /// produces: `(candidate_id, timestamp_id)` for an edge straight into an
    /// in-window timestamp, and `(chunk_id, entity_id, timestamp_id)` for a
    /// candidate chunk that `contains` an entity with such an edge.
    ///
    /// A directly anchored candidate is a chunk when the caller passed it as
    /// one (`chunk_ids`), not by its node type. Port of Python's
    /// `temporal_anchors_from_rows`.
    pub fn from_rows<D, V>(direct_rows: D, via_rows: V, chunk_ids: &HashSet<String>) -> Self
    where
        D: IntoIterator<Item = (String, String)>,
        V: IntoIterator<Item = (String, String, String)>,
    {
        let mut anchors = Self::default();
        for (candidate_id, timestamp_id) in direct_rows {
            anchors.timestamp_ids.insert(timestamp_id.clone());
            if chunk_ids.contains(&candidate_id) {
                anchors
                    .chunk_timestamps
                    .entry(candidate_id.clone())
                    .or_default()
                    .insert(timestamp_id);
                anchors.chunk_ids.insert(candidate_id);
            } else {
                anchors.entity_ids.insert(candidate_id);
            }
        }
        for (chunk_id, entity_id, timestamp_id) in via_rows {
            anchors.timestamp_ids.insert(timestamp_id.clone());
            anchors
                .chunk_timestamps
                .entry(chunk_id.clone())
                .or_default()
                .insert(timestamp_id);
            anchors.chunk_ids.insert(chunk_id);
            anchors.entity_ids.insert(entity_id);
        }
        anchors
    }

    /// Whether no candidate reached an in-window timestamp.
    pub fn is_empty(&self) -> bool {
        self.timestamp_ids.is_empty()
    }
}

/// A JSON integer as Python's `int(value)` reads one: an integral number, a
/// float (truncated toward zero), or a string of digits. Anything else is
/// `None`.
fn json_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| {
            number
                .as_f64()
                .filter(|float| float.is_finite())
                .map(|float| float.trunc() as i64)
        }),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// A `Timestamp` node's period `(time_at, time_until)` from its properties.
///
/// `time_at` must be an integer (as Python's `int(value)` reads one); a missing, null, empty or
/// unparseable `time_until` defaults to `time_at + 1000`, as in Python's
/// `timestamp_overlaps`. (Python would raise on a non-numeric `time_until`
/// string; no writer produces one, and treating it as absent keeps one bad
/// row from failing the whole search.)
pub fn timestamp_period(properties: &NodeData) -> Option<(i64, i64)> {
    let time_at = json_int(properties.get("time_at")?)?;
    let time_until = properties
        .get("time_until")
        .and_then(json_int)
        .unwrap_or_else(|| time_at.saturating_add(LEGACY_TIMESTAMP_SPAN_MS));
    Some((time_at, time_until))
}

/// Order a window's timestamps the way the native queries do: by `time_at`,
/// then by id.
pub(crate) fn sort_window(timestamps: &mut [WindowTimestamp]) {
    timestamps.sort_by(|a, b| a.time_at.cmp(&b.time_at).then_with(|| a.id.cmp(&b.id)));
}

fn is_type(properties: Option<&NodeData>, node_type: &str) -> bool {
    properties
        .and_then(|props| props.get("type"))
        .and_then(Value::as_str)
        == Some(node_type)
}

/// The in-window period of a node, when it is a `Timestamp` overlapping
/// `[start, end)`.
fn in_window(
    node_id: &str,
    properties: Option<&NodeData>,
    start: Option<i64>,
    end: Option<i64>,
) -> bool {
    let Some(properties) = properties else {
        return false;
    };
    is_type(Some(properties), Timestamp::TYPE_NAME)
        && WindowTimestamp::from_properties(node_id, properties)
            .is_some_and(|timestamp| timestamp.overlaps(start, end))
}

/// Default [`GraphDBTrait::get_timestamps_in_range`]: scan
/// [`get_graph_data`](GraphDBTrait::get_graph_data) for `Timestamp` nodes
/// overlapping `[start, end)`.
///
/// Port of Python's default (`graph_db_interface.py`), which scans the whole
/// graph the same way; adapters with a native query override it. The result
/// is ordered by `(time_at, id)` like the native queries.
pub async fn default_timestamps_in_range<G>(
    graph: &G,
    start: Option<i64>,
    end: Option<i64>,
) -> GraphDBResult<Vec<WindowTimestamp>>
where
    G: GraphDBTrait + ?Sized,
{
    let (nodes, _edges) = graph.get_graph_data().await?;
    let mut matches: Vec<WindowTimestamp> = nodes
        .iter()
        .filter(|(_, properties)| is_type(Some(properties), Timestamp::TYPE_NAME))
        .filter_map(|(id, properties)| WindowTimestamp::from_properties(id, properties))
        .filter(|timestamp| timestamp.overlaps(start, end))
        .collect();
    sort_window(&mut matches);
    Ok(matches)
}

/// Default [`GraphDBTrait::get_temporal_anchors`]: walk
/// [`get_neighborhood`](GraphDBTrait::get_neighborhood) from the candidates
/// and filter in Rust.
///
/// Port of Python's default (`graph_db_interface.py`): one depth-1
/// neighbourhood around the candidates finds their direct edges into the
/// window and the entities the candidate chunks `contains`; a second one
/// around those entities finds the entities' edges into the window. The
/// relationship of an edge into a timestamp is not inspected.
pub async fn default_temporal_anchors<G>(
    graph: &G,
    chunk_ids: &[String],
    entity_ids: &[String],
    start: Option<i64>,
    end: Option<i64>,
) -> GraphDBResult<TemporalAnchors>
where
    G: GraphDBTrait + ?Sized,
{
    let chunk_set: HashSet<String> = chunk_ids.iter().cloned().collect();
    let entity_set: HashSet<String> = entity_ids.iter().cloned().collect();
    if chunk_set.is_empty() && entity_set.is_empty() {
        return Ok(TemporalAnchors::default());
    }

    let mut seeds: Vec<String> = chunk_set.union(&entity_set).cloned().collect();
    seeds.sort();
    let (nodes, edges) = graph.get_neighborhood(&seeds, 1).await?;
    let properties_by_id: HashMap<&str, &NodeData> = nodes
        .iter()
        .map(|(id, props)| (id.as_str(), props))
        .collect();

    let mut direct_rows: Vec<(String, String)> = Vec::new();
    // Candidate chunk -> the entities it contains.
    let mut contained: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (source, target, relationship, _) in &edges {
        let target_props = properties_by_id.get(target.as_str()).copied();
        if chunk_set.contains(source)
            && relationship == CONTAINS_RELATIONSHIP
            && is_type(target_props, ENTITY_NODE_TYPE)
        {
            contained
                .entry(source.as_str())
                .or_default()
                .insert(target.as_str());
        }
        if (chunk_set.contains(source) || entity_set.contains(source))
            && in_window(target, target_props, start, end)
        {
            direct_rows.push((source.clone(), target.clone()));
        }
    }

    let mut via_rows: Vec<(String, String, String)> = Vec::new();
    let mut reached: Vec<String> = contained
        .values()
        .flatten()
        .map(|id| (*id).to_string())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    reached.sort();
    if !reached.is_empty() {
        let reached_set: HashSet<&str> = reached.iter().map(String::as_str).collect();
        let (via_nodes, via_edges) = graph.get_neighborhood(&reached, 1).await?;
        let via_properties: HashMap<&str, &NodeData> = via_nodes
            .iter()
            .map(|(id, props)| (id.as_str(), props))
            .collect();
        for (source, target, _, _) in &via_edges {
            if !reached_set.contains(source.as_str())
                || !in_window(
                    target,
                    via_properties.get(target.as_str()).copied(),
                    start,
                    end,
                )
            {
                continue;
            }
            for (chunk_id, entities) in &contained {
                if entities.contains(source.as_str()) {
                    via_rows.push(((*chunk_id).to_string(), source.clone(), target.clone()));
                }
            }
        }
    }
    Ok(TemporalAnchors::from_rows(
        direct_rows,
        via_rows,
        &chunk_set,
    ))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::borrow::Cow;

    use serde_json::json;

    use super::*;

    fn props(pairs: &[(&'static str, Value)]) -> NodeData {
        pairs
            .iter()
            .map(|(key, value)| (Cow::Borrowed(*key), value.clone()))
            .collect()
    }

    #[test]
    fn period_defaults_a_missing_or_blank_time_until_to_one_second() {
        assert_eq!(
            timestamp_period(&props(&[("time_at", json!(5))])),
            Some((5, 1005))
        );
        assert_eq!(
            timestamp_period(&props(&[("time_at", json!(5)), ("time_until", json!(""))])),
            Some((5, 1005))
        );
        assert_eq!(
            timestamp_period(&props(&[
                ("time_at", json!(5)),
                ("time_until", Value::Null)
            ])),
            Some((5, 1005))
        );
        assert_eq!(
            timestamp_period(&props(&[
                ("time_at", json!("7")),
                ("time_until", json!(9.9))
            ])),
            Some((7, 9))
        );
        assert_eq!(timestamp_period(&props(&[("time_at", json!("x"))])), None);
        assert_eq!(timestamp_period(&props(&[])), None);
    }

    #[test]
    fn overlap_is_half_open_on_both_sides() {
        let ts = WindowTimestamp {
            id: "t".into(),
            timestamp_str: None,
            time_at: 10,
            time_until: 20,
        };
        assert!(ts.overlaps(None, None));
        assert!(ts.overlaps(Some(19), Some(30)));
        assert!(!ts.overlaps(Some(20), Some(30)), "time_until is exclusive");
        assert!(ts.overlaps(Some(0), Some(11)));
        assert!(
            !ts.overlaps(Some(0), Some(10)),
            "the window end is exclusive"
        );
    }

    #[test]
    fn anchors_from_rows_tell_chunks_from_entities_by_what_the_caller_passed() {
        let chunks: HashSet<String> = ["c1".to_string()].into();
        let anchors = TemporalAnchors::from_rows(
            [
                ("c1".to_string(), "t1".to_string()),
                ("e1".to_string(), "t2".to_string()),
            ],
            [("c2".to_string(), "e2".to_string(), "t3".to_string())],
            &chunks,
        );
        assert_eq!(anchors.timestamp_ids.len(), 3);
        assert_eq!(
            anchors.chunk_ids,
            ["c1".to_string(), "c2".to_string()].into()
        );
        assert_eq!(
            anchors.entity_ids,
            ["e1".to_string(), "e2".to_string()].into()
        );
        assert_eq!(anchors.chunk_timestamps["c1"], ["t1".to_string()].into());
        assert_eq!(anchors.chunk_timestamps["c2"], ["t3".to_string()].into());
        assert!(!anchors.is_empty());
        assert!(TemporalAnchors::default().is_empty());
    }
}
