use cognee_rust::database::{DatabaseTrait, SqliteDatabase};
use cognee_rust::ingestion::IngestPipeline;
use cognee_rust::models::DataInput;
use cognee_rust::storage::{LocalStorage, StorageTrait};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    println!("=== Cognee Add Operation Example ===\n");

    // Initialize storage
    println!("1. Initializing local file storage...");
    let storage_path = PathBuf::from("./data");
    let storage = Arc::new(LocalStorage::new(storage_path.clone()));
    storage.initialize().await?;
    println!("   ✓ Storage initialized at {:?}\n", storage_path);

    // Initialize database
    println!("2. Initializing SQLite database...");
    let database = Arc::new(SqliteDatabase::new("sqlite:./cognee.db").await?);
    database.initialize().await?;
    println!("   ✓ Database initialized at ./cognee.db\n");

    // Create ingestion pipeline
    println!("3. Creating ingestion pipeline...");
    let pipeline = IngestPipeline::new(storage.clone(), database.clone());
    println!("   ✓ Pipeline created\n");

    // Create a test user
    let owner_id = Uuid::new_v4();
    println!("4. Created owner ID: {}\n", owner_id);

    // Example 1: Add text content
    println!("5. Adding text content...");
    let text_inputs = vec![
        DataInput::Text("Hello, this is a test document about AI memory systems.".to_string()),
        DataInput::Text("Another piece of text content for testing purposes.".to_string()),
    ];

    let text_data = pipeline.add(text_inputs, "text_dataset", owner_id).await?;
    println!("   ✓ Added {} text items", text_data.len());
    for data in &text_data {
        println!("      - ID: {}, Name: {}", data.id, data.name);
    }
    println!();

    // Example 2: Test deduplication
    println!("6. Testing deduplication (adding same content again)...");
    let duplicate_inputs = vec![DataInput::Text(
        "Hello, this is a test document about AI memory systems.".to_string(),
    )];

    let duplicate_data = pipeline
        .add(duplicate_inputs, "text_dataset", owner_id)
        .await?;
    println!(
        "   ✓ Added {} items (should reuse existing data)",
        duplicate_data.len()
    );
    println!(
        "      - Same ID as before? {}",
        duplicate_data[0].id == text_data[0].id
    );
    println!();

    // Example 3: Add to a different dataset
    println!("7. Adding content to a different dataset...");
    let inputs = vec![DataInput::Text(
        "Content for a different dataset.".to_string(),
    )];

    let dataset2_data = pipeline.add(inputs, "dataset_two", owner_id).await?;
    println!("   ✓ Added {} items to 'dataset_two'", dataset2_data.len());
    println!();

    // Example 4: Retrieve dataset data
    println!("8. Retrieving all data for 'text_dataset'...");
    // Note: We need to get the dataset first to get its ID
    // For now, just show what we added
    println!("   Dataset contains {} data items:", text_data.len());
    for data in &text_data {
        println!(
            "      - {}: {}",
            data.name,
            data.content_hash[..16].to_string() + "..."
        );
    }
    println!();

    // Example 5: Verify storage
    println!("9. Verifying stored files...");
    for data in &text_data {
        let exists = storage.exists(&data.raw_data_location).await?;
        println!(
            "      - File exists at {}: {}",
            data.raw_data_location, exists
        );
    }
    println!();

    println!("=== Example completed successfully! ===");
    println!("\nYou can inspect:");
    println!("  - Stored files: ./data/");
    println!("  - Database: ./cognee.db (use SQLite browser)");
    println!("\nTo clean up:");
    println!("  rm -rf ./data ./cognee.db");

    Ok(())
}
