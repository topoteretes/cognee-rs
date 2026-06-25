//! Type definitions for graph database operations.
//!
//! Type aliases for graph data structures:
//! - NodeData: arbitrary key-value properties
//! - EdgeData: (source_id, target_id, relationship_name, properties)
//! - GraphNode: (node_id, properties)

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

/// Node data: arbitrary key-value properties
/// Uses Cow<'static, str> for keys to avoid allocating static strings
pub type NodeData = HashMap<Cow<'static, str>, serde_json::Value>;

/// Graph node: (node_id, properties)
pub type GraphNode = (String, NodeData);

/// Edge data: (source_id, target_id, relationship_name, properties)
pub type EdgeData = (
    String,
    String,
    String,
    HashMap<Cow<'static, str>, serde_json::Value>,
);

/// Structured graph edge for easier construction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Source node ID
    pub source_id: String,
    /// Target node ID
    pub target_id: String,
    /// Relationship name (edge label)
    pub relationship_name: String,
    /// Edge properties
    pub properties: HashMap<Cow<'static, str>, serde_json::Value>,
}

impl GraphEdge {
    /// Create a new graph edge
    pub fn new(source_id: String, target_id: String, relationship_name: String) -> Self {
        Self {
            source_id,
            target_id,
            relationship_name,
            properties: HashMap::new(),
        }
    }

    /// Create a new graph edge with properties
    pub fn with_properties(
        source_id: String,
        target_id: String,
        relationship_name: String,
        properties: HashMap<Cow<'static, str>, serde_json::Value>,
    ) -> Self {
        Self {
            source_id,
            target_id,
            relationship_name,
            properties,
        }
    }

    /// Convert to EdgeData tuple
    pub fn to_edge_data(self) -> EdgeData {
        (
            self.source_id,
            self.target_id,
            self.relationship_name,
            self.properties,
        )
    }

    /// Create from EdgeData tuple
    pub fn from_edge_data(edge: EdgeData) -> Self {
        Self {
            source_id: edge.0,
            target_id: edge.1,
            relationship_name: edge.2,
            properties: edge.3,
        }
    }
}

impl From<GraphEdge> for EdgeData {
    fn from(edge: GraphEdge) -> Self {
        edge.to_edge_data()
    }
}

impl From<EdgeData> for GraphEdge {
    fn from(edge: EdgeData) -> Self {
        GraphEdge::from_edge_data(edge)
    }
}
