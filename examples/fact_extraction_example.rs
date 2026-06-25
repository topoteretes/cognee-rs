//! Example: Fact extraction using the FactExtractor.
//!
//! This example demonstrates how to use the FactExtractor to extract
//! knowledge graphs from text using a local Ollama instance.
//!
//! Prerequisites:
//! 1. Start Ollama container: cd docker/ollama && ./start.sh
//! 2. Set environment variables:
//!    export OPENAI_URL="http://localhost:11435/v1"
//!    export OPENAI_TOKEN="not-needed"
//! 3. Run: cargo run --example fact_extraction_example

use cognee_cognify::{FactExtractor, KnowledgeGraph};
use cognee_llm::OpenAIAdapter;
use std::env;
use std::sync::Arc;

const SAMPLE_TEXT: &str = r#"
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧠 Cognee Fact Extraction Example\n");
    println!("═══════════════════════════════════════════════════════════════\n");

    // Get LLM configuration from environment
    let base_url =
        env::var("OPENAI_URL").unwrap_or_else(|_| "http://localhost:11435/v1".to_string());
    let api_token = env::var("OPENAI_TOKEN").unwrap_or_else(|_| "not-needed".to_string());

    println!("📡 Connecting to LLM:");
    println!("   URL: {base_url}");
    println!("   Model: llama3.2:3b\n");

    // Create LLM adapter
    let llm = Arc::new(OpenAIAdapter::new(
        "llama3.2:3b",
        api_token,
        Some(base_url),
    )?);

    // Create fact extractor
    let extractor = FactExtractor::new(llm);

    println!("📄 Input Text ({} chars):", SAMPLE_TEXT.len());
    println!("{}\n", SAMPLE_TEXT.trim());
    println!("═══════════════════════════════════════════════════════════════\n");

    // Extract facts
    println!("🔍 Extracting knowledge graph...\n");
    let graph = extractor.extract_facts(SAMPLE_TEXT.trim(), None).await?;

    // Display results
    display_knowledge_graph(&graph);

    // Demonstrate batch extraction
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("🔄 Batch Extraction Example\n");

    let texts = vec![
        "Albert Einstein developed the theory of relativity.".to_string(),
        "Marie Curie won the Nobel Prize in Physics and Chemistry.".to_string(),
    ];

    println!("Processing {} text chunks in parallel...\n", texts.len());

    let graphs = extractor.extract_facts_batch(texts, None).await?;

    for (i, graph) in graphs.iter().enumerate() {
        println!(
            "Chunk {}: {} nodes, {} edges",
            i + 1,
            graph.node_count(),
            graph.edge_count()
        );
    }

    println!("\n✅ Done!");

    Ok(())
}

fn display_knowledge_graph(graph: &KnowledgeGraph) {
    println!("📊 Extraction Results:");
    println!("   Nodes: {}", graph.node_count());
    println!("   Edges: {}", graph.edge_count());

    if graph.is_empty() {
        println!("\n⚠️  No facts extracted");
        return;
    }

    // Display nodes grouped by type
    println!("\n🔵 Nodes:");
    let mut nodes_by_type: std::collections::HashMap<String, Vec<&cognee_cognify::Node>> =
        std::collections::HashMap::new();

    for node in &graph.nodes {
        nodes_by_type
            .entry(node.node_type.clone())
            .or_default()
            .push(node);
    }

    for (node_type, nodes) in nodes_by_type {
        println!("\n   {} ({} nodes):", node_type, nodes.len());
        for node in nodes {
            println!("     • {} - {}", node.name, node.description);
        }
    }

    // Display edges
    if !graph.edges.is_empty() {
        println!("\n🔗 Relationships:");
        for edge in &graph.edges {
            println!(
                "     {} --[{}]--> {}",
                edge.source_node_id, edge.relationship_name, edge.target_node_id
            );
        }
    }
}
