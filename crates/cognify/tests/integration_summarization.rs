//! Integration tests for text summarization using the SummaryExtractor.
//!
//! These tests require environment variables to be set:
//! - OPENAI_URL: Base URL for the OpenAI-compatible API
//! - OPENAI_TOKEN: API token (use "not-needed" for Ollama)
//!
//! Tests are automatically skipped if environment variables are not set.
//!
//! Run with: cargo test --package cognee-cognify --test integration_summarization

use cognee_chunking::CutType;
use cognee_cognify::{SummarizedContent, SummaryExtractor, TextSummary};
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_models::DocumentChunk;
use std::sync::Arc;
use uuid::Uuid;

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

/// Test data: Long article text suitable for summarization
const TEST_TEXT_ARTICLE: &str = r#"
Artificial intelligence has made remarkable progress over the past decade, transforming 
industries ranging from healthcare to transportation. Machine learning algorithms can now 
diagnose diseases with accuracy rivaling human experts, while autonomous vehicles are 
becoming a reality on roads worldwide. Natural language processing models have achieved 
unprecedented capabilities in understanding and generating human language.

The development of large language models, particularly transformer-based architectures, 
has been a key driver of this progress. These models can perform a wide variety of tasks, 
from translation and summarization to code generation and creative writing. Their ability 
to learn from vast amounts of data has enabled them to capture complex patterns in language 
and knowledge.

However, these advances also raise important ethical considerations. Issues of bias, privacy, 
and the environmental impact of training large models have become increasingly prominent in 
academic and public discourse. Researchers and policymakers are working to develop frameworks 
that ensure AI systems are developed and deployed responsibly.

Looking ahead, the integration of AI into everyday life will continue to accelerate. Edge 
computing and more efficient model architectures will enable AI capabilities on personal 
devices, while advances in multi-modal learning will allow systems to understand and generate 
content across text, images, and audio simultaneously.
"#;

const TEST_TEXT_SHORT: &str = r#"
Quantum computing represents a paradigm shift in computation. Unlike classical computers 
that use bits, quantum computers use qubits that can exist in superposition states. This 
allows them to solve certain problems exponentially faster than traditional computers.
"#;

#[tokio::test]
async fn test_summarization_single_text() {
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing summarization with single text");
    println!("   Model: {}", adapter.model());
    println!("   Text length: {} chars", TEST_TEXT_ARTICLE.len());

    let extractor = SummaryExtractor::new(adapter);

    let result = extractor.extract_summary(TEST_TEXT_ARTICLE, None).await;

    match result {
        Ok(summarized) => {
            println!("\n✓ Summarization successful!");
            print_summarized_content(&summarized);

            // Basic assertions
            assert!(
                !summarized.summary.is_empty(),
                "Summary should not be empty"
            );
            assert!(
                !summarized.description.is_empty(),
                "Description should not be empty"
            );
            assert!(
                summarized.summary.len() < TEST_TEXT_ARTICLE.len(),
                "Summary should be shorter than original text"
            );

            // Verify summary is actually a summary (shorter and different)
            println!("\n📊 Metrics:");
            println!(
                "   Compression ratio: {:.1}%",
                100.0 * summarized.summary.len() as f64 / TEST_TEXT_ARTICLE.len() as f64
            );
        }
        Err(e) => {
            panic!("❌ Summarization failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_summarization_batch() {
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing batch summarization");
    println!("   Model: {}", adapter.model());

    // Create test chunks
    let document_id = Uuid::new_v4();
    let chunks = vec![
        DocumentChunk {
            id: Uuid::new_v4(),
            document_id,
            text: TEST_TEXT_ARTICLE.to_string(),
            chunk_index: 0,
            chunk_size: TEST_TEXT_ARTICLE.len(),
            cut_type: CutType::ParagraphEnd.to_string(),
        },
        DocumentChunk {
            id: Uuid::new_v4(),
            document_id,
            text: TEST_TEXT_SHORT.to_string(),
            chunk_index: 1,
            chunk_size: TEST_TEXT_SHORT.len(),
            cut_type: CutType::ParagraphEnd.to_string(),
        },
    ];

    println!("   Processing {} chunks", chunks.len());

    let extractor = SummaryExtractor::new(adapter);

    let result = extractor.summarize_chunks(&chunks, None).await;

    match result {
        Ok(summaries) => {
            println!("\n✓ Batch summarization successful!");
            println!("   Generated {} summaries", summaries.len());

            assert_eq!(
                summaries.len(),
                chunks.len(),
                "Should generate one summary per chunk"
            );

            for (idx, summary) in summaries.iter().enumerate() {
                println!("\n📝 Summary {}:", idx + 1);
                print_text_summary(summary);

                // Verify deterministic UUID
                assert_eq!(
                    summary.chunk_id, chunks[idx].id,
                    "Summary should link to correct chunk"
                );
                assert_eq!(
                    summary.id,
                    Uuid::new_v5(&chunks[idx].id, b"TextSummary"),
                    "Summary ID should be deterministic uuid5"
                );

                assert!(!summary.text.is_empty(), "Summary text should not be empty");
                assert_eq!(summary.model, "llama3.2:3b", "Model name should match");
            }
        }
        Err(e) => {
            panic!("❌ Batch summarization failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_summarization_deterministic_ids() {
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing deterministic summary IDs");

    let chunk_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let document_id = Uuid::new_v4();

    let chunk = DocumentChunk {
        id: chunk_id,
        document_id,
        text: TEST_TEXT_SHORT.to_string(),
        chunk_index: 0,
        chunk_size: TEST_TEXT_SHORT.len(),
        cut_type: CutType::ParagraphEnd.to_string(),
    };

    let extractor = SummaryExtractor::new(adapter);

    // Generate summary twice for the same chunk
    let result1 = extractor
        .summarize_chunks(std::slice::from_ref(&chunk), None)
        .await;

    let result2 = extractor
        .summarize_chunks(std::slice::from_ref(&chunk), None)
        .await;

    match (result1, result2) {
        (Ok(summaries1), Ok(summaries2)) => {
            assert_eq!(summaries1.len(), 1);
            assert_eq!(summaries2.len(), 1);

            let summary1 = &summaries1[0];
            let summary2 = &summaries2[0];

            println!("\n✓ Generated summaries with deterministic IDs");
            println!("   Summary ID: {}", summary1.id);

            // Same chunk_id should produce same summary id
            assert_eq!(
                summary1.id, summary2.id,
                "Same chunk should produce same summary ID"
            );

            // Verify the ID is the expected uuid5
            let expected_id = Uuid::new_v5(&chunk_id, b"TextSummary");
            assert_eq!(
                summary1.id, expected_id,
                "Summary ID should be uuid5(chunk_id, 'TextSummary')"
            );
        }
        _ => {
            panic!("❌ Deterministic ID test failed");
        }
    }
}

#[tokio::test]
async fn test_summarization_empty_chunks() {
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing summarization with empty chunks");

    let extractor = SummaryExtractor::new(adapter);

    let result = extractor.summarize_chunks(&[], None).await;

    match result {
        Ok(summaries) => {
            println!("✓ Empty chunks handled correctly");
            assert!(
                summaries.is_empty(),
                "Empty input should produce empty output"
            );
        }
        Err(e) => {
            panic!("❌ Empty chunks test failed: {}", e);
        }
    }
}

#[tokio::test]
async fn test_summarization_custom_prompt() {
    let adapter = match create_adapter_from_env() {
        Ok(a) => a,
        Err(_) => return, // Skip test
    };

    println!("\n🧪 Testing summarization with custom prompt");

    let custom_prompt = r#"
You are a summarization expert. Summarize the following text clearly and concisely.
Provide a brief summary (1-2 sentences) and a detailed description that preserves key information.
Both summary and description should be plain text strings, not structured data.
"#;

    let extractor = SummaryExtractor::new(adapter);

    let result = extractor
        .extract_summary(TEST_TEXT_SHORT, Some(custom_prompt))
        .await;

    match result {
        Ok(summarized) => {
            println!("\n✓ Custom prompt summarization successful!");
            print_summarized_content(&summarized);

            assert!(
                !summarized.summary.is_empty(),
                "Summary should not be empty"
            );
            assert!(
                !summarized.description.is_empty(),
                "Description should not be empty"
            );
        }
        Err(e) => {
            panic!("❌ Custom prompt summarization failed: {}", e);
        }
    }
}

// Helper functions for pretty printing

fn print_summarized_content(summarized: &SummarizedContent) {
    println!("   Summary: {}", summarized.summary);
    println!("   Description: {}", summarized.description);
}

fn print_text_summary(summary: &TextSummary) {
    println!("   ID: {}", summary.id);
    println!("   Chunk ID: {}", summary.chunk_id);
    println!("   Model: {}", summary.model);
    println!("   Text: {}", summary.text);
    if let Some(desc) = &summary.description {
        println!("   Description: {}", desc);
    }
}
