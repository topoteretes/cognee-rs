//! Knowledge graph data models.
//!
//! Port of Python's cognee/shared/data_models.py
//! These models represent the extracted knowledge graph structure:
//! - Node: Entities and concepts in the graph
//! - Edge: Relationships between nodes
//! - KnowledgeGraph: Collection of nodes and edges

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Marker trait for types that can be used as graph extraction models.
///
/// Types implementing this trait can be extracted from text via LLM
/// structured output. The LLM generates JSON conforming to the type's
/// [`JsonSchema`], which is then deserialized into the concrete type.
///
/// The built-in [`KnowledgeGraph`] model implements this trait with
/// `is_default_knowledge_graph() == true`, which triggers additional
/// post-processing (entity/edge expansion, deduplication, graph DB storage).
/// Custom models return `false`, causing the extracted value to be stored
/// directly in [`DocumentChunk::contains`] as serialized JSON — mirroring
/// the Python branching at `extract_graph_from_data.py:99-103`.
///
/// # Required bounds
/// `Serialize + DeserializeOwned + JsonSchema + Clone + Send + Sync + 'static`
pub trait GraphModel:
    Serialize + DeserializeOwned + JsonSchema + Clone + Send + Sync + 'static
{
    /// Returns `true` if this is the built-in [`KnowledgeGraph`] model.
    ///
    /// Custom models should leave the default (`false`), which changes
    /// the processing flow: extracted data is stored as-is in chunk metadata
    /// instead of being expanded into graph nodes and edges.
    fn is_default_knowledge_graph() -> bool {
        false
    }
}

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
/// * `description` - Concrete one-sentence fact expressed by this edge
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Edge {
    /// ID of the source node
    pub source_node_id: String,

    /// ID of the target node
    pub target_node_id: String,

    /// Type of relationship (snake_case, e.g., "works_at", "founded", "located_in")
    pub relationship_name: String,

    /// Concrete one-sentence fact expressed by this edge, using endpoint names.
    /// Mirrors Python `KnowledgeGraph.Edge.description` (data_models.py:62-71).
    /// Becomes the `edge_text` graph-edge property, feeding EdgeType + Triplet
    /// embeddings. Optional because older/custom outputs may omit it.
    #[serde(default)]
    pub description: Option<String>,
}

/// Knowledge graph extracted from text.
///
/// Contains nodes (entities/concepts) and edges (relationships).
/// This is the primary output of fact extraction.
///
/// # Fields
/// * `nodes` - List of extracted entities and concepts
/// * `edges` - List of relationships between nodes
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

impl GraphModel for KnowledgeGraph {
    fn is_default_knowledge_graph() -> bool {
        true
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
            description: None,
        };

        assert_eq!(edge.relationship_name, "works_at");
    }

    #[test]
    fn test_edge_serializes_description() {
        let edge = Edge {
            source_node_id: "alice".to_string(),
            target_node_id: "acme".to_string(),
            relationship_name: "founded".to_string(),
            description: Some("Alice founded Acme".to_string()),
        };

        let json = serde_json::to_string(&edge).unwrap();
        assert!(json.contains("\"description\":\"Alice founded Acme\""));
    }

    #[test]
    fn test_edge_deserializes_without_description() {
        // Back-compat: JSON omitting `description` defaults to None.
        let json = r#"{
            "source_node_id": "alice",
            "target_node_id": "acme",
            "relationship_name": "founded"
        }"#;
        let edge: Edge = serde_json::from_str(json).unwrap();
        assert_eq!(edge.relationship_name, "founded");
        assert_eq!(edge.description, None);
    }

    #[test]
    fn test_edge_deserializes_with_description() {
        let json = r#"{
            "source_node_id": "alice",
            "target_node_id": "acme",
            "relationship_name": "founded",
            "description": "Alice founded Acme"
        }"#;
        let edge: Edge = serde_json::from_str(json).unwrap();
        assert_eq!(edge.description.as_deref(), Some("Alice founded Acme"));
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
            description: None,
        });

        assert!(!graph.is_empty());
        assert_eq!(graph.node_count(), 1);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn test_knowledge_graph_is_default() {
        assert!(KnowledgeGraph::is_default_knowledge_graph());
    }

    /// A custom graph model for testing the `GraphModel` trait.
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct CustomModel {
        items: Vec<String>,
    }

    impl GraphModel for CustomModel {}

    #[test]
    fn test_custom_model_is_not_default() {
        assert!(!CustomModel::is_default_knowledge_graph());
    }

    #[test]
    fn test_custom_model_roundtrip() {
        let model = CustomModel {
            items: vec!["a".to_string(), "b".to_string()],
        };
        let json = serde_json::to_string(&model).unwrap();
        let deserialized: CustomModel = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.items, vec!["a", "b"]);
    }
}
