//! Bounded subgraph selection for graph visualization.
//!
//! Port of Python's `cognee/modules/visualization/subgraph_data.py`.
//!
//! A visualization renders a bounded subgraph by default instead of the whole
//! graph. Seeds are resolved by priority — explicit node ids, a recall/search
//! result's graph provenance, or a query string — and, when none of those are
//! given, the highest-degree nodes, so a bare call still shows a
//! representative view. The seed neighbourhood is read through
//! [`GraphDBTrait::get_bounded_neighborhood`]; adapters with a native
//! implementation stop at `max_nodes` in the store. Set
//! [`VisualizationBounds::full`] to read the entire graph (legacy behaviour).
//!
//! Query seeding needs a vector engine and an embedder, which this crate does
//! not depend on: the caller passes a [`SeedSearcher`] (the HTTP server
//! implements it over the dataset's vector store).

use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use cognee_graph::{EdgeData, GraphDBTrait, GraphNode, NodeData, hop_distances, unique_node_ids};
use serde_json::Value;
use thiserror::Error;
use tracing::{info, warn};

use crate::error::VisualizationError;
use crate::preprocessor::{SEMANTIC_TYPE_KEY, py_str};

/// Default k-hop expansion depth around the seeds (Python
/// `DEFAULT_NEIGHBORHOOD_DEPTH`).
pub const DEFAULT_NEIGHBORHOOD_DEPTH: usize = 2;
/// Default maximum number of seed nodes (Python `DEFAULT_SEED_TOP_K`).
pub const DEFAULT_SEED_TOP_K: usize = 10;
/// Default hard cap on rendered nodes (Python `DEFAULT_MAX_NODES`).
pub const DEFAULT_MAX_NODES: usize = 500;
/// Hits read per collection when a query is turned into seeds (Python
/// `DEFAULT_WIDE_SEARCH_TOP_K`).
pub const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;

/// Largest `neighborhood_depth` the HTTP routes accept (`le=10`).
pub const MAX_NEIGHBORHOOD_DEPTH: usize = 10;
/// Largest `neighborhood_seed_top_k` the HTTP routes accept (`le=100`).
pub const MAX_SEED_TOP_K: usize = 100;
/// Largest `max_nodes` for a non-streamed read: the HTML route and the JSON
/// route without streaming (Python `JSON_MAX_NODES`).
pub const JSON_MAX_NODES: usize = 5_000;
/// Largest `max_nodes` for a streamed read (Python `STREAM_MAX_NODES`).
pub const STREAM_MAX_NODES: usize = 20_000;

/// Node vector collections searched to turn a query into seed nodes — the same
/// node-bearing collections search uses (Python `_SEED_VECTOR_COLLECTIONS`).
/// Edge collections are excluded: their hits are not graph nodes and cannot
/// seed a neighbourhood.
pub const SEED_VECTOR_COLLECTIONS: [&str; 4] = [
    "Entity_name",
    "TextSummary_text",
    "EntityType_name",
    "DocumentChunk_text",
];

/// The node type whose semantic type [`resolve_entity_types`] looks up.
pub const ENTITY_NODE_TYPE: &str = "Entity";

/// A bounds value out of range.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BoundsError {
    /// `field` is below its minimum (Python `"<field> must be >= <min>"`).
    #[error("{field} must be >= {min}")]
    BelowMinimum {
        /// The parameter name as the caller knows it.
        field: &'static str,
        /// The smallest accepted value.
        min: usize,
    },
    /// `field` is above its maximum (Python's `le=` query validation).
    #[error("{field} must be <= {max}")]
    AboveMaximum {
        /// The parameter name as the caller knows it.
        field: &'static str,
        /// The largest accepted value.
        max: usize,
    },
    /// A full-graph read was asked to stream; only bounded reads stream.
    #[error("a full-graph read cannot be streamed")]
    FullStream,
}

/// What to show: the whole graph, or a bounded neighbourhood and how to seed
/// it. Port of the keyword arguments of Python's
/// `fetch_visualization_graph_data`.
///
/// [`Default`] is Python's default: bounded, seeded by degree, depth 2, 10
/// seeds, 500 nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualizationBounds {
    /// Read the entire graph (`get_graph_data`), ignoring every other field.
    pub full: bool,
    /// A query whose nearest vector hits seed the subgraph (needs a
    /// [`SeedSearcher`]).
    pub query: Option<String>,
    /// Explicit seed node ids; win over everything else.
    pub seed_node_ids: Vec<String>,
    /// Node ids from a recall/search result's graph provenance (see
    /// [`resolve_seeds_from_recall`]); win over the query.
    pub recall_node_ids: Vec<String>,
    /// k-hop expansion depth around the seeds.
    pub depth: usize,
    /// Maximum number of seed nodes.
    pub seed_top_k: usize,
    /// Hard cap on nodes after expansion.
    pub max_nodes: usize,
}

impl Default for VisualizationBounds {
    fn default() -> Self {
        Self {
            full: false,
            query: None,
            seed_node_ids: Vec::new(),
            recall_node_ids: Vec::new(),
            depth: DEFAULT_NEIGHBORHOOD_DEPTH,
            seed_top_k: DEFAULT_SEED_TOP_K,
            max_nodes: DEFAULT_MAX_NODES,
        }
    }
}

impl VisualizationBounds {
    /// The whole graph (`full = true`): what `render` / `visualize` read.
    pub fn full_graph() -> Self {
        Self {
            full: true,
            ..Self::default()
        }
    }

    /// The checks `fetch_visualization_graph_data` itself makes: depth,
    /// seed_top_k and max_nodes must all be at least 1 (checked even when
    /// `full` is set, as in Python).
    pub fn validate(&self) -> Result<(), BoundsError> {
        for (field, value) in [
            ("neighborhood_depth", self.depth),
            ("seed_top_k", self.seed_top_k),
            ("max_nodes", self.max_nodes),
        ] {
            if value < 1 {
                return Err(BoundsError::BelowMinimum { field, min: 1 });
            }
        }
        Ok(())
    }

    /// The HTTP routes' query validation: `neighborhood_depth` in
    /// `1..=10`, `neighborhood_seed_top_k` in `1..=100` and `max_nodes` in
    /// `1..=max_nodes_cap` — [`JSON_MAX_NODES`] for the HTML route and a
    /// non-streamed JSON read, [`STREAM_MAX_NODES`] for a streamed one.
    pub fn validate_request(&self, max_nodes_cap: usize) -> Result<(), BoundsError> {
        for (field, value, max) in [
            ("neighborhood_depth", self.depth, MAX_NEIGHBORHOOD_DEPTH),
            ("neighborhood_seed_top_k", self.seed_top_k, MAX_SEED_TOP_K),
            ("max_nodes", self.max_nodes, max_nodes_cap),
        ] {
            if value < 1 {
                return Err(BoundsError::BelowMinimum { field, min: 1 });
            }
            if value > max {
                return Err(BoundsError::AboveMaximum { field, max });
            }
        }
        Ok(())
    }
}

/// Error a [`SeedSearcher`] reports; surfaced as
/// [`VisualizationError::SeedSearch`].
pub type SeedSearchError = Box<dyn std::error::Error + Send + Sync>;

/// Turns a query into seed node ids by vector search (Python
/// `resolve_seeds_from_query` over `NodeEdgeVectorSearch`).
///
/// Implemented outside this crate, so the visualization crate does not depend
/// on a vector engine or an embedder.
#[async_trait]
pub trait SeedSearcher: Send + Sync {
    /// Node ids for `query`, nearest first and de-duplicated, searched across
    /// `collections` (always [`SEED_VECTOR_COLLECTIONS`]) with
    /// `wide_search_top_k` hits per collection. A node found in several
    /// collections ranks by its best score — [`rank_seed_hits`] does exactly
    /// that from raw `(id, distance)` hits. A collection that does not exist
    /// contributes nothing; no hit at all is `Ok(vec![])`.
    async fn search_seed_node_ids(
        &self,
        query: &str,
        collections: &[&str],
        wide_search_top_k: usize,
    ) -> Result<Vec<String>, SeedSearchError>;
}

/// Unique node ids from raw vector hits, nearest first (Python
/// `NodeEdgeVectorSearch.extract_relevant_node_ids`).
///
/// Scores are distances (lower is closer); a missing score ranks last. A node
/// hit in several collections ranks by its best score; ties keep first-seen
/// order. Empty ids are skipped.
pub fn rank_seed_hits<I>(hits: I) -> Vec<String>
where
    I: IntoIterator<Item = (String, Option<f64>)>,
{
    let mut order: Vec<String> = Vec::new();
    let mut best: HashMap<String, f64> = HashMap::new();
    for (id, score) in hits {
        if id.is_empty() {
            continue;
        }
        let score = score.unwrap_or(f64::INFINITY);
        match best.get_mut(&id) {
            Some(current) => {
                if score < *current {
                    *current = score;
                }
            }
            None => {
                best.insert(id.clone(), score);
                order.push(id);
            }
        }
    }
    let mut ranked: Vec<(String, f64)> = order
        .into_iter()
        .map(|id| {
            let score = best.get(&id).copied().unwrap_or(f64::INFINITY);
            (id, score)
        })
        .collect();
    // Stable, like Python's `sorted`.
    ranked.sort_by(|a, b| a.1.total_cmp(&b.1));
    ranked.into_iter().map(|(id, _)| id).collect()
}

/// Where the seeds of a bounded read came from (Python's `source` string).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeedSource {
    /// [`VisualizationBounds::seed_node_ids`].
    Explicit,
    /// [`VisualizationBounds::recall_node_ids`].
    Recall,
    /// The [`SeedSearcher`]'s hits for [`VisualizationBounds::query`].
    Query,
    /// The highest-degree nodes.
    Degree,
    /// Nothing resolved (an empty graph).
    None,
}

impl SeedSource {
    /// The wire string: `explicit`, `recall`, `query`, `degree` or `none`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Recall => "recall",
            Self::Query => "query",
            Self::Degree => "degree",
            Self::None => "none",
        }
    }
}

impl fmt::Display for SeedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Node ids from a value that is either a `node_ids` list or a provenance
/// mapping (`{"node_ids": [...]}`); anything else yields nothing (Python
/// `_coerce_node_ids`).
fn coerce_node_ids(value: Option<&Value>) -> Vec<String> {
    let value = match value {
        Some(Value::Object(map)) => map.get("node_ids"),
        other => other,
    };
    match value {
        Some(Value::Array(items)) => items.iter().map(py_str).collect(),
        _ => Vec::new(),
    }
}

/// Seed node ids from a recall/search result's graph provenance, as JSON
/// (Python `resolve_seeds_from_recall`).
///
/// Handles a mapping with a `node_ids` list (a citation/provenance payload),
/// a mapping or a list of QA entries carrying `used_graph_element_ids`. Any
/// other shape yields `[]` rather than failing. De-duplicated, first-seen
/// order. The result goes into [`VisualizationBounds::recall_node_ids`].
pub fn resolve_seeds_from_recall(recall_result: &Value) -> Vec<String> {
    let seeds = match recall_result {
        Value::Null => Vec::new(),
        Value::Object(map) => {
            let from_node_ids = coerce_node_ids(map.get("node_ids"));
            if from_node_ids.is_empty() {
                coerce_node_ids(map.get("used_graph_element_ids"))
            } else {
                from_node_ids
            }
        }
        Value::Array(items) => items
            .iter()
            .flat_map(|item| match item {
                Value::Object(map) => coerce_node_ids(map.get("used_graph_element_ids")),
                _ => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    };
    unique_node_ids(&seeds)
}

/// Resolve seeds by priority: explicit ids > recall ids > query hits >
/// highest degree (Python `resolve_seed_node_ids`).
///
/// Explicit and recall ids are de-duplicated and cut to `seed_top_k`. A query
/// is only searched when a `seed_searcher` is given; without one (or with no
/// hits) the degree seeds are used. An empty graph yields
/// `([], SeedSource::None)`.
pub async fn resolve_seed_node_ids(
    graph_db: &dyn GraphDBTrait,
    bounds: &VisualizationBounds,
    seed_searcher: Option<&dyn SeedSearcher>,
) -> Result<(Vec<String>, SeedSource), VisualizationError> {
    let top_k = bounds.seed_top_k;
    if !bounds.seed_node_ids.is_empty() {
        let mut seeds = unique_node_ids(&bounds.seed_node_ids);
        seeds.truncate(top_k);
        return Ok((seeds, SeedSource::Explicit));
    }

    let mut recall = unique_node_ids(&bounds.recall_node_ids);
    if !recall.is_empty() {
        recall.truncate(top_k);
        return Ok((recall, SeedSource::Recall));
    }

    if let Some(query) = bounds.query.as_deref().filter(|query| !query.is_empty()) {
        match seed_searcher {
            Some(searcher) => {
                let hits = searcher
                    .search_seed_node_ids(
                        query,
                        &SEED_VECTOR_COLLECTIONS,
                        DEFAULT_WIDE_SEARCH_TOP_K,
                    )
                    .await
                    .map_err(VisualizationError::SeedSearch)?;
                let mut seeds = unique_node_ids(&hits);
                seeds.truncate(top_k);
                if !seeds.is_empty() {
                    return Ok((seeds, SeedSource::Query));
                }
            }
            None => warn!(
                "Subgraph visualization: a query was given but no seed searcher; \
                 seeding by degree instead."
            ),
        }
    }

    let degree = graph_db.get_top_degree_node_ids(top_k).await?;
    if degree.is_empty() {
        Ok((Vec::new(), SeedSource::None))
    } else {
        Ok((degree, SeedSource::Degree))
    }
}

/// Cap the subgraph at `max_nodes`, keeping the seeds and their nearest nodes
/// (Python `truncate_subgraph`).
///
/// Nodes rank by hop distance from the seeds (seeds first, unreachable last),
/// ties by input order; edges are kept only when both endpoints survive. The
/// flag says whether anything was cut. `max_nodes == 0` keeps everything.
pub fn truncate_subgraph(
    nodes: Vec<GraphNode>,
    edges: Vec<EdgeData>,
    seed_ids: &[String],
    max_nodes: usize,
) -> ((Vec<GraphNode>, Vec<EdgeData>), bool) {
    if max_nodes == 0 || nodes.len() <= max_nodes {
        return ((nodes, edges), false);
    }
    const UNREACHABLE: usize = 10_000;
    let distance = hop_distances(&edges, seed_ids);
    let mut ranked: Vec<(usize, usize, GraphNode)> = nodes
        .into_iter()
        .enumerate()
        .map(|(rank, node)| {
            let hops = distance.get(&node.0).copied().unwrap_or(UNREACHABLE);
            (hops, rank, node)
        })
        .collect();
    ranked.sort_by_key(|a| (a.0, a.1));
    ranked.truncate(max_nodes);
    let kept: Vec<GraphNode> = ranked.into_iter().map(|(_, _, node)| node).collect();
    let kept_ids: std::collections::HashSet<&str> = kept.iter().map(|n| n.0.as_str()).collect();
    let kept_edges: Vec<EdgeData> = edges
        .into_iter()
        .filter(|e| kept_ids.contains(e.0.as_str()) && kept_ids.contains(e.1.as_str()))
        .collect();
    ((kept, kept_edges), true)
}

fn is_entity(data: &NodeData) -> bool {
    data.get("type").and_then(Value::as_str) == Some(ENTITY_NODE_TYPE)
}

/// The ids of the `Entity` nodes among `nodes`.
pub(crate) fn entity_ids<'a>(
    nodes: impl IntoIterator<Item = (&'a str, &'a NodeData)>,
) -> Vec<String> {
    nodes
        .into_iter()
        .filter(|(_, data)| is_entity(data))
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Look up the EntityType names of the given entity ids; `None` on failure.
pub(crate) async fn lookup_entity_types(
    graph_db: &dyn GraphDBTrait,
    entity_ids: &[String],
) -> Option<HashMap<String, String>> {
    if entity_ids.is_empty() {
        return Some(HashMap::new());
    }
    match graph_db.get_entity_type_names(entity_ids).await {
        Ok(types) => Some(types),
        Err(error) => {
            warn!(
                count = entity_ids.len(),
                %error,
                "Entity type lookup failed; they keep the types the read itself shows."
            );
            None
        }
    }
}

/// The EntityType name of each `Entity` in `nodes`, read from the store
/// (Python `resolve_entity_types`, SDK-794).
///
/// A bounded read returns only the edges between its members, so an entity
/// whose EntityType node was not admitted has no way to tell its type; this
/// asks the store for each entity's `is_a` target. Only `Entity` nodes are
/// looked up (no entity: `Some(empty)` without a store call). A failed lookup
/// is logged and yields `None`, so the read still returns.
pub async fn resolve_entity_types(
    graph_db: &dyn GraphDBTrait,
    nodes: &[GraphNode],
) -> Option<HashMap<String, String>> {
    let ids = entity_ids(nodes.iter().map(|(id, data)| (id.as_str(), data)));
    lookup_entity_types(graph_db, &ids).await
}

/// Set [`SEMANTIC_TYPE_KEY`] on one node when `entity_types` resolves it.
pub(crate) fn set_semantic_type(
    id: &str,
    data: &mut NodeData,
    entity_types: &HashMap<String, String>,
) {
    if let Some(name) = entity_types.get(id) {
        data.insert(
            std::borrow::Cow::Borrowed(SEMANTIC_TYPE_KEY),
            Value::String(name.clone()),
        );
    }
}

/// Set [`SEMANTIC_TYPE_KEY`] (`_semantic_type`) on each node `entity_types`
/// resolves (Python `with_entity_types`). The preprocessor turns it into the
/// public `entity_type` field.
pub fn apply_entity_types(nodes: &mut [GraphNode], entity_types: &HashMap<String, String>) {
    for (id, data) in nodes {
        set_semantic_type(id, data, entity_types);
    }
}

/// How a bounded read was seeded and capped (what Python logs, and the
/// streamed read's `meta` minus `chunk_size`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubgraphMeta {
    /// The resolved seeds, in priority order.
    pub seeds: Vec<String>,
    /// Where they came from.
    pub seed_source: SeedSource,
    /// The expansion depth used.
    pub depth: usize,
    /// The node cap used.
    pub max_nodes: usize,
}

/// The result of [`fetch_visualization_graph_data`].
#[derive(Debug, Clone, PartialEq)]
pub struct VisualizationGraph {
    /// Nodes in `get_graph_data` shape (bounded reads: membership order, seeds
    /// first, entities carrying `_semantic_type` where the store knows it).
    pub nodes: Vec<GraphNode>,
    /// Edges in `get_graph_data` shape.
    pub edges: Vec<EdgeData>,
    /// How the read was bounded; `None` for a full read.
    pub meta: Option<SubgraphMeta>,
}

/// Graph data for a visualization: a bounded subgraph by default, or the
/// whole graph when [`VisualizationBounds::full`] is set (Python
/// `fetch_visualization_graph_data`).
///
/// Validates `bounds` ([`VisualizationBounds::validate`]) first. A full read
/// is `get_graph_data()` unchanged. Otherwise the seeds are resolved
/// ([`resolve_seed_node_ids`]); none yields an empty graph. The seeds'
/// neighbourhood is read with `get_bounded_neighborhood(seeds, depth,
/// max_nodes)` and each Entity is typed from the store
/// ([`resolve_entity_types`]).
pub async fn fetch_visualization_graph_data(
    graph_db: &dyn GraphDBTrait,
    bounds: &VisualizationBounds,
    seed_searcher: Option<&dyn SeedSearcher>,
) -> Result<VisualizationGraph, VisualizationError> {
    bounds.validate()?;
    if bounds.full {
        let (nodes, edges) = graph_db.get_graph_data().await?;
        return Ok(VisualizationGraph {
            nodes,
            edges,
            meta: None,
        });
    }

    let (seeds, seed_source) = resolve_seed_node_ids(graph_db, bounds, seed_searcher).await?;
    let meta = SubgraphMeta {
        seeds,
        seed_source,
        depth: bounds.depth,
        max_nodes: bounds.max_nodes,
    };
    if meta.seeds.is_empty() {
        info!("Subgraph visualization: no seeds resolved; rendering empty graph.");
        return Ok(VisualizationGraph {
            nodes: Vec::new(),
            edges: Vec::new(),
            meta: Some(meta),
        });
    }

    let neighborhood = graph_db
        .get_bounded_neighborhood(&meta.seeds, meta.depth, meta.max_nodes, None)
        .await?;
    let (mut nodes, edges) = neighborhood.into_graph_data();
    if let Some(types) = resolve_entity_types(graph_db, &nodes).await {
        apply_entity_types(&mut nodes, &types);
    }
    info!(
        seeds = meta.seeds.len(),
        source = %meta.seed_source,
        depth = meta.depth,
        max_nodes = meta.max_nodes,
        nodes = nodes.len(),
        edges = edges.len(),
        "Subgraph visualization"
    );
    Ok(VisualizationGraph {
        nodes,
        edges,
        meta: Some(meta),
    })
}
