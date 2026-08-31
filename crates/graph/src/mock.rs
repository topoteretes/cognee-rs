//! Mock graph database implementation for testing.
//!
//! Provides an in-memory HashMap-based implementation of GraphDBTrait
//! for use in unit tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "mock infrastructure — panics are acceptable"
)]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

use crate::{EdgeData, GraphDBError, GraphDBResult, GraphDBTrait, NodeData};

/// In-memory mock graph database for testing.
///
/// Thread-safe implementation using Arc<Mutex<>> for interior mutability.
#[derive(Clone)]
pub struct MockGraphDB {
    nodes: Arc<Mutex<HashMap<String, NodeData>>>,
    edges: Arc<Mutex<Vec<EdgeData>>>,
    call_log: Arc<Mutex<Vec<String>>>,
    /// Optional injected error returned from `get_node_truth_state` calls.
    truth_state_error: Arc<Mutex<Option<String>>>,
    /// Optional injected error returned from node writes, before any node is
    /// stored. Lets a test make the graph write fail while everything that was
    /// supposed to happen *before* it still ran.
    add_nodes_error: Arc<Mutex<Option<String>>>,
    /// Optional injected error returned from `add_edges`, before any edge is
    /// stored.
    add_edges_error: Arc<Mutex<Option<String>>>,
    /// Optional injected error returned from `delete_nodes`, before any node is
    /// removed. Lets a test make artifact deletion fail while everything that
    /// was supposed to survive it — ownership rows, completion markers — is
    /// still there to assert on.
    delete_nodes_error: Arc<Mutex<Option<String>>>,
    /// When set, [`GraphDBTrait::close`] never returns. Lets a test stand in for
    /// a slot whose teardown outlives the caller's patience — a large embedded
    /// checkpoint, or a pool waiting on a connection that will not come back —
    /// which is what makes a bounded teardown's *ordering* observable.
    hang_on_close: bool,
}

impl MockGraphDB {
    /// Create a new empty mock graph database.
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(Mutex::new(HashMap::new())),
            edges: Arc::new(Mutex::new(Vec::new())),
            call_log: Arc::new(Mutex::new(Vec::new())),
            truth_state_error: Arc::new(Mutex::new(None)),
            add_nodes_error: Arc::new(Mutex::new(None)),
            add_edges_error: Arc::new(Mutex::new(None)),
            delete_nodes_error: Arc::new(Mutex::new(None)),
            hang_on_close: false,
        }
    }

    /// A mock whose `close()` never completes.
    ///
    /// For tests that assert what a *bounded* teardown gets through before its
    /// budget runs out: with a store that hangs, the timeout is guaranteed to
    /// fire, so the assertion is about ordering rather than about how slow the
    /// machine is.
    pub fn hanging_on_close() -> Self {
        Self {
            hang_on_close: true,
            ..Self::new()
        }
    }

    /// Inject an error that will be returned from subsequent
    /// `get_node_truth_state` calls as `GraphDBError::QueryError`.
    pub fn set_truth_state_error(&self, msg: impl Into<String>) {
        let mut slot = self.truth_state_error.lock().unwrap(); // lock poison is unrecoverable
        *slot = Some(msg.into());
    }

    /// Inject an error returned from every subsequent node write
    /// (`add_node_raw` / `add_nodes_raw` / `add_nodes`) as
    /// `GraphDBError::NodeError`, before anything is stored.
    pub fn set_add_nodes_error(&self, msg: impl Into<String>) {
        let mut slot = self.add_nodes_error.lock().unwrap(); // lock poison is unrecoverable
        *slot = Some(msg.into());
    }

    /// Inject an error returned from every subsequent `add_edges` call as
    /// `GraphDBError::EdgeError`, before anything is stored.
    pub fn set_add_edges_error(&self, msg: impl Into<String>) {
        let mut slot = self.add_edges_error.lock().unwrap(); // lock poison is unrecoverable
        *slot = Some(msg.into());
    }

    /// Inject an error returned from every subsequent `delete_nodes` call as
    /// `GraphDBError::NodeError`, before any node is removed.
    pub fn set_delete_nodes_error(&self, msg: impl Into<String>) {
        let mut slot = self.delete_nodes_error.lock().unwrap(); // lock poison is unrecoverable
        *slot = Some(msg.into());
    }

    /// Clear an error previously armed by [`MockGraphDB::set_delete_nodes_error`],
    /// so a test can assert that re-running a failed operation converges.
    pub fn clear_delete_nodes_error(&self) {
        let mut slot = self.delete_nodes_error.lock().unwrap(); // lock poison is unrecoverable
        *slot = None;
    }

    /// Remove `node_ids` and every edge incident to one of them.
    ///
    /// Mirrors what the real backends do on a node delete — Ladybug issues
    /// `DETACH DELETE`, the Postgres adapter relies on `ON DELETE CASCADE` —
    /// so a test driving the mock sees a graph state a live backend could
    /// actually produce. Edges are dropped by endpoint id whether or not the
    /// node itself was stored: the mock lets a test add an edge without its
    /// endpoints, and no real backend can hold such an edge past the delete.
    fn detach_delete(&self, node_ids: &[String]) {
        let removed: HashSet<&str> = node_ids.iter().map(String::as_str).collect();

        let mut nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        for node_id in node_ids {
            nodes.remove(node_id);
        }

        let mut edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        edges.retain(|(src, tgt, _, _)| {
            !removed.contains(src.as_str()) && !removed.contains(tgt.as_str())
        });
    }

    /// Get the current node count (for testing).
    pub fn node_count(&self) -> usize {
        self.nodes.lock().unwrap().len() // lock poison is unrecoverable
    }

    /// Get the current edge count (for testing).
    pub fn edge_count(&self) -> usize {
        self.edges.lock().unwrap().len() // lock poison is unrecoverable
    }

    /// Clear all data (for testing).
    pub fn clear(&self) {
        self.nodes.lock().unwrap().clear(); // lock poison is unrecoverable
        self.edges.lock().unwrap().clear(); // lock poison is unrecoverable
        self.call_log.lock().unwrap().clear(); // lock poison is unrecoverable
    }

    /// Get a snapshot of the call log — the names of methods invoked on
    /// this mock in invocation order.
    ///
    /// Currently records `"get_graph_data"`, `"get_filtered_graph_data"`,
    /// `"get_candidate_nodes_by_label"`, `"get_nodeset_subgraph"`,
    /// `"get_neighborhood"`, `"get_node_truth_state"`,
    /// `"set_node_truth_state"`, `"delete_nodes"`, and `"close"`.
    pub fn get_call_log(&self) -> Vec<String> {
        self.call_log.lock().unwrap().clone() // lock poison is unrecoverable
    }
}

impl Default for MockGraphDB {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GraphDBTrait for MockGraphDB {
    async fn initialize(&self) -> GraphDBResult<()> {
        Ok(())
    }

    /// A no-op close, unless the mock was built by
    /// [`MockGraphDB::hanging_on_close`], in which case it never returns.
    async fn close(&self) -> GraphDBResult<()> {
        self.call_log
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push("close".to_string());
        if self.hang_on_close {
            std::future::pending::<()>().await;
        }
        Ok(())
    }

    async fn is_empty(&self) -> GraphDBResult<bool> {
        Ok(self.nodes.lock().unwrap().is_empty()) // lock poison is unrecoverable
    }

    async fn query(
        &self,
        _query: &str,
        _params: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
    ) -> GraphDBResult<Vec<Vec<serde_json::Value>>> {
        Err(GraphDBError::QueryError(
            "Query not supported in MockGraphDB".to_string(),
        ))
    }

    async fn delete_graph(&self) -> GraphDBResult<()> {
        self.clear();
        Ok(())
    }

    async fn has_node(&self, node_id: &str) -> GraphDBResult<bool> {
        Ok(self.nodes.lock().unwrap().contains_key(node_id)) // lock poison is unrecoverable
    }

    async fn add_node_raw(&self, node: Value) -> GraphDBResult<()> {
        // Error-injection hook for tests: fail before any node is stored.
        {
            let slot = self.add_nodes_error.lock().unwrap(); // lock poison is unrecoverable
            if let Some(msg) = slot.as_ref() {
                return Err(GraphDBError::NodeError(msg.clone()));
            }
        }

        let mut node_data = HashMap::new();
        if let Value::Object(map) = node {
            for (k, v) in map {
                node_data.insert(Cow::from(k), v);
            }
        }

        let id = node_data
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GraphDBError::NodeError("Node missing 'id' field".to_string()))?
            .to_string();

        self.nodes.lock().unwrap().insert(id, node_data); // lock poison is unrecoverable
        Ok(())
    }

    async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()> {
        for node in nodes {
            self.add_node_raw(node).await?;
        }
        Ok(())
    }

    async fn delete_node(&self, node_id: &str) -> GraphDBResult<()> {
        self.detach_delete(&[node_id.to_string()]);
        Ok(())
    }

    async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()> {
        // Error-injection hook for tests: fail before any node is removed.
        {
            let slot = self.delete_nodes_error.lock().unwrap(); // lock poison is unrecoverable
            if let Some(msg) = slot.as_ref() {
                return Err(GraphDBError::NodeError(msg.clone()));
            }
        }

        self.call_log
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push("delete_nodes".to_string());

        self.detach_delete(node_ids);
        Ok(())
    }

    async fn get_node(&self, node_id: &str) -> GraphDBResult<Option<NodeData>> {
        Ok(self.nodes.lock().unwrap().get(node_id).cloned()) // lock poison is unrecoverable
    }

    async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
        let nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        Ok(node_ids
            .iter()
            .filter_map(|id| nodes.get(id).cloned())
            .collect())
    }

    async fn has_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
    ) -> GraphDBResult<bool> {
        let edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        Ok(edges.iter().any(|(src, tgt, rel, _)| {
            src == source_id && tgt == target_id && rel == relationship_name
        }))
    }

    async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
        let stored_edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        let mut existing = Vec::new();

        for (src, tgt, rel, props) in edges {
            if stored_edges
                .iter()
                .any(|(s, t, r, _)| s == src && t == tgt && r == rel)
            {
                existing.push((src.clone(), tgt.clone(), rel.clone(), props.clone()));
            }
        }

        Ok(existing)
    }

    async fn add_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        properties: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
    ) -> GraphDBResult<()> {
        let edge = (
            source_id.to_string(),
            target_id.to_string(),
            relationship_name.to_string(),
            properties.unwrap_or_default(),
        );
        self.edges.lock().unwrap().push(edge); // lock poison is unrecoverable
        Ok(())
    }

    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
        // Error-injection hook for tests: fail before any edge is stored.
        {
            let slot = self.add_edges_error.lock().unwrap(); // lock poison is unrecoverable
            if let Some(msg) = slot.as_ref() {
                return Err(GraphDBError::EdgeError(msg.clone()));
            }
        }

        let mut stored_edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        for edge in edges {
            stored_edges.push(edge.clone());
        }
        Ok(())
    }

    async fn get_edges(&self, node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
        let edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        Ok(edges
            .iter()
            .filter(|(src, tgt, _, _)| src == node_id || tgt == node_id)
            .cloned()
            .collect())
    }

    async fn get_neighbors(&self, node_id: &str) -> GraphDBResult<Vec<NodeData>> {
        let edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        let nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable

        let neighbor_ids: Vec<String> = edges
            .iter()
            .filter_map(|(src, tgt, _, _)| {
                if src == node_id {
                    Some(tgt.clone())
                } else if tgt == node_id {
                    Some(src.clone())
                } else {
                    None
                }
            })
            .collect();

        Ok(neighbor_ids
            .iter()
            .filter_map(|id| nodes.get(id).cloned())
            .collect())
    }

    async fn get_connections(
        &self,
        node_id: &str,
    ) -> GraphDBResult<
        Vec<(
            NodeData,
            HashMap<Cow<'static, str>, serde_json::Value>,
            NodeData,
        )>,
    > {
        let edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        let nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable

        let mut connections = Vec::new();
        for (src, tgt, _, props) in edges.iter() {
            if src == node_id {
                if let (Some(source_node), Some(target_node)) =
                    (nodes.get(src).cloned(), nodes.get(tgt).cloned())
                {
                    connections.push((source_node, props.clone(), target_node));
                }
            } else if tgt == node_id
                && let (Some(source_node), Some(target_node)) =
                    (nodes.get(src).cloned(), nodes.get(tgt).cloned())
            {
                connections.push((source_node, props.clone(), target_node));
            }
        }

        Ok(connections)
    }

    async fn get_graph_data(&self) -> GraphDBResult<(Vec<(String, NodeData)>, Vec<EdgeData>)> {
        self.call_log
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push("get_graph_data".to_string());

        let nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        let edges = self.edges.lock().unwrap(); // lock poison is unrecoverable

        let node_vec: Vec<(String, NodeData)> = nodes
            .iter()
            .map(|(id, data)| (id.clone(), data.clone()))
            .collect();

        Ok((node_vec, edges.clone()))
    }

    async fn get_graph_metrics(
        &self,
        _include_optional: bool,
    ) -> GraphDBResult<HashMap<Cow<'static, str>, serde_json::Value>> {
        let node_count = self.node_count();
        let edge_count = self.edge_count();

        let mut metrics = HashMap::new();
        metrics.insert(
            Cow::Borrowed("node_count"),
            serde_json::Value::Number(node_count.into()),
        );
        metrics.insert(
            Cow::Borrowed("edge_count"),
            serde_json::Value::Number(edge_count.into()),
        );

        Ok(metrics)
    }

    async fn get_degree_one_nodes(
        &self,
        node_type: &str,
    ) -> GraphDBResult<Vec<(String, crate::types::NodeData)>> {
        let nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        let edges = self.edges.lock().unwrap(); // lock poison is unrecoverable

        // Build degree map from edges
        let mut degree: HashMap<String, usize> = HashMap::new();
        for (src, tgt, _, _) in edges.iter() {
            *degree.entry(src.clone()).or_default() += 1;
            *degree.entry(tgt.clone()).or_default() += 1;
        }

        Ok(nodes
            .iter()
            .filter(|(id, data)| {
                let type_matches = data
                    .get("type")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| t == node_type);
                let deg = degree.get(*id).copied().unwrap_or(0);
                type_matches && deg == 1
            })
            .map(|(id, data)| (id.clone(), data.clone()))
            .collect())
    }

    async fn get_all_relationship_names(&self) -> GraphDBResult<HashSet<String>> {
        let edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
        Ok(edges.iter().map(|(_, _, rel, _)| rel.clone()).collect())
    }

    async fn get_zero_degree_edge_type_nodes(
        &self,
    ) -> GraphDBResult<Vec<(String, crate::types::NodeData)>> {
        let nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        let edges = self.edges.lock().unwrap(); // lock poison is unrecoverable

        // Collect active relationship names from edges
        let active_rel_names: HashSet<String> =
            edges.iter().map(|(_, _, rel, _)| rel.clone()).collect();

        // Build degree map
        let mut degree: HashMap<String, usize> = HashMap::new();
        for (src, tgt, _, _) in edges.iter() {
            *degree.entry(src.clone()).or_default() += 1;
            *degree.entry(tgt.clone()).or_default() += 1;
        }

        Ok(nodes
            .iter()
            .filter(|(id, data)| {
                let is_edge_type = data
                    .get("type")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| t == "EdgeType");
                if !is_edge_type {
                    return false;
                }
                let deg = degree.get(*id).copied().unwrap_or(0);
                if deg > 0 {
                    return false;
                }
                let rel_name = data
                    .get("relationship_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                !active_rel_names.contains(rel_name)
            })
            .map(|(id, data)| (id.clone(), data.clone()))
            .collect())
    }

    /// Stands in for the best case a real backend can manage: the exact
    /// predicate, node rows only, no edge scan. Recorded in the call log so a
    /// test can assert a caller stopped reaching for `get_graph_data`.
    async fn get_candidate_nodes_by_label(
        &self,
        needle: &str,
    ) -> GraphDBResult<Vec<(String, NodeData)>> {
        self.call_log
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push("get_candidate_nodes_by_label".to_string());

        let nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        Ok(nodes
            .iter()
            .filter(|(_, data)| crate::node_label_contains(data, needle))
            .map(|(id, data)| (id.clone(), data.clone()))
            .collect())
    }

    /// Ignores the filters entirely and returns the whole graph, so a caller
    /// that pushes a predicate down here is NOT narrowed by the mock — the
    /// call is logged under its own name to make that visible.
    async fn get_filtered_graph_data(
        &self,
        _attribute_filters: &HashMap<Cow<'static, str>, Vec<serde_json::Value>>,
    ) -> GraphDBResult<(Vec<(String, NodeData)>, Vec<EdgeData>)> {
        self.call_log
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push("get_filtered_graph_data".to_string());
        self.get_graph_data().await
    }

    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
        node_name_filter_operator: &str,
    ) -> GraphDBResult<(Vec<(String, NodeData)>, Vec<EdgeData>)> {
        self.call_log
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push("get_nodeset_subgraph".to_string());

        // Empty name filter -> empty result (matches PG adapter behavior).
        if node_names.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let nodes_guard = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        let edges_guard = self.edges.lock().unwrap(); // lock poison is unrecoverable

        // Step 1: Select primary nodes: nodes whose `type` == node_type AND
        // whose `name` is in node_names (exact case-sensitive match, matching
        // the PG adapter).
        let name_set: HashSet<&str> = node_names.iter().map(|s| s.as_str()).collect();
        let primary_ids: HashSet<String> = nodes_guard
            .iter()
            .filter(|(_, data)| {
                let ty = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
                ty == node_type && name_set.contains(name)
            })
            .map(|(id, _)| id.clone())
            .collect();

        // Step 2: Determine included nodes based on the operator.
        //
        // OR:  included = primaries ∪ any neighbor of ANY primary.
        // AND: included = primaries ∪ nodes that are neighbors of EVERY primary.
        //
        // Anything other than "OR" or "AND" defaults to OR, matching the PG
        // adapter's forgiving behavior.
        let operator_and = node_name_filter_operator == "AND";

        let mut included: HashSet<String> = primary_ids.clone();

        if !operator_and {
            // OR semantics: include every neighbor reached via any edge from a
            // primary node (either endpoint direction).
            for (src, tgt, _, _) in edges_guard.iter() {
                if primary_ids.contains(src) {
                    included.insert(tgt.clone());
                }
                if primary_ids.contains(tgt) {
                    included.insert(src.clone());
                }
            }
        } else {
            // AND semantics: neighbor must be connected to every primary node.
            // For each candidate neighbor, count how many distinct primaries
            // connect to it.
            //
            // neighbor_id -> set of primaries that connect to it.
            let mut neighbor_to_primaries: HashMap<String, HashSet<String>> = HashMap::new();
            for (src, tgt, _, _) in edges_guard.iter() {
                if primary_ids.contains(src) && !primary_ids.contains(tgt) {
                    neighbor_to_primaries
                        .entry(tgt.clone())
                        .or_default()
                        .insert(src.clone());
                }
                if primary_ids.contains(tgt) && !primary_ids.contains(src) {
                    neighbor_to_primaries
                        .entry(src.clone())
                        .or_default()
                        .insert(tgt.clone());
                }
            }

            let primary_count = primary_ids.len();
            for (neighbor_id, connected_primaries) in neighbor_to_primaries {
                if connected_primaries.len() == primary_count {
                    included.insert(neighbor_id);
                }
            }
        }

        // Step 3: Collect included nodes (with their data) and edges whose
        // BOTH endpoints are in the included set.
        let node_vec: Vec<(String, NodeData)> = included
            .iter()
            .filter_map(|id| nodes_guard.get(id).map(|data| (id.clone(), data.clone())))
            .collect();

        let edge_vec: Vec<EdgeData> = edges_guard
            .iter()
            .filter(|(src, tgt, _, _)| included.contains(src) && included.contains(tgt))
            .cloned()
            .collect();

        Ok((node_vec, edge_vec))
    }

    async fn get_neighborhood(
        &self,
        node_ids: &[String],
        depth: usize,
    ) -> GraphDBResult<(Vec<(String, NodeData)>, Vec<EdgeData>)> {
        self.call_log
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push("get_neighborhood".to_string());

        if node_ids.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let nodes_guard = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        let edges_guard = self.edges.lock().unwrap(); // lock poison is unrecoverable

        // Two phases, mirroring the real adapters (`PgGraphAdapter`'s recursive
        // CTE and `LadybugAdapter`'s traversal): first resolve the id set by
        // undirected BFS out to `depth`, then return the **induced subgraph** over
        // it — every edge with both endpoints resolved, per the contract on
        // `GraphDBTrait::get_neighborhood`.
        //
        // Collecting edges during the walk instead (only those incident to the
        // current frontier) silently omits edges between two nodes discovered at
        // the same depth, so a caller that partitions the result itself — graph
        // search keeps edges with at least one *seed* endpoint — would see a
        // different set here than in production.
        //
        // Read straight from the internal edge store rather than delegating to
        // `get_connections`, which drops `relationship_name`. Edge tuples are
        // cloned unmodified, so the true stored direction is preserved.
        let mut resolved: HashSet<String> = node_ids.iter().cloned().collect();
        let mut frontier: Vec<String> = resolved.iter().cloned().collect();

        for _ in 0..depth {
            let frontier_set: HashSet<&String> = frontier.iter().collect();
            let mut next_frontier: Vec<String> = Vec::new();

            for (src, tgt, _, _) in edges_guard.iter() {
                let src_in = frontier_set.contains(src);
                let tgt_in = frontier_set.contains(tgt);
                if src_in && resolved.insert(tgt.clone()) {
                    next_frontier.push(tgt.clone());
                }
                if tgt_in && resolved.insert(src.clone()) {
                    next_frontier.push(src.clone());
                }
            }

            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }

        // An isolated seed still appears, because it is in `resolved`. A seed with
        // no row in the node store is dropped here — same as the previous
        // behaviour, and same as the real adapters, whose node halves select from
        // `graph_node`/`(n:Node)` and so cannot invent a row either.
        let nodes: Vec<(String, NodeData)> = resolved
            .iter()
            .filter_map(|id| nodes_guard.get(id).map(|data| (id.clone(), data.clone())))
            .collect();

        let mut edge_keys: HashSet<(String, String, String)> = HashSet::new();
        let edges: Vec<EdgeData> = edges_guard
            .iter()
            .filter(|(src, tgt, _, _)| resolved.contains(src) && resolved.contains(tgt))
            .filter(|(src, tgt, rel, _)| edge_keys.insert((src.clone(), tgt.clone(), rel.clone())))
            .cloned()
            .collect();

        Ok((nodes, edges))
    }

    async fn get_node_truth_state(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<HashMap<String, crate::NodeTruthState>> {
        self.call_log
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push("get_node_truth_state".to_string());

        // Error-injection hook for tests: fail before any read.
        {
            let slot = self.truth_state_error.lock().unwrap(); // lock poison is unrecoverable
            if let Some(msg) = slot.as_ref() {
                return Err(GraphDBError::QueryError(msg.clone()));
            }
        }

        let nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        let mut out = HashMap::with_capacity(node_ids.len());
        for id in node_ids {
            if let Some(node) = nodes.get(id) {
                out.insert(
                    id.clone(),
                    crate::NodeTruthState {
                        truth_alignment: crate::traits::extract_truth_alignment(
                            node.get("truth_alignment"),
                        ),
                        truth_epoch: crate::traits::extract_truth_epoch(node.get("truth_epoch")),
                    },
                );
            }
        }
        Ok(out)
    }

    async fn set_node_truth_state(
        &self,
        updates: &HashMap<String, crate::NodeTruthState>,
    ) -> GraphDBResult<HashMap<String, bool>> {
        self.call_log
            .lock()
            .unwrap() // lock poison is unrecoverable
            .push("set_node_truth_state".to_string());

        let mut nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        let mut out = HashMap::with_capacity(updates.len());
        for (id, state) in updates {
            if let Some(node) = nodes.get_mut(id) {
                node.insert(
                    Cow::Borrowed("truth_alignment"),
                    serde_json::json!(state.truth_alignment),
                );
                node.insert(
                    Cow::Borrowed("truth_epoch"),
                    serde_json::json!(state.truth_epoch),
                );
                out.insert(id.clone(), true);
            } else {
                out.insert(id.clone(), false);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GraphDBTraitExt;
    use cognee_models::Entity;

    #[tokio::test]
    async fn test_mock_db_creation() {
        let db = MockGraphDB::new();
        assert_eq!(db.node_count(), 0);
        assert_eq!(db.edge_count(), 0);
    }

    #[tokio::test]
    async fn test_add_and_get_node() {
        let db = MockGraphDB::new();
        let entity = Entity::new("Alice", None, "A person", None);

        db.add_node(&entity).await.unwrap();
        assert_eq!(db.node_count(), 1);

        let node = db.get_node(&entity.base.id.to_string()).await.unwrap();
        assert!(node.is_some());
    }

    #[tokio::test]
    async fn test_add_and_check_edge() {
        let db = MockGraphDB::new();

        db.add_edge("node1", "node2", "relates_to", None)
            .await
            .unwrap();
        assert_eq!(db.edge_count(), 1);

        let exists = db.has_edge("node1", "node2", "relates_to").await.unwrap();
        assert!(exists);
    }

    #[tokio::test]
    async fn test_has_edges_batch() {
        let db = MockGraphDB::new();

        // Add some edges
        db.add_edge("a", "b", "rel1", None).await.unwrap();
        db.add_edge("c", "d", "rel2", None).await.unwrap();

        // Query for edges (some exist, some don't)
        let query_edges = vec![
            (
                "a".to_string(),
                "b".to_string(),
                "rel1".to_string(),
                HashMap::new(),
            ),
            (
                "e".to_string(),
                "f".to_string(),
                "rel3".to_string(),
                HashMap::new(),
            ),
        ];

        let existing = db.has_edges(&query_edges).await.unwrap();
        assert_eq!(existing.len(), 1); // Only the first edge exists
    }

    #[tokio::test]
    async fn test_clear() {
        let db = MockGraphDB::new();
        let entity = Entity::new("Alice", None, "A person", None);

        db.add_node(&entity).await.unwrap();
        db.add_edge("a", "b", "rel", None).await.unwrap();

        db.clear();
        assert_eq!(db.node_count(), 0);
        assert_eq!(db.edge_count(), 0);
    }

    #[tokio::test]
    async fn get_id_filtered_graph_data_returns_subset() {
        let db = MockGraphDB::new();

        // Add three nodes with raw JSON (id field required by MockGraphDB)
        db.add_node_raw(serde_json::json!({"id": "n1", "label": "Node1"}))
            .await
            .unwrap();
        db.add_node_raw(serde_json::json!({"id": "n2", "label": "Node2"}))
            .await
            .unwrap();
        db.add_node_raw(serde_json::json!({"id": "n3", "label": "Node3"}))
            .await
            .unwrap();

        // Add edges: n1→n2 (both requested), n2→n3 (n3 not requested), n1→n3 (n3 not requested)
        db.add_edge("n1", "n2", "connects", None).await.unwrap();
        db.add_edge("n2", "n3", "connects", None).await.unwrap();
        db.add_edge("n1", "n3", "connects", None).await.unwrap();

        let node_ids = vec!["n1".to_string(), "n2".to_string()];
        let (nodes, edges) = db.get_id_filtered_graph_data(&node_ids).await.unwrap();

        // Only n1 and n2 should be returned
        assert_eq!(nodes.len(), 2);
        let returned_ids: std::collections::HashSet<&str> =
            nodes.iter().map(|(id, _)| id.as_str()).collect();
        assert!(returned_ids.contains("n1"));
        assert!(returned_ids.contains("n2"));
        assert!(!returned_ids.contains("n3"));

        // Only the edge n1→n2 should be returned (both endpoints in the requested set)
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].0, "n1");
        assert_eq!(edges[0].1, "n2");
    }

    #[tokio::test]
    async fn get_degree_one_nodes_returns_orphans() {
        let db = MockGraphDB::new();

        // Entity with degree 1 (orphan — only connected to its type)
        db.add_node_raw(serde_json::json!({"id": "e1", "type": "Entity", "name": "Alice"}))
            .await
            .unwrap();
        // Entity with degree 2 (well-connected — should NOT be returned)
        db.add_node_raw(serde_json::json!({"id": "e2", "type": "Entity", "name": "Bob"}))
            .await
            .unwrap();
        // EntityType with degree 1 (orphan)
        db.add_node_raw(serde_json::json!({"id": "et1", "type": "EntityType", "name": "Person"}))
            .await
            .unwrap();
        // An unrelated node
        db.add_node_raw(serde_json::json!({"id": "c1", "type": "DocumentChunk", "text": "hello"}))
            .await
            .unwrap();

        // e1 -> et1 (one edge each for e1 and et1)
        db.add_edge("e1", "et1", "is_a", None).await.unwrap();
        // e2 -> et1 (second edge for e2 and et1)
        db.add_edge("e2", "et1", "is_a", None).await.unwrap();
        // e2 -> c1 (third edge for e2)
        db.add_edge("c1", "e2", "contains", None).await.unwrap();

        // e1 has degree 1, e2 has degree 2
        let orphan_entities = db.get_degree_one_nodes("Entity").await.unwrap();
        assert_eq!(orphan_entities.len(), 1);
        assert_eq!(orphan_entities[0].0, "e1");

        // et1 has degree 2 (is_a from e1 and e2), so no orphan EntityTypes
        let orphan_types = db.get_degree_one_nodes("EntityType").await.unwrap();
        assert_eq!(orphan_types.len(), 0);

        // No DocumentChunk with degree 1 check (c1 has degree 1)
        let orphan_chunks = db.get_degree_one_nodes("DocumentChunk").await.unwrap();
        assert_eq!(orphan_chunks.len(), 1);
    }

    #[tokio::test]
    async fn get_degree_one_nodes_empty_graph() {
        let db = MockGraphDB::new();
        let result = db.get_degree_one_nodes("Entity").await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn get_id_filtered_graph_data_empty_ids_returns_empty() {
        let db = MockGraphDB::new();
        db.add_node_raw(serde_json::json!({"id": "n1", "label": "Node1"}))
            .await
            .unwrap();

        let (nodes, edges) = db.get_id_filtered_graph_data(&[]).await.unwrap();

        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }

    #[tokio::test]
    async fn get_all_relationship_names_returns_distinct() {
        let db = MockGraphDB::new();

        db.add_edge("a", "b", "knows", None).await.unwrap();
        db.add_edge("c", "d", "knows", None).await.unwrap();
        db.add_edge("a", "c", "works_at", None).await.unwrap();

        let names = db.get_all_relationship_names().await.unwrap();
        assert_eq!(names.len(), 2);
        assert!(names.contains("knows"));
        assert!(names.contains("works_at"));
    }

    #[tokio::test]
    async fn get_all_relationship_names_empty_graph() {
        let db = MockGraphDB::new();
        let names = db.get_all_relationship_names().await.unwrap();
        assert!(names.is_empty());
    }

    #[tokio::test]
    async fn get_zero_degree_edge_type_nodes_finds_orphans() {
        let db = MockGraphDB::new();

        // Orphaned EdgeType (no edges at all, relationship_name not in any edge)
        db.add_node_raw(serde_json::json!({
            "id": "et_orphan",
            "type": "EdgeType",
            "relationship_name": "obsolete_rel"
        }))
        .await
        .unwrap();

        // Non-orphaned EdgeType (edges with "knows" exist)
        db.add_node_raw(serde_json::json!({
            "id": "et_active",
            "type": "EdgeType",
            "relationship_name": "knows"
        }))
        .await
        .unwrap();

        // Non-EdgeType node (should be ignored)
        db.add_node_raw(serde_json::json!({
            "id": "e1",
            "type": "Entity",
            "name": "Alice"
        }))
        .await
        .unwrap();

        // Edge with "knows" relationship
        db.add_edge("e1", "e1", "knows", None).await.unwrap();

        let orphans = db.get_zero_degree_edge_type_nodes().await.unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].0, "et_orphan");
    }

    #[tokio::test]
    async fn get_zero_degree_edge_type_nodes_empty_graph() {
        let db = MockGraphDB::new();
        let orphans = db.get_zero_degree_edge_type_nodes().await.unwrap();
        assert!(orphans.is_empty());
    }

    #[tokio::test]
    async fn get_neighborhood_preserves_edge_direction() {
        let db = MockGraphDB::new();
        db.add_node_raw(serde_json::json!({"id": "seed", "type": "T"}))
            .await
            .unwrap();
        db.add_node_raw(serde_json::json!({"id": "target", "type": "T"}))
            .await
            .unwrap();
        db.add_node_raw(serde_json::json!({"id": "source", "type": "T"}))
            .await
            .unwrap();

        // seed is the stored TARGET of source->seed and the stored SOURCE of
        // seed->target — the exact shape that would flip under a source-as-
        // queried-node bug.
        db.add_edge("seed", "target", "out", None).await.unwrap();
        db.add_edge("source", "seed", "in", None).await.unwrap();

        let (nodes, edges) = db.get_neighborhood(&["seed".to_string()], 1).await.unwrap();

        let node_ids: HashSet<&str> = nodes.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(node_ids.len(), 3);
        assert!(node_ids.contains("seed"));
        assert!(node_ids.contains("target"));
        assert!(node_ids.contains("source"));

        let edge_set: HashSet<(&str, &str, &str)> = edges
            .iter()
            .map(|(s, t, r, _)| (s.as_str(), t.as_str(), r.as_str()))
            .collect();
        assert!(edge_set.contains(&("source", "seed", "in")));
        assert!(edge_set.contains(&("seed", "target", "out")));
        // Flipped forms must be absent (locked-decision-6 anti-flip guard).
        assert!(!edge_set.contains(&("seed", "source", "in")));
        assert!(!edge_set.contains(&("target", "seed", "out")));

        assert!(db.get_call_log().contains(&"get_neighborhood".to_string()));
    }

    #[tokio::test]
    async fn get_neighborhood_includes_relationship_name() {
        let db = MockGraphDB::new();
        db.add_node_raw(serde_json::json!({"id": "a", "type": "T"}))
            .await
            .unwrap();
        db.add_node_raw(serde_json::json!({"id": "b", "type": "T"}))
            .await
            .unwrap();
        db.add_edge("a", "b", "connects_to", None).await.unwrap();

        let (_nodes, edges) = db.get_neighborhood(&["a".to_string()], 1).await.unwrap();
        assert_eq!(edges.len(), 1);
        // Guards against the get_connections relationship-name-drop gap: the
        // mock override must carry the stored rel name through.
        assert!(!edges[0].2.is_empty());
        assert_eq!(edges[0].2, "connects_to");
    }

    #[tokio::test]
    async fn node_truth_state_round_trip_and_logs_calls() {
        let db = MockGraphDB::new();
        db.add_node_raw(serde_json::json!({"id": "n1", "type": "T"}))
            .await
            .unwrap();

        let mut updates = HashMap::new();
        updates.insert(
            "n1".to_string(),
            crate::NodeTruthState {
                truth_alignment: vec![0.5, 1.5],
                truth_epoch: 7,
            },
        );
        // Missing node reports false; present node reports true.
        updates.insert("ghost".to_string(), crate::NodeTruthState::default());
        let set_res = db.set_node_truth_state(&updates).await.unwrap();
        assert_eq!(set_res.get("n1"), Some(&true));
        assert_eq!(set_res.get("ghost"), Some(&false));

        let got = db.get_node_truth_state(&["n1".to_string()]).await.unwrap();
        let state = got.get("n1").expect("n1 present");
        assert_eq!(state.truth_alignment, vec![0.5, 1.5]);
        assert_eq!(state.truth_epoch, 7);

        let log = db.get_call_log();
        assert!(log.contains(&"set_node_truth_state".to_string()));
        assert!(log.contains(&"get_node_truth_state".to_string()));
    }

    #[tokio::test]
    async fn delete_nodes_detaches_incident_edges() {
        let db = MockGraphDB::new();
        for id in ["a", "b", "c"] {
            db.add_node_raw(serde_json::json!({"id": id, "type": "T"}))
                .await
                .unwrap();
        }
        // b is the target of one edge and the source of another, so a cascade
        // that only looked at one endpoint column would leave one behind.
        db.add_edge("a", "b", "in", None).await.unwrap();
        db.add_edge("b", "c", "out", None).await.unwrap();
        db.add_edge("a", "c", "untouched", None).await.unwrap();

        db.delete_nodes(&["b".to_string()]).await.unwrap();

        assert_eq!(db.node_count(), 2);
        assert!(!db.has_edge("a", "b", "in").await.unwrap());
        assert!(!db.has_edge("b", "c", "out").await.unwrap());
        // Edges between surviving nodes stay.
        assert!(db.has_edge("a", "c", "untouched").await.unwrap());
        assert_eq!(db.edge_count(), 1);
    }

    #[tokio::test]
    async fn delete_node_detaches_incident_edges() {
        let db = MockGraphDB::new();
        db.add_node_raw(serde_json::json!({"id": "a", "type": "T"}))
            .await
            .unwrap();
        db.add_node_raw(serde_json::json!({"id": "b", "type": "T"}))
            .await
            .unwrap();
        db.add_edge("a", "b", "r", None).await.unwrap();

        db.delete_node("a").await.unwrap();

        assert!(!db.has_edge("a", "b", "r").await.unwrap());
        assert_eq!(db.edge_count(), 0);
    }

    #[tokio::test]
    async fn get_zero_degree_edge_type_with_edges_not_orphaned() {
        let db = MockGraphDB::new();

        // EdgeType node that is directly connected via an edge (degree > 0)
        db.add_node_raw(serde_json::json!({
            "id": "et1",
            "type": "EdgeType",
            "relationship_name": "related"
        }))
        .await
        .unwrap();
        db.add_node_raw(serde_json::json!({
            "id": "other",
            "type": "Entity",
            "name": "X"
        }))
        .await
        .unwrap();
        db.add_edge("et1", "other", "structural", None)
            .await
            .unwrap();

        let orphans = db.get_zero_degree_edge_type_nodes().await.unwrap();
        assert!(
            orphans.is_empty(),
            "EdgeType with degree > 0 should not be orphaned"
        );
    }
}
