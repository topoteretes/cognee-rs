//! The graph reads a bounded graph view makes: degree seeds, a bounded seed
//! neighbourhood, and the EntityType of each entity.
//!
//! Port of Python cognee's
//! `cognee/infrastructure/databases/graph/bounded_neighborhood.py` plus the
//! default `get_top_degree_node_ids`, `iter_bounded_neighborhood` and
//! `get_entity_type_names` of `graph_db_interface.py` (SDK-610, SDK-786,
//! SDK-794).
//!
//! The functions named `default_*` are the trait's default implementations,
//! kept as free functions so a backend with a native override can be checked
//! against them on the same graph (the shared adapter tests do exactly that).
//!
//! The membership rule all implementations share is documented on
//! [`BoundedNeighborhood`].

use std::collections::{HashMap, HashSet, VecDeque};

use crate::traits::{EdgeKey, GraphDBTrait};
use crate::{EdgeData, GraphDBError, GraphDBResult, GraphNode, NodeData};

/// Default nodes per chunk for [`BoundedNeighborhood::into_chunks`] (Python's
/// `DEFAULT_NEIGHBORHOOD_CHUNK_SIZE`).
pub const DEFAULT_NEIGHBORHOOD_CHUNK_SIZE: usize = 2000;

/// How many stored edges a native [`GraphDBTrait::get_top_degree_node_ids`]
/// samples to rank degree (Python's `EDGE_SAMPLE_ROWS` / `_SEED_SAMPLE_ROWS`).
/// Below this many edges the native ranking is exact and equals the default.
pub const DEGREE_SEED_EDGE_SAMPLE_ROWS: usize = 200_000;

/// The relationship from an entity to its EntityType.
pub const IS_A_RELATIONSHIP: &str = "is_a";

/// The node type an `is_a` edge must point at to name an entity's type.
pub const ENTITY_TYPE_NODE_TYPE: &str = "EntityType";

/// One member of a [`BoundedNeighborhood`].
#[derive(Debug, Clone, PartialEq)]
pub struct BoundedNode {
    /// The node id (also under `data["id"]`).
    pub id: String,
    /// Hops from the nearest seed: `0` for a seed.
    pub hops: usize,
    /// The node's properties, projected when the read asked for
    /// `property_keys`.
    pub data: NodeData,
}

/// The result of [`GraphDBTrait::get_bounded_neighborhood`].
///
/// # Membership rule
///
/// Python leaves "which nodes of the last admitted hop are kept" to the
/// adapter. Here every implementation shares one deterministic rule, so a
/// native override returns exactly what the default does:
///
/// 1. The seeds that exist, in the given order (duplicates collapsed), take
///    the first slots, up to `max_nodes`.
/// 2. Each hop, every frontier node lists its neighbours (either edge
///    direction) that are not yet members, sorted by id in byte order. The
///    budget is shared **round robin** over the frontier, in frontier order:
///    every frontier node's first such neighbour, then every second one, and
///    so on, so one hub cannot take all of it. A neighbour reached from two
///    frontier nodes is admitted once, at its first turn.
/// 3. The admitted nodes are the next frontier, in admission order. The walk
///    stops after `depth` hops, once `max_nodes` members are held, or when a
///    hop admits nothing.
///
/// # Shape
///
/// - `nodes` are in membership order: the seeds in the given order, then
///   hop by hop (see the module docs for the rule). At most `max_nodes`.
/// - `edges` are every stored edge whose two endpoints are members, each
///   once, in their stored direction. They are ordered by the membership
///   position of their **later** endpoint (ties by `(source, target,
///   relationship)`), so [`into_chunks`](Self::into_chunks) can hand out each
///   edge in the chunk that completes it and never before both endpoints
///   (Python's `chunk_members` contract).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoundedNeighborhood {
    pub nodes: Vec<BoundedNode>,
    pub edges: Vec<EdgeData>,
}

impl BoundedNeighborhood {
    /// Number of member nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether no node was admitted (no seed exists in the graph).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Hop distance of every member, keyed by id.
    pub fn hop_distances(&self) -> HashMap<String, usize> {
        self.nodes.iter().map(|n| (n.id.clone(), n.hops)).collect()
    }

    /// The `(nodes, edges)` shape `get_graph_data` returns, in membership
    /// order.
    pub fn into_graph_data(self) -> (Vec<GraphNode>, Vec<EdgeData>) {
        let nodes = self.nodes.into_iter().map(|n| (n.id, n.data)).collect();
        (nodes, self.edges)
    }

    /// Split into chunks of at most `chunk_size` nodes (`0` is treated as
    /// `1`), Python's `iter_bounded_neighborhood` chunks: nodes in membership
    /// order, and each edge in the chunk holding its later endpoint, so no
    /// chunk refers to a node the caller has not received yet. An empty
    /// neighbourhood yields no chunk.
    pub fn into_chunks(self, chunk_size: usize) -> Vec<(Vec<GraphNode>, Vec<EdgeData>)> {
        let chunk_size = chunk_size.max(1);
        let position: HashMap<String, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect();
        let chunk_count = self.nodes.len().div_ceil(chunk_size);
        let mut chunks: Vec<(Vec<GraphNode>, Vec<EdgeData>)> =
            (0..chunk_count).map(|_| (Vec::new(), Vec::new())).collect();
        for edge in self.edges {
            let later = match (position.get(&edge.0), position.get(&edge.1)) {
                (Some(s), Some(t)) => (*s).max(*t),
                // Not an induced edge: the struct's invariant says this
                // cannot happen, and dropping is Python's behaviour anyway.
                _ => continue,
            };
            chunks[later / chunk_size].1.push(edge);
        }
        for (index, node) in self.nodes.into_iter().enumerate() {
            chunks[index / chunk_size].0.push((node.id, node.data));
        }
        chunks
    }

    /// Assemble the result from a membership decided by [`BoundedBfs`], the
    /// node rows read for it and any superset of the induced edges.
    ///
    /// Members whose row is missing (deleted since membership was decided)
    /// are dropped together with their edges, as Python's per-chunk read
    /// does. Properties are projected with [`project_node_properties`].
    pub(crate) fn assemble(
        members: &[(String, usize)],
        rows: impl IntoIterator<Item = NodeData>,
        edges: impl IntoIterator<Item = EdgeData>,
        property_keys: Option<&[String]>,
    ) -> Self {
        let mut by_id: HashMap<String, NodeData> = HashMap::new();
        for row in rows {
            if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
                by_id.entry(id.to_string()).or_insert(row);
            }
        }
        let mut nodes = Vec::with_capacity(members.len());
        for (id, hops) in members {
            if let Some(data) = by_id.remove(id) {
                nodes.push(BoundedNode {
                    id: id.clone(),
                    hops: *hops,
                    data: project_node_properties(data, property_keys),
                });
            }
        }
        let position: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let mut seen: HashSet<EdgeKey> = HashSet::new();
        let mut kept: Vec<(usize, EdgeData)> = Vec::new();
        for edge in edges {
            let (Some(s), Some(t)) = (position.get(edge.0.as_str()), position.get(edge.1.as_str()))
            else {
                continue;
            };
            let later = (*s).max(*t);
            if seen.insert((edge.0.clone(), edge.1.clone(), edge.2.clone())) {
                kept.push((later, edge));
            }
        }
        kept.sort_by(|(pa, a), (pb, b)| {
            pa.cmp(pb)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        Self {
            nodes,
            edges: kept.into_iter().map(|(_, e)| e).collect(),
        }
    }
}

/// `data` cut down to `id`, `name`, `type` and `property_keys` (Python's
/// `project_properties`, which keeps `name` and `type`; the id lives in the
/// row here). `None` keeps everything; keys the node lacks are not invented.
pub fn project_node_properties(mut data: NodeData, property_keys: Option<&[String]>) -> NodeData {
    if let Some(keys) = property_keys {
        data.retain(|k, _| {
            matches!(k.as_ref(), "id" | "name" | "type") || keys.iter().any(|x| x == k)
        });
    }
    data
}

/// Reject meaningless bounds before an adapter reads anything (Python's
/// `validate_bounded_neighborhood_args`; chunking is not part of the read
/// here, so there is no chunk size to check).
pub fn validate_bounded_neighborhood_args(depth: usize, max_nodes: usize) -> GraphDBResult<()> {
    if depth < 1 {
        return Err(GraphDBError::InvalidArgument(
            "depth must be >= 1".to_string(),
        ));
    }
    if max_nodes < 1 {
        return Err(GraphDBError::InvalidArgument(
            "max_nodes must be >= 1".to_string(),
        ));
    }
    Ok(())
}

/// Reject `top_k == 0` (Python's `top_k must be >= 1`).
pub fn validate_top_k(top_k: usize) -> GraphDBResult<()> {
    if top_k < 1 {
        return Err(GraphDBError::InvalidArgument(
            "top_k must be >= 1".to_string(),
        ));
    }
    Ok(())
}

/// Ids de-duplicated in first-seen order (Python's `unique_node_ids`).
pub fn unique_node_ids(node_ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    node_ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect()
}

/// Undirected breadth-first hop distance from the seeds over `edges`
/// (Python's `hop_distances`). Nodes unreachable from every seed are absent.
pub fn hop_distances(edges: &[EdgeData], seed_ids: &[String]) -> HashMap<String, usize> {
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for (source, target, _, _) in edges {
        adjacency.entry(source).or_default().push(target);
        adjacency.entry(target).or_default().push(source);
    }
    let mut distance: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for seed in seed_ids {
        if !distance.contains_key(seed) {
            distance.insert(seed.clone(), 0);
            queue.push_back(seed);
        }
    }
    while let Some(node) = queue.pop_front() {
        let hops = distance.get(node).copied().unwrap_or(0);
        for neighbor in adjacency.get(node).map(Vec::as_slice).unwrap_or(&[]) {
            if !distance.contains_key(*neighbor) {
                distance.insert((*neighbor).to_string(), hops + 1);
                queue.push_back(neighbor);
            }
        }
    }
    distance
}

/// The membership walk every implementation of
/// [`GraphDBTrait::get_bounded_neighborhood`] shares; see the module docs for
/// the rule. The caller supplies each hop's neighbour lists, however its store
/// reads them.
pub(crate) struct BoundedBfs {
    members: Vec<(String, usize)>,
    reached: HashSet<String>,
    frontier: Vec<String>,
    max_nodes: usize,
    hop: usize,
}

impl BoundedBfs {
    /// Start from the seeds that exist, in seed order (already de-duplicated
    /// and filtered by the caller); at most `max_nodes` of them.
    pub(crate) fn new(existing_seeds: Vec<String>, max_nodes: usize) -> Self {
        let mut reached = HashSet::new();
        let mut members = Vec::new();
        for seed in existing_seeds {
            if members.len() >= max_nodes {
                break;
            }
            if reached.insert(seed.clone()) {
                members.push((seed, 0));
            }
        }
        let frontier = members.iter().map(|(id, _)| id.clone()).collect();
        Self {
            members,
            reached,
            frontier,
            max_nodes,
            hop: 0,
        }
    }

    /// The nodes whose neighbours the next hop needs.
    pub(crate) fn frontier(&self) -> &[String] {
        &self.frontier
    }

    /// Slots left under `max_nodes`.
    pub(crate) fn remaining(&self) -> usize {
        self.max_nodes.saturating_sub(self.members.len())
    }

    /// Whether another hop could admit anything.
    pub(crate) fn is_done(&self) -> bool {
        self.frontier.is_empty() || self.remaining() == 0
    }

    /// Admit one hop. `neighbors` maps a frontier id to its neighbours, in
    /// any order and possibly with duplicates, members or the node itself;
    /// a frontier id with no entry has no neighbours. Per frontier node, the
    /// first `max_nodes` neighbours by id are always enough (at most
    /// `max_nodes - remaining` of them can already be members), which is how
    /// a native read can cap a hub's fan-out without changing the result.
    pub(crate) fn advance(&mut self, neighbors: &HashMap<String, Vec<String>>) {
        let remaining = self.remaining();
        self.hop += 1;
        let lists: Vec<Vec<&str>> = self
            .frontier
            .iter()
            .map(|f| {
                let mut list: Vec<&str> = neighbors
                    .get(f)
                    .map(|n| {
                        n.iter()
                            .map(String::as_str)
                            .filter(|id| *id != f.as_str() && !self.reached.contains(*id))
                            .collect()
                    })
                    .unwrap_or_default();
                list.sort_unstable();
                list.dedup();
                list
            })
            .collect();
        let longest = lists.iter().map(Vec::len).max().unwrap_or(0);
        let mut admitted: Vec<String> = Vec::new();
        let mut taken: HashSet<&str> = HashSet::new();
        'rounds: for rank in 0..longest {
            for list in &lists {
                if admitted.len() >= remaining {
                    break 'rounds;
                }
                if let Some(candidate) = list.get(rank)
                    && taken.insert(candidate)
                {
                    admitted.push((*candidate).to_string());
                }
            }
        }
        for id in &admitted {
            self.reached.insert(id.clone());
            self.members.push((id.clone(), self.hop));
        }
        self.frontier = admitted;
    }

    /// The members, in membership order, with their hop distance.
    pub(crate) fn into_members(self) -> Vec<(String, usize)> {
        self.members
    }
}

/// The trait's default [`GraphDBTrait::get_top_degree_node_ids`]: rank every
/// node by degree over a full [`get_graph_data`](GraphDBTrait::get_graph_data)
/// read, highest first, ties by id (byte order); isolated nodes rank last
/// with degree 0. A self-loop counts twice, as both endpoints. O(graph) in
/// memory — Python's inherited default, with its warning.
pub async fn default_top_degree_node_ids<G: GraphDBTrait + ?Sized>(
    db: &G,
    top_k: usize,
) -> GraphDBResult<Vec<String>> {
    validate_top_k(top_k)?;
    let (nodes, edges) = db.get_graph_data().await?;
    let mut degree: HashMap<&str, usize> = nodes.iter().map(|(id, _)| (id.as_str(), 0)).collect();
    for (source, target, _, _) in &edges {
        for endpoint in [source.as_str(), target.as_str()] {
            if let Some(count) = degree.get_mut(endpoint) {
                *count += 1;
            }
        }
    }
    let mut ranked: Vec<(&str, usize)> = degree.into_iter().collect();
    ranked.sort_unstable_by(|(a_id, a), (b_id, b)| b.cmp(a).then_with(|| a_id.cmp(b_id)));
    Ok(ranked
        .into_iter()
        .take(top_k)
        .map(|(id, _)| id.to_string())
        .collect())
}

/// The trait's default [`GraphDBTrait::get_bounded_neighborhood`]: one
/// [`get_neighborhood`](GraphDBTrait::get_neighborhood) read of the whole
/// `depth`-hop neighbourhood, then the shared membership walk over it in
/// memory. Honours the whole contract except the bound on the read itself
/// (Python's default does the same).
pub async fn default_bounded_neighborhood<G: GraphDBTrait + ?Sized>(
    db: &G,
    node_ids: &[String],
    depth: usize,
    max_nodes: usize,
    property_keys: Option<&[String]>,
) -> GraphDBResult<BoundedNeighborhood> {
    validate_bounded_neighborhood_args(depth, max_nodes)?;
    let seeds = unique_node_ids(node_ids);
    if seeds.is_empty() {
        return Ok(BoundedNeighborhood::default());
    }
    let (nodes, edges) = db.get_neighborhood(&seeds, depth).await?;
    let present: HashSet<&str> = nodes.iter().map(|(id, _)| id.as_str()).collect();
    let existing: Vec<String> = seeds
        .iter()
        .filter(|id| present.contains(id.as_str()))
        .cloned()
        .collect();

    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for (source, target, _, _) in &edges {
        if !present.contains(source.as_str()) || !present.contains(target.as_str()) {
            continue;
        }
        adjacency
            .entry(source.clone())
            .or_default()
            .push(target.clone());
        adjacency
            .entry(target.clone())
            .or_default()
            .push(source.clone());
    }

    let mut bfs = BoundedBfs::new(existing, max_nodes);
    for _ in 0..depth {
        if bfs.is_done() {
            break;
        }
        bfs.advance(&adjacency);
    }
    let members = bfs.into_members();
    Ok(BoundedNeighborhood::assemble(
        &members,
        nodes.into_iter().map(|(_, data)| data),
        edges,
        property_keys,
    ))
}

/// Fold `(entity id, type name)` rows into the
/// [`GraphDBTrait::get_entity_type_names`] map. Empty names are skipped (an
/// unnamed type labels nothing, Python's `if row[1]`); an entity with several
/// typed `is_a` targets keeps the smallest name, so every backend answers the
/// same whatever order its rows come in.
pub(crate) fn fold_entity_type_rows(
    rows: impl IntoIterator<Item = (String, String)>,
) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    for (entity, name) in rows {
        if name.is_empty() {
            continue;
        }
        match out.get_mut(&entity) {
            Some(current) if name < *current => *current = name,
            Some(_) => {}
            None => {
                out.insert(entity, name);
            }
        }
    }
    out
}

/// The trait's default [`GraphDBTrait::get_entity_type_names`], Python's: a
/// one-hop [`get_neighborhood`](GraphDBTrait::get_neighborhood) read around
/// the entities, keeping the `is_a` edges that leave an entity for an
/// `EntityType` node. That read hydrates every neighbour and returns every
/// edge among them, so overrides should read just the `is_a` edges and the
/// target names. It goes through `get_neighborhood` rather than `get_edges`
/// because only the former reports stored direction on every backend.
pub async fn default_entity_type_names<G: GraphDBTrait + ?Sized>(
    db: &G,
    entity_ids: &[String],
) -> GraphDBResult<HashMap<String, String>> {
    let entities = unique_node_ids(entity_ids);
    if entities.is_empty() {
        return Ok(HashMap::new());
    }
    let (nodes, edges) = db.get_neighborhood(&entities, 1).await?;
    let type_names: HashMap<&str, &str> = nodes
        .iter()
        .filter(|(_, data)| {
            data.get("type").and_then(|v| v.as_str()) == Some(ENTITY_TYPE_NODE_TYPE)
        })
        .filter_map(|(id, data)| Some((id.as_str(), data.get("name")?.as_str()?)))
        .collect();
    let members: HashSet<&str> = entities.iter().map(String::as_str).collect();
    Ok(fold_entity_type_rows(edges.iter().filter_map(
        |(source, target, relationship, _)| {
            if relationship != IS_A_RELATIONSHIP || !members.contains(source.as_str()) {
                return None;
            }
            type_names
                .get(target.as_str())
                .map(|name| (source.clone(), (*name).to_string()))
        },
    )))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn adjacency(pairs: &[(&str, &str)]) -> HashMap<String, Vec<String>> {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for (a, b) in pairs {
            out.entry(a.to_string()).or_default().push(b.to_string());
            out.entry(b.to_string()).or_default().push(a.to_string());
        }
        out
    }

    #[test]
    fn round_robin_shares_the_budget_over_the_frontier() {
        // Hub `h` has many neighbours, `s` has two; with budget 4 each seed
        // gets its first neighbour, then its second.
        let adj = adjacency(&[
            ("h", "a1"),
            ("h", "a2"),
            ("h", "a3"),
            ("h", "a4"),
            ("s", "b1"),
            ("s", "b2"),
        ]);
        let mut bfs = BoundedBfs::new(ids(&["h", "s"]), 6);
        bfs.advance(&adj);
        let members = bfs.into_members();
        assert_eq!(
            members,
            vec![
                ("h".to_string(), 0),
                ("s".to_string(), 0),
                ("a1".to_string(), 1),
                ("b1".to_string(), 1),
                ("a2".to_string(), 1),
                ("b2".to_string(), 1),
            ]
        );
    }

    #[test]
    fn a_shared_neighbour_is_admitted_once_and_members_are_skipped() {
        let adj = adjacency(&[("s1", "x"), ("s2", "x"), ("s1", "s2"), ("s2", "y")]);
        let mut bfs = BoundedBfs::new(ids(&["s1", "s2", "s1"]), 10);
        bfs.advance(&adj);
        let members: Vec<String> = bfs.into_members().into_iter().map(|(i, _)| i).collect();
        assert_eq!(members, ids(&["s1", "s2", "x", "y"]));
    }

    #[test]
    fn seeds_beyond_the_budget_take_no_slot() {
        let bfs = BoundedBfs::new(ids(&["a", "b", "c"]), 2);
        assert!(bfs.is_done());
        assert_eq!(bfs.into_members().len(), 2);
    }

    #[test]
    fn chunks_place_each_edge_with_its_later_endpoint() {
        let node = |id: &str| {
            let mut d = NodeData::new();
            d.insert(Cow::Borrowed("id"), serde_json::json!(id));
            d
        };
        let edge = |a: &str, b: &str| {
            (
                a.to_string(),
                b.to_string(),
                "r".to_string(),
                HashMap::new(),
            )
        };
        let members = vec![
            ("a".to_string(), 0),
            ("b".to_string(), 1),
            ("c".to_string(), 1),
            ("d".to_string(), 2),
        ];
        let hood = BoundedNeighborhood::assemble(
            &members,
            ["d", "c", "b", "a"].map(node),
            vec![
                edge("d", "a"),
                edge("a", "b"),
                edge("b", "c"),
                edge("a", "zz"),
                edge("a", "b"),
            ],
            None,
        );
        assert_eq!(hood.edges.len(), 3, "dangling and duplicate edges dropped");
        let chunks = hood.into_chunks(2);
        assert_eq!(chunks.len(), 2);
        let chunk_ids: Vec<Vec<&str>> = chunks
            .iter()
            .map(|(n, _)| n.iter().map(|(i, _)| i.as_str()).collect())
            .collect();
        assert_eq!(chunk_ids, vec![vec!["a", "b"], vec!["c", "d"]]);
        assert_eq!(chunks[0].1.len(), 1); // a-b
        assert_eq!(chunks[1].1.len(), 2); // b-c, d-a
    }

    #[test]
    fn projection_keeps_core_keys_and_listed_ones() {
        let mut d = NodeData::new();
        for k in ["id", "name", "type", "text", "big"] {
            d.insert(Cow::Owned(k.to_string()), serde_json::json!(1));
        }
        let p = project_node_properties(d.clone(), Some(&["text".to_string()]));
        let mut keys: Vec<&str> = p.keys().map(|k| k.as_ref()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["id", "name", "text", "type"]);
        assert_eq!(project_node_properties(d, None).len(), 5);
    }

    #[test]
    fn hop_distances_are_undirected_bfs() {
        let edge = |a: &str, b: &str| {
            (
                a.to_string(),
                b.to_string(),
                "r".to_string(),
                HashMap::new(),
            )
        };
        let d = hop_distances(&[edge("b", "a"), edge("b", "c")], &ids(&["a"]));
        assert_eq!(d.get("a"), Some(&0));
        assert_eq!(d.get("b"), Some(&1));
        assert_eq!(d.get("c"), Some(&2));
    }

    #[test]
    fn entity_type_rows_keep_the_smallest_non_empty_name() {
        let out = fold_entity_type_rows(vec![
            ("e1".to_string(), "Person".to_string()),
            ("e1".to_string(), "Human".to_string()),
            ("e2".to_string(), String::new()),
        ]);
        assert_eq!(out.get("e1").map(String::as_str), Some("Human"));
        assert!(!out.contains_key("e2"));
    }
}
