//! Integration tests for fact extraction using the FactExtractor.
//!
//! These tests require environment variables to be set:
//! - OPENAI_URL: Base URL for the OpenAI-compatible API
//! - OPENAI_TOKEN: API token (use "not-needed" for Ollama)
//!
//! Tests are automatically skipped if environment variables are not set.
//!
//! Run with: cargo test --package cognee-cognify --test integration_fact_extraction

use cognee_cognify::{FactExtractor, KnowledgeGraph};
use cognee_llm::{Llm, OpenAIAdapter};
use std::collections::HashMap;
use std::sync::Arc;

/// Helper to get environment variables or skip test
fn get_env_or_skip(var_name: &str) -> Result<String, ()> {
    std::env::var(var_name).map_err(|_| {
        eprintln!("⚠️  Skipping test: {} not set", var_name);
    })
}

/// Helper to create OpenAI adapter from environment variables
fn create_adapter_from_env() -> Result<Arc<OpenAIAdapter>, ()> {
    let base_url = get_env_or_skip("OPENAI_URL")?;
    let api_token = get_env_or_skip("OPENAI_TOKEN")?;

    OpenAIAdapter::new("llama3.2:3b", api_token, Some(base_url))
        .map(Arc::new)
        .map_err(|e| {
            eprintln!("⚠️  Failed to create adapter: {}", e);
        })
}

/// Test data: Multi-paragraph text about a technology company
const TEST_TEXT_TECHCORP: &str = r#"
Alice Johnson is a software engineer at TechCorp, a technology company based in San Francisco, California. 
She has been working there for five years, specializing in machine learning and artificial intelligence.

Bob Smith, the CEO of TechCorp, founded the company in 2010 with a vision to revolutionize how businesses 
use data. Under his leadership, TechCorp has grown from a small startup to a company with over 500 employees.

The company's headquarters is located in the heart of San Francisco's financial district, occupying three 
floors of a modern office building. TechCorp also has satellite offices in New York City and Austin, Texas.

Last month, Alice presented her latest project at the AI Conference in Seattle, Washington. Her work on 
improving natural language processing models received significant attention from industry experts. She 
collaborated with Dr. Emma Chen from Stanford University on this research.

TechCorp recently announced a partnership with DataSystems Inc., another major player in the technology sector. 
This partnership aims to integrate TechCorp's AI capabilities with DataSystems' cloud infrastructure platform.
"#;

/// Test data: Multi-paragraph text about scientific research
const TEST_TEXT_RESEARCH: &str = r#"
Dr. Maria Rodriguez leads the Quantum Computing Laboratory at MIT, where she has been conducting groundbreaking 
research on quantum error correction since 2018. Her team consists of twelve researchers from various countries, 
including Dr. James Lee from South Korea and Dr. Fatima Abbas from Egypt.

The laboratory is funded by a $10 million grant from the National Science Foundation, which was awarded in 2020. 
This funding has enabled the acquisition of state-of-the-art quantum computers manufactured by QuantumTech Industries, 
a Canadian company specializing in quantum hardware.

Dr. Rodriguez recently published a paper in Nature Physics, co-authored with Professor Chen Wei from Tsinghua 
University in Beijing. The research demonstrates a novel approach to reducing quantum decoherence, which could 
significantly improve the reliability of quantum computers.

The MIT laboratory collaborates with several institutions worldwide, including Cambridge University in the UK, 
the Max Planck Institute in Germany, and RIKEN in Japan. These partnerships facilitate the exchange of ideas 
and resources in the rapidly evolving field of quantum computing.
"#;

#[tokio::test]
async fn test_fact_extraction_single_text() {
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

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
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing batch fact extraction with multiple texts");
    println!("   Model: {}", adapter.model());
    println!("   Number of texts: 2");

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
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing fact extraction with custom prompt");
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
    println!("\n📝 Node Details (first 5):");
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
