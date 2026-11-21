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
//! A re-added repository keeps its `Data` row (the manifest's pinned id), so
//! a reload records its rows under the same owner as the load before it.
//!
//! After the typed relations, the load removes what earlier loads of the same
//! repositories derived and this snapshot no longer does (the stale sweep,
//! Python's `_sweep_stale_code_graph`), strips node-set tags the repository's
//! nodes no longer carry (Python's `_detag_stale_code_graph`), and only then
//! stamps the snapshot identity on the repository nodes, so a load that failed
//! part way is never skipped past. Python reads the whole graph for both; here
//! one scoped read is taken instead: the repositories' nodes and their
//! one-hop neighbourhood (`get_neighborhood`), which holds every code node of
//! those repositories (each has a `part_of` edge to its repository), the
//! node sets they belong to, and every edge among them. The detag takes the
//! nodes' own tags from the node write's pre-write read instead, as Python
//! does, since the write replaces the tags of the nodes it rewrites.
//!
//! The sweep does not touch the ledger: a swept node's rows stay with their
//! owner, as Python's do, and deleting that owner later deletes nothing more.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::Utc;
use cognee_database::provenance_ids::{provenance_edge_id, provenance_node_id, triplet_slug};
use cognee_database::{GraphEdge as LedgerEdge, GraphNode as LedgerNode, ProvenanceDb};
use cognee_graph::{EdgeData, EdgeKey, GraphDBTrait, GraphNode, NodeData};
use cognee_models::DataPoint;
use cognee_utils::generate_node_id;
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
use crate::snapshot::{Fact, ParsedSnapshot, Receipt};

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

/// Ids per graph delete call in the sweep and the detag. Python's
/// `_SWEEP_CHUNK_SIZE`.
const SWEEP_CHUNK_SIZE: usize = 2000;

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

/// The stamp a repository node carries: what the unchanged-snapshot skip
/// compares. Python's `_stored_repository_state` result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredRepositoryState {
    /// `last_snapshot_id`, `None` when absent or empty.
    pub snapshot_id: Option<String>,
    /// `last_mapping_version` as an integer, `None` when absent or not one.
    pub mapping_version: Option<i64>,
    /// Whether the node carries a non-empty `belongs_to_set`.
    pub tagged: bool,
}

/// The stored mapping version as an integer, whatever scalar type the
/// adapter returned. Python's `_as_mapping_version`.
fn as_mapping_version(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| {
            number
                .as_f64()
                .filter(|float| float.fract() == 0.0 && float.is_finite())
                .map(|float| float as i64)
        }),
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty() && text.chars().all(|c| c.is_ascii_digit()))
                .then(|| text.parse().ok())
                .flatten()
        }
        _ => None,
    }
}

/// Read the stamp off the repository node of `repo`. Python's
/// `_stored_repository_state`: the snapshot id lives on the repository node
/// in the graph itself, so a marker can never outlive the graph it describes.
///
/// # Errors
///
/// The graph read failed.
pub async fn stored_repository_state(
    graph: &dyn GraphDBTrait,
    repo: &str,
) -> CodeGraphResult<StoredRepositoryState> {
    let node = graph
        .get_node(&fact_node_id(repo, "repository", repo).to_string())
        .await?;
    let Some(node) = node else {
        return Ok(StoredRepositoryState::default());
    };
    let snapshot_id = match node_property(&node, "last_snapshot_id") {
        Some(Value::String(id)) if !id.is_empty() => Some(id),
        _ => None,
    };
    let mapping_version = node_property(&node, "last_mapping_version")
        .as_ref()
        .and_then(as_mapping_version);
    Ok(StoredRepositoryState {
        snapshot_id,
        mapping_version,
        tagged: !tag_values(&node).is_empty(),
    })
}

/// Record a snapshot's nodes and edges in the ledger without touching the
/// graph: what a skipped load does for its owner, so the owner claims the
/// graph it would have written (see [`load_code_graph`](crate::load_code_graph)).
///
/// # Errors
///
/// The ledger write failed.
pub(crate) async fn record_snapshot_in_ledger(
    ledger: &CodeGraphLedger<'_>,
    data_points: &[CodeNode],
    edges: &[CodeGraphEdge],
) -> CodeGraphResult<()> {
    let points: Vec<&CodeNode> = data_points.iter().collect();
    let (nodes, mut structural) = storage_walk(&points);
    structural.extend(edges.iter().map(typed_edge));
    record_in_ledger(Some(ledger), &nodes, &structural).await
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

/// The repositories a snapshot covers and the node ids it derives.
struct RepositoryScope {
    /// The repository of facts without a `repo` label.
    fallback_repo: String,
    /// Python's `_snapshot_repos`.
    repos: BTreeSet<String>,
    /// Python's `_current_code_node_ids`: every mappable fact's node and the
    /// repository nodes.
    current_ids: HashSet<String>,
}

impl RepositoryScope {
    /// The whole snapshot: every repository it covers.
    fn new(facts: &[Fact], fallback_repo: &str) -> Self {
        let repos = snapshot_repos(facts, fallback_repo);
        let mut current_ids: HashSet<String> = facts
            .iter()
            .filter(|fact| is_mappable_fact(fact))
            .filter_map(|fact| {
                let kind = fact.get("kind").and_then(Value::as_str)?;
                let name = fact.get("name").and_then(Value::as_str)?;
                Some(fact_node_id(fact_repo(fact, fallback_repo), kind, name).to_string())
            })
            .collect();
        current_ids.extend(
            repos
                .iter()
                .map(|repo| fact_node_id(repo, "repository", repo).to_string()),
        );
        Self {
            fallback_repo: fallback_repo.to_string(),
            repos,
            current_ids,
        }
    }

    /// One member of a multi-repo snapshot: the repository `scope` and the
    /// nodes of the facts labelled with it (`fallback_repo` labels the
    /// unlabelled ones). Python's `_current_code_node_ids` over the facts
    /// whose `_fact_repo` is `repo_scope`.
    fn scoped(facts: &[Fact], fallback_repo: &str, scope: &str) -> Self {
        let mut current_ids: HashSet<String> = facts
            .iter()
            .filter(|fact| is_mappable_fact(fact) && fact_repo(fact, fallback_repo) == scope)
            .filter_map(|fact| {
                let kind = fact.get("kind").and_then(Value::as_str)?;
                let name = fact.get("name").and_then(Value::as_str)?;
                Some(fact_node_id(scope, kind, name).to_string())
            })
            .collect();
        current_ids.insert(fact_node_id(scope, "repository", scope).to_string());
        Self {
            fallback_repo: scope.to_string(),
            repos: BTreeSet::from([scope.to_string()]),
            current_ids,
        }
    }

    /// Whether a stored node is a code node of one of these repositories:
    /// a code type, and its repository (`name` on a repository node, `repo`
    /// on a fact node) is covered. Python's `_is_repo_code_node`.
    fn is_repo_code_node(&self, node: &NodeData) -> bool {
        let Some(Value::String(node_type)) = node_property(node, "type") else {
            return false;
        };
        if CodeNodeType::from_class_name(&node_type).is_none() {
            return false;
        }
        let key = if node_type == CodeNodeType::CodeRepository.class_name() {
            "name"
        } else {
            "repo"
        };
        matches!(node_property(node, key), Some(Value::String(repo)) if self.repos.contains(&repo))
    }
}

/// What the graph holds around a snapshot's repositories, read after the
/// load's writes. Python's pre-write `get_graph_data()`: a node the load
/// wrote is one the snapshot derives, so it is never stale, and an edge it
/// wrote is one the snapshot expects, so reading after the writes decides the
/// same removals.
struct StoredRepositoryGraph {
    nodes: Vec<GraphNode>,
    edge_keys: HashSet<EdgeKey>,
}

/// The repositories' nodes, their one-hop neighbourhood and every edge among
/// them: every code node of the repositories (each has a `part_of` edge to
/// its repository), the node sets the repositories belong to, and the edges
/// between all of these.
async fn read_repository_graph(
    graph: &dyn GraphDBTrait,
    repos: &BTreeSet<String>,
) -> CodeGraphResult<StoredRepositoryGraph> {
    let repo_ids: Vec<String> = repos
        .iter()
        .map(|repo| fact_node_id(repo, "repository", repo).to_string())
        .collect();
    let (nodes, edges) = graph.get_neighborhood(&repo_ids, 1).await?;
    let edge_keys = edges
        .into_iter()
        .map(|(source, target, relationship, _)| (source, target, relationship))
        .collect();
    Ok(StoredRepositoryGraph { nodes, edge_keys })
}

/// The typed edges one member of a multi-repo snapshot writes: those
/// touching the member's nodes (`scope_ids`) whose other endpoint is the
/// member's own or already in the graph. An edge into a member not loaded yet
/// is written by that member's load, so members load in any order, one at a
/// time. Python's `repo_scope` filter in `add_code_graph_edges`, with the
/// other endpoints looked up (`get_nodes`) instead of read from the whole
/// graph.
///
/// # Errors
///
/// The graph read failed.
pub(crate) async fn member_edges<'e>(
    graph: &dyn GraphDBTrait,
    edges: &'e [CodeGraphEdge],
    scope_ids: &HashSet<String>,
) -> CodeGraphResult<Vec<&'e CodeGraphEdge>> {
    let candidates: Vec<(&CodeGraphEdge, String, String)> = edges
        .iter()
        .map(|edge| (edge, edge.source_id.to_string(), edge.target_id.to_string()))
        .filter(|(_, source, target)| scope_ids.contains(source) || scope_ids.contains(target))
        .collect();
    let foreign: Vec<String> = candidates
        .iter()
        .flat_map(|(_, source, target)| [source, target])
        .filter(|id| !scope_ids.contains(*id))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut known: HashSet<String> = HashSet::new();
    for chunk in foreign.chunks(SWEEP_CHUNK_SIZE) {
        known.extend(
            graph
                .get_nodes(chunk)
                .await?
                .into_iter()
                .filter_map(|node| node.get("id").and_then(Value::as_str).map(str::to_string)),
        );
    }
    let is_known = |id: &String| scope_ids.contains(id) || known.contains(id);
    Ok(candidates
        .into_iter()
        .filter(|(_, source, target)| is_known(source) && is_known(target))
        .map(|(edge, _, _)| edge)
        .collect())
}

/// What the stale sweep removed.
struct SweepResult {
    nodes_removed: usize,
    edges_removed: usize,
    samples_removed: Vec<String>,
}

/// Remove the code nodes and edges of the snapshot's repositories that the
/// snapshot no longer derives. Python's `_sweep_stale_code_graph`.
///
/// Only nodes of code types whose repository the snapshot covers are
/// considered, so other repositories and non-code nodes are untouched. Edges
/// are swept only between nodes the snapshot derives, so an edge to a
/// non-code node (`belongs_to_set` to a node set) always survives here.
async fn sweep_stale_code_graph(
    graph: &dyn GraphDBTrait,
    facts: &[Fact],
    current_edges: &[CodeGraphEdge],
    scope: &RepositoryScope,
    stored: &StoredRepositoryGraph,
) -> CodeGraphResult<SweepResult> {
    let fallback_repo = scope.fallback_repo.as_str();
    let mut stale_ids: Vec<String> = Vec::new();
    let mut stale_names: Vec<String> = Vec::new();
    for (node_id, properties) in &stored.nodes {
        if scope.current_ids.contains(node_id) || !scope.is_repo_code_node(properties) {
            continue;
        }
        stale_ids.push(node_id.clone());
        let name = match node_property(properties, "name") {
            Some(Value::String(name)) if !name.is_empty() => name,
            _ => node_id.clone(),
        };
        stale_names.push(name);
    }
    for chunk in stale_ids.chunks(SWEEP_CHUNK_SIZE) {
        graph.delete_nodes(chunk).await?;
    }
    if !stale_ids.is_empty() {
        tracing::info!("Swept {} stale code graph node(s).", stale_ids.len());
    }

    let mut expected: HashSet<(&str, &str, &str)> = HashSet::new();
    let current_edge_keys: Vec<(String, String, &str)> = current_edges
        .iter()
        .map(|edge| {
            (
                edge.source_id.to_string(),
                edge.target_id.to_string(),
                edge.relationship_name.as_str(),
            )
        })
        .collect();
    // The structural `part_of` edge of every fact node to its repository.
    let part_of_keys: Vec<(String, String)> = facts
        .iter()
        .filter(|fact| is_mappable_fact(fact))
        .filter_map(|fact| {
            let kind = fact.get("kind").and_then(Value::as_str)?;
            let name = fact.get("name").and_then(Value::as_str)?;
            let repo = fact_repo(fact, fallback_repo);
            Some((
                fact_node_id(repo, kind, name).to_string(),
                fact_node_id(repo, "repository", repo).to_string(),
            ))
        })
        .collect();
    for (source, target, relationship) in &current_edge_keys {
        expected.insert((source, target, relationship));
    }
    for (source, target) in &part_of_keys {
        expected.insert((source, target, "part_of"));
    }

    let mut stale_edges: Vec<EdgeKey> = stored
        .edge_keys
        .iter()
        .filter(|(source, target, relationship)| {
            scope.current_ids.contains(source)
                && scope.current_ids.contains(target)
                && !expected.contains(&(source.as_str(), target.as_str(), relationship.as_str()))
        })
        .cloned()
        .collect();
    stale_edges.sort();
    for chunk in stale_edges.chunks(SWEEP_CHUNK_SIZE) {
        graph.delete_edge_triples(chunk).await?;
    }
    if !stale_edges.is_empty() {
        tracing::info!("Swept {} stale code graph edge(s).", stale_edges.len());
    }

    Ok(SweepResult {
        nodes_removed: stale_ids.len(),
        edges_removed: stale_edges.len(),
        samples_removed: delta_samples(stale_names),
    })
}

/// Python's `NodeSet.id_for(name)`: `generate_node_id("NodeSet:<name>")`.
fn node_set_id(name: &str) -> String {
    generate_node_id(&format!("NodeSet:{name}")).to_string()
}

/// The node-set names a stored node's `belongs_to_set` property lists. A
/// JSON-text value (adapter dependent) is read as the list it encodes.
fn tag_names(node: &NodeData) -> Vec<String> {
    tag_values(node)
        .into_iter()
        .filter_map(|tag| match tag {
            Value::String(name) => Some(name),
            _ => None,
        })
        .collect()
}

/// The entries of a stored node's `belongs_to_set` property, whatever their
/// shape; JSON text is read as the list it encodes.
fn tag_values(node: &NodeData) -> Vec<Value> {
    let tags = match node_property(node, "belongs_to_set") {
        Some(Value::String(text)) => serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
        Some(other) => other,
        None => Value::Null,
    };
    match tags {
        Value::Array(tags) => tags,
        _ => Vec::new(),
    }
}

/// Strip the node-set tags the snapshot's code nodes carry but should not.
/// Python's `_detag_stale_code_graph`; returns the removed names, sorted.
///
/// Stale tags are read off the nodes themselves (their `belongs_to_set`
/// edges and property), so a tag a crashed run left behind is found too. An
/// edge is stale by node-set id, the property by exact name: a respelling
/// (`Team-A` to `team-a`) keeps the shared edge but drops the old spelling.
///
/// The code nodes and their property tags come from `existing_nodes`, the
/// snapshot's nodes as the graph held them before this load's node write
/// (Python reads its pre-write `existing_nodes` here): the write replaces the
/// property of a rewritten node on most backends, so reading it afterwards
/// would miss an old spelling. Edges and node-set names come from the
/// post-write read: the write adds edges, never removes them, and only
/// rewrites node sets this call still carries.
async fn detag_stale_code_graph(
    graph: &dyn GraphDBTrait,
    scope: &RepositoryScope,
    existing_nodes: &[GraphNode],
    stored: &StoredRepositoryGraph,
    node_set: Option<&[String]>,
) -> CodeGraphResult<Vec<String>> {
    let code_node_ids: Vec<&str> = existing_nodes
        .iter()
        .filter(|(node_id, properties)| {
            scope.current_ids.contains(node_id) && scope.is_repo_code_node(properties)
        })
        .map(|(node_id, _)| node_id.as_str())
        .collect();
    if code_node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let code_node_id_set: HashSet<&str> = code_node_ids.iter().copied().collect();

    let node_set_names: HashMap<&str, String> = stored
        .nodes
        .iter()
        .filter_map(|(node_id, properties)| {
            match (
                node_property(properties, "type"),
                node_property(properties, "name"),
            ) {
                (Some(Value::String(node_type)), Some(Value::String(name)))
                    if node_type == "NodeSet" =>
                {
                    Some((node_id.as_str(), name))
                }
                _ => None,
            }
        })
        .collect();
    let property_names: BTreeSet<String> = existing_nodes
        .iter()
        .filter(|(node_id, _)| code_node_id_set.contains(node_id.as_str()))
        .flat_map(|(_, properties)| tag_names(properties))
        .collect();
    let edge_names: BTreeSet<String> = stored
        .edge_keys
        .iter()
        .filter(|(source, _, relationship)| {
            relationship == "belongs_to_set" && code_node_id_set.contains(source.as_str())
        })
        .filter_map(|(_, target, _)| node_set_names.get(target.as_str()).cloned())
        .collect();

    let current: BTreeSet<&str> = node_set
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect();
    let current_set_ids: HashSet<String> = current.iter().map(|name| node_set_id(name)).collect();
    let stale_edge_names: Vec<&String> = edge_names
        .union(&property_names)
        .filter(|name| !current_set_ids.contains(&node_set_id(name)))
        .collect();
    let stale_property_names: Vec<&String> = property_names
        .iter()
        .filter(|name| !current.contains(name.as_str()))
        .collect();
    if stale_edge_names.is_empty() && stale_property_names.is_empty() {
        return Ok(Vec::new());
    }

    let stale_edges: Vec<EdgeKey> = code_node_ids
        .iter()
        .flat_map(|node_id| {
            stale_edge_names.iter().map(move |name| {
                (
                    node_id.to_string(),
                    node_set_id(name),
                    "belongs_to_set".to_string(),
                )
            })
        })
        .collect();
    for chunk in stale_edges.chunks(SWEEP_CHUNK_SIZE) {
        graph.delete_edge_triples(chunk).await?;
    }
    if !stale_property_names.is_empty() {
        // Re-read: the load may have rewritten these nodes since the read
        // above, and the property must keep the tags it carries now.
        let stale: HashSet<&str> = stale_property_names
            .iter()
            .map(|name| name.as_str())
            .collect();
        let ids: Vec<String> = code_node_ids.iter().map(|id| id.to_string()).collect();
        for chunk in ids.chunks(SWEEP_CHUNK_SIZE) {
            for node in graph.get_nodes(chunk).await? {
                let Some(Value::String(node_id)) = node.get("id").cloned() else {
                    continue;
                };
                let tags = tag_names(&node);
                if !tags.iter().any(|tag| stale.contains(tag.as_str())) {
                    continue;
                }
                let kept: Vec<Value> = tags
                    .into_iter()
                    .filter(|tag| !stale.contains(tag.as_str()))
                    .map(Value::from)
                    .collect();
                graph
                    .update_node_property(&node_id, "belongs_to_set", Value::Array(kept))
                    .await?;
            }
        }
    }

    let removed: Vec<String> = stale_edge_names
        .into_iter()
        .chain(stale_property_names)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    tracing::info!(
        "Removed stale node_set tag(s) {:?} from {} code graph node(s).",
        removed,
        code_node_ids.len()
    );
    Ok(removed)
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
/// With `repo_scope` (one member of a multi-repo snapshot) only that member
/// is stamped: the other members have not loaded yet, and a stamp would make
/// their load skip.
///
/// The nodes are rewritten whole (as Python's `add_nodes` does), so they
/// carry the node-set tag themselves, as names: leaving it out would erase
/// it on adapters that overwrite node properties.
#[allow(clippy::too_many_arguments)]
async fn stamp_snapshot_identity(
    graph: &dyn GraphDBTrait,
    facts: &[Fact],
    repo_path: Option<&str>,
    snapshot_id: &str,
    delta: &Map<String, Value>,
    receipt: Option<&Receipt>,
    repo_scope: Option<&str>,
    node_set: Option<&[String]>,
) -> CodeGraphResult<()> {
    // Python's `repo_scope or ...`: an empty scope counts as none.
    let repo_scope = repo_scope.filter(|scope| !scope.is_empty());
    let fallback_repo = match repo_scope {
        Some(scope) => scope.to_string(),
        None => resolve_fallback_repo(facts, repo_path),
    };
    let repos = match repo_scope {
        Some(scope) => BTreeSet::from([scope.to_string()]),
        None => snapshot_repos(facts, &fallback_repo),
    };
    let node_set = node_set.filter(|names| !names.is_empty());
    let last_receipt = receipt_projection(receipt).map(Value::Object);
    let repositories: Vec<Value> = repos
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

/// Write the snapshot's typed relations, sweep and detag what it no longer
/// derives, then stamp the repository nodes. Python's `add_code_graph_edges`.
///
/// `state` is what [`add_code_graph_data_points`] returned; an empty one (a
/// skipped load) writes, sweeps and stamps nothing and returns `None`. Only
/// edges the graph did not have before the load are written: the candidates
/// are looked up (`has_edges`), minus the structural edges the node write
/// itself just added. Then the snapshot's repositories are read (see the
/// module docs), their code nodes and edges the snapshot no longer derives
/// are deleted (Python's `_sweep_stale_code_graph`), and node-set tags their
/// nodes carry but `node_set` does not are removed (Python's
/// `_detag_stale_code_graph`). The repository stamp comes last, so a load
/// that failed part way is never recorded as done. Returns the delta stamped
/// on the repository (`None` also when the snapshot has no identity to
/// stamp).
///
/// `repo_path` must be the value the nodes were mapped with, so edge
/// endpoints match node ids. Edges already on the snapshot
/// ([`ParsedSnapshot::edges`]) are used as they are.
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
    write_code_graph_edges(graph, ledger, state, snapshot, repo_path, None, node_set).await
}

/// [`add_code_graph_edges`], optionally for one member of a multi-repo
/// snapshot. Python's `add_code_graph_edges(..., repo_scope=...)`.
///
/// With `repo_scope` only the member's edges are written and recorded
/// ([`member_edges`]), only the member's stale tags are stripped (the other
/// members carry their own rows' tags), and only the member is stamped. The
/// stale sweep still covers the whole snapshot, as Python's does: every
/// member's current nodes and edges are in it, so nothing another member
/// still derives is removed.
pub(crate) async fn write_code_graph_edges(
    graph: &dyn GraphDBTrait,
    ledger: Option<&CodeGraphLedger<'_>>,
    state: &CodeGraphLoadState,
    snapshot: &ParsedSnapshot,
    repo_path: Option<&str>,
    repo_scope: Option<&str>,
    node_set: Option<&[String]>,
) -> CodeGraphResult<Option<Map<String, Value>>> {
    if state.data_points.is_empty() {
        return Ok(None);
    }

    // Resolved ahead by `extract_code_graph` (off the runtime), else here.
    let resolved;
    let edges: &[CodeGraphEdge] = match snapshot.edges.as_deref() {
        Some(edges) => edges,
        None => {
            let (edges, skipped) = build_code_graph_edges(&snapshot.facts, repo_path);
            tracing::info!(
                "Resolved {} code graph edge(s), skipped {}.",
                edges.len(),
                skipped
            );
            resolved = edges;
            &resolved
        }
    };

    let fallback_repo = resolve_fallback_repo(&snapshot.facts, repo_path);
    let member = repo_scope
        .filter(|scope| !scope.is_empty())
        .map(|scope| RepositoryScope::scoped(&snapshot.facts, &fallback_repo, scope));
    let writable: Vec<&CodeGraphEdge> = match &member {
        Some(member) => member_edges(graph, edges, &member.current_ids).await?,
        None => edges.iter().collect(),
    };
    let typed_edges: Vec<EdgeData> = writable.into_iter().map(typed_edge).collect();
    // Every typed relation of the snapshot (of the member, for a member load)
    // is this owner's, written now or already present (see the node write).
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

    // Insert-then-sweep: with the snapshot fully merged, remove what earlier
    // loads derived that this snapshot no longer does. Runs even with zero
    // edges: a shrunken repository still needs its sweep.
    let scope = RepositoryScope::new(&snapshot.facts, &fallback_repo);
    let stored = read_repository_graph(graph, &scope.repos).await?;
    let swept = sweep_stale_code_graph(graph, &snapshot.facts, edges, &scope, &stored).await?;
    // Retag: strip node-set names the repository's nodes carried before but
    // not any more. No graph write when nothing is stale, the untagged case
    // included. A member load only looks at the member's own nodes.
    let detagged = detag_stale_code_graph(
        graph,
        member.as_ref().unwrap_or(&scope),
        &state.existing_nodes,
        &stored,
        node_set,
    )
    .await?;

    let Some(snapshot_id) = snapshot.identity() else {
        return Ok(None);
    };
    let mut delta = match serde_json::to_value(&state.node_delta) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    delta.insert("edges_added".into(), json!(new_edges.len()));
    delta.insert("edges_removed".into(), json!(swept.edges_removed));
    delta.insert("nodes_removed".into(), json!(swept.nodes_removed));
    delta.insert("samples_removed".into(), json!(swept.samples_removed));
    delta.insert("node_set_detagged".into(), json!(detagged));
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
        member.as_ref().map(|member| member.fallback_repo.as_str()),
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
    use cognee_graph::MockGraphDB;

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

    const REPO: &str = "demo_repo";
    const SNAPSHOT_ID: &str = "sha256:feedface";

    /// Python's `FACTS` in `test_incremental_code_graph.py`.
    fn write_snapshot(dir: &std::path::Path) -> ParsedSnapshot {
        let facts = [
            json!({"kind": "symbol", "name": "app/db.Database", "file": "app/db.py", "repo": REPO}),
            json!({
                "kind": "symbol",
                "name": "app/api.handler",
                "file": "app/api.py",
                "repo": REPO,
                "relations": [{"kind": "calls", "target": "app/db.Database"}],
            }),
        ];
        let lines: Vec<String> = facts.iter().map(Value::to_string).collect();
        std::fs::write(dir.join("facts.jsonl"), lines.join("\n") + "\n").unwrap();
        std::fs::write(
            dir.join("receipt.json"),
            json!({"snapshot_id": SNAPSHOT_ID}).to_string(),
        )
        .unwrap();
        ParsedSnapshot::read(dir).unwrap()
    }

    fn node_id(kind: &str, name: &str) -> String {
        fact_node_id(REPO, kind, name).to_string()
    }

    async fn seed_node(graph: &MockGraphDB, id: &str, properties: Value) {
        let mut node = properties.as_object().unwrap().clone();
        node.insert("id".into(), json!(id));
        graph.add_node_raw(Value::Object(node)).await.unwrap();
    }

    /// Python's `test_sweep_removes_stale_nodes_and_edges_and_stamps_snapshot`.
    /// The stale symbol has its `part_of` edge, which every code node the
    /// writer stores has: the scoped read reaches a repository's code nodes
    /// through it (see the module docs).
    #[tokio::test]
    async fn the_sweep_removes_stale_nodes_and_edges_and_stamps_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = write_snapshot(dir.path());
        let repo_path = format!("/repos/{REPO}");
        let repo_id = node_id("repository", REPO);
        let database = node_id("symbol", "app/db.Database");
        let handler = node_id("symbol", "app/api.handler");
        let stale = node_id("symbol", "app/legacy.OldHelper");
        let elsewhere = fact_node_id("other_repo", "symbol", "elsewhere").to_string();
        let node_set = node_set_id("code");

        let graph = MockGraphDB::new();
        seed_node(
            &graph,
            &repo_id,
            json!({"type": "CodeRepository", "name": REPO}),
        )
        .await;
        for id in [&database, &handler, &stale] {
            seed_node(&graph, id, json!({"type": "CodeSymbol", "repo": REPO})).await;
        }
        seed_node(
            &graph,
            &elsewhere,
            json!({"type": "CodeSymbol", "repo": "other_repo"}),
        )
        .await;
        seed_node(
            &graph,
            &node_set,
            json!({"type": "NodeSet", "name": "code"}),
        )
        .await;
        let edge = |source: &str, target: &str, relationship: &str| -> EdgeData {
            (
                source.to_string(),
                target.to_string(),
                relationship.to_string(),
                HashMap::new(),
            )
        };
        graph
            .add_edges(&[
                edge(&database, &repo_id, "part_of"),
                edge(&handler, &repo_id, "part_of"),
                edge(&stale, &repo_id, "part_of"),
                // Another repository's node next to this one: out of scope.
                edge(&elsewhere, &repo_id, "depends_on"),
                edge(&handler, &database, "calls"),
                // The dependency was removed from the code: stale.
                edge(&database, &handler, "calls"),
                // An edge to a non-code node survives.
                edge(&handler, &node_set, "belongs_to_set"),
                edge(&repo_id, &node_set, "belongs_to_set"),
            ])
            .await
            .unwrap();

        let tag = vec!["code".to_string()];
        let state = CodeGraphLoadState {
            data_points: crate::map_facts_to_data_points(
                &snapshot.facts,
                Some(&repo_path),
                None,
                Some(&tag),
            ),
            ..CodeGraphLoadState::default()
        };
        let delta = add_code_graph_edges(
            &graph,
            None,
            &state,
            &snapshot,
            Some(&repo_path),
            Some(&tag),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(!graph.has_node(&stale).await.unwrap());
        for id in [&repo_id, &database, &handler, &elsewhere, &node_set] {
            assert!(graph.has_node(id).await.unwrap(), "{id} survives");
        }
        assert!(!graph.has_edge(&database, &handler, "calls").await.unwrap());
        assert!(graph.has_edge(&handler, &database, "calls").await.unwrap());
        assert!(
            graph
                .has_edge(&handler, &node_set, "belongs_to_set")
                .await
                .unwrap()
        );
        assert_eq!(delta["nodes_removed"], json!(1));
        assert_eq!(delta["edges_removed"], json!(1));
        assert_eq!(delta["samples_removed"], json!([stale]));
        assert_eq!(delta["node_set_detagged"], json!([]));
        assert_eq!(delta["edges_added"], json!(0));

        let stamped = graph.get_node(&repo_id).await.unwrap().unwrap();
        assert_eq!(stamped["last_snapshot_id"], json!(SNAPSHOT_ID));
        assert_eq!(stamped["last_delta"], Value::Object(delta));
    }

    /// Python's `test_delta_write_adds_only_missing_edges_and_stamps_delta`.
    #[tokio::test]
    async fn present_edges_are_not_rewritten_and_nothing_is_swept() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = write_snapshot(dir.path());
        let repo_path = format!("/repos/{REPO}");
        let repo_id = node_id("repository", REPO);
        let database = node_id("symbol", "app/db.Database");
        let handler = node_id("symbol", "app/api.handler");
        let graph = MockGraphDB::new();
        seed_node(
            &graph,
            &repo_id,
            json!({"type": "CodeRepository", "name": REPO}),
        )
        .await;
        for id in [&database, &handler] {
            seed_node(&graph, id, json!({"type": "CodeSymbol", "repo": REPO})).await;
        }
        for (source, target, relationship) in [
            (&database, &repo_id, "part_of"),
            (&handler, &repo_id, "part_of"),
            (&handler, &database, "calls"),
        ] {
            graph
                .add_edge(source, target, relationship, None)
                .await
                .unwrap();
        }

        let state = CodeGraphLoadState {
            data_points: crate::map_facts_to_data_points(
                &snapshot.facts,
                Some(&repo_path),
                None,
                None,
            ),
            ..CodeGraphLoadState::default()
        };
        let delta = add_code_graph_edges(&graph, None, &state, &snapshot, Some(&repo_path), None)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(graph.edge_count(), 3);
        let calls = graph.get_call_log();
        assert!(
            !calls.iter().any(|call| call == "delete_nodes"),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call == "delete_edge_triples"),
            "{calls:?}"
        );
        assert_eq!(delta["edges_added"], json!(0));
        assert_eq!(delta["edges_removed"], json!(0));
        assert_eq!(delta["nodes_removed"], json!(0));
        assert_eq!(delta["snapshot_id"], json!(SNAPSHOT_ID));
    }

    /// Python's `test_skipped_snapshot_short_circuits_downstream_tasks`.
    #[tokio::test]
    async fn an_empty_load_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = write_snapshot(dir.path());
        let graph = MockGraphDB::new();
        let state = add_code_graph_data_points(&graph, None, Vec::new())
            .await
            .unwrap();
        assert!(state.data_points.is_empty());
        let delta = add_code_graph_edges(&graph, None, &state, &snapshot, None, None)
            .await
            .unwrap();
        assert_eq!(delta, None);
        assert!(graph.get_call_log().is_empty());
        assert_eq!(graph.node_count(), 0);
    }

    /// Python's `test_extract_skips_when_stored_snapshot_id_matches` and
    /// `test_extract_reads_marker_from_json_properties_blob`: the stamp is
    /// read flat or from a JSON `properties` blob, and a missing node has
    /// none.
    #[tokio::test]
    async fn the_stored_stamp_is_read_in_every_shape() {
        let graph = MockGraphDB::new();
        assert_eq!(
            stored_repository_state(&graph, REPO).await.unwrap(),
            StoredRepositoryState::default()
        );

        let repo_id = node_id("repository", REPO);
        seed_node(
            &graph,
            &repo_id,
            json!({"last_snapshot_id": SNAPSHOT_ID, "last_mapping_version": CODE_GRAPH_MAPPING_VERSION}),
        )
        .await;
        assert_eq!(
            stored_repository_state(&graph, REPO).await.unwrap(),
            StoredRepositoryState {
                snapshot_id: Some(SNAPSHOT_ID.to_string()),
                mapping_version: Some(CODE_GRAPH_MAPPING_VERSION),
                tagged: false,
            }
        );

        let blob = json!({
            "last_snapshot_id": SNAPSHOT_ID,
            "last_mapping_version": "1",
            "belongs_to_set": "[\"team-a\"]",
        })
        .to_string();
        seed_node(&graph, &repo_id, json!({"properties": blob})).await;
        assert_eq!(
            stored_repository_state(&graph, REPO).await.unwrap(),
            StoredRepositoryState {
                snapshot_id: Some(SNAPSHOT_ID.to_string()),
                mapping_version: Some(1),
                tagged: true,
            }
        );

        seed_node(
            &graph,
            &repo_id,
            json!({"last_snapshot_id": "", "last_mapping_version": true, "belongs_to_set": []}),
        )
        .await;
        assert_eq!(
            stored_repository_state(&graph, REPO).await.unwrap(),
            StoredRepositoryState::default()
        );
    }

    #[test]
    fn mapping_versions_are_read_from_any_integer_scalar() {
        assert_eq!(as_mapping_version(&json!(1)), Some(1));
        assert_eq!(as_mapping_version(&json!(2.0)), Some(2));
        assert_eq!(as_mapping_version(&json!(2.5)), None);
        assert_eq!(as_mapping_version(&json!(" 3 ")), Some(3));
        assert_eq!(as_mapping_version(&json!("-3")), None);
        assert_eq!(as_mapping_version(&json!("")), None);
        assert_eq!(as_mapping_version(&json!(true)), None);
        assert_eq!(as_mapping_version(&Value::Null), None);
    }

    #[test]
    fn node_set_ids_are_pythons() {
        let entry = crate::map_facts_to_data_points(
            &[
                serde_json::from_value(json!({"kind": "symbol", "name": "a", "repo": "r"}))
                    .unwrap(),
            ],
            None,
            None,
            Some(&["Team-A".to_string()]),
        );
        let CodeNode::Repository(repository) = &entry[0] else {
            panic!("repositories come first");
        };
        let sets = repository.base.belongs_to_set.as_ref().unwrap();
        assert_eq!(sets[0]["id"], json!(node_set_id("Team-A")));
    }

    #[test]
    fn labels_follow_index_fields() {
        assert_eq!(node_label("  a   b ", true, "CodeSymbol"), "a b");
        assert_eq!(node_label(&"x".repeat(100), true, "CodeSymbol").len(), 80);
        assert_eq!(node_label("team  a", false, "NodeSet"), "team  a");
        assert_eq!(node_label("  ", true, "CodeSymbol"), "CodeSymbol");
    }
}
