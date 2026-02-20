//! Example demonstrating JSON schema generation for structured LLM outputs.
//!
//! Run with: cargo run --example schema_example

use cognee_llm::schema::{build_schema_prompt, generate_json_schema, generate_json_schema_string};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Example: Knowledge graph node
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Node {
    /// Unique identifier for the node
    id: String,
    /// Type of entity (e.g., Person, Organization, Location)
    entity_type: String,
    /// Display name or label
    label: String,
    /// Additional properties
    #[serde(default)]
    properties: std::collections::HashMap<String, String>,
}

/// Example: Knowledge graph edge
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Edge {
    /// Source node ID
    source: String,
    /// Target node ID
    target: String,
    /// Relationship type (e.g., "KNOWS", "WORKS_FOR")
    relationship: String,
    /// Edge weight or confidence score
    #[serde(default)]
    weight: Option<f32>,
}

/// Example: Complete knowledge graph
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct KnowledgeGraph {
    /// List of all nodes in the graph
    nodes: Vec<Node>,
    /// List of all edges in the graph
    edges: Vec<Edge>,
    /// Optional metadata about the extraction
    #[serde(default)]
    metadata: Option<ExtractionMetadata>,
}

/// Metadata about the extraction process
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ExtractionMetadata {
    /// Source text that was processed
    source: String,
    /// Number of entities extracted
    entity_count: usize,
    /// Confidence score (0.0 - 1.0)
    confidence: f32,
}

fn main() {
    println!("=== JSON Schema Generation Examples ===\n");

    // Example 1: Generate schema as JSON value
    println!("1. Generate schema for Node:");
    let node_schema = generate_json_schema::<Node>();
    println!("{}\n", serde_json::to_string_pretty(&node_schema).unwrap());

    // Example 2: Generate schema as string (compact)
    println!("2. Generate compact schema string for Edge:");
    let edge_schema_compact = generate_json_schema_string::<Edge>(false);
    println!("{}\n", edge_schema_compact);

    // Example 3: Generate schema as string (pretty)
    println!("3. Generate pretty schema string for KnowledgeGraph:");
    let kg_schema = generate_json_schema_string::<KnowledgeGraph>(true);
    println!("{}\n", kg_schema);

    // Example 4: Build a complete prompt with embedded schema
    println!("4. Build complete LLM prompt with schema:");
    let prompt = build_schema_prompt::<KnowledgeGraph>(
        "Extract a knowledge graph from the following text. \
         Identify all entities (people, organizations, locations) as nodes \
         and their relationships as edges. Include confidence scores where applicable.",
    );
    println!("{}\n", prompt);

    // Example 5: Demonstrate what an LLM implementation would do
    println!("5. How an LLM adapter would use this:");
    println!("   a) Generate schema from the expected type");
    let schema = generate_json_schema::<KnowledgeGraph>();

    println!("   b) Include schema in the request (OpenAI function calling example):");
    let function_def = serde_json::json!({
        "name": "extract_knowledge_graph",
        "description": "Extract entities and relationships into a knowledge graph",
        "parameters": schema
    });
    println!(
        "   {}\n",
        serde_json::to_string_pretty(&function_def).unwrap()
    );

    println!("   c) Or embed schema in system prompt:");
    let system_prompt = build_schema_prompt::<KnowledgeGraph>(
        "You are an expert at extracting structured information. \
         Extract entities and relationships from the user's text.",
    );
    println!("   [System Prompt Length: {} chars]\n", system_prompt.len());

    // Example 6: Show typical response validation
    println!("6. Validating LLM JSON response:");
    let sample_response = r#"{
        "nodes": [
            {
                "id": "person_1",
                "entity_type": "Person",
                "label": "Alice",
                "properties": {"role": "engineer"}
            },
            {
                "id": "org_1",
                "entity_type": "Organization",
                "label": "TechCorp",
                "properties": {}
            }
        ],
        "edges": [
            {
                "source": "person_1",
                "target": "org_1",
                "relationship": "WORKS_FOR",
                "weight": 0.95
            }
        ],
        "metadata": {
            "source": "Alice works at TechCorp as an engineer",
            "entity_count": 2,
            "confidence": 0.9
        }
    }"#;

    match serde_json::from_str::<KnowledgeGraph>(sample_response) {
        Ok(kg) => {
            println!("   ✓ Valid response parsed successfully!");
            println!("   - Nodes: {}", kg.nodes.len());
            println!("   - Edges: {}", kg.edges.len());
            if let Some(meta) = kg.metadata {
                println!("   - Confidence: {}", meta.confidence);
            }
        }
        Err(e) => {
            println!("   ✗ Invalid response: {}", e);
        }
    }
}
