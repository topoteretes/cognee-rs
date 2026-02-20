//! Mock graph database implementation for testing.
//!
//! Provides an in-memory HashMap-based implementation of GraphDBTrait
//! for use in unit tests.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Serialize;

use crate::{EdgeData, GraphDBError, GraphDBResult, GraphDBTrait, NodeData};

/// In-memory mock graph database for testing.
///
/// Thread-safe implementation using Arc<Mutex<>> for interior mutability.
#[derive(Clone)]
pub struct MockGraphDB {
    nodes: Arc<Mutex<HashMap<String, NodeData>>>,
    edges: Arc<Mutex<Vec<EdgeData>>>,
}

impl MockGraphDB {
    /// Create a new empty mock graph database.
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(Mutex::new(HashMap::new())),
            edges: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get the current node count (for testing).
    pub fn node_count(&self) -> usize {
        self.nodes.lock().unwrap().len()
    }

    /// Get the current edge count (for testing).
    pub fn edge_count(&self) -> usize {
        self.edges.lock().unwrap().len()
    }

    /// Clear all data (for testing).
    pub fn clear(&self) {
        self.nodes.lock().unwrap().clear();
        self.edges.lock().unwrap().clear();
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
        Ok(self.nodes.lock().unwrap().is_empty())
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
        Ok(self.nodes.lock().unwrap().contains_key(node_id))
    }

    async fn add_node<T: Serialize + Sync>(&self, node: &T) -> GraphDBResult<()> {
        let json = serde_json::to_value(node)?;

        let mut node_data = HashMap::new();
        if let serde_json::Value::Object(map) = json {
            for (k, v) in map {
                node_data.insert(Cow::from(k), v);
            }
        }

        let id = node_data
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GraphDBError::NodeError("Node missing 'id' field".to_string()))?
            .to_string();

        self.nodes.lock().unwrap().insert(id, node_data);
        Ok(())
    }

    async fn add_nodes<T: Serialize + Sync>(&self, nodes: &[&T]) -> GraphDBResult<()> {
        for node in nodes {
            self.add_node(*node).await?;
        }
        Ok(())
    }

    async fn delete_node(&self, node_id: &str) -> GraphDBResult<()> {
        self.nodes.lock().unwrap().remove(node_id);
        Ok(())
    }

    async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()> {
        let mut nodes = self.nodes.lock().unwrap();
        for node_id in node_ids {
            nodes.remove(node_id);
        }
        Ok(())
    }

    async fn get_node(&self, node_id: &str) -> GraphDBResult<Option<NodeData>> {
        Ok(self.nodes.lock().unwrap().get(node_id).cloned())
    }

    async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
        let nodes = self.nodes.lock().unwrap();
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
        let edges = self.edges.lock().unwrap();
        Ok(edges.iter().any(|(src, tgt, rel, _)| {
            src == source_id && tgt == target_id && rel == relationship_name
        }))
    }

    async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
        let stored_edges = self.edges.lock().unwrap();
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
        self.edges.lock().unwrap().push(edge);
        Ok(())
    }

    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
        let mut stored_edges = self.edges.lock().unwrap();
        for edge in edges {
            stored_edges.push(edge.clone());
        }
        Ok(())
    }

    async fn get_edges(&self, node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
        let edges = self.edges.lock().unwrap();
        Ok(edges
            .iter()
            .filter(|(src, tgt, _, _)| src == node_id || tgt == node_id)
            .cloned()
            .collect())
    }

    async fn get_neighbors(&self, node_id: &str) -> GraphDBResult<Vec<NodeData>> {
        let edges = self.edges.lock().unwrap();
        let nodes = self.nodes.lock().unwrap();

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
        let edges = self.edges.lock().unwrap();
        let nodes = self.nodes.lock().unwrap();

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
        let nodes = self.nodes.lock().unwrap();
        let edges = self.edges.lock().unwrap();

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

    async fn get_filtered_graph_data(
        &self,
        _attribute_filters: &HashMap<Cow<'static, str>, Vec<serde_json::Value>>,
    ) -> GraphDBResult<(Vec<(String, NodeData)>, Vec<EdgeData>)> {
        self.get_graph_data().await
    }

    async fn get_nodeset_subgraph(
        &self,
        _node_type: &str,
        _node_names: &[String],
    ) -> GraphDBResult<(Vec<(String, NodeData)>, Vec<EdgeData>)> {
        self.get_graph_data().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
