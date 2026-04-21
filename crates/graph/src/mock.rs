//! Mock graph database implementation for testing.
//!
//! Provides an in-memory HashMap-based implementation of GraphDBTrait
//! for use in unit tests.

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
}

impl MockGraphDB {
    /// Create a new empty mock graph database.
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(Mutex::new(HashMap::new())),
            edges: Arc::new(Mutex::new(Vec::new())),
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
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
    /// Currently records `"get_graph_data"` and `"get_nodeset_subgraph"`.
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
        self.nodes.lock().unwrap().remove(node_id); // lock poison is unrecoverable
        Ok(())
    }

    async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()> {
        let mut nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
        for node_id in node_ids {
            nodes.remove(node_id);
        }
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

    async fn get_filtered_graph_data(
        &self,
        _attribute_filters: &HashMap<Cow<'static, str>, Vec<serde_json::Value>>,
    ) -> GraphDBResult<(Vec<(String, NodeData)>, Vec<EdgeData>)> {
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
}
