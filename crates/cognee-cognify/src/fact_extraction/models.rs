//! Knowledge graph data models.
//!
//! Port of Python's cognee/shared/data_models.py
//! These models represent the extracted knowledge graph structure:
//! - Node: Entities and concepts in the graph
//! - Edge: Relationships between nodes
//! - KnowledgeGraph: Collection of nodes and edges

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Node in a knowledge graph.
///
/// Represents an entity or concept extracted from text.
/// Nodes are akin to Wikipedia nodes - they represent distinct entities.
///
/// # Fields
/// * `id` - Unique identifier (human-readable, not an integer)
/// * `name` - Display name of the entity
/// * `node_type` - Type classification (e.g., "PERSON", "ORGANIZATION", "CONCEPT")
/// * `description` - Brief description of the entity
///
/// # Python equivalent
/// ```python
/// class Node(BaseModel):
///     id: str
///     name: str
///     type: str  # renamed to node_type in Rust (type is a keyword)
///     description: str
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    /// Unique identifier for the node (human-readable, e.g., "Albert Einstein")
    pub id: String,

    /// Display name of the entity
    pub name: String,

    /// Entity type (e.g., "PERSON", "ORGANIZATION", "CONCEPT")
    /// Use uppercase for consistency with Python
    #[serde(rename = "type")]
    pub node_type: String,

    /// Brief description of the entity (1-2 sentences)
    pub description: String,
}

/// Edge in a knowledge graph.
///
/// Represents a relationship between two nodes.
/// Edges are akin to Wikipedia links - they connect related concepts.
///
/// # Fields
/// * `source_node_id` - ID of the source node
/// * `target_node_id` - ID of the target node
/// * `relationship_name` - Type of relationship (use snake_case, e.g., "works_at")
///
/// # Python equivalent
/// ```python
/// class Edge(BaseModel):
///     source_node_id: str
///     target_node_id: str
///     relationship_name: str
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Edge {
    /// ID of the source node
    pub source_node_id: String,

    /// ID of the target node
    pub target_node_id: String,

    /// Type of relationship (snake_case, e.g., "works_at", "founded", "located_in")
    pub relationship_name: String,
}

/// Knowledge graph extracted from text.
///
/// Contains nodes (entities/concepts) and edges (relationships).
/// This is the primary output of fact extraction.
///
/// # Fields
/// * `nodes` - List of extracted entities and concepts
/// * `edges` - List of relationships between nodes
///
/// # Python equivalent
/// ```python
/// class KnowledgeGraph(BaseModel):
///     nodes: List[Node] = Field(..., default_factory=list)
///     edges: List[Edge] = Field(..., default_factory=list)
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeGraph {
    /// List of nodes (entities and concepts)
    #[serde(default)]
    pub nodes: Vec<Node>,

    /// List of edges (relationships between nodes)
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl KnowledgeGraph {
    /// Create a new empty knowledge graph.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Check if the graph is empty (no nodes or edges).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }

    /// Get the number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get the number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

impl Default for KnowledgeGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_serialization() {
        let node = Node {
            id: "alice_johnson".to_string(),
            name: "Alice Johnson".to_string(),
            node_type: "PERSON".to_string(),
            description: "Software engineer at TechCorp".to_string(),
        };

        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains("\"type\":\"PERSON\""));

        let deserialized: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.node_type, "PERSON");
    }

    #[test]
    fn test_edge_creation() {
        let edge = Edge {
            source_node_id: "alice_johnson".to_string(),
            target_node_id: "techcorp".to_string(),
            relationship_name: "works_at".to_string(),
        };

        assert_eq!(edge.relationship_name, "works_at");
    }

    #[test]
    fn test_knowledge_graph() {
        let mut graph = KnowledgeGraph::new();
        assert!(graph.is_empty());
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);

        graph.nodes.push(Node {
            id: "alice".to_string(),
            name: "Alice".to_string(),
            node_type: "PERSON".to_string(),
            description: "A person".to_string(),
        });

        graph.edges.push(Edge {
            source_node_id: "alice".to_string(),
            target_node_id: "techcorp".to_string(),
            relationship_name: "works_at".to_string(),
        });

        assert!(!graph.is_empty());
        assert_eq!(graph.node_count(), 1);
        assert_eq!(graph.edge_count(), 1);
    }
}
