//! Graph database trait interface.
//!
//! Defines the complete async API for graph database operations.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::{EdgeData, GraphDBResult, GraphNode, NodeData};

/// Composite key uniquely identifying an edge in the graph:
/// `(source_id, target_id, relationship_name)`.
pub type EdgeKey = (String, String, String);

/// Per-node truth-subspace alignment state: coordinates against the current
/// centroid slots plus the epoch they were computed against.
///
/// `truth_epoch` uses `-1` as the "never scored" sentinel (NOT `0`, which is a
/// legitimate first real epoch). [`Default`] yields `truth_epoch = 0`, so the
/// read path must default a MISSING/invalid epoch to `-1` explicitly rather
/// than relying on `Default`. Callers must treat `-1` (not `0`) as "no truth
/// state yet" and fall back to unweighted scoring.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeTruthState {
    pub truth_alignment: Vec<f64>,
    pub truth_epoch: i64,
}

/// Extract a `truth_alignment` coordinate vector from a stored JSON property.
///
/// If the value is a JSON array, each element is coerced via `.as_f64()` and
/// non-numeric elements are dropped (a structural-parity approximation of
/// Python's dynamically-typed pass-through). Any non-array/missing value
/// yields an empty vector.
pub(crate) fn extract_truth_alignment(value: Option<&Value>) -> Vec<f64> {
    match value.and_then(|v| v.as_array()) {
        Some(arr) => arr.iter().filter_map(|e| e.as_f64()).collect(),
        None => Vec::new(),
    }
}

/// Extract a `truth_epoch` version stamp from a stored JSON property.
///
/// Accepts a JSON integer, a float-encoded number (e.g. `3.0`, truncated toward
/// zero to match Python's `int(epoch)` in the ladybug adapter), or a numeric
/// JSON string (e.g. `"3"`), matching Python's tolerance for stringly-typed
/// epoch values. Anything missing, non-numeric, or non-finite (`NaN`/`inf`)
/// yields the `-1` "never scored" sentinel.
pub(crate) fn extract_truth_epoch(value: Option<&Value>) -> i64 {
    match value {
        Some(v) => v
            .as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
            // Float-encoded epochs (e.g. `3.0`) are rejected by `as_i64()`;
            // truncate toward zero like Python's `int()`. Guard against
            // `NaN`/`inf` so they fall through to the -1 sentinel instead of
            // producing a bogus value.
            .or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
            .unwrap_or(-1),
        None => -1,
    }
}

/// The node properties that can carry a node's type-ish label.
///
/// cognee's graph rows reach Rust from several writers — the Rust pipeline, the
/// Python SDK, and hand-written imports — which disagree on where they put a
/// node's kind. Postgres and ladybug both have a first-class `type` column, but
/// anything the writer put under `node_type`, `kind`, `label` or `labels` lands
/// in the JSON `properties` blob and is merged back into the flat [`NodeData`]
/// map on read, so a reader that only looks at `type` misses those rows.
pub const NODE_LABEL_KEYS: [&str; 5] = ["type", "node_type", "kind", "label", "labels"];

/// Whether any of [`NODE_LABEL_KEYS`] on `node_data` contains `needle`,
/// compared ASCII-case-insensitively.
///
/// A key holding a JSON string matches on substring; a key holding a JSON array
/// matches if any string element does (Neo4j-style multi-`labels`). `needle`
/// must already be lowercase — the constants callers pass are.
///
/// This is the exact predicate that
/// [`GraphDBTrait::get_candidate_nodes_by_label`] narrows towards, factored out
/// so that the needle a caller pushes into the query cannot drift from the one
/// it re-applies to the rows that come back.
pub fn node_label_contains(node_data: &NodeData, needle: &str) -> bool {
    NODE_LABEL_KEYS.iter().any(|key| {
        node_data
            .get(*key)
            .map(|value| match value {
                Value::String(text) => text.to_ascii_lowercase().contains(needle),
                Value::Array(values) => values.iter().any(|item| {
                    item.as_str()
                        .is_some_and(|text| text.to_ascii_lowercase().contains(needle))
                }),
                _ => false,
            })
            .unwrap_or(false)
    })
}

/// Graph database interface trait.
///
/// This trait defines the complete set of operations for graph database interaction,
/// providing a consistent API for any graph database backend.
///
/// # Methods
///
/// ## Core Operations
/// - `initialize()` - Set up database schema
/// - `is_empty()` - Check if database is empty
/// - `query()` - Execute raw query
/// - `delete_graph()` - Remove all data
///
/// ## Node Operations
/// - `add_node()` - Add single node
/// - `add_nodes()` - Add multiple nodes
/// - `delete_node()` - Delete single node
/// - `delete_nodes()` - Delete multiple nodes
/// - `get_node()` - Get single node
/// - `get_nodes()` - Get multiple nodes
/// - `has_node()` - Check node existence
///
/// ## Edge Operations
/// - `add_edge()` - Add single edge
/// - `add_edges()` - Add multiple edges
/// - `has_edge()` - Check edge existence
/// - `has_edges()` - Check multiple edges existence
/// - `get_edges()` - Get all edges for a node
///
/// ## Graph Queries
/// - `get_neighbors()` - Get neighboring nodes
/// - `get_connections()` - Get all connections (nodes + edges)
/// - `get_graph_data()` - Get all nodes and edges
/// - `get_graph_metrics()` - Get graph statistics
/// - `get_filtered_graph_data()` - Get filtered subgraph
/// - `get_nodeset_subgraph()` - Get subgraph for specific nodes
/// - `get_neighborhood()` - Get k-hop subgraph around a set of seed nodes
/// - `get_candidate_nodes_by_label()` - Get nodes-only candidates by type-ish label
#[async_trait]
pub trait GraphDBTrait: Send + Sync {
    /// Initialize the database schema.
    ///
    /// Creates necessary tables, indexes, and constraints.
    ///
    async fn initialize(&self) -> GraphDBResult<()>;

    /// Release the OS resources this store owns, instead of waiting for `Drop`.
    ///
    /// Mirrors `cognee_database::close` for the relational pool: a `Drop` is
    /// not a close. An embedded file-backed graph (ladybug) holds a write lock
    /// on its main database file plus an un-checkpointed `.wal` (ladybug's own
    /// suffix — not SQLite's `-wal`, which the relational pool leaves), and a Postgres
    /// adapter holds its **own** sqlx pool whose destructor only flags the pool
    /// closed and lets each connection tear down on an arbitrary thread. Both
    /// outlive the last `Arc` by an unbounded amount of time, which is
    /// observable as orphaned sidecar files and as server-side backends that
    /// stay open until the process exits (topoteretes/cognee-rs#132).
    ///
    /// Contract:
    /// - **Idempotent.** Calling it twice is a no-op the second time.
    /// - **Safe to call while other `Arc` clones are alive.** The closed state
    ///   lives *inside* the shared inner handle (as sqlx puts its closed flag
    ///   inside `PoolInner`), so surviving clones fail their next operation with
    ///   a "closed" error rather than silently reconnecting or reopening.
    /// - **Post-close operations fail — for backends that actually close
    ///   something.** This is a deliberate, user-visible extension of the
    ///   relational contract, not a bug. It does *not* bind an implementor whose
    ///   `close` is the no-op default below: with nothing to release there is
    ///   nothing to invalidate, so such a backend keeps serving, and the unit
    ///   test for the default asserts exactly that.
    /// - The **default body is a no-op**, meaning "this backend owns nothing
    ///   closable beyond memory". An adapter that does own OS resources must
    ///   override it, or it will leak invisibly.
    async fn close(&self) -> GraphDBResult<()> {
        Ok(())
    }

    /// Check if the database is empty (no nodes).
    ///
    async fn is_empty(&self) -> GraphDBResult<bool>;

    /// Execute a raw database query.
    ///
    /// # Arguments
    /// * `query` - Query string (Cypher-like for Ladybug)
    /// * `params` - Query parameters
    ///
    async fn query(
        &self,
        query: &str,
        params: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
    ) -> GraphDBResult<Vec<Vec<serde_json::Value>>>;

    /// Delete the entire graph (all nodes and edges).
    ///
    async fn delete_graph(&self) -> GraphDBResult<()>;

    /// Check if a node exists by ID.
    ///
    async fn has_node(&self, node_id: &str) -> GraphDBResult<bool>;

    /// Add a single node (type-erased). Takes a pre-serialized JSON value.
    /// Prefer [`GraphDBTraitExt::add_node`] for typed access.
    async fn add_node_raw(&self, node: Value) -> GraphDBResult<()>;

    /// Add multiple nodes (type-erased). Takes pre-serialized JSON values.
    /// Prefer [`GraphDBTraitExt::add_nodes`] for typed access.
    async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()>;

    /// Delete a node by ID.
    ///
    async fn delete_node(&self, node_id: &str) -> GraphDBResult<()>;

    /// Delete multiple nodes by IDs.
    ///
    async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()>;

    /// Get a single node by ID.
    ///
    /// Returns None if node doesn't exist.
    ///
    async fn get_node(&self, node_id: &str) -> GraphDBResult<Option<NodeData>>;

    /// Get multiple nodes by IDs.
    ///
    async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>>;

    /// Check if an edge exists between two nodes.
    ///
    /// # Arguments
    /// * `source_id` - Source node ID
    /// * `target_id` - Target node ID
    /// * `relationship_name` - Edge label/relationship type
    ///
    async fn has_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
    ) -> GraphDBResult<bool>;

    /// Check which edges exist from a list.
    ///
    /// Returns only edges that exist in the database.
    ///
    async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>>;

    /// Add a single edge between two nodes.
    ///
    /// # Arguments
    /// * `source_id` - Source node ID
    /// * `target_id` - Target node ID
    /// * `relationship_name` - Edge label/relationship type
    /// * `properties` - Optional edge properties
    ///
    async fn add_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        properties: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
    ) -> GraphDBResult<()>;

    /// Add multiple edges in a batch operation.
    ///
    /// # Arguments
    /// * `edges` - Vector of EdgeData tuples
    ///
    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()>;

    /// Get all edges connected to a node.
    ///
    /// Returns edges in format: (source_id, target_id, relationship_name, properties)
    ///
    async fn get_edges(&self, node_id: &str) -> GraphDBResult<Vec<EdgeData>>;

    /// Get all neighboring nodes (directly connected).
    ///
    async fn get_neighbors(&self, node_id: &str) -> GraphDBResult<Vec<NodeData>>;

    /// Get all connections (nodes + edges) for a node.
    ///
    /// Returns: Vec<(source_node, edge_properties, target_node)>
    ///
    async fn get_connections(
        &self,
        node_id: &str,
    ) -> GraphDBResult<
        Vec<(
            NodeData,
            HashMap<Cow<'static, str>, serde_json::Value>,
            NodeData,
        )>,
    >;

    /// Get all nodes and edges in the graph.
    ///
    /// Returns: (nodes, edges) where:
    /// - nodes: Vec<(node_id, properties)>
    /// - edges: Vec<(source_id, target_id, relationship_name, properties)>
    ///
    async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)>;

    /// Get graph metrics and statistics.
    ///
    /// Returns metrics like node count, edge count, density, etc.
    ///
    async fn get_graph_metrics(
        &self,
        include_optional: bool,
    ) -> GraphDBResult<HashMap<Cow<'static, str>, serde_json::Value>>;

    /// Get a filtered subgraph based on attribute filters.
    ///
    /// # Arguments
    /// * `attribute_filters` - Filters as key-value pairs
    ///
    async fn get_filtered_graph_data(
        &self,
        attribute_filters: &HashMap<Cow<'static, str>, Vec<serde_json::Value>>,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)>;

    /// Get subgraph for a specific set of nodes.
    ///
    /// # Arguments
    /// * `node_type` - Type name of nodes to retrieve
    /// * `node_names` - Names of specific nodes
    /// * `node_name_filter_operator` - "OR" to include neighbors of ANY named node,
    ///   "AND" to include only neighbors connected to ALL named nodes
    ///
    /// Returns nodes and edges connecting them.
    ///
    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
        node_name_filter_operator: &str,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)>;

    /// Find nodes of the given type that have exactly one edge (any direction).
    ///
    /// Used by hard-delete mode to locate orphaned Entity/EntityType nodes that
    /// are no longer meaningfully connected after a soft deletion.
    ///
    /// Default implementation fetches the full graph and computes degree in
    /// memory (O(N+E)).  Backends may override with an efficient Cypher/SQL query.
    async fn get_degree_one_nodes(&self, node_type: &str) -> GraphDBResult<Vec<crate::GraphNode>> {
        let (nodes, edges) = self.get_graph_data().await?;

        // Build a degree map from edges (count both endpoints)
        let mut degree: HashMap<String, usize> = HashMap::new();
        for (src, tgt, _, _) in &edges {
            *degree.entry(src.clone()).or_default() += 1;
            *degree.entry(tgt.clone()).or_default() += 1;
        }

        Ok(nodes
            .into_iter()
            .filter(|(id, props)| {
                let type_matches = props
                    .get("type")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| t == node_type);
                let deg = degree.get(id).copied().unwrap_or(0);
                type_matches && deg == 1
            })
            .collect())
    }

    /// Return the set of all unique relationship names from edges in the graph.
    ///
    /// Used by orphan cleanup to determine which EdgeType nodes still have
    /// corresponding edges. Default implementation fetches the full graph via
    /// `get_graph_data()` and collects distinct relationship names.
    /// Backends may override with a more efficient query.
    async fn get_all_relationship_names(&self) -> GraphDBResult<HashSet<String>> {
        let (_, edges) = self.get_graph_data().await?;
        Ok(edges.into_iter().map(|(_, _, rel, _)| rel).collect())
    }

    /// Find EdgeType nodes in the graph that have zero edges (degree 0).
    ///
    /// Used by hard-delete orphan sweep to find EdgeType nodes whose
    /// relationship name no longer appears in any edge.
    ///
    /// Default implementation fetches the full graph and filters in memory.
    /// Backends may override with a more efficient query.
    async fn get_zero_degree_edge_type_nodes(&self) -> GraphDBResult<Vec<crate::GraphNode>> {
        let (nodes, edges) = self.get_graph_data().await?;

        // Collect all relationship names still in use
        let active_rel_names: HashSet<&str> =
            edges.iter().map(|(_, _, rel, _)| rel.as_str()).collect();

        // Build a degree map from edges
        let mut degree: HashMap<String, usize> = HashMap::new();
        for (src, tgt, _, _) in &edges {
            *degree.entry(src.clone()).or_default() += 1;
            *degree.entry(tgt.clone()).or_default() += 1;
        }

        Ok(nodes
            .into_iter()
            .filter(|(id, props)| {
                let is_edge_type = props
                    .get("type")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| t == "EdgeType");
                if !is_edge_type {
                    return false;
                }
                // Check degree is 0 (no edges at all)
                let deg = degree.get(id).copied().unwrap_or(0);
                if deg > 0 {
                    return false;
                }
                // Also check that the relationship_name is not in any edge
                let rel_name = props
                    .get("relationship_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                !active_rel_names.contains(rel_name)
            })
            .collect())
    }

    /// Update a single property on a node.
    ///
    /// # Arguments
    /// * `node_id` - The node identifier
    /// * `key` - Property name
    /// * `value` - New property value
    ///
    /// Default implementation fetches the node and its edges, modifies the
    /// property, removes the old node (which may cascade-delete edges), re-adds
    /// the node, and restores the edges. Backends should override with an
    /// in-place `SET` operation for better performance and atomicity.
    async fn update_node_property(
        &self,
        node_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> GraphDBResult<()> {
        let node = self
            .get_node(node_id)
            .await?
            .ok_or_else(|| crate::GraphDBError::NodeError(format!("Node not found: {node_id}")))?;

        // Save edges before deleting the node, since delete_node may cascade.
        let edges = self.get_edges(node_id).await.unwrap_or_default();

        let mut props = serde_json::Map::new();
        for (k, v) in node {
            props.insert(k.into_owned(), v);
        }
        props.insert(key.to_string(), value);

        self.delete_node(node_id).await?;
        self.add_node_raw(Value::Object(props)).await?;

        // Restore edges that were removed by the cascade delete.
        if !edges.is_empty() {
            self.add_edges(&edges).await?;
        }

        Ok(())
    }

    /// Update a single property on an edge.
    ///
    /// # Arguments
    /// * `source_id` - Source node ID
    /// * `target_id` - Target node ID
    /// * `relationship_name` - Edge label/relationship type
    /// * `key` - Property name
    /// * `value` - New property value
    ///
    /// Default implementation is a no-op that logs a warning. Backends that
    /// support in-place edge property updates should override this method.
    async fn update_edge_property(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        key: &str,
        value: serde_json::Value,
    ) -> GraphDBResult<()> {
        let _ = (source_id, target_id, relationship_name, key, value);
        tracing::warn!(
            "update_edge_property not implemented for this backend; \
             edge {source_id} -> {target_id} ({relationship_name}) property {key} not updated"
        );
        Ok(())
    }

    /// Batch-fetch `feedback_weight` values for the given node IDs.
    ///
    /// Returns only IDs that exist and have a numeric `feedback_weight`
    /// property. IDs missing from the graph or missing the property are
    /// omitted from the result map.
    ///
    /// Default implementation calls [`get_node`] per id; backends should
    /// override with a single batch query for efficiency.
    async fn get_node_feedback_weights(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<HashMap<String, f64>> {
        let mut out = HashMap::with_capacity(node_ids.len());
        for id in node_ids {
            if let Some(node) = self.get_node(id).await?
                && let Some(v) = node.get("feedback_weight").and_then(|v| v.as_f64())
            {
                out.insert(id.clone(), v);
            }
        }
        Ok(out)
    }

    /// Batch-write `feedback_weight` values on the given nodes.
    ///
    /// Returns a map `node_id -> success` indicating whether each update
    /// succeeded. Default implementation delegates to `update_node_property`
    /// for each id; backends should override with a single batch query.
    async fn set_node_feedback_weights(
        &self,
        updates: &HashMap<String, f64>,
    ) -> GraphDBResult<HashMap<String, bool>> {
        let mut out = HashMap::with_capacity(updates.len());
        for (id, w) in updates {
            let ok = self
                .update_node_property(id, "feedback_weight", serde_json::json!(w))
                .await
                .is_ok();
            out.insert(id.clone(), ok);
        }
        Ok(out)
    }

    /// Batch-fetch per-node truth-subspace state for the given node IDs.
    ///
    /// Unlike [`get_node_feedback_weights`], which omits nodes lacking the
    /// property, this method returns an entry for **every node that exists** in
    /// the graph, defaulting `truth_alignment` to `vec![]` and `truth_epoch` to
    /// the `-1` "never scored" sentinel when the properties are absent or
    /// malformed. Nodes not present in the graph are omitted.
    ///
    /// `truth_epoch` is read from either a JSON number or a numeric JSON string
    /// (e.g. `"3"`); anything else falls back to `-1`. `truth_alignment` is
    /// read from a JSON array (non-numeric elements dropped), else `vec![]`.
    ///
    /// Default implementation calls [`get_node`] per id; backends should
    /// override with a single batch query for efficiency.
    ///
    /// [`get_node`]: GraphDBTrait::get_node
    /// [`get_node_feedback_weights`]: GraphDBTrait::get_node_feedback_weights
    async fn get_node_truth_state(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<HashMap<String, NodeTruthState>> {
        let mut out = HashMap::with_capacity(node_ids.len());
        for id in node_ids {
            if let Some(node) = self.get_node(id).await? {
                out.insert(
                    id.clone(),
                    NodeTruthState {
                        truth_alignment: extract_truth_alignment(node.get("truth_alignment")),
                        truth_epoch: extract_truth_epoch(node.get("truth_epoch")),
                    },
                );
            }
        }
        Ok(out)
    }

    /// Batch-write per-node truth-subspace state on the given nodes.
    ///
    /// Stores `truth_alignment` as a JSON array of numbers and `truth_epoch` as
    /// a JSON number. Returns a map `node_id -> success`; an entry is `true`
    /// only if **both** the `truth_alignment` and `truth_epoch` writes succeed.
    ///
    /// The base-trait default issues two [`update_node_property`] round-trips
    /// per id (each of which may itself be a full delete+re-add on backends
    /// that do not override `update_node_property`), so backends should
    /// override with a single batch query for efficiency.
    ///
    /// [`update_node_property`]: GraphDBTrait::update_node_property
    async fn set_node_truth_state(
        &self,
        updates: &HashMap<String, NodeTruthState>,
    ) -> GraphDBResult<HashMap<String, bool>> {
        let mut out = HashMap::with_capacity(updates.len());
        for (id, state) in updates {
            let align_ok = self
                .update_node_property(
                    id,
                    "truth_alignment",
                    serde_json::json!(state.truth_alignment),
                )
                .await
                .is_ok();
            let epoch_ok = self
                .update_node_property(id, "truth_epoch", serde_json::json!(state.truth_epoch))
                .await
                .is_ok();
            out.insert(id.clone(), align_ok && epoch_ok);
        }
        Ok(out)
    }

    /// Batch-fetch `feedback_weight` values for the given edges.
    ///
    /// Default implementation returns an empty map and logs a warning,
    /// because the generic `GraphDBTrait` does not expose a per-edge
    /// property read. Backends that support edge-property queries should
    /// override this method.
    async fn get_edge_feedback_weights(
        &self,
        edge_keys: &[EdgeKey],
    ) -> GraphDBResult<HashMap<EdgeKey, f64>> {
        if !edge_keys.is_empty() {
            tracing::warn!(
                "get_edge_feedback_weights not implemented for this backend; \
                 returning empty map for {} edge(s)",
                edge_keys.len()
            );
        }
        Ok(HashMap::new())
    }

    /// Batch-write `feedback_weight` values on the given edges.
    ///
    /// Default implementation delegates to [`update_edge_property`] per
    /// edge. Backends with no edge-update support will silently succeed
    /// (because the default `update_edge_property` returns `Ok(())` with
    /// a warning).
    async fn set_edge_feedback_weights(
        &self,
        updates: &HashMap<EdgeKey, f64>,
    ) -> GraphDBResult<HashMap<EdgeKey, bool>> {
        let mut out = HashMap::with_capacity(updates.len());
        for (key, w) in updates {
            let ok = self
                .update_edge_property(
                    &key.0,
                    &key.1,
                    &key.2,
                    "feedback_weight",
                    serde_json::json!(w),
                )
                .await
                .is_ok();
            out.insert(key.clone(), ok);
        }
        Ok(out)
    }

    /// Retrieve a subgraph containing only the specified nodes and edges between them.
    ///
    /// Default implementation fetches the full graph and filters in memory.
    /// Backends may override this with a more efficient query.
    async fn get_id_filtered_graph_data(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        if node_ids.is_empty() {
            return Ok((vec![], vec![]));
        }
        let (all_nodes, all_edges) = self.get_graph_data().await?;
        let id_set: std::collections::HashSet<&str> = node_ids.iter().map(String::as_str).collect();

        let filtered_nodes: Vec<GraphNode> = all_nodes
            .into_iter()
            .filter(|(id, _)| id_set.contains(id.as_str()))
            .collect();

        let filtered_edges: Vec<EdgeData> = all_edges
            .into_iter()
            .filter(|(src, tgt, _, _)| {
                id_set.contains(src.as_str()) && id_set.contains(tgt.as_str())
            })
            .collect();

        Ok((filtered_nodes, filtered_edges))
    }

    /// Fetch **candidate** nodes whose type-ish label contains `needle`,
    /// without materialising the edge half of the graph.
    ///
    /// The predicate callers actually want is [`node_label_contains`]: any of
    /// [`NODE_LABEL_KEYS`] containing `needle` ASCII-case-insensitively, over
    /// both string and string-array values. That is a substring test across
    /// five keys, four of which live inside the JSON `properties` blob, so it
    /// is **not** expressible through
    /// [`get_filtered_graph_data`](GraphDBTrait::get_filtered_graph_data), whose
    /// contract is an exact `IN (…)` match on the `id`/`name`/`type` columns
    /// (`PgGraphAdapter` even rejects any other attribute name outright).
    ///
    /// So this method is deliberately specified as a *narrowing* read:
    /// implementations may return a **superset** of the exact predicate, and
    /// callers MUST re-apply [`node_label_contains`] to the rows that come
    /// back. What every implementation does guarantee is that no node matching
    /// the exact predicate is missing, and that no edge row is read at all.
    ///
    /// Default implementation falls back to the full graph load and returns its
    /// node half — no worse than what a caller would have done by hand, and it
    /// keeps out-of-tree backends compiling. `PgGraphAdapter` and
    /// `LadybugAdapter` override it.
    async fn get_candidate_nodes_by_label(&self, needle: &str) -> GraphDBResult<Vec<GraphNode>> {
        let _ = needle;
        Ok(self.get_graph_data().await?.0)
    }

    /// Fetch the raw k-hop neighborhood subgraph around a set of seed node ids.
    ///
    /// Returns every node reachable within `depth` hops of any seed, together
    /// with every edge whose endpoints are both in that resolved set, in the
    /// same `(nodes, edges)` shape as
    /// [`get_graph_data`](GraphDBTrait::get_graph_data). Edges preserve their
    /// **true stored** `(source_id, target_id)` direction; the caller is
    /// responsible for any partitioning (e.g. keeping only edges incident to a
    /// seed).
    ///
    /// Default implementation runs the same two phases as the real adapters
    /// (`PgGraphAdapter`'s recursive CTE, `LadybugAdapter`'s traversal), only
    /// through the per-node trait surface: an undirected BFS over
    /// [`get_edges`](GraphDBTrait::get_edges) resolves the id set out to
    /// `depth`, then a second pass re-reads the edges incident to every
    /// resolved node and keeps the ones whose **both** endpoints are resolved —
    /// the induced subgraph the contract above promises. Collecting edges
    /// during the walk instead (only the ones incident to the current frontier)
    /// silently omits edges between two nodes discovered at the same depth,
    /// which is exactly the outermost layer of every result.
    ///
    /// Backends should still override this with a single batched query: the
    /// default costs one `get_edges` round trip per resolved node plus one
    /// batched [`get_nodes`](GraphDBTrait::get_nodes), and Ladybug must
    /// override to return direction-correct edges (its
    /// `get_connections`/`get_edges` report the queried node as the source
    /// regardless of the stored direction, so the induced-subgraph filter here
    /// would still select the right edges but the emitted direction would be
    /// wrong).
    ///
    /// What the default guarantees, given a `get_edges` that reports edges
    /// verbatim: the node set, the edge set, `relationship_name` and edge
    /// properties all match the overrides, including seed-to-seed edges at
    /// `depth == 0` (the previous frontier-only version returned no edges at
    /// all for that case). What it cannot repair is a backend whose
    /// `get_edges` itself rewrites direction — the tuples are passed through
    /// untouched.
    async fn get_neighborhood(
        &self,
        node_ids: &[String],
        depth: usize,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        if node_ids.is_empty() {
            return Ok((vec![], vec![]));
        }

        // Phase 1: resolve the id set by undirected BFS out to `depth`. The
        // edges read here only serve to find the next layer; keeping *them* as
        // the result is what dropped the same-depth edges.
        let mut resolved: HashSet<String> = HashSet::new();
        let mut frontier: Vec<String> = Vec::new();
        for id in node_ids {
            if resolved.insert(id.clone()) {
                frontier.push(id.clone());
            }
        }

        // Edges already fetched, keyed by the node they were fetched for, so
        // phase 2 only pays for the layer the BFS never expanded.
        let mut fetched: HashMap<String, Vec<EdgeData>> = HashMap::new();

        for _ in 0..depth {
            let mut next_frontier: Vec<String> = Vec::new();
            for id in &frontier {
                let incident = self.get_edges(id).await?;
                for (src, tgt, _, _) in &incident {
                    // Undirected step: whichever endpoint is not the queried
                    // node is a candidate for the next layer. A self-loop
                    // yields `id` itself, which is already resolved.
                    let neighbor = if src == id { tgt } else { src };
                    if resolved.insert(neighbor.clone()) {
                        next_frontier.push(neighbor.clone());
                    }
                }
                fetched.insert(id.clone(), incident);
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }

        // Phase 2: the induced subgraph over `resolved`. Every edge of that set
        // is incident to at least one of its nodes, so reading the incident
        // edges of all of them and dropping the ones with an unresolved
        // endpoint yields exactly the induced edge set. Each surviving edge is
        // seen twice (once per endpoint), hence the `EdgeKey` dedup.
        //
        // Sorted ids keep the output order deterministic across runs, which a
        // `HashSet` iteration would not.
        let mut resolved_ids: Vec<String> = resolved.iter().cloned().collect();
        resolved_ids.sort();

        let mut edges: Vec<EdgeData> = Vec::new();
        let mut edge_keys: HashSet<EdgeKey> = HashSet::new();

        for id in &resolved_ids {
            let incident = match fetched.remove(id) {
                Some(incident) => incident,
                None => self.get_edges(id).await?,
            };
            for edge in incident {
                let (src, tgt, rel, _) = &edge;
                if !resolved.contains(src.as_str()) || !resolved.contains(tgt.as_str()) {
                    continue;
                }
                if edge_keys.insert((src.clone(), tgt.clone(), rel.clone())) {
                    edges.push(edge);
                }
            }
        }

        // One batched read for the node half. A resolved id with no row in the
        // node store is dropped, matching the overrides — their node halves
        // select from `graph_node`/`(n:Node)` and cannot invent a row either.
        let nodes: Vec<GraphNode> = self
            .get_nodes(&resolved_ids)
            .await?
            .into_iter()
            .filter_map(|data| {
                let id = data.get("id").and_then(|v| v.as_str())?.to_string();
                Some((id, data))
            })
            .collect();

        Ok((nodes, edges))
    }
}

/// Extension trait providing generic convenience methods on top of [`GraphDBTrait`].
/// Auto-implemented for all types that implement `GraphDBTrait`.
#[async_trait]
pub trait GraphDBTraitExt: GraphDBTrait {
    /// Add a single node to the graph.
    async fn add_node<T: Serialize + Sync>(&self, node: &T) -> GraphDBResult<()> {
        let value = serde_json::to_value(node).map_err(|e| {
            crate::GraphDBError::QueryError(format!("Failed to serialize node: {e}"))
        })?;
        self.add_node_raw(value).await
    }

    /// Add multiple nodes in a batch operation.
    async fn add_nodes<T: Serialize + Sync>(&self, nodes: &[&T]) -> GraphDBResult<()> {
        let values: Vec<Value> = nodes
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()
            .map_err(|e| {
                crate::GraphDBError::QueryError(format!("Failed to serialize nodes: {e}"))
            })?;
        self.add_nodes_raw(values).await
    }
}

impl<T: GraphDBTrait + ?Sized> GraphDBTraitExt for T {}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::mock::MockGraphDB;

    /// Delegating wrapper that forwards every *required* trait method to an
    /// inner [`MockGraphDB`] but deliberately does NOT override
    /// `get_node_truth_state`, `set_node_truth_state`, or
    /// `update_node_property` — so exercising those on this type runs the
    /// base-trait DEFAULT implementations (the mock overrides them, so it
    /// cannot cover the default path itself).
    #[derive(Default)]
    struct DefaultImplDb(MockGraphDB);

    #[async_trait]
    impl GraphDBTrait for DefaultImplDb {
        async fn initialize(&self) -> GraphDBResult<()> {
            self.0.initialize().await
        }
        async fn is_empty(&self) -> GraphDBResult<bool> {
            self.0.is_empty().await
        }
        async fn query(
            &self,
            query: &str,
            params: Option<HashMap<Cow<'static, str>, Value>>,
        ) -> GraphDBResult<Vec<Vec<Value>>> {
            self.0.query(query, params).await
        }
        async fn delete_graph(&self) -> GraphDBResult<()> {
            self.0.delete_graph().await
        }
        async fn has_node(&self, node_id: &str) -> GraphDBResult<bool> {
            self.0.has_node(node_id).await
        }
        async fn add_node_raw(&self, node: Value) -> GraphDBResult<()> {
            self.0.add_node_raw(node).await
        }
        async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()> {
            self.0.add_nodes_raw(nodes).await
        }
        async fn delete_node(&self, node_id: &str) -> GraphDBResult<()> {
            self.0.delete_node(node_id).await
        }
        async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()> {
            self.0.delete_nodes(node_ids).await
        }
        async fn get_node(&self, node_id: &str) -> GraphDBResult<Option<NodeData>> {
            self.0.get_node(node_id).await
        }
        async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
            self.0.get_nodes(node_ids).await
        }
        async fn has_edge(
            &self,
            source_id: &str,
            target_id: &str,
            relationship_name: &str,
        ) -> GraphDBResult<bool> {
            self.0
                .has_edge(source_id, target_id, relationship_name)
                .await
        }
        async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
            self.0.has_edges(edges).await
        }
        async fn add_edge(
            &self,
            source_id: &str,
            target_id: &str,
            relationship_name: &str,
            properties: Option<HashMap<Cow<'static, str>, Value>>,
        ) -> GraphDBResult<()> {
            self.0
                .add_edge(source_id, target_id, relationship_name, properties)
                .await
        }
        async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
            self.0.add_edges(edges).await
        }
        async fn get_edges(&self, node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
            self.0.get_edges(node_id).await
        }
        async fn get_neighbors(&self, node_id: &str) -> GraphDBResult<Vec<NodeData>> {
            self.0.get_neighbors(node_id).await
        }
        async fn get_connections(
            &self,
            node_id: &str,
        ) -> GraphDBResult<Vec<(NodeData, HashMap<Cow<'static, str>, Value>, NodeData)>> {
            self.0.get_connections(node_id).await
        }
        async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            self.0.get_graph_data().await
        }
        async fn get_graph_metrics(
            &self,
            include_optional: bool,
        ) -> GraphDBResult<HashMap<Cow<'static, str>, Value>> {
            self.0.get_graph_metrics(include_optional).await
        }
        async fn get_filtered_graph_data(
            &self,
            attribute_filters: &HashMap<Cow<'static, str>, Vec<Value>>,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            self.0.get_filtered_graph_data(attribute_filters).await
        }
        async fn get_nodeset_subgraph(
            &self,
            node_type: &str,
            node_names: &[String],
            node_name_filter_operator: &str,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            self.0
                .get_nodeset_subgraph(node_type, node_names, node_name_filter_operator)
                .await
        }
    }

    /// Seed a small graph on `db`: nodes `ids`, edges `edges`.
    async fn seed(db: &DefaultImplDb, ids: &[&str], edges: &[(&str, &str, &str)]) {
        for id in ids {
            db.add_node_raw(serde_json::json!({"id": *id, "name": *id, "type": "T"}))
                .await
                .unwrap();
        }
        for (src, tgt, rel) in edges {
            db.add_edge(src, tgt, rel, None).await.unwrap();
        }
    }

    fn edge_keys(edges: &[EdgeData]) -> Vec<EdgeKey> {
        let mut keys: Vec<EdgeKey> = edges
            .iter()
            .map(|(s, t, r, _)| (s.clone(), t.clone(), r.clone()))
            .collect();
        keys.sort();
        keys
    }

    fn node_ids(nodes: &[GraphNode]) -> Vec<String> {
        let mut ids: Vec<String> = nodes.iter().map(|(id, _)| id.clone()).collect();
        ids.sort();
        ids
    }

    /// The contract says "every edge whose endpoints are both in that resolved
    /// set". The frontier-only walk this replaced never queried the outermost
    /// layer, so `b -> c` — both ends at depth 1 — went missing.
    #[tokio::test]
    async fn default_neighborhood_returns_the_induced_subgraph() {
        let db = DefaultImplDb::default();
        seed(
            &db,
            &["a", "b", "c"],
            &[("a", "b", "r1"), ("a", "c", "r2"), ("b", "c", "r3")],
        )
        .await;

        let (nodes, edges) = db
            .get_neighborhood(&["a".to_string()], 1)
            .await
            .expect("neighborhood");

        assert_eq!(node_ids(&nodes), vec!["a", "b", "c"]);
        assert_eq!(
            edge_keys(&edges),
            vec![
                ("a".to_string(), "b".to_string(), "r1".to_string()),
                ("a".to_string(), "c".to_string(), "r2".to_string()),
                ("b".to_string(), "c".to_string(), "r3".to_string()),
            ],
            "the edge between the two depth-1 nodes must be included"
        );
    }

    /// `depth == 0` used to return the seeds with no edges at all, which is not
    /// what `PgGraphAdapter`/`LadybugAdapter`/`MockGraphDB` do — they return the
    /// seed-to-seed edges. The default now agrees with them.
    #[tokio::test]
    async fn default_neighborhood_includes_seed_to_seed_edges_at_depth_zero() {
        let db = DefaultImplDb::default();
        seed(&db, &["a", "b", "c"], &[("a", "b", "r1"), ("b", "c", "r2")]).await;

        let (nodes, edges) = db
            .get_neighborhood(&["a".to_string(), "b".to_string()], 0)
            .await
            .expect("neighborhood");

        assert_eq!(node_ids(&nodes), vec!["a", "b"]);
        assert_eq!(
            edge_keys(&edges),
            vec![("a".to_string(), "b".to_string(), "r1".to_string())],
        );
    }

    /// Edges are read through `get_edges`, which carries the relationship name;
    /// the previous walk read them through `get_connections`, which does not, so
    /// every edge came back with an empty `relationship_name` and two parallel
    /// edges collapsed into one.
    #[tokio::test]
    async fn default_neighborhood_preserves_relationship_names_and_direction() {
        let db = DefaultImplDb::default();
        seed(
            &db,
            &["a", "b"],
            &[("b", "a", "authored"), ("b", "a", "reviewed")],
        )
        .await;

        let (_, edges) = db
            .get_neighborhood(&["a".to_string()], 1)
            .await
            .expect("neighborhood");

        assert_eq!(
            edge_keys(&edges),
            vec![
                ("b".to_string(), "a".to_string(), "authored".to_string()),
                ("b".to_string(), "a".to_string(), "reviewed".to_string()),
            ],
            "both parallel edges survive, with the stored b -> a direction"
        );
    }

    /// The depth boundary itself is an equivalence pin — the frontier-only walk
    /// stopped at `depth` too, and dropped the edge leaving the set rather than
    /// dragging its far endpoint in. The `relationship_name` in the expectation
    /// is not: that half fails before the fix.
    #[tokio::test]
    async fn default_neighborhood_stops_at_depth_and_drops_dangling_edges() {
        let db = DefaultImplDb::default();
        seed(&db, &["a", "b", "c"], &[("a", "b", "r1"), ("b", "c", "r2")]).await;

        let (nodes, edges) = db
            .get_neighborhood(&["a".to_string()], 1)
            .await
            .expect("neighborhood");

        assert_eq!(node_ids(&nodes), vec!["a", "b"]);
        assert_eq!(
            edge_keys(&edges),
            vec![("a".to_string(), "b".to_string(), "r1".to_string())],
        );
    }

    /// Equivalence pin (passes before and after the fix): an isolated seed still
    /// appears as a node, and a seed with no row in the store is still dropped,
    /// matching every override. The seed half moved from a per-seed `get_node`
    /// to one batched `get_nodes`, so it is worth holding in place.
    #[tokio::test]
    async fn default_neighborhood_keeps_isolated_seeds_and_drops_unknown_ids() {
        let db = DefaultImplDb::default();
        seed(&db, &["lonely"], &[]).await;

        let (nodes, edges) = db
            .get_neighborhood(&["lonely".to_string(), "ghost".to_string()], 1)
            .await
            .expect("neighborhood");

        assert_eq!(node_ids(&nodes), vec!["lonely"]);
        assert!(edges.is_empty());
    }

    #[test]
    fn node_label_contains_scans_every_label_key_case_insensitively() {
        use serde_json::json;

        let mut data = NodeData::new();
        data.insert(Cow::Borrowed("type"), json!("DocumentChunk"));
        assert!(!node_label_contains(&data, "interaction"));

        // A key that only exists inside the JSON properties blob still counts.
        data.insert(Cow::Borrowed("kind"), json!("UserINTERACTION"));
        assert!(node_label_contains(&data, "interaction"));

        // Array-valued labels (Neo4j-style multi-label) match element-wise.
        let mut arr = NodeData::new();
        arr.insert(Cow::Borrowed("labels"), json!(["Node", "CodingRule"]));
        assert!(node_label_contains(&arr, "rule"));
        assert!(!node_label_contains(&arr, "interaction"));

        // Non-string values are ignored rather than stringified.
        let mut num = NodeData::new();
        num.insert(Cow::Borrowed("kind"), json!(42));
        assert!(!node_label_contains(&num, "4"));

        // Keys outside NODE_LABEL_KEYS are not consulted.
        let mut other = NodeData::new();
        other.insert(Cow::Borrowed("text"), json!("a rule about rules"));
        assert!(!node_label_contains(&other, "rule"));
    }

    /// The default `get_candidate_nodes_by_label` is a documented fallback to
    /// the full graph load, so it must return the node half *unfiltered* — the
    /// caller's own predicate is what narrows it.
    #[tokio::test]
    async fn default_candidate_nodes_by_label_returns_the_whole_node_half() {
        let db = DefaultImplDb::default();
        seed(&db, &["a", "b"], &[("a", "b", "r1")]).await;

        let nodes = db
            .get_candidate_nodes_by_label("interaction")
            .await
            .expect("candidates");
        assert_eq!(node_ids(&nodes), vec!["a", "b"]);
    }

    /// The defaulted `close()` must be a no-op *and* idempotent for a backend
    /// that owns nothing closable — that is what lets all in-tree impls (and
    /// out-of-tree adapters) keep compiling untouched after the hook is added.
    #[tokio::test]
    async fn default_close_is_a_noop_and_idempotent() {
        let db = DefaultImplDb::default();
        assert!(db.close().await.is_ok());
        assert!(db.close().await.is_ok());
        // A no-op close must not disturb the store.
        db.add_node_raw(serde_json::json!({"id": "c1", "name": "C1", "type": "T"}))
            .await
            .unwrap();
        assert!(db.has_node("c1").await.unwrap());
    }

    #[tokio::test]
    async fn default_truth_state_round_trip_and_sentinel() {
        let db = DefaultImplDb::default();
        db.add_node_raw(serde_json::json!({"id": "d1", "name": "D1", "type": "T"}))
            .await
            .unwrap();
        db.add_node_raw(serde_json::json!({"id": "d2", "name": "D2", "type": "T"}))
            .await
            .unwrap();

        // Missing epoch/alignment default to the -1 sentinel and [] via the
        // default get_node_truth_state.
        let before = db.get_node_truth_state(&["d1".to_string()]).await.unwrap();
        let d1 = before.get("d1").expect("d1 present with defaults");
        assert_eq!(d1.truth_alignment, Vec::<f64>::new());
        assert_eq!(d1.truth_epoch, -1);

        // Default set writes both properties via the default update_node_property.
        let mut updates = HashMap::new();
        updates.insert(
            "d1".to_string(),
            NodeTruthState {
                truth_alignment: vec![0.5, 0.25],
                truth_epoch: 0,
            },
        );
        updates.insert(
            "d2".to_string(),
            NodeTruthState {
                truth_alignment: vec![1.0],
                truth_epoch: 9,
            },
        );
        let set_res = db.set_node_truth_state(&updates).await.unwrap();
        assert_eq!(set_res.get("d1"), Some(&true));
        assert_eq!(set_res.get("d2"), Some(&true));

        let got = db
            .get_node_truth_state(&["d1".to_string(), "d2".to_string()])
            .await
            .unwrap();
        let d1 = got.get("d1").expect("d1 present");
        assert_eq!(d1.truth_alignment, vec![0.5, 0.25]);
        // Real epoch 0 must survive as 0, not the -1 sentinel.
        assert_eq!(d1.truth_epoch, 0);
        let d2 = got.get("d2").expect("d2 present");
        assert_eq!(d2.truth_alignment, vec![1.0]);
        assert_eq!(d2.truth_epoch, 9);

        // Absent node omitted from the result map.
        let missing = db
            .get_node_truth_state(&["ghost".to_string()])
            .await
            .unwrap();
        assert!(!missing.contains_key("ghost"));
    }

    #[test]
    fn extract_truth_epoch_accepts_int_float_and_sentinels() {
        use serde_json::json;

        // Plain JSON integer is unchanged.
        assert_eq!(extract_truth_epoch(Some(&json!(3))), 3);
        // Numeric JSON string parses.
        assert_eq!(extract_truth_epoch(Some(&json!("3"))), 3);
        // Float-encoded epoch is accepted (matches Python `int(3.0)`).
        assert_eq!(extract_truth_epoch(Some(&json!(3.0))), 3);
        // Fractional float truncates toward zero (matches Python `int(3.9)`).
        assert_eq!(extract_truth_epoch(Some(&json!(3.9))), 3);
        assert_eq!(extract_truth_epoch(Some(&json!(-2.9))), -2);

        // Non-numeric / missing values fall back to the -1 sentinel.
        assert_eq!(extract_truth_epoch(Some(&json!("bad"))), -1);
        assert_eq!(extract_truth_epoch(Some(&json!(null))), -1);
        assert_eq!(extract_truth_epoch(None), -1);

        // NaN/inf cannot be represented as a serde_json number
        // (`Number::from_f64` returns None for them), so the `is_finite`
        // guard is defensive: any `Value` that reaches `as_f64()` is finite.
        assert!(serde_json::Number::from_f64(f64::NAN).is_none());
        assert!(serde_json::Number::from_f64(f64::INFINITY).is_none());
    }
}
