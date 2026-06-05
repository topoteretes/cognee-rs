use std::sync::Arc;

use cognee_lib::add::build_add_pipeline;
use cognee_lib::add::{HashAlgorithm, generate_dataset_id};
use cognee_lib::core::{NoopWatcher, Value, execute};
use cognee_lib::database::{IngestDb, ops};
use cognee_lib::models::{Data, DataInput};
use cognee_lib::storage::LocalStorage;
use cognee_lib::storage::StorageTrait;
use cognee_test_utils::{MockStorage, test_task_context};
use tempfile::TempDir;
use uuid::Uuid;

/// Downcast an `Arc<dyn Value>` to `&T` by going through the vtable.
///
/// Direct `.as_any()` on `Arc<dyn Value>` hits the blanket `Value` impl for
/// the `Arc` itself. We must deref through the `Arc` first to reach the inner
/// vtable dispatch.
fn downcast_ref<T: 'static>(v: &Arc<dyn Value>) -> &T {
    (**v)
        .as_any()
        .downcast_ref::<T>()
        .unwrap_or_else(|| panic!("expected {}", std::any::type_name::<T>()))
}

#[tokio::test]
async fn pipeline_based_add_text() {
    let (_handle, ctx, db) = test_task_context().await;
    let storage: Arc<dyn cognee_lib::storage::StorageTrait> = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    let pipeline = build_add_pipeline(
        storage,
        db.clone() as Arc<dyn IngestDb>,
        HashAlgorithm::default(),
        "test_ds",
        owner_id,
        None,
    );

    let inputs: Vec<Arc<dyn Value>> =
        vec![Arc::new(DataInput::Text("Hello pipeline!".to_string()))];

    let results = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();
    assert_eq!(results.len(), 1);

    let data: &Data = downcast_ref(&results[0]);
    assert!(data.name.starts_with("text_"));
    assert_eq!(data.mime_type, "text/plain");
    assert_eq!(data.extension, "txt");
    assert_eq!(data.owner_id, owner_id);

    // Verify it's actually in the DB
    let dataset_id = generate_dataset_id("test_ds", owner_id, None);
    let ds_data = ops::datasets::get_dataset_data(&db, dataset_id)
        .await
        .unwrap();
    assert_eq!(ds_data.len(), 1);
}

#[tokio::test]
async fn pipeline_based_add_url_html() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/robots.txt")
        .with_status(404)
        .create_async()
        .await;
    let html = concat!(
        "<html><head><title>Pipeline URL Fixture</title>",
        "<style>.hidden{display:none}</style></head>",
        "<body><h1>Pipeline URL heading</h1>",
        "<p>Local URL body from the add-pipeline E2E test.</p>",
        "<script>window.secret = true;</script></body></html>"
    );
    let page = server
        .mock("GET", "/page.html")
        .with_status(200)
        .with_header("content-type", "text/html; charset=utf-8")
        .with_body(html)
        .create_async()
        .await;

    let (_handle, ctx, db) = test_task_context().await;
    let storage_dir = TempDir::new().expect("temp storage dir");
    let local_storage = Arc::new(LocalStorage::new(storage_dir.path().to_path_buf()));
    local_storage
        .initialize()
        .await
        .expect("initialize storage");
    let storage: Arc<dyn cognee_lib::storage::StorageTrait> = local_storage.clone();
    let owner_id = Uuid::new_v4();
    let url = format!("{}/page.html", server.url());

    let pipeline = build_add_pipeline(
        storage,
        db.clone() as Arc<dyn IngestDb>,
        HashAlgorithm::default(),
        "url_ds",
        owner_id,
        None,
    );

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(DataInput::Url(url.clone()))];
    let results = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();
    assert_eq!(results.len(), 1);

    let data: &Data = downcast_ref(&results[0]);
    assert_eq!(data.mime_type, "text/plain");
    assert_eq!(data.extension, "txt");
    assert_eq!(data.original_mime_type.as_deref(), Some("text/html"));
    assert_eq!(data.original_extension.as_deref(), Some("html"));
    assert_eq!(data.loader_engine.as_deref(), Some("beautiful_soup_loader"));
    assert_ne!(data.raw_data_location, data.original_data_location);

    let extracted = local_storage
        .retrieve(&data.raw_data_location)
        .await
        .expect("extracted text stored");
    let extracted_text = String::from_utf8(extracted).expect("extracted text is utf8");
    assert!(extracted_text.contains("Pipeline URL heading"));
    assert!(extracted_text.contains("Local URL body from the add-pipeline E2E test."));
    assert!(!extracted_text.contains("<html>"));
    assert!(!extracted_text.contains("window.secret"));

    let raw_html = local_storage
        .retrieve(&data.original_data_location)
        .await
        .expect("raw html stored");
    assert_eq!(raw_html, html.as_bytes());

    let metadata: serde_json::Value =
        serde_json::from_str(data.external_metadata.as_deref().expect("url metadata"))
            .expect("metadata json");
    assert_eq!(metadata["source"], "url");
    assert_eq!(metadata["url"], url);
    assert_eq!(metadata["final_url"], url);
    assert_eq!(metadata["content_type"], "text/html; charset=utf-8");
    assert_eq!(metadata["title"], "Pipeline URL Fixture");

    let dataset_id = generate_dataset_id("url_ds", owner_id, None);
    let ds_data = ops::datasets::get_dataset_data(&db, dataset_id)
        .await
        .unwrap();
    assert_eq!(ds_data.len(), 1);
    assert_eq!(ds_data[0].id, data.id);
    page.assert_async().await;
}

#[tokio::test]
async fn pipeline_based_add_multiple() {
    let (_handle, ctx, db) = test_task_context().await;
    let storage: Arc<dyn cognee_lib::storage::StorageTrait> = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    let pipeline = build_add_pipeline(
        storage,
        db.clone() as Arc<dyn IngestDb>,
        HashAlgorithm::default(),
        "multi_ds",
        owner_id,
        None,
    );

    let inputs: Vec<Arc<dyn Value>> = vec![
        Arc::new(DataInput::Text("First".to_string())),
        Arc::new(DataInput::Text("Second".to_string())),
    ];

    let results = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();
    assert_eq!(results.len(), 2);

    let dataset_id = generate_dataset_id("multi_ds", owner_id, None);
    let ds_data = ops::datasets::get_dataset_data(&db, dataset_id)
        .await
        .unwrap();
    assert_eq!(ds_data.len(), 2);
}

#[tokio::test]
async fn pipeline_deduplication() {
    let (_handle, ctx, db) = test_task_context().await;
    let storage: Arc<dyn cognee_lib::storage::StorageTrait> = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();

    let pipeline = build_add_pipeline(
        storage,
        db.clone() as Arc<dyn IngestDb>,
        HashAlgorithm::default(),
        "dedup_ds",
        owner_id,
        None,
    );

    // Process the same content twice
    let inputs: Vec<Arc<dyn Value>> = vec![
        Arc::new(DataInput::Text("duplicate content".to_string())),
        Arc::new(DataInput::Text("duplicate content".to_string())),
    ];

    let results = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();
    assert_eq!(results.len(), 2);

    let d1: &Data = downcast_ref(&results[0]);
    let d2: &Data = downcast_ref(&results[1]);
    assert_eq!(d1.id, d2.id, "duplicate content should yield same data_id");

    // Only one record in the DB
    let dataset_id = generate_dataset_id("dedup_ds", owner_id, None);
    let ds_data = ops::datasets::get_dataset_data(&db, dataset_id)
        .await
        .unwrap();
    assert_eq!(ds_data.len(), 1);
}

#[tokio::test]
async fn pipeline_tenant_isolation() {
    let (_handle, ctx, db) = test_task_context().await;
    let storage: Arc<dyn cognee_lib::storage::StorageTrait> = Arc::new(MockStorage::new());
    let owner_id = Uuid::new_v4();
    let tenant_id = Uuid::new_v4();

    let pipeline = build_add_pipeline(
        storage,
        db.clone() as Arc<dyn IngestDb>,
        HashAlgorithm::default(),
        "tenant_ds",
        owner_id,
        Some(tenant_id),
    );

    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(DataInput::Text("tenant content".to_string()))];

    let results = execute(&pipeline, inputs, ctx, &NoopWatcher).await.unwrap();
    assert_eq!(results.len(), 1);

    let data: &Data = downcast_ref(&results[0]);
    assert_eq!(data.tenant_id, Some(tenant_id));
}
