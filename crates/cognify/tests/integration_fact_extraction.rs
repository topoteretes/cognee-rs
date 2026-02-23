//! Integration tests for fact extraction using the FactExtractor.
//!
//! These tests require environment variables to be set:
//! - OPENAI_URL: Base URL for the OpenAI-compatible API
//! - OPENAI_TOKEN: API token (use "not-needed" for Ollama)
//! - OPENAI_MODEL: Model name to use for extraction
//!
//! Run with: cargo test --package cognee-cognify --test integration_fact_extraction

use cognee_cognify::{FactExtractor, KnowledgeGraph};
use cognee_llm::{Llm, OpenAIAdapter};
use std::collections::HashMap;

mod test_data;
mod test_utils;

use test_data::{TEST_TEXT_RESEARCH, TEST_TEXT_TECHCORP};
use test_utils::create_adapter_from_env;

#[tokio::test]
async fn test_fact_extraction_single_text() {
    let adapter = create_adapter_from_env();

    println!("\n🧪 Testing fact extraction with single text");
    println!("   Model: {}", adapter.model());
    println!("   Text length: {} chars", TEST_TEXT_TECHCORP.len());

    let extractor = FactExtractor::new(adapter);

    let result = extractor.extract_facts(TEST_TEXT_TECHCORP, None).await;

    match result {
        Ok(graph) => {
            println!("\n✓ Fact extraction successful!");
            print_knowledge_graph(&graph);

            // Basic assertions
            assert!(!graph.is_empty(), "Knowledge graph should not be empty");
            assert!(
                graph.node_count() >= 3,
                "Should extract at least 3 nodes (people, orgs, locations)"
            );
            assert!(
                graph.edge_count() >= 2,
                "Should extract at least 2 relationships"
            );

            // Check for specific entities
            let node_names: Vec<String> =
                graph.nodes.iter().map(|n| n.name.to_lowercase()).collect();

            assert!(
                node_names.iter().any(|n| n.contains("alice")
                    || n.contains("johnson")
                    || n.contains("alice johnson")),
                "Should extract Alice Johnson"
            );
            assert!(
                node_names
                    .iter()
                    .any(|n| n.contains("techcorp") || n.contains("tech corp")),
                "Should extract TechCorp"
            );
            assert!(
                node_names.iter().any(|n| n.contains("san francisco")),
                "Should extract San Francisco"
            );

            // Check for different node types
            let node_types: std::collections::HashSet<_> =
                graph.nodes.iter().map(|n| n.node_type.as_str()).collect();
            assert!(
                node_types.len() >= 2,
                "Should have at least 2 different node types"
            );

            println!("\n✅ All assertions passed!");
        }
        Err(e) => {
            panic!("❌ Fact extraction failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_fact_extraction_batch() {
    let adapter = create_adapter_from_env();

    println!("\n  Testing batch fact extraction with multiple texts");
    println!("   Model: {}", adapter.model());
    println!("   Number of texts: 2");

    println!(
        "\n  Effective Prompt (default):\n{}",
        FactExtractor::<OpenAIAdapter>::default_graph_prompt()
    );
    println!("\n  Input Text 1 (TechCorp):\n{}", TEST_TEXT_TECHCORP);
    println!("\n  Input Text 2 (Research):\n{}", TEST_TEXT_RESEARCH);

    let extractor = FactExtractor::new(adapter);

    let texts = vec![
        TEST_TEXT_TECHCORP.to_string(),
        TEST_TEXT_RESEARCH.to_string(),
    ];

    let result = extractor.extract_facts_batch(texts, None).await;

    match result {
        Ok(graphs) => {
            println!("\n✓ Batch fact extraction successful!");
            println!("   Extracted {} knowledge graphs", graphs.len());

            assert_eq!(graphs.len(), 2, "Should return 2 knowledge graphs");

            println!(
                "\n  Graph 1 Raw JSON:\n{}",
                serde_json::to_string_pretty(&graphs[0])
                    .unwrap_or_else(|_| "<failed to serialize graph 1>".to_string())
            );
            println!(
                "\n  Graph 2 Raw JSON:\n{}",
                serde_json::to_string_pretty(&graphs[1])
                    .unwrap_or_else(|_| "<failed to serialize graph 2>".to_string())
            );

            // Analyze each graph
            for (i, graph) in graphs.iter().enumerate() {
                println!("\n📊 Graph {} Statistics:", i + 1);
                println!("   Nodes: {}", graph.node_count());
                println!("   Edges: {}", graph.edge_count());

                assert!(!graph.is_empty(), "Graph {} should not be empty", i + 1);
                assert!(
                    graph.node_count() >= 2,
                    "Graph {} should have at least 2 nodes",
                    i + 1
                );
            }

            // First graph should contain TechCorp entities
            let graph1_nodes: Vec<String> = graphs[0]
                .nodes
                .iter()
                .map(|n| n.name.to_lowercase())
                .collect();
            assert!(
                graph1_nodes
                    .iter()
                    .any(|n| n.contains("techcorp") || n.contains("tech corp")),
                "First graph should contain TechCorp"
            );

            // Second graph should contain research entities
            let graph2_nodes: Vec<String> = graphs[1]
                .nodes
                .iter()
                .map(|n| n.name.to_lowercase())
                .collect();
            println!("\n  Graph 2 Node Names: {:?}", graph2_nodes);
            assert!(
                graph2_nodes.iter().any(|n| n.contains("maria")
                    || n.contains("rodriguez")
                    || n.contains("maria rodriguez")),
                "Second graph should contain Maria Rodriguez"
            );

            println!("\n✅ All assertions passed!");
        }
        Err(e) => {
            panic!("❌ Batch fact extraction failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_fact_extraction_with_custom_prompt() {
    let adapter = create_adapter_from_env();

    println!("\n  Testing fact extraction with custom prompt");
    println!("   Model: {}", adapter.model());

    let extractor = FactExtractor::new(adapter);

    let custom_prompt = r#"
Extract a knowledge graph from the text with special focus on people and their professional relationships.

For each entity (especially people), create nodes with:
- id: unique identifier
- name: the entity name
- type: the entity type (PERSON, ORGANIZATION, LOCATION, etc.)
- description: relevant details

Create edges that represent relationships like: WORKS_AT, COLLABORATES_WITH, FOUNDED, etc.

Pay special attention to extracting all people mentioned and their professional connections.
"#;

    let result = extractor
        .extract_facts(TEST_TEXT_TECHCORP, Some(custom_prompt))
        .await;

    match result {
        Ok(graph) => {
            println!("\n✓ Fact extraction with custom prompt successful!");
            print_knowledge_graph(&graph);

            assert!(!graph.is_empty(), "Knowledge graph should not be empty");

            // With custom prompt focusing on people, we should have person nodes
            let person_nodes: Vec<_> = graph
                .nodes
                .iter()
                .filter(|n| {
                    n.node_type.to_lowercase().contains("person")
                        || n.node_type.to_lowercase().contains("engineer")
                        || n.node_type.to_lowercase().contains("ceo")
                })
                .collect();

            assert!(
                !person_nodes.is_empty(),
                "Should extract person nodes with custom prompt"
            );

            println!("\n✅ Custom prompt test passed!");
        }
        Err(e) => {
            panic!("❌ Fact extraction with custom prompt failed: {}", e);
        }
    }
}

/// Helper function to print knowledge graph statistics and details
fn print_knowledge_graph(graph: &KnowledgeGraph) {
    println!("\n📊 Knowledge Graph Statistics:");
    println!("   Nodes: {}", graph.node_count());
    println!("   Edges: {}", graph.edge_count());

    // Group nodes by type
    let mut nodes_by_type: HashMap<String, Vec<&str>> = HashMap::new();
    for node in &graph.nodes {
        nodes_by_type
            .entry(node.node_type.clone())
            .or_default()
            .push(&node.name);
    }

    println!("\n🔵 Nodes by Type:");
    for (node_type, names) in &nodes_by_type {
        println!("   {} ({}):", node_type, names.len());
        for name in names {
            println!("     - {}", name);
        }
    }

    println!("\n🔗 Relationships:");
    for edge in &graph.edges {
        // Find node names from IDs
        let source_name = graph
            .nodes
            .iter()
            .find(|n| n.id == edge.source_node_id)
            .map(|n| n.name.as_str())
            .unwrap_or(&edge.source_node_id);
        let target_name = graph
            .nodes
            .iter()
            .find(|n| n.id == edge.target_node_id)
            .map(|n| n.name.as_str())
            .unwrap_or(&edge.target_node_id);

        println!(
            "     {} --[{}]--> {}",
            source_name, edge.relationship_name, target_name
        );
    }

    // Print some detailed node information
    println!("\n  Node Details (first 5):");
    for node in graph.nodes.iter().take(5) {
        println!("   {}:", node.name);
        println!("     Type: {}", node.node_type);
        if !node.description.is_empty() {
            let desc = if node.description.len() > 100 {
                format!("{}...", &node.description[..100])
            } else {
                node.description.clone()
            };
            println!("     Description: {}", desc);
        }
    }
}
