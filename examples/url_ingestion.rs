//! Example: ingest an explicit HTTP(S) URL with `AddPipeline`.
//!
//! This example intentionally does not pick a public default URL. Start a local
//! fixture server or pass a URL you are comfortable fetching:
//!
//! ```bash
//! cargo run --example url_ingestion -- http://127.0.0.1:8000/page.html
//! ```

use cognee_core::RayonThreadPool;
use cognee_database::{IngestDb, connect, initialize};
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let Some(url) = std::env::args().nth(1) else {
        println!("=== Cognee URL Ingestion Example ===\n");
        println!("Pass an explicit HTTP(S) URL to fetch.");
        println!("Example:");
        println!("  cargo run --example url_ingestion -- http://127.0.0.1:8000/page.html");
        println!();
        println!("No network request was made.");
        return Ok(());
    };

    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("URL must start with http:// or https://".into());
    }

    println!("=== Cognee URL Ingestion Example ===\n");

    println!("1. Initializing local file storage...");
    let storage = Arc::new(LocalStorage::new("./url_ingestion_data".into()));
    storage.initialize().await?;
    println!("   Storage initialized at ./url_ingestion_data\n");

    println!("2. Initializing SQLite database...");
    let db = connect("sqlite:./url_ingestion.db").await?;
    initialize(&db).await?;
    let database = Arc::new(db);
    println!("   Database initialized at ./url_ingestion.db\n");

    println!("3. Creating ingestion pipeline...");
    let pipeline = AddPipeline::new(storage.clone(), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(RayonThreadPool::with_default_threads()?))
        .with_graph_db(Arc::new(MockGraphDB::new()))
        .with_vector_db(Arc::new(MockVectorDB::new()))
        .with_database(Arc::clone(&database));
    println!("   Pipeline created\n");

    let owner_id = Uuid::new_v4();
    println!("4. Ingesting URL: {url}");
    let data_items = pipeline
        .add(
            vec![DataInput::Url(url.clone())],
            "url_ingestion_dataset",
            owner_id,
            None,
        )
        .await?;

    println!("   Added {} data item(s)\n", data_items.len());
    for data in &data_items {
        println!("Data item:");
        println!("  ID:                 {}", data.id);
        println!("  Name:               {}", data.name);
        println!("  MIME type:          {}", data.mime_type);
        println!("  Extension:          {}", data.extension);
        println!("  Raw data:           {}", data.raw_data_location);
        println!("  Original data:      {}", data.original_data_location);
        println!("  Original MIME type: {:?}", data.original_mime_type);
        println!("  Loader engine:      {:?}", data.loader_engine);

        if let Some(metadata) = &data.external_metadata {
            println!("  URL metadata:       {metadata}");
        }
    }

    println!("\nNext steps:");
    println!(
        "  - HTML URLs store extracted text as raw_data_location and raw HTML as original_data_location."
    );
    println!(
        "  - Run cognify with LLM and embedding settings to build chunks, graph facts, and URL provenance."
    );
    println!(
        "  - Search can then retrieve the URL-sourced content like any other ingested document."
    );
    println!("\nTo clean up:");
    println!("  rm -rf ./url_ingestion_data ./url_ingestion.db");

    Ok(())
}
