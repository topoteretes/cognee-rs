//! Write the code graph: nodes, typed edges and the repository stamp.
//!
//! Port of Python's `add_code_graph_data_points`, `add_code_graph_edges`,
//! `receipt_projection` and `_stamp_snapshot_identity`
//! (`cognee/tasks/code_graph/extract_code_graph.py`). Writes are graph-only:
//! no embedding call, no vector index (`index_vectors` is a later opt-in).
//!
//! Nodes go through the storage walk Python's `add_data_points` does for a
//! code node: the node itself, a `part_of` edge to its repository, and for
//! each node set a `NodeSet` node with a `belongs_to_set` edge. Typed
//! relations (`calls`, `imports`, ...) are written directly afterwards, as
//! Python writes them through the graph engine.
//!
//! Writes are deltas against the graph: a fact node whose `fact_hash` is
//! unchanged is not rewritten, and an edge the graph already has is not added
//! again. Repository nodes are always rewritten. Python reads the whole graph
//! once for that; here only the snapshot's own nodes and candidate edges are
//! looked up (`get_nodes`, `has_edges`), which decides the same writes
//! without loading the rest of the graph.
//!
//! Every node and edge of the snapshot, written or left unchanged, is
//! recorded in the relational provenance ledger (`node` / `edge` tables)
//! under the owning `Data` row when a [`CodeGraphLedger`] is given, so
//! deleting that row finds them, and deleting another row that also loaded
//! them (an earlier owner of the same repository) leaves them in place. The
//! inserts are idempotent upserts. The rows are written before the graph
//! writes they describe, as cognify writes them, so a failed write can always
//! be swept.
//!
//! Not here yet (SDK-1053 O6): the unchanged-snapshot skip, the stale node
//! and edge sweep, and the node-set detag. The stamped `last_delta` reports
//! zero for what they would remove.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::Utc;
use cognee_database::provenance_ids::{provenance_edge_id, provenance_node_id, triplet_slug};
use cognee_database::{GraphEdge as LedgerEdge, GraphNode as LedgerNode, ProvenanceDb};
use cognee_graph::{EdgeData, EdgeKey, GraphDBTrait, GraphNode, NodeData};
use cognee_models::DataPoint;
use cognee_utils::id_generation::generate_edge_object_id;
use serde::Serialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::edges::{CodeGraphEdge, build_code_graph_edges};
use crate::error::CodeGraphResult;
use crate::mapping::{
    CODE_GRAPH_MAPPING_VERSION, fact_node_id, fact_repo, is_mappable_fact, resolve_fallback_repo,
};
use crate::models::{CODE_INDEX_FIELDS, CodeNode, CodeNodeType, CodeRepository, code_metadata};
use crate::snapshot::{Fact, ParsedSnapshot, Receipt, snapshot_identity};

/// `receipt.json` fields kept on the repository node. Python's
/// `_RECEIPT_PROJECTION_KEYS`: run specifics that describe the machine rather
/// than the snapshot are left out, except `generated_at`, which dates it.
pub const RECEIPT_PROJECTION_KEYS: &[&str] = &[
    "format_version",
    "enola_version",
    "extractor_version",
    "generated_at",
    "git",
    "extractors",
    "fact_count",
    "insight_count",
    "quality",
];

/// Names kept per delta sample list. Python's `_DELTA_SAMPLE_LIMIT`.
const DELTA_SAMPLE_LIMIT: usize = 20;

/// Python's `NodeSet.metadata`: node-set names are filters, not embedded.
fn node_set_metadata() -> HashMap<String, Value> {
    HashMap::from([
        ("index_fields".to_string(), json!([])),
        ("identity_fields".to_string(), json!(["name"])),
    ])
}

/// Who the ledger rows of one load are attributed to: the `Data` row the
/// code graph was loaded for, in its dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeGraphOwner {
    /// The user's tenant, if any (Python's `str(None)` otherwise).
    pub tenant_id: Option<Uuid>,
    /// The owning user.
    pub user_id: Uuid,
    /// The dataset the data item is in.
    pub dataset_id: Uuid,
    /// The data item (the repository's `Data` row).
    pub data_id: Uuid,
    /// The pipeline run writing the rows; `None` outside a pipeline run.
    pub pipeline_run_id: Option<Uuid>,
}

/// The relational provenance ledger a load records its nodes and edges in.
#[derive(Clone, Copy)]
pub struct CodeGraphLedger<'a> {
    /// The relational database.
    pub db: &'a dyn ProvenanceDb,
    /// Who the rows belong to.
    pub owner: CodeGraphOwner,
}

impl std::fmt::Debug for CodeGraphLedger<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeGraphLedger")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// What the node write changed. Python's `node_delta` dict.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NodeDelta {
    /// Fact nodes the graph did not have.
    pub nodes_added: usize,
    /// Fact nodes whose `fact_hash` changed (or that carry none).
    pub nodes_updated: usize,
    /// Fact nodes left untouched.
    pub nodes_unchanged: usize,
    /// Up to 20 added names, sorted.
    pub samples_added: Vec<String>,
    /// Up to 20 updated names, sorted.
    pub samples_updated: Vec<String>,
}

/// The node write's result, handed to [`add_code_graph_edges`]: the data
/// points (passed through) and what the node write found and added. Python's
/// `_CodeGraphLoadState`.
#[derive(Debug, Clone, Default)]
pub struct CodeGraphLoadState {
    /// The data points the load was given, unchanged. Empty when the
    /// extraction skipped the load.
    pub data_points: Vec<CodeNode>,
    /// The snapshot's nodes the graph held before the write (the scoped read;
    /// nodes outside the snapshot are not read).
    pub existing_nodes: Vec<GraphNode>,
    /// `(source, target, relationship)` of the structural edges the node
    /// write added that the graph did not hold before it. The edge write
    /// checks its candidates after the node write, so these are told apart
    /// from edges that were there before the load.
    pub added_edge_keys: HashSet<EdgeKey>,
    /// What the node write changed.
    pub node_delta: NodeDelta,
}

/// One property of a stored node: top level, or inside a `properties` key
/// that may itself be JSON text (adapter dependent). Python's
/// `_read_repo_node_property`.
pub(crate) fn node_property(node: &NodeData, key: &str) -> Option<Value> {
    if let Some(value) = node.get(key).filter(|value| !value.is_null()) {
        return Some(value.clone());
    }
    let properties = match node.get("properties")? {
        Value::String(text) => serde_json::from_str::<Value>(text).ok()?,
        other => other.clone(),
    };
    properties
        .get(key)
        .filter(|value| !value.is_null())
        .cloned()
}

fn delta_samples(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.truncate(DELTA_SAMPLE_LIMIT);
    names
}

/// Python's `_trim_preview`: whitespace collapsed, at most 80 characters.
fn trim_preview(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

/// The `edge_text` label of a node (Python's `_get_node_label`): the first
/// non-blank index field, trimmed; else the non-blank name; else the class.
fn node_label(name: &str, indexes_name: bool, class_name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        class_name.to_string()
    } else if indexes_name {
        trim_preview(name)
    } else {
        name.to_string()
    }
}

/// Edge properties of a structural edge: Python's `_create_edge_properties`
/// followed by `ensure_default_edge_properties`.
fn structural_edge(
    source: Uuid,
    target: Uuid,
    relationship_name: &str,
    labels: &HashMap<Uuid, String>,
) -> EdgeData {
    let (source_text, target_text) = (source.to_string(), target.to_string());
    let phrase = relationship_name.trim().replace('_', " ");
    let phrase = if phrase.is_empty() {
        "related to".to_string()
    } else {
        phrase
    };
    let label = |id: Uuid, text: &str| labels.get(&id).cloned().unwrap_or_else(|| text.into());
    let edge_text = format!(
        "{} {phrase} {}.",
        label(source, &source_text),
        label(target, &target_text)
    );
    let properties: HashMap<Cow<'static, str>, Value> = HashMap::from([
        (Cow::Borrowed("source_node_id"), json!(source_text)),
        (Cow::Borrowed("target_node_id"), json!(target_text)),
        (Cow::Borrowed("relationship_name"), json!(relationship_name)),
        (
            Cow::Borrowed("updated_at"),
            json!(Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        ),
        (
            Cow::Borrowed("edge_object_id"),
            json!(
                generate_edge_object_id(&source_text, &target_text, relationship_name).to_string()
            ),
        ),
        (Cow::Borrowed("feedback_weight"), json!(0.5)),
        (Cow::Borrowed("edge_text"), json!(edge_text)),
    ]);
    (
        source_text,
        target_text,
        relationship_name.to_string(),
        properties,
    )
}

/// A `NodeSet` graph node: Python's `NodeSet(id=..., name=...)`.
#[derive(Serialize)]
struct NodeSetNode {
    #[serde(flatten)]
    base: DataPoint,
    name: String,
}

/// A node the storage walk writes, with what its ledger row needs.
struct WalkNode {
    id: Uuid,
    name: String,
    class_name: String,
    indexed_fields: Value,
    properties: Value,
}

/// Python's `get_graph_from_model` over `points`: nodes (each once, first
/// occurrence wins) and structural edges (each triple once).
fn storage_walk(points: &[&CodeNode]) -> (Vec<WalkNode>, Vec<EdgeData>) {
    let mut nodes: Vec<WalkNode> = Vec::new();
    let mut seen_nodes: HashSet<Uuid> = HashSet::new();
    let mut labels: HashMap<Uuid, String> = HashMap::new();
    let mut triples: Vec<(Uuid, Uuid, &'static str)> = Vec::new();
    let mut seen_edges: HashSet<(Uuid, Uuid, &'static str)> = HashSet::new();
    let mut push_edge = |triple: (Uuid, Uuid, &'static str)| {
        if seen_edges.insert(triple) {
            triples.push(triple);
        }
    };

    for point in points {
        let id = point.id();
        let class_name = point
            .node_type()
            .map_or("CodeGraphEntity", CodeNodeType::class_name);
        labels.insert(id, node_label(point.name(), true, class_name));
        if seen_nodes.insert(id) {
            nodes.push(WalkNode {
                id,
                name: point.name().to_string(),
                class_name: class_name.to_string(),
                indexed_fields: json!(CODE_INDEX_FIELDS),
                properties: Value::Object(point.graph_properties()),
            });
        }

        // `belongs_to_set` is a DataPoint base field, so Python walks it
        // before the model's own `part_of`.
        let sets = match point {
            CodeNode::Repository(repository) => repository.base.belongs_to_set.as_ref(),
            CodeNode::Entity(entity) => entity.base.belongs_to_set.as_ref(),
        };
        for set in sets.into_iter().flatten() {
            let Value::Object(set) = set else { continue };
            let (Some(name), Some(set_id)) = (
                set.get("name").and_then(Value::as_str),
                set.get("id")
                    .and_then(Value::as_str)
                    .and_then(|text| Uuid::parse_str(text).ok()),
            ) else {
                continue;
            };
            labels.insert(set_id, node_label(name, false, "NodeSet"));
            if seen_nodes.insert(set_id) {
                let mut base = DataPoint::with_metadata("NodeSet", None, node_set_metadata());
                base.id = set_id;
                let properties = serde_json::to_value(NodeSetNode {
                    base,
                    name: name.to_string(),
                })
                .unwrap_or(Value::Null);
                nodes.push(WalkNode {
                    id: set_id,
                    name: name.to_string(),
                    class_name: "NodeSet".to_string(),
                    indexed_fields: json!([]),
                    properties,
                });
            }
            push_edge((id, set_id, "belongs_to_set"));
        }

        if let CodeNode::Entity(entity) = point
            && let Some(repository_id) = entity.part_of
        {
            push_edge((id, repository_id, "part_of"));
        }
    }

    let edges = triples
        .into_iter()
        .map(|(source, target, relationship)| {
            structural_edge(source, target, relationship, &labels)
        })
        .collect();
    (nodes, edges)
}

/// A typed relation as the graph stores it.
fn typed_edge(edge: &CodeGraphEdge) -> EdgeData {
    (
        edge.source_id.to_string(),
        edge.target_id.to_string(),
        edge.relationship_name.clone(),
        edge.properties
            .iter()
            .map(|(key, value)| (Cow::Owned(key.clone()), value.clone()))
            .collect(),
    )
}

fn ledger_node(owner: &CodeGraphOwner, node: &WalkNode) -> LedgerNode {
    LedgerNode {
        id: provenance_node_id(
            owner.tenant_id,
            owner.user_id,
            owner.dataset_id,
            owner.data_id,
            node.id,
        ),
        slug: node.id,
        user_id: owner.user_id,
        data_id: owner.data_id,
        dataset_id: owner.dataset_id,
        pipeline_run_id: owner.pipeline_run_id,
        label: Some(node.name.clone()),
        node_type: node.class_name.clone(),
        indexed_fields: node.indexed_fields.clone(),
        attributes: Some(node.properties.clone()),
        created_at: Utc::now(),
    }
}

/// The ledger row of an edge. None of the code graph's relations is a
/// `contains`, so the row's edge text is the relationship name.
fn ledger_edge(owner: &CodeGraphOwner, edge: &EdgeData) -> Option<LedgerEdge> {
    let (source, target, relationship, properties) = edge;
    let source = Uuid::parse_str(source).ok()?;
    let target = Uuid::parse_str(target).ok()?;
    Some(LedgerEdge {
        id: provenance_edge_id(
            owner.tenant_id,
            owner.user_id,
            owner.dataset_id,
            owner.data_id,
            source,
            relationship,
            target,
        ),
        slug: triplet_slug(source, relationship, target),
        user_id: owner.user_id,
        data_id: owner.data_id,
        dataset_id: owner.dataset_id,
        pipeline_run_id: owner.pipeline_run_id,
        source_node_id: source,
        destination_node_id: target,
        relationship_name: relationship.clone(),
        label: Some(relationship.clone()),
        attributes: serde_json::to_value(properties).ok(),
        created_at: Utc::now(),
    })
}

async fn record_in_ledger(
    ledger: Option<&CodeGraphLedger<'_>>,
    nodes: &[WalkNode],
    edges: &[EdgeData],
) -> CodeGraphResult<()> {
    let Some(ledger) = ledger else {
        return Ok(());
    };
    let node_rows: Vec<LedgerNode> = nodes
        .iter()
        .map(|node| ledger_node(&ledger.owner, node))
        .collect();
    let edge_rows: Vec<LedgerEdge> = edges
        .iter()
        .filter_map(|edge| ledger_edge(&ledger.owner, edge))
        .collect();
    ledger
        .db
        .upsert_provenance_graph(&node_rows, &edge_rows)
        .await?;
    Ok(())
}

/// Store code nodes as a delta against the graph. Python's
/// `add_code_graph_data_points` with `graph_only=True`.
///
/// Only the snapshot's nodes are read from the graph. A fact node is written
/// when the graph lacks it or its `fact_hash` differs (or it has none);
/// repository nodes are always written and not counted.
///
/// An empty `data_points` (a skipped load) writes and reads nothing.
///
/// # Errors
///
/// A graph read or write, or the ledger write, failed.
pub async fn add_code_graph_data_points(
    graph: &dyn GraphDBTrait,
    ledger: Option<&CodeGraphLedger<'_>>,
    data_points: Vec<CodeNode>,
) -> CodeGraphResult<CodeGraphLoadState> {
    if data_points.is_empty() {
        return Ok(CodeGraphLoadState::default());
    }

    let mut point_ids: Vec<String> = Vec::with_capacity(data_points.len());
    let mut seen_ids: HashSet<Uuid> = HashSet::with_capacity(data_points.len());
    for point in &data_points {
        if seen_ids.insert(point.id()) {
            point_ids.push(point.id().to_string());
        }
    }
    // `get_nodes` is a set lookup in storage order: key rows by their own id.
    let existing_nodes: Vec<GraphNode> = graph
        .get_nodes(&point_ids)
        .await?
        .into_iter()
        .filter_map(|node| {
            let id = node.get("id").and_then(Value::as_str)?.to_string();
            Some((id, node))
        })
        .collect();
    let existing_hashes: HashMap<&str, Option<Value>> = existing_nodes
        .iter()
        .map(|(id, properties)| (id.as_str(), node_property(properties, "fact_hash")))
        .collect();

    let mut to_write: Vec<&CodeNode> = Vec::new();
    let (mut added, mut updated, mut unchanged) = (Vec::new(), Vec::new(), 0usize);
    for point in &data_points {
        let CodeNode::Entity(entity) = point else {
            // Repository nodes carry the snapshot stamp: always rewritten,
            // never counted as a content change.
            to_write.push(point);
            continue;
        };
        let point_id = entity.base.id.to_string();
        match existing_hashes.get(point_id.as_str()) {
            None => {
                to_write.push(point);
                added.push(entity.name.clone());
            }
            Some(stored) => {
                let same = matches!(
                    (&entity.fact_hash, stored),
                    (Some(hash), Some(Value::String(stored))) if hash == stored
                );
                if same {
                    unchanged += 1;
                } else {
                    to_write.push(point);
                    updated.push(entity.name.clone());
                }
            }
        }
    }
    tracing::info!(
        "Code graph node delta: {} added, {} updated, {} unchanged.",
        added.len(),
        updated.len(),
        unchanged
    );

    // The ledger covers the whole snapshot, not only what is written: a node
    // left unchanged still belongs to this owner, and without its row a
    // delete of an earlier owner that wrote it would take it out of the graph.
    if ledger.is_some() {
        let all: Vec<&CodeNode> = data_points.iter().collect();
        let (nodes, edges) = storage_walk(&all);
        record_in_ledger(ledger, &nodes, &edges).await?;
    }
    let mut added_edge_keys: HashSet<EdgeKey> = HashSet::new();
    if !to_write.is_empty() {
        let (nodes, edges) = storage_walk(&to_write);
        if !edges.is_empty() {
            let present: HashSet<EdgeKey> = graph
                .has_edges(&edges)
                .await?
                .into_iter()
                .map(|(source, target, relationship, _)| (source, target, relationship))
                .collect();
            added_edge_keys = edges
                .iter()
                .map(|(source, target, relationship, _)| {
                    (source.clone(), target.clone(), relationship.clone())
                })
                .filter(|key| !present.contains(key))
                .collect();
        }
        graph
            .add_nodes_raw(nodes.into_iter().map(|node| node.properties).collect())
            .await?;
        if !edges.is_empty() {
            graph.add_edges(&edges).await?;
        }
    }

    Ok(CodeGraphLoadState {
        data_points,
        existing_nodes,
        added_edge_keys,
        node_delta: NodeDelta {
            nodes_added: added.len(),
            nodes_updated: updated.len(),
            nodes_unchanged: unchanged,
            samples_added: delta_samples(added),
            samples_updated: delta_samples(updated),
        },
    })
}

/// Every repository a snapshot covers. Python's `_snapshot_repos`.
fn snapshot_repos(facts: &[Fact], fallback_repo: &str) -> BTreeSet<String> {
    let mut repos = BTreeSet::from([fallback_repo.to_string()]);
    for fact in facts {
        if is_mappable_fact(fact) {
            repos.insert(fact_repo(fact, fallback_repo).to_string());
        }
    }
    repos
}

/// The `receipt.json` fields kept on the repository node
/// ([`RECEIPT_PROJECTION_KEYS`]), `None` when none is present. Python's
/// `receipt_projection`.
pub fn receipt_projection(receipt: Option<&Receipt>) -> Option<Map<String, Value>> {
    let receipt = receipt?;
    let projection: Map<String, Value> = RECEIPT_PROJECTION_KEYS
        .iter()
        .filter_map(|key| {
            receipt
                .get(*key)
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect();
    (!projection.is_empty()).then_some(projection)
}

/// Record the loaded snapshot's identity, delta, receipt and tag on every
/// repository node of the snapshot. Python's `_stamp_snapshot_identity`.
///
/// The nodes are rewritten whole (as Python's `add_nodes` does), so they
/// carry the node-set tag themselves, as names: leaving it out would erase
/// it on adapters that overwrite node properties.
async fn stamp_snapshot_identity(
    graph: &dyn GraphDBTrait,
    facts: &[Fact],
    repo_path: Option<&str>,
    snapshot_id: &str,
    delta: &Map<String, Value>,
    receipt: Option<&Receipt>,
    node_set: Option<&[String]>,
) -> CodeGraphResult<()> {
    let fallback_repo = resolve_fallback_repo(facts, repo_path);
    let node_set = node_set.filter(|names| !names.is_empty());
    let last_receipt = receipt_projection(receipt).map(Value::Object);
    let repositories: Vec<Value> = snapshot_repos(facts, &fallback_repo)
        .into_iter()
        .map(|repo| {
            let mut base = DataPoint::with_metadata(
                CodeNodeType::CodeRepository.class_name(),
                None,
                code_metadata(),
            );
            base.id = fact_node_id(&repo, "repository", &repo);
            base.belongs_to_set = node_set.map(|names| {
                names
                    .iter()
                    .map(|name| Value::from(name.as_str()))
                    .collect()
            });
            base.source_node_set = node_set.map(|names| names.join(", "));
            let path = match repo_path {
                Some(path) if !path.is_empty() && repo == fallback_repo => path.to_string(),
                _ => repo.clone(),
            };
            let repository = CodeRepository {
                base,
                name: repo,
                path,
                last_snapshot_id: Some(snapshot_id.to_string()),
                last_mapping_version: Some(CODE_GRAPH_MAPPING_VERSION),
                last_delta: Some(Value::Object(delta.clone())),
                last_receipt: last_receipt.clone(),
            };
            Value::Object(CodeNode::Repository(repository).graph_properties())
        })
        .collect();
    graph.add_nodes_raw(repositories).await?;
    Ok(())
}

/// Write the snapshot's typed relations, then stamp the repository nodes.
/// Python's `add_code_graph_edges`.
///
/// `state` is what [`add_code_graph_data_points`] returned; an empty one (a
/// skipped load) writes, sweeps and stamps nothing and returns `None`. Only
/// edges the graph did not have before the load are written: the candidates
/// are looked up (`has_edges`), minus the structural edges the node write
/// itself just added. The repository
/// stamp comes last, so a load that failed part way is never recorded as
/// done. Returns the delta stamped on the repository (`None` also when the
/// snapshot has no identity to stamp).
///
/// `repo_path` must be the value the nodes were mapped with, so edge
/// endpoints match node ids.
///
/// # Errors
///
/// A graph or ledger write failed.
pub async fn add_code_graph_edges(
    graph: &dyn GraphDBTrait,
    ledger: Option<&CodeGraphLedger<'_>>,
    state: &CodeGraphLoadState,
    snapshot: &ParsedSnapshot,
    repo_path: Option<&str>,
    node_set: Option<&[String]>,
) -> CodeGraphResult<Option<Map<String, Value>>> {
    if state.data_points.is_empty() {
        return Ok(None);
    }

    let (edges, skipped) = build_code_graph_edges(&snapshot.facts, repo_path);
    tracing::info!(
        "Resolved {} code graph edge(s), skipped {}.",
        edges.len(),
        skipped
    );

    let typed_edges: Vec<EdgeData> = edges.iter().map(typed_edge).collect();
    // Every typed relation of the snapshot is this owner's, written now or
    // already present (see the node write).
    record_in_ledger(ledger, &[], &typed_edges).await?;
    let present: HashSet<EdgeKey> = if typed_edges.is_empty() {
        HashSet::new()
    } else {
        graph
            .has_edges(&typed_edges)
            .await?
            .into_iter()
            .map(|(source, target, relationship, _)| (source, target, relationship))
            .filter(|key| !state.added_edge_keys.contains(key))
            .collect()
    };
    let new_edges: Vec<EdgeData> = typed_edges
        .into_iter()
        .filter(|(source, target, relationship, _)| {
            !present.contains(&(source.clone(), target.clone(), relationship.clone()))
        })
        .collect();
    tracing::info!(
        "Code graph edge delta: {} new, {} already present.",
        new_edges.len(),
        edges.len() - new_edges.len()
    );
    if !new_edges.is_empty() {
        graph.add_edges(&new_edges).await?;
    }

    let Some(snapshot_id) = snapshot_identity(&snapshot.snapshot_dir, snapshot.receipt.as_ref())
    else {
        return Ok(None);
    };
    let mut delta = match serde_json::to_value(&state.node_delta) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    delta.insert("edges_added".into(), json!(new_edges.len()));
    // The stale sweep and the node-set detag (SDK-1053 O6) fill these.
    delta.insert("edges_removed".into(), json!(0));
    delta.insert("nodes_removed".into(), json!(0));
    delta.insert("samples_removed".into(), json!([]));
    delta.insert("node_set_detagged".into(), json!([]));
    delta.insert("snapshot_id".into(), json!(snapshot_id));
    delta.insert(
        "loaded_at".into(),
        json!(Utc::now().format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()),
    );

    stamp_snapshot_identity(
        graph,
        &snapshot.facts,
        repo_path,
        &snapshot_id,
        &delta,
        snapshot.receipt.as_ref(),
        node_set,
    )
    .await?;
    Ok(Some(delta))
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
    fn receipt_projection_keeps_the_listed_keys_only() {
        let receipt: Receipt = serde_json::from_value(json!({
            "format_version": 1,
            "enola_version": "0.5.1",
            "repo_path": "/home/me/shop",
            "duration_ms": 12,
            "quality": {"edge_coverage": 0.9},
        }))
        .unwrap();
        assert_eq!(
            Value::Object(receipt_projection(Some(&receipt)).unwrap()),
            json!({"format_version": 1, "enola_version": "0.5.1", "quality": {"edge_coverage": 0.9}})
        );
        let unrelated: Receipt = serde_json::from_value(json!({"repo_path": "/x"})).unwrap();
        assert_eq!(receipt_projection(Some(&unrelated)), None);
        assert_eq!(receipt_projection(None), None);
    }

    #[test]
    fn node_property_reads_flat_and_nested_shapes() {
        let flat: NodeData = HashMap::from([(Cow::Borrowed("fact_hash"), json!("h"))]);
        assert_eq!(node_property(&flat, "fact_hash"), Some(json!("h")));
        let nested_text: NodeData =
            HashMap::from([(Cow::Borrowed("properties"), json!("{\"fact_hash\": \"t\"}"))]);
        assert_eq!(node_property(&nested_text, "fact_hash"), Some(json!("t")));
        let nested: NodeData =
            HashMap::from([(Cow::Borrowed("properties"), json!({"fact_hash": "o"}))]);
        assert_eq!(node_property(&nested, "fact_hash"), Some(json!("o")));
        assert_eq!(node_property(&nested, "missing"), None);
    }

    #[test]
    fn structural_edge_text_is_pythons_fallback_sentence() {
        let (source, target) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let labels = HashMap::from([
            (source, "OrderID".to_string()),
            (target, "acme/shop".to_string()),
        ]);
        let (_, _, relationship, properties) = structural_edge(source, target, "part_of", &labels);
        assert_eq!(relationship, "part_of");
        assert_eq!(properties["edge_text"], json!("OrderID part of acme/shop."));
        assert_eq!(properties["feedback_weight"], json!(0.5));
        assert_eq!(
            properties["edge_object_id"],
            json!(
                generate_edge_object_id(&source.to_string(), &target.to_string(), "part_of")
                    .to_string()
            )
        );
    }

    #[test]
    fn labels_follow_index_fields() {
        assert_eq!(node_label("  a   b ", true, "CodeSymbol"), "a b");
        assert_eq!(node_label(&"x".repeat(100), true, "CodeSymbol").len(), 80);
        assert_eq!(node_label("team  a", false, "NodeSet"), "team  a");
        assert_eq!(node_label("  ", true, "CodeSymbol"), "CodeSymbol");
    }
}
