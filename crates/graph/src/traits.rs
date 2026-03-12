//! Graph database trait interface.
//!
//! Defines the complete async API for graph database operations.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;

use crate::{EdgeData, GraphDBResult, GraphNode, NodeData};

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
#[async_trait]
pub trait GraphDBTrait: Send + Sync {
    /// Initialize the database schema.
    ///
    /// Creates necessary tables, indexes, and constraints.
    ///
    async fn initialize(&self) -> GraphDBResult<()>;

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
    ///
    /// Returns nodes and edges connecting them.
    ///
    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)>;
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
