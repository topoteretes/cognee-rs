//! Example demonstrating OpenAI adapter with structured output generation.
//!
//! This example shows how to use the OpenAI adapter to extract structured data
//! from text using JSON schema-guided generation.
//!
//! Set your OpenAI API key:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_example
//! ```

use cognee_llm::{GenerationOptions, Llm, Message, OpenAIAdapter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Example: Extract entities from text
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EntityExtraction {
    /// List of people mentioned in the text
    people: Vec<String>,
    /// List of organizations mentioned  
    organizations: Vec<String>,
    /// List of locations mentioned
    locations: Vec<String>,
}

/// Example: Knowledge graph extraction
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct KnowledgeGraph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Node {
    id: String,
    label: String,
    entity_type: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Edge {
    source: String,
    target: String,
    relationship: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get API key from environment
    let api_key =
        std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY environment variable must be set");

    // Create OpenAI adapter
    println!("Creating OpenAI adapter...");
    let llm = OpenAIAdapter::new("gpt-4", &api_key, None)?;

    println!("Model: {}", llm.model());
    println!("Max context: {} tokens", llm.max_context_length());
    println!(
        "Supports function calling: {}",
        llm.supports_function_calling()
    );
    println!();

    // Example 1: Simple entity extraction
    println!("=== Example 1: Entity Extraction ===");
    let text = "Apple Inc. CEO Tim Cook announced the new iPhone at their headquarters \
                in Cupertino, California. The event was attended by executives from \
                Microsoft and Google.";

    let entities: EntityExtraction = llm
        .create_structured_output(
            text,
            "Extract all entities (people, organizations, and locations) from the text.",
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(500),
                ..Default::default()
            }),
        )
        .await?;

    println!("Input: {}", text);
    println!("\nExtracted entities:");
    println!("  People: {:?}", entities.people);
    println!("  Organizations: {:?}", entities.organizations);
    println!("  Locations: {:?}", entities.locations);
    println!();

    // Example 2: Knowledge graph extraction
    println!("=== Example 2: Knowledge Graph ===");
    let text = "Alice works at TechCorp as a software engineer. \
                She reports to Bob, the engineering manager. \
                TechCorp is headquartered in San Francisco.";

    let graph: KnowledgeGraph = llm
        .create_structured_output(
            text,
            "Extract a knowledge graph with nodes (entities) and edges (relationships). \
             Use clear relationship types like WORKS_AT, REPORTS_TO, LOCATED_IN.",
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(1000),
                ..Default::default()
            }),
        )
        .await?;

    println!("Input: {}", text);
    println!("\nKnowledge Graph:");
    println!("  Nodes:");
    for node in &graph.nodes {
        println!("    - {} ({}): {}", node.id, node.entity_type, node.label);
    }
    println!("  Edges:");
    for edge in &graph.edges {
        println!(
            "    - {} -> {} [{}]",
            edge.source, edge.target, edge.relationship
        );
    }
    println!();

    // Example 3: Using custom messages (multi-turn)
    println!("=== Example 3: Multi-turn Conversation ===");
    let messages = vec![
        Message::system("You are an expert at extracting structured information."),
        Message::user("I need to buy milk, eggs, and bread from the store."),
    ];

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct ShoppingList {
        items: Vec<String>,
    }

    let shopping_list: ShoppingList = llm
        .create_structured_output_with_messages(
            messages,
            Some(GenerationOptions {
                temperature: Some(0.0),
                ..Default::default()
            }),
        )
        .await?;

    println!("Extracted shopping list: {:?}", shopping_list.items);
    println!();

    // Example 4: Simple text generation (non-structured)
    println!("=== Example 4: Simple Text Generation ===");
    let response = llm
        .generate(
            vec![
                Message::system("You are a helpful assistant."),
                Message::user("What is the capital of France?"),
            ],
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(50),
                ..Default::default()
            }),
        )
        .await?;

    println!("Question: What is the capital of France?");
    println!("Answer: {}", response.content);
    if let Some(usage) = response.usage {
        println!(
            "Tokens used: {} prompt + {} completion = {} total",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }

    Ok(())
}
