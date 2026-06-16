#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for text summarization using the SummaryExtractor.
//!
//! These tests require environment variables to be set:
//! - OPENAI_URL: Base URL for the OpenAI-compatible API
//! - OPENAI_TOKEN: API token (use "not-needed" for Ollama)
//! - OPENAI_MODEL: Model name to use for summarization
//!
//! Run with: cargo test --package cognee-cognify --test integration_summarization

use cognee_chunking::CutType;
use cognee_cognify::{SummarizedContent, SummaryExtractor, TextSummary};
use cognee_llm::Llm;
use cognee_models::DocumentChunk;
use uuid::Uuid;

mod test_data;
mod test_utils;

use test_data::{TEST_TEXT_ARTICLE, TEST_TEXT_SHORT};
use test_utils::create_adapter_from_env;

#[tokio::test]
async fn test_summarization_single_text() {
    let adapter = create_adapter_from_env();

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
            panic!("❌ Summarization failed: {e}");
        }
    }
}

#[tokio::test]
async fn test_summarization_batch() {
    let adapter = create_adapter_from_env();

    println!("\n🧪 Testing batch summarization");
    println!("   Model: {}", adapter.model());

    // Create test chunks
    let document_id = Uuid::new_v4();
    let chunks = vec![
        DocumentChunk::new(
            Uuid::new_v4(),
            TEST_TEXT_ARTICLE.to_string(),
            TEST_TEXT_ARTICLE.len(),
            0,
            CutType::ParagraphEnd.to_string(),
            document_id,
        ),
        DocumentChunk::new(
            Uuid::new_v4(),
            TEST_TEXT_SHORT.to_string(),
            TEST_TEXT_SHORT.len(),
            1,
            CutType::ParagraphEnd.to_string(),
            document_id,
        ),
    ];

    println!("   Processing {} chunks", chunks.len());
    let expected_model = adapter.model().to_string();

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
                    summary.made_from,
                    Some(chunks[idx].base.id),
                    "Summary should link to correct chunk"
                );
                assert_eq!(
                    summary.base.id,
                    Uuid::new_v5(&chunks[idx].base.id, b"TextSummary"),
                    "Summary ID should be deterministic uuid5"
                );

                assert!(!summary.text.is_empty(), "Summary text should not be empty");
                assert_eq!(summary.model, expected_model, "Model name should match");
            }
        }
        Err(e) => {
            panic!("❌ Batch summarization failed: {e}");
        }
    }
}

#[tokio::test]
async fn test_summarization_deterministic_ids() {
    let adapter = create_adapter_from_env();

    println!("\n🧪 Testing deterministic summary IDs");

    let chunk_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let document_id = Uuid::new_v4();

    let chunk = DocumentChunk::new(
        chunk_id,
        TEST_TEXT_SHORT.to_string(),
        TEST_TEXT_SHORT.len(),
        0,
        CutType::ParagraphEnd.to_string(),
        document_id,
    );

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
            println!("   Summary ID: {}", summary1.base.id);

            // Same chunk_id should produce same summary id
            assert_eq!(
                summary1.base.id, summary2.base.id,
                "Same chunk should produce same summary ID"
            );

            // Verify the ID is the expected uuid5
            let expected_id = Uuid::new_v5(&chunk_id, b"TextSummary");
            assert_eq!(
                summary1.base.id, expected_id,
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
    let adapter = create_adapter_from_env();

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
            panic!("❌ Empty chunks test failed: {e}");
        }
    }
}

#[tokio::test]
async fn test_summarization_custom_prompt() {
    let adapter = create_adapter_from_env();

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
            panic!("❌ Custom prompt summarization failed: {e}");
        }
    }
}

// Helper functions for pretty printing

fn print_summarized_content(summarized: &SummarizedContent) {
    println!("   Summary: {}", summarized.summary);
    println!("   Description: {}", summarized.description);
}

fn print_text_summary(summary: &TextSummary) {
    println!("   ID: {}", summary.base.id);
    if let Some(made_from) = summary.made_from {
        println!("   Made from chunk: {made_from}");
    }
    println!("   Model: {}", summary.model);
    println!("   Text: {}", summary.text);
    if let Some(desc) = &summary.description {
        println!("   Description: {desc}");
    }
}
