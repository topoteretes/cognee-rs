//! Ingest pipeline built on the cognee-core [`Pipeline`] framework.
//!
//! Public surface:
//! - [`ProcessedInput`] — intermediate type between the two pipeline tasks
//! - [`process_input`] — Task 1: stream input to storage, compute hash
//! - [`persist_data`] — Task 2: resolve dataset, deduplicate, persist record
//! - [`make_process_input_task`] / [`make_persist_data_task`] — [`TypedTask`] wrappers
//! - [`build_add_pipeline`] — build a composable cognee-core [`Pipeline`]
//! - [`AddPipeline`] — convenience wrapper with a simple `add()` API

use std::path::Path;
use std::sync::Arc;
use tracing::{info, instrument};
use uuid::Uuid;

use cognee_core::{Pipeline, PipelineBuilder, TypedTask};
use cognee_database::IngestDb;
use cognee_models::{Data, DataInput, Dataset};
use cognee_storage::StorageTrait;

use crate::content_hasher::HashAlgorithm;
use crate::id_generation::{generate_data_id, generate_dataset_id};
use crate::loader_registry::get_loader_name;
use crate::url_crawler::{HtmlParser, UrlFetcher};

// ---------------------------------------------------------------------------
// ProcessedInput
// ---------------------------------------------------------------------------

/// Metadata extracted from a [`DataInput`] during streaming processing.
///
/// Contains everything needed by [`persist_data`] to create a [`Data`] record
/// without needing the original `DataInput`.
#[derive(Debug, Clone)]
pub struct ProcessedInput {
    pub content_hash: String,
    pub data_id: Uuid,
    pub storage_location: String,
    pub label: Option<String>,
    pub original_extension: String,
    pub original_mime_type: String,
    pub loader_engine: String,
    pub data_size: i64,
    pub name: String,
    pub raw_data_uri: String,
    pub original_location: String,
    pub owner_id: Uuid,
    pub tenant_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Task 1 implementation: DataInput → ProcessedInput
// ---------------------------------------------------------------------------

/// Process a single [`DataInput`]: resolve URLs, stream to storage, compute
/// content hash, and extract all metadata needed to create a [`Data`] record.
///
/// This is the first step of the ingest pipeline (Task 1).
#[instrument(name = "ingestion.process_input", skip(input, storage))]
pub async fn process_input(
    input: &DataInput,
    storage: &dyn StorageTrait,
    hash_algorithm: HashAlgorithm,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<ProcessedInput, Box<dyn std::error::Error>> {
    use tokio::sync::Mutex;

    // For URL inputs: fetch HTML and extract plain text first, then process
    // the text content exactly like a Text input. The original URL is preserved
    // via `extract_original_location` which returns the URL as-is.
    let resolved_text: Option<DataInput> = if let DataInput::Url(url) = input {
        let html = UrlFetcher::new()?.fetch(url).await?;
        let text = HtmlParser::extract_text(&html);
        Some(DataInput::Text(text))
    } else {
        None
    };
    let effective_input: &DataInput = resolved_text.as_ref().unwrap_or(input);

    // Determine filename and metadata before streaming.
    // For URL→Text: metadata comes from the original URL input, not the resolved text.
    let (file_name, original_extension, original_mime_type, label) = extract_file_metadata(input);

    let loader_engine = get_loader_name(&original_extension);

    // Use Arc<Mutex<>> so closures can share the hasher and writer
    let size_counter: Arc<Mutex<i64>> = Arc::new(Mutex::new(0i64));
    let writer = Arc::new(Mutex::new(storage.create_writer(&file_name).await?));

    // Accumulate raw bytes for hashing (streaming, but buffered for hash computation)
    let raw_bytes: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let size_clone = size_counter.clone();
    let writer_clone = writer.clone();
    let raw_bytes_clone = raw_bytes.clone();

    effective_input
        .process_by_chunks(move |chunk| {
            let size = size_clone.clone();
            let writer = writer_clone.clone();
            let bytes = raw_bytes_clone.clone();
            let chunk_owned = chunk.to_vec();
            async move {
                *size.lock().await += chunk_owned.len() as i64;
                writer.lock().await.write_chunk(&chunk_owned).await?;
                bytes.lock().await.extend_from_slice(&chunk_owned);
                Ok::<(), Box<dyn std::error::Error>>(())
            }
        })
        .await?;

    // Finalise hash — content-only, no owner_id (Python compatible)
    let collected = Arc::try_unwrap(raw_bytes)
        .map_err(|_| "Failed to unwrap bytes")?
        .into_inner();
    let content_hash =
        crate::content_hasher::ContentHasher::hash_content(&collected, hash_algorithm);
    let data_size = Arc::try_unwrap(size_counter)
        .map_err(|_| "Failed to unwrap size counter")?
        .into_inner();

    let data_id = generate_data_id(&content_hash, owner_id, tenant_id);

    let writer = Arc::try_unwrap(writer)
        .map_err(|_| "Failed to unwrap writer")?
        .into_inner();
    let storage_location = writer.finish().await?;

    // Compute derived fields that previously lived in add()
    let raw_data_uri = storage_location_to_uri(storage.base_path(), &storage_location);
    let name = extract_name(input, &content_hash);
    let original_location = match input {
        DataInput::Text(_) => raw_data_uri.clone(),
        _ => extract_original_location(input),
    };

    Ok(ProcessedInput {
        content_hash,
        data_id,
        storage_location,
        label,
        original_extension,
        original_mime_type,
        loader_engine: loader_engine.to_string(),
        data_size,
        name,
        raw_data_uri,
        original_location,
        owner_id,
        tenant_id,
    })
}

// ---------------------------------------------------------------------------
// Task 2 implementation: ProcessedInput → Data
// ---------------------------------------------------------------------------

/// Persist a [`ProcessedInput`] as a [`Data`] record: resolve or create the
/// dataset, deduplicate by content hash, create the record if new, and attach
/// it to the dataset.
///
/// Dataset resolution uses a deterministic UUID5 ID so the lookup + optional
/// `INSERT OR IGNORE` is idempotent and cheap — safe to call once per item.
///
/// This is the second step of the ingest pipeline (Task 2).
#[instrument(
    name = "ingestion.persist_data",
    skip(processed, database),
    fields(data_id = %processed.data_id)
)]
pub async fn persist_data(
    processed: &ProcessedInput,
    database: &dyn IngestDb,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<Data, Box<dyn std::error::Error>> {
    // Resolve or create the dataset (idempotent: deterministic UUID5 ID).
    let dataset_id = generate_dataset_id(dataset_name, owner_id, tenant_id);
    let dataset = match database
        .get_dataset_by_name(dataset_name, owner_id, tenant_id)
        .await?
    {
        Some(ds) => ds,
        None => {
            let new_dataset =
                Dataset::new(dataset_name.to_string(), owner_id, tenant_id, dataset_id);
            database.create_dataset(new_dataset).await?
        }
    };
    info!(dataset_id = %dataset.id, "dataset resolved");

    let data_id = processed.data_id;

    if let Some(existing_data) = database.get_data(data_id).await? {
        database.attach_data_to_dataset(dataset.id, data_id).await?;
        info!(data_id = %data_id, is_duplicate = true, "input processed");
        return Ok(existing_data);
    }

    let mut data_builder = Data::builder(
        data_id,
        processed.name.clone(),
        processed.raw_data_uri.clone(),
        processed.original_location.clone(),
        processed.original_extension.clone(),
        processed.original_mime_type.clone(),
        processed.content_hash.clone(),
        processed.owner_id,
    )
    .original_extension(processed.original_extension.clone())
    .original_mime_type(processed.original_mime_type.clone())
    .loader_engine(processed.loader_engine.clone())
    .raw_content_hash(processed.content_hash.clone())
    .data_size(processed.data_size);
    if let Some(tid) = processed.tenant_id {
        data_builder = data_builder.tenant_id(tid);
    }
    if let Some(ref lbl) = processed.label {
        data_builder = data_builder.label(lbl.clone());
    }
    let data = data_builder.build();

    let saved_data = database.create_data(data).await?;

    database.attach_data_to_dataset(dataset.id, data_id).await?;

    info!(data_id = %data_id, is_duplicate = false, "input processed");
    Ok(saved_data)
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Return `(file_name, extension, mime_type, label)` for the given input.
fn extract_file_metadata(input: &DataInput) -> (String, String, String, Option<String>) {
    match input {
        DataInput::FilePath(path) => {
            let clean_path = path.strip_prefix("file://").unwrap_or(path);
            let p = Path::new(clean_path);
            let file_name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file.bin")
                .to_string();
            let extension = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string();
            let mime = mime_guess::from_path(clean_path)
                .first_or_octet_stream()
                .to_string();
            (file_name, extension, mime, None)
        }
        DataInput::Text(_) => {
            // Will be renamed to text_<hash>.txt after hashing; use placeholder for now
            (
                "text_placeholder.txt".to_string(),
                "txt".to_string(),
                "text/plain".to_string(),
                None,
            )
        }
        DataInput::Url(_url) => {
            // Fetched HTML is extracted to plain text and stored as text_<hash>.txt.
            // Extension and MIME reflect the original source (HTML), loader = beautiful_soup_loader.
            (
                "text_placeholder.txt".to_string(),
                "html".to_string(),
                "text/html".to_string(),
                None,
            )
        }
        DataInput::S3Path(_) => (
            "s3_file.bin".to_string(),
            "bin".to_string(),
            "application/octet-stream".to_string(),
            None,
        ),
        DataInput::Binary { name, .. } => {
            let ext = Path::new(name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("bin")
                .to_string();
            let mime = mime_guess::from_path(name)
                .first_or_octet_stream()
                .to_string();
            (name.clone(), ext, mime, None)
        }
        DataInput::DataItem { data, label } => {
            let (file_name, ext, mime, _) = extract_file_metadata(data);
            (file_name, ext, mime, Some(label.clone()))
        }
    }
}

/// Convert a relative storage location into a `file://` absolute URI.
fn storage_location_to_uri(base_path: &str, location: &str) -> String {
    if base_path.is_empty() {
        // MockStorage or other non-filesystem backend — return as-is
        location.to_string()
    } else {
        let abs = Path::new(base_path).join(location);
        format!("file://{}", abs.display())
    }
}

/// Derive a human-readable name for the stored Data record.
fn extract_name(input: &DataInput, content_hash: &str) -> String {
    match input {
        DataInput::Text(_) => format!("text_{}", content_hash),
        DataInput::FilePath(path) => {
            let clean_path = path.strip_prefix("file://").unwrap_or(path);
            Path::new(clean_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        }
        DataInput::Url(_) => format!("text_{}", content_hash),
        DataInput::S3Path(path) => path
            .split('/')
            .next_back()
            .unwrap_or("s3_content")
            .to_string(),
        DataInput::Binary { name, .. } => name.clone(),
        DataInput::DataItem { data, .. } => extract_name(data, content_hash),
    }
}

fn extract_original_location(input: &DataInput) -> String {
    match input {
        DataInput::Text(_) => "text://inline".to_string(),
        DataInput::FilePath(path) => {
            if path.starts_with("file://") {
                path.clone()
            } else {
                format!("file://{}", path)
            }
        }
        DataInput::Url(url) => url.clone(),
        DataInput::S3Path(path) => path.clone(),
        DataInput::Binary { name, .. } => format!("binary://{}", name),
        DataInput::DataItem { data, .. } => extract_original_location(data),
    }
}

// ---------------------------------------------------------------------------
// Task 1 wrapper: DataInput → ProcessedInput
// ---------------------------------------------------------------------------

/// Build a [`TypedTask`] that streams a [`DataInput`] to storage, hashes its
/// content, and returns a self-contained [`ProcessedInput`].
pub fn make_process_input_task(
    storage: Arc<dyn StorageTrait>,
    hash_algorithm: HashAlgorithm,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> TypedTask<DataInput, ProcessedInput> {
    TypedTask::async_fn(move |input: &DataInput, _ctx| {
        let input = input.clone();
        let storage = Arc::clone(&storage);
        Box::pin(async move {
            process_input(&input, &*storage, hash_algorithm, owner_id, tenant_id)
                .await
                .map(Box::new)
                .map_err(|e| format!("{e}").into())
        })
    })
}

// ---------------------------------------------------------------------------
// Task 2 wrapper: ProcessedInput → Data
// ---------------------------------------------------------------------------

/// Build a [`TypedTask`] that resolves or creates the dataset, deduplicates by
/// content hash, persists a new [`Data`] record if needed, and returns it.
pub fn make_persist_data_task(
    database: Arc<dyn IngestDb>,
    dataset_name: String,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> TypedTask<ProcessedInput, Data> {
    TypedTask::async_fn(move |processed: &ProcessedInput, _ctx| {
        let processed = processed.clone();
        let database = Arc::clone(&database);
        let dataset_name = dataset_name.clone();
        Box::pin(async move {
            persist_data(&processed, &*database, &dataset_name, owner_id, tenant_id)
                .await
                .map(Box::new)
                .map_err(|e| format!("{e}").into())
        })
    })
}

// ---------------------------------------------------------------------------
// Pipeline builder
// ---------------------------------------------------------------------------

/// Build a complete ingest [`Pipeline`]: [`DataInput`] → [`ProcessedInput`] → [`Data`].
pub fn build_add_pipeline(
    storage: Arc<dyn StorageTrait>,
    database: Arc<dyn IngestDb>,
    hash_algorithm: HashAlgorithm,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Pipeline {
    PipelineBuilder::new_with_task(
        "ingestion.add",
        make_process_input_task(Arc::clone(&storage), hash_algorithm, owner_id, tenant_id),
    )
    .add_task(make_persist_data_task(
        database,
        dataset_name.to_string(),
        owner_id,
        tenant_id,
    ))
    .with_name("ingestion")
    .build()
}

// ---------------------------------------------------------------------------
// AddPipeline — convenience wrapper
// ---------------------------------------------------------------------------

/// Ingest pipeline driven by the cognee-core task framework.
///
/// Wraps [`build_add_pipeline`] and the underlying free functions
/// ([`process_input`] + [`persist_data`]) behind a simple
/// `add(inputs, dataset_name, owner_id, tenant_id) -> Vec<Data>` API.
///
/// For composable pipeline-based execution (with concurrency, retry, etc.),
/// use [`build_add_pipeline`] + [`cognee_core::execute`] directly.
///
/// [`cognee_core::execute`]: cognee_core::execute
pub struct AddPipeline {
    storage: Arc<dyn StorageTrait>,
    database: Arc<dyn IngestDb>,
    hash_algorithm: HashAlgorithm,
}

impl AddPipeline {
    /// Create with the default MD5 hashing (Python-compatible).
    pub fn new(storage: Arc<dyn StorageTrait>, database: Arc<dyn IngestDb>) -> Self {
        Self {
            storage,
            database,
            hash_algorithm: HashAlgorithm::default(),
        }
    }

    /// Create with an explicit hash algorithm.
    pub fn new_with_algorithm(
        storage: Arc<dyn StorageTrait>,
        database: Arc<dyn IngestDb>,
        hash_algorithm: HashAlgorithm,
    ) -> Self {
        Self {
            storage,
            database,
            hash_algorithm,
        }
    }

    #[instrument(
        name = "ingestion.add",
        skip(self, inputs),
        fields(dataset_name, owner_id = %owner_id, inputs_count = inputs.len())
    )]
    pub async fn add(
        &self,
        inputs: Vec<DataInput>,
        dataset_name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Vec<Data>, Box<dyn std::error::Error>> {
        let mut created_data = Vec::new();

        for input in &inputs {
            let processed = process_input(
                input,
                &*self.storage,
                self.hash_algorithm,
                owner_id,
                tenant_id,
            )
            .await?;

            let data = persist_data(
                &processed,
                &*self.database,
                dataset_name,
                owner_id,
                tenant_id,
            )
            .await?;
            created_data.push(data);
        }

        Ok(created_data)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_database::{connect, initialize, ops};
    use cognee_storage::MockStorage;
    use std::io::Write;
    use tempfile::NamedTempFile;

    async fn make_pipeline() -> (AddPipeline, Arc<cognee_database::DatabaseConnection>) {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let db = Arc::new(db);
        let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
        let pipeline = AddPipeline::new(storage, db.clone() as Arc<dyn IngestDb>);
        (pipeline, db)
    }

    #[tokio::test]
    async fn test_add_text_input() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let inputs = vec![DataInput::Text("Hello, world!".to_string())];
        let result = pipeline.add(inputs, "test_dataset", owner_id, None).await;
        assert!(result.is_ok(), "add should succeed: {:?}", result.err());

        let data = result.unwrap();
        assert_eq!(data.len(), 1);
        // Name for text inputs is text_<hash>
        assert!(
            data[0].name.starts_with("text_"),
            "name should start with text_"
        );
        assert_eq!(data[0].mime_type, "text/plain");
        assert_eq!(data[0].extension, "txt");

        let datasets = ops::datasets::list_datasets_by_owner(&db, owner_id)
            .await
            .unwrap();
        assert_eq!(datasets.len(), 1);
        let ds_data = ops::datasets::get_dataset_data(&db, datasets[0].id)
            .await
            .unwrap();
        assert_eq!(ds_data.len(), 1);
    }

    #[tokio::test]
    async fn test_add_file_input() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "Test file content").unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();

        let inputs = vec![DataInput::FilePath(file_path)];
        let result = pipeline.add(inputs, "test_dataset", owner_id, None).await;
        assert!(result.is_ok());

        let data = result.unwrap();
        assert_eq!(data.len(), 1);
        assert!(!data[0].name.is_empty());

        let datasets = ops::datasets::list_datasets_by_owner(&db, owner_id)
            .await
            .unwrap();
        assert_eq!(datasets.len(), 1);
    }

    #[tokio::test]
    async fn test_add_multiple_inputs() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let inputs = vec![
            DataInput::Text("First text".to_string()),
            DataInput::Text("Second text".to_string()),
        ];
        let result = pipeline.add(inputs, "test_dataset", owner_id, None).await;
        assert!(result.is_ok());

        let data = result.unwrap();
        assert_eq!(data.len(), 2);

        let datasets = ops::datasets::list_datasets_by_owner(&db, owner_id)
            .await
            .unwrap();
        assert_eq!(datasets.len(), 1);
        let ds_data = ops::datasets::get_dataset_data(&db, datasets[0].id)
            .await
            .unwrap();
        assert_eq!(ds_data.len(), 2);
    }

    #[tokio::test]
    async fn test_deduplication_same_content() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let content = "Duplicate content";
        let result1 = pipeline
            .add(
                vec![DataInput::Text(content.to_string())],
                "test_dataset",
                owner_id,
                None,
            )
            .await
            .unwrap();
        let result2 = pipeline
            .add(
                vec![DataInput::Text(content.to_string())],
                "test_dataset",
                owner_id,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result1[0].id, result2[0].id);
        assert_eq!(result1[0].content_hash, result2[0].content_hash);

        let dataset = ops::datasets::get_dataset_by_name(&db, "test_dataset", owner_id, None)
            .await
            .unwrap()
            .unwrap();
        let ds_data = ops::datasets::get_dataset_data(&db, dataset.id)
            .await
            .unwrap();
        assert_eq!(ds_data.len(), 1);
    }

    #[tokio::test]
    async fn test_different_owners_same_hash_different_ids() {
        let (pipeline, _db) = make_pipeline().await;
        let owner1 = Uuid::new_v4();
        let owner2 = Uuid::new_v4();

        let result1 = pipeline
            .add(
                vec![DataInput::Text("Same content".to_string())],
                "ds1",
                owner1,
                None,
            )
            .await
            .unwrap();
        let result2 = pipeline
            .add(
                vec![DataInput::Text("Same content".to_string())],
                "ds2",
                owner2,
                None,
            )
            .await
            .unwrap();

        // Content hash is content-only (Python compat): same content → same hash
        assert_eq!(
            result1[0].content_hash, result2[0].content_hash,
            "content hash is owner-independent"
        );
        // But data_id differs because owner_id is mixed into UUID5 seed
        assert_ne!(result1[0].id, result2[0].id, "data_id must differ by owner");
    }

    #[tokio::test]
    async fn test_multiple_datasets() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        pipeline
            .add(
                vec![DataInput::Text("Content 1".to_string())],
                "dataset1",
                owner_id,
                None,
            )
            .await
            .unwrap();
        pipeline
            .add(
                vec![DataInput::Text("Content 2".to_string())],
                "dataset2",
                owner_id,
                None,
            )
            .await
            .unwrap();

        let datasets = ops::datasets::list_datasets_by_owner(&db, owner_id)
            .await
            .unwrap();
        assert_eq!(datasets.len(), 2);
    }

    #[tokio::test]
    async fn test_reuse_dataset() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        pipeline
            .add(
                vec![DataInput::Text("Content 1".to_string())],
                "same_dataset",
                owner_id,
                None,
            )
            .await
            .unwrap();
        pipeline
            .add(
                vec![DataInput::Text("Content 2".to_string())],
                "same_dataset",
                owner_id,
                None,
            )
            .await
            .unwrap();

        let datasets = ops::datasets::list_datasets_by_owner(&db, owner_id)
            .await
            .unwrap();
        assert_eq!(datasets.len(), 1);
        let ds_data = ops::datasets::get_dataset_data(&db, datasets[0].id)
            .await
            .unwrap();
        assert_eq!(ds_data.len(), 2);
    }

    #[tokio::test]
    async fn test_content_hash_deterministic() {
        let (pipeline, _db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let result1 = pipeline
            .add(
                vec![DataInput::Text("Test content".to_string())],
                "dataset1",
                owner_id,
                None,
            )
            .await
            .unwrap();
        let result2 = pipeline
            .add(
                vec![DataInput::Text("Test content".to_string())],
                "dataset1",
                owner_id,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result1[0].content_hash, result2[0].content_hash);
        assert_eq!(result1[0].id, result2[0].id);
    }

    #[tokio::test]
    async fn test_dataset_id_is_deterministic() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        pipeline
            .add(
                vec![DataInput::Text("any content".to_string())],
                "my_dataset",
                owner_id,
                None,
            )
            .await
            .unwrap();
        pipeline
            .add(
                vec![DataInput::Text("other content".to_string())],
                "my_dataset",
                owner_id,
                None,
            )
            .await
            .unwrap();

        // There must be exactly one dataset (deterministic ID prevents duplicates)
        let datasets = ops::datasets::list_datasets_by_owner(&db, owner_id)
            .await
            .unwrap();
        assert_eq!(
            datasets.len(),
            1,
            "deterministic dataset ID should prevent duplicate creation"
        );
    }

    #[tokio::test]
    async fn test_loader_engine_populated() {
        let (pipeline, _db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "content").unwrap();
        // Rename to .pdf to check loader_engine mapping
        let pdf_path = temp_file.path().with_extension("pdf");
        std::fs::copy(temp_file.path(), &pdf_path).unwrap();

        let result = pipeline
            .add(
                vec![DataInput::FilePath(pdf_path.to_str().unwrap().to_string())],
                "ds",
                owner_id,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result[0].loader_engine.as_deref(), Some("pypdf_loader"));
        let _ = std::fs::remove_file(&pdf_path);
    }

    #[tokio::test]
    async fn test_tenant_id_stored() {
        let (pipeline, _db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();

        let result = pipeline
            .add(
                vec![DataInput::Text("tenant content".to_string())],
                "ds",
                owner_id,
                Some(tenant_id),
            )
            .await
            .unwrap();

        assert_eq!(result[0].tenant_id, Some(tenant_id));
    }

    #[tokio::test]
    async fn test_data_item_label_stored() {
        let (pipeline, _db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let result = pipeline
            .add(
                vec![DataInput::DataItem {
                    data: Box::new(DataInput::Text("labelled content".to_string())),
                    label: "my-label".to_string(),
                }],
                "ds",
                owner_id,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].label.as_deref(),
            Some("my-label"),
            "DataItem label must be stored in the Data record"
        );
    }
}
