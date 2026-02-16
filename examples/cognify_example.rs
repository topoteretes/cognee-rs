//! End-to-end example: ingest text data → cognify (classify + chunk) → print chunks.
//!
//! Run with: cargo run --example cognify_example

use std::sync::Arc;

use cognee_database::{DatabaseTrait, SqliteDatabase};
use cognee_ingestion::IngestPipeline;
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    println!("=== Cognee Cognify Example ===\n");

    // Initialize storage and database
    println!("1. Initializing storage and database...");
    let storage = Arc::new(LocalStorage::new("./cognify_data".into()));
    storage.initialize().await?;

    let database = Arc::new(SqliteDatabase::new("sqlite:./cognify.db").await?);
    database.initialize().await?;
    println!("   Storage and database initialized\n");

    // Ingest some text data
    let owner_id = Uuid::new_v4();
    let ingest = IngestPipeline::new(storage.clone(), database.clone());

    let inputs = vec![
        DataInput::Text(
            "Artificial intelligence is transforming the world. \
             Machine learning models can now understand natural language, \
             generate images, and write code.\n\
             Deep learning architectures like transformers have revolutionized \
             NLP. Attention mechanisms allow models to focus on relevant parts \
             of the input sequence."
                .into(),
        ),
        DataInput::Text(
            "Rust is a systems programming language focused on safety and performance. \
             It prevents data races at compile time through its ownership system.\n\
             The borrow checker ensures memory safety without garbage collection. \
             This makes Rust ideal for embedded systems and WebAssembly."
                .into(),
        ),
    ];

    println!("2. Ingesting data...");
    let data_items = ingest.add(inputs, "example_dataset", owner_id).await?;
    println!("   Ingested {} data items\n", data_items.len());

    // NOTE: Full cognify pipeline now requires an LLM for knowledge graph extraction.
    // This example demonstrates just the chunking part via ExtractTextChunksPipeline.
    // For full graph extraction, see integration tests or set up an LLM adapter.

    use cognee_chunking::ExtractTextChunksPipeline;
    let chunk_pipeline = ExtractTextChunksPipeline::new(storage);
    let max_chunk_size = 10; // 10 words per chunk for demonstration

    println!(
        "3. Running text chunking (max_chunk_size={} words)...\n",
        max_chunk_size
    );
    let chunks = chunk_pipeline
        .extract_chunks(data_items, max_chunk_size)
        .await?;

    println!("Generated {} chunks:\n", chunks.len());
    for  chunk in &chunks {
        println!("--- Chunk {} ---", chunk.chunk_index);
        println!("  ID:        {}", chunk.id);
        println!("  Size:      {} words", chunk.chunk_size);
        println!("  Cut type:  {}", chunk.cut_type);
        println!("  Doc ID:    {}", chunk.document_id);
        println!("  Text:      {:?}", chunk.text);
        println!();
    }

    println!("=== Example completed! ===");
    println!("\nNote: Full cognify (with knowledge graph extraction) requires an LLM.");
    println!("      See integration tests for examples with OpenAI/Ollama.\n");
    println!("To clean up:");
    println!("  rm -rf ./cognify_data ./cognify.db");

    Ok(())
}
