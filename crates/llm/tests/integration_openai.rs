//! Integration tests for OpenAI adapter with fact extraction.
//!
//! These tests require environment variables to be set:
//! - OPENAI_URL: Base URL for the OpenAI-compatible API
//! - OPENAI_TOKEN: API token (use "not-needed" for Ollama)
//!
//! Tests are automatically skipped if environment variables are not set.
//!
//! Run with: cargo test --package cognee-llm --test integration_openai

use cognee_llm::{GenerationOptions, Llm, OpenAIAdapter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Helper to get environment variables or skip test
fn get_env_or_skip(var_name: &str) -> Result<String, ()> {
    std::env::var(var_name).map_err(|_| {
        eprintln!("⚠️  Skipping test: {} not set", var_name);
    })
}

/// Helper to create OpenAI adapter from environment variables
fn create_adapter_from_env() -> Result<OpenAIAdapter, ()> {
    let base_url = get_env_or_skip("OPENAI_URL")?;
    let api_token = get_env_or_skip("OPENAI_TOKEN")?;

    OpenAIAdapter::new("llama3.2:3b", api_token, Some(base_url)).map_err(|e| {
        eprintln!("⚠️  Failed to create adapter: {}", e);
    })
}

/// Test data: Multi-paragraph text for fact extraction
const TEST_TEXT: &str = r#"
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

/// Fact extraction model: Entities
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EntityExtraction {
    /// People mentioned in the text
    people: Vec<Person>,
    /// Organizations mentioned in the text
    organizations: Vec<Organization>,
    /// Locations mentioned in the text
    locations: Vec<Location>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Person {
    name: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    organization: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Organization {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    location: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Location {
    name: String,
    #[serde(default, alias = "type")]
    location_type: Option<String>,
}

/// Fact extraction model: Knowledge graph
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

#[tokio::test]
async fn test_entity_extraction() {
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing entity extraction with OpenAI-compatible API");
    println!("   Model: {}", adapter.model());
    println!("   Text length: {} chars", TEST_TEXT.len());

    let result: Result<EntityExtraction, _> = adapter
        .create_structured_output(
            TEST_TEXT,
            "Extract all entities from the text following this exact JSON structure:\n\
             - people: array of {name: string, role?: string, organization?: string}\n\
             - organizations: array of {name: string, description?: string, location?: string}\n\
             - locations: array of {name: string, location_type?: string}\n\
             \n\
             Important: Keep it simple - use flat objects with string values only, no nested objects.\n\
             Be thorough and include all relevant information.",
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(2000),
                ..Default::default()
            }),
        )
        .await;

    match result {
        Ok(extraction) => {
            println!("\n✓ Entity extraction successful!");
            println!("\n📊 Results:");
            println!("   People: {}", extraction.people.len());
            for person in &extraction.people {
                println!(
                    "     - {} {}{}",
                    person.name,
                    person
                        .role
                        .as_ref()
                        .map(|r| format!("({})", r))
                        .unwrap_or_default(),
                    person
                        .organization
                        .as_ref()
                        .map(|o| format!(" at {}", o))
                        .unwrap_or_default()
                );
            }

            println!("   Organizations: {}", extraction.organizations.len());
            for org in &extraction.organizations {
                println!(
                    "     - {} {}",
                    org.name,
                    org.location
                        .as_ref()
                        .map(|l| format!("({})", l))
                        .unwrap_or_default()
                );
            }

            println!("   Locations: {}", extraction.locations.len());
            for loc in &extraction.locations {
                println!(
                    "     - {} ({})",
                    loc.name,
                    loc.location_type.as_ref().unwrap_or(&"unknown".to_string())
                );
            }

            // Basic assertions
            assert!(
                !extraction.people.is_empty(),
                "Should extract at least one person"
            );
            assert!(
                !extraction.organizations.is_empty(),
                "Should extract at least one organization"
            );
            assert!(
                !extraction.locations.is_empty(),
                "Should extract at least one location"
            );

            // Specific assertions based on test text
            assert!(
                extraction
                    .people
                    .iter()
                    .any(|p| p.name.to_lowercase().contains("alice")),
                "Should extract Alice Johnson"
            );
            assert!(
                extraction
                    .organizations
                    .iter()
                    .any(|o| o.name.to_lowercase().contains("techcorp")),
                "Should extract TechCorp"
            );
            assert!(
                extraction
                    .locations
                    .iter()
                    .any(|l| l.name.to_lowercase().contains("san francisco")),
                "Should extract San Francisco"
            );

            println!("\n✅ All assertions passed!");
        }
        Err(e) => {
            panic!("❌ Entity extraction failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_knowledge_graph_extraction() {
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing knowledge graph extraction with OpenAI-compatible API");
    println!("   Model: {}", adapter.model());

    let result: Result<KnowledgeGraph, _> = adapter
        .create_structured_output(
            TEST_TEXT,
            "Extract a knowledge graph from the text. Create nodes for all entities \
             (people, organizations, locations) and edges for relationships between them. \
             Use clear relationship types like WORKS_AT, FOUNDED, LOCATED_IN, COLLABORATES_WITH, etc.",
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(2000),
                ..Default::default()
            }),
        )
        .await;

    match result {
        Ok(graph) => {
            println!("\n✓ Knowledge graph extraction successful!");
            println!("\n📊 Graph Statistics:");
            println!("   Nodes: {}", graph.nodes.len());
            println!("   Edges: {}", graph.edges.len());

            println!("\n🔵 Nodes:");
            for node in &graph.nodes {
                println!("     {} ({}) - {}", node.id, node.entity_type, node.label);
            }

            println!("\n🔗 Edges:");
            for edge in &graph.edges {
                println!(
                    "     {} --[{}]--> {}",
                    edge.source, edge.relationship, edge.target
                );
            }

            // Basic assertions
            assert!(
                graph.nodes.len() >= 3,
                "Should have at least 3 nodes (people, orgs, locations)"
            );
            assert!(
                graph.edges.len() >= 2,
                "Should have at least 2 relationships"
            );

            // Check that we have different entity types
            let entity_types: std::collections::HashSet<_> =
                graph.nodes.iter().map(|n| &n.entity_type).collect();
            assert!(
                entity_types.len() >= 2,
                "Should have at least 2 different entity types"
            );

            println!("\n✅ All assertions passed!");
        }
        Err(e) => {
            panic!("❌ Knowledge graph extraction failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_simple_text_generation() {
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing simple text generation");

    use cognee_llm::Message;

    let result = adapter
        .generate(
            vec![
                Message::system("You are a helpful assistant."),
                Message::user("What is 2+2? Answer with just the number."),
            ],
            Some(GenerationOptions {
                temperature: Some(0.0),
                max_tokens: Some(10),
                ..Default::default()
            }),
        )
        .await;

    match result {
        Ok(response) => {
            println!("\n✓ Generation successful!");
            println!("   Response: {}", response.content);
            println!("   Model: {}", response.model);

            if let Some(usage) = response.usage {
                println!(
                    "   Tokens: {} prompt + {} completion = {} total",
                    usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                );
            }

            assert!(!response.content.is_empty(), "Response should not be empty");

            println!("\n✅ Test passed!");
        }
        Err(e) => {
            panic!("❌ Text generation failed: {}", e);
        }
    }
}
