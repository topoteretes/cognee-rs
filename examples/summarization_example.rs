//! Example demonstrating text summarization feature.
//!
//! This example shows how to use the SummaryExtractor to generate summaries
//! from text chunks using an LLM.
//!
//! Run with:
//! cargo run --example summarization_example
//!
//! Requires environment variables:
//! - OPENAI_URL: Base URL for the LLM API (e.g., http://localhost:11435/v1 for Ollama)
//! - OPENAI_TOKEN: API token (use "not-needed" for Ollama)

use cognee_chunking::CutType;
use cognee_cognify::{SummarizedContent, SummaryExtractor};
use cognee_llm::OpenAIAdapter;
use cognee_models::DocumentChunk;
use std::sync::Arc;
use uuid::Uuid;

const SAMPLE_TEXT: &str = r#"
Artificial intelligence has revolutionized numerous industries over the past decade.
Machine learning algorithms can now diagnose diseases with remarkable accuracy,
while natural language processing models understand and generate human language
with unprecedented capabilities. However, these advances also raise important
ethical questions about bias, privacy, and the environmental impact of training
large models. Researchers and policymakers worldwide are working to ensure AI
systems are developed and deployed responsibly.
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🤖 Text Summarization Example\n");

    // Read environment variables
    let base_url =
        std::env::var("OPENAI_URL").unwrap_or_else(|_| "http://localhost:11435/v1".to_string());
    let api_token = std::env::var("OPENAI_TOKEN").unwrap_or_else(|_| "not-needed".to_string());

    println!("📡 Connecting to LLM at: {base_url}");
    println!("🔧 Model: llama3.2:3b\n");

    // Create LLM adapter
    let llm = Arc::new(OpenAIAdapter::new(
        "llama3.2:3b",
        api_token,
        Some(base_url),
    )?);

    // Create summary extractor
    let extractor = SummaryExtractor::new(llm);

    // Example 1: Single text summarization
    println!("Example 1: Single Text Summarization");
    println!("=====================================\n");
    println!("Original text ({} chars):", SAMPLE_TEXT.len());
    println!("{SAMPLE_TEXT}\n");

    let summary: SummarizedContent = extractor.extract_summary(SAMPLE_TEXT, None).await?;

    println!("✓ Summary:");
    println!("  {}\n", summary.summary);
    println!("✓ Description:");
    println!("  {}\n", summary.description);

    // Example 2: Batch summarization from document chunks
    println!("\nExample 2: Batch Summarization (DocumentChunks)");
    println!("================================================\n");

    let document_id = Uuid::new_v4();
    let chunks = vec![
        DocumentChunk::new(
            Uuid::new_v4(),
            SAMPLE_TEXT.to_string(),
            SAMPLE_TEXT.len(),
            0,
            CutType::ParagraphEnd.to_string(),
            document_id,
        ),
        DocumentChunk::new(
            Uuid::new_v4(),
            "Quantum computing represents a paradigm shift in computation.".to_string(),
            62,
            1,
            CutType::SentenceEnd.to_string(),
            document_id,
        ),
    ];

    println!("Processing {} chunks...\n", chunks.len());

    let summaries = extractor.summarize_chunks(&chunks, None).await?;

    for (idx, summary) in summaries.iter().enumerate() {
        println!("Chunk {}: ID={}", idx + 1, summary.base.id);
        println!("  Summary: {}", summary.text);
        if let Some(desc) = &summary.description {
            println!("  Description: {desc}\n");
        }
    }

    // Example 3: Custom prompt
    println!("\nExample 3: Custom Prompt");
    println!("========================\n");

    let custom_prompt = r#"
You are an expert scientific summarizer. Summarize the following text with
emphasis on technical accuracy and scientific terminology.
"#;

    let custom_summary = extractor
        .extract_summary(SAMPLE_TEXT, Some(custom_prompt))
        .await?;

    println!("✓ Custom summary:");
    println!("  {}\n", custom_summary.summary);

    println!("✅ All examples completed successfully!");

    Ok(())
}
