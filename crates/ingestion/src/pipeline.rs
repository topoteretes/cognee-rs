//! Ingest pipeline built on the cognee-core [`Pipeline`] framework.
//!
//! Public surface:
//! - [`ProcessedInput`] — intermediate type between the two pipeline tasks
//! - [`process_input`] — Task 1: stream input to storage, compute hash
//! - [`persist_data`] — Task 2: resolve dataset, deduplicate, persist record
//! - [`make_process_input_task`] / [`make_persist_data_task`] — [`TypedTask`] wrappers
//! - [`build_add_pipeline`] — build a composable cognee-core [`Pipeline`]
//! - [`AddPipeline`] — convenience wrapper with a simple `add()` API

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, instrument};
use uuid::Uuid;

use cognee_core::CpuPool;
#[cfg(test)]
use cognee_core::RayonThreadPool;
use cognee_core::pipeline::DataIdFn;
use cognee_core::pipeline_run_registry::DbPipelineWatcher;
use cognee_core::task::Value;
use cognee_core::{Pipeline, PipelineBuilder, PipelineContext, TaskContextBuilder, TypedTask};
use cognee_database::{AclDb, DatabaseConnection, IngestDb, PipelineRunRepository};
use cognee_graph::GraphDBTrait;
use cognee_models::{Data, DataInput, Dataset};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;

use crate::content_hasher::HashAlgorithm;
use crate::id_generation::{generate_data_id, generate_dataset_id};
use crate::loader_registry::get_loader_name;
use crate::url_resolver::resolve_url_input;

// ---------------------------------------------------------------------------
// AddParams
// ---------------------------------------------------------------------------

/// Optional parameters for the [`AddPipeline::add`] method.
///
/// All fields default to `None`/sensible values. Use the builder methods or
/// struct literal syntax to configure.
#[derive(Debug, Clone, Default)]
pub struct AddParams {
    /// List of node identifiers for graph organisation and access control grouping.
    /// Stored as a JSON string in `Data.node_set`.
    pub node_set: Option<Vec<String>>,

    /// Target an existing dataset by UUID instead of name.
    pub dataset_id: Option<Uuid>,

    /// Maps MIME types or file extensions to preferred loader names.
    pub preferred_loaders: Option<HashMap<String, String>>,

    /// Importance weight (0.0 to 1.0) for relevance scoring.
    pub importance_weight: Option<f64>,
}

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
    pub stored_extension: String,
    pub stored_mime_type: String,
    pub original_extension: String,
    pub original_mime_type: String,
    pub loader_engine: String,
    pub data_size: i64,
    pub name: String,
    pub raw_data_uri: String,
    pub original_location: String,
    pub owner_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub external_metadata: Option<String>,
    /// JSON-serialized node set identifiers for graph organisation.
    pub node_set: Option<String>,
    /// Importance weight for ranking (0.0 to 1.0).
    pub importance_weight: Option<f64>,
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

    let resolved_url = if let DataInput::Url(url) = input {
        Some(resolve_url_input(url).await?)
    } else {
        None
    };
    let effective_input: &DataInput = resolved_url
        .as_ref()
        .map(|resolved| &resolved.input)
        .unwrap_or(input);

    // Determine filename and metadata before streaming.
    // For URL inputs the Content-Type-derived metadata takes precedence.
    let (
        file_name,
        stored_extension,
        stored_mime_type,
        original_extension,
        original_mime_type,
        label,
        loader_engine,
    ) = if let Some(resolved) = resolved_url.as_ref() {
        let metadata = &resolved.metadata;
        let fname = format!("text_placeholder.{}", metadata.stored_extension);
        (
            fname,
            metadata.stored_extension.clone(),
            metadata.stored_mime_type.clone(),
            metadata.source_extension.clone(),
            metadata.source_mime_type.clone(),
            None,
            metadata.loader_engine.clone(),
        )
    } else {
        let (fname, ext, mime, lbl) = extract_file_metadata(input);
        let loader = get_loader_name(&ext).to_string();
        (fname, ext.clone(), mime.clone(), ext, mime, lbl, loader)
    };

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

    let external_metadata = match input {
        DataInput::DataItem {
            external_metadata, ..
        } => external_metadata.clone(),
        _ => None,
    };

    Ok(ProcessedInput {
        content_hash,
        data_id,
        storage_location,
        label,
        stored_extension,
        stored_mime_type,
        original_extension,
        original_mime_type,
        loader_engine: loader_engine.to_string(),
        data_size,
        name,
        raw_data_uri,
        original_location,
        owner_id,
        tenant_id,
        external_metadata,
        node_set: None,
        importance_weight: None,
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
    persist_data_with_acl(
        processed,
        database,
        dataset_name,
        owner_id,
        tenant_id,
        None,
        None,
    )
    .await
}

/// Like [`persist_data`], but optionally grants all four ACL permissions
/// (read, write, delete, share) to the owner when a new dataset is created.
///
/// When `acl_db` is `Some`, the owner is ensured as a principal and receives
/// all permissions on newly created datasets, matching Python's
/// `create_authorized_dataset()` behavior.
///
/// When `target_dataset_id` is `Some`, the pipeline looks up the dataset by UUID
/// instead of name, allowing callers to target a specific existing dataset.
#[instrument(
    name = "ingestion.persist_data_with_acl",
    skip(processed, database, acl_db),
    fields(data_id = %processed.data_id)
)]
pub async fn persist_data_with_acl(
    processed: &ProcessedInput,
    database: &dyn IngestDb,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    acl_db: Option<&dyn AclDb>,
    target_dataset_id: Option<Uuid>,
) -> Result<Data, Box<dyn std::error::Error>> {
    // Resolve the dataset: prefer explicit UUID, fall back to name-based lookup.
    let is_new_dataset;
    let dataset = if let Some(ds_id) = target_dataset_id {
        match database.get_dataset(ds_id).await? {
            Some(ds) => {
                is_new_dataset = false;
                ds
            }
            None => {
                return Err(format!("Dataset with id {ds_id} not found").into());
            }
        }
    } else {
        let generated_id = generate_dataset_id(dataset_name, owner_id, tenant_id);
        match database
            .get_dataset_by_name(dataset_name, owner_id, tenant_id)
            .await?
        {
            Some(ds) => {
                is_new_dataset = false;
                ds
            }
            None => {
                is_new_dataset = true;
                let new_dataset =
                    Dataset::new(dataset_name.to_string(), owner_id, tenant_id, generated_id);
                database.create_dataset(new_dataset).await?
            }
        }
    };
    info!(dataset_id = %dataset.id, "dataset resolved");

    // Grant all permissions to the owner when a new dataset is created.
    if is_new_dataset && let Some(acl) = acl_db {
        cognee_database::ops::acl::grant_all_permissions_on_dataset_via_trait(
            acl, owner_id, dataset.id,
        )
        .await?;
        info!(
            dataset_id = %dataset.id,
            owner_id = %owner_id,
            "ACL permissions granted on new dataset"
        );
    }

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
        processed.stored_extension.clone(),
        processed.stored_mime_type.clone(),
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
    if let Some(ref meta) = processed.external_metadata {
        data_builder = data_builder.external_metadata(meta.clone());
    }
    if let Some(ref ns) = processed.node_set {
        data_builder = data_builder.node_set(ns.clone());
    }
    if let Some(w) = processed.importance_weight {
        data_builder = data_builder.importance_weight(w);
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

/// Resolve the MIME type for a file extension.
///
/// If the extension maps to `"text_loader"` in the loader registry, return
/// `"text/plain"` to match Python's behaviour (Python's `filetype.guess()`
/// returns `text/plain` for `.md`, `.json`, `.xml`, etc. because they have no
/// magic bytes). Otherwise fall back to `mime_guess`.
fn resolve_mime(extension: &str, path_for_guess: &str) -> String {
    if get_loader_name(extension) == "text_loader" {
        "text/plain".to_string()
    } else {
        mime_guess::from_path(path_for_guess)
            .first_or_octet_stream()
            .to_string()
    }
}

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
            let mime = resolve_mime(&extension, clean_path);
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
            let mime = resolve_mime(&ext, name);
            (name.clone(), ext, mime, None)
        }
        DataInput::DataItem { data, label, .. } => {
            let (file_name, ext, mime, _) = extract_file_metadata(data);
            (file_name, ext, mime, Some(label.clone()))
        }
    }
}

/// Convert a relative storage location into a `file://` absolute URI.
///
/// Mirrors Python's `Path(full_file_path).as_uri()` which always produces
/// an absolute `file:///…` URI.  If `base_path` is relative, it is
/// resolved against the current working directory first so that the URI
/// stored in the database is always absolute and self-contained.
fn storage_location_to_uri(base_path: &str, location: &str) -> String {
    if base_path.is_empty() {
        // MockStorage or other non-filesystem backend — return as-is
        location.to_string()
    } else {
        let joined = Path::new(base_path).join(location);
        // Canonicalize to absolute; fall back to manual cwd join on error
        // (e.g. the path doesn't exist on disk yet during tests).
        let abs = if joined.is_absolute() {
            joined
        } else {
            std::env::current_dir().unwrap_or_default().join(&joined)
        };
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
                .file_stem()
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
    make_persist_data_task_with_acl(database, dataset_name, owner_id, tenant_id, None)
}

/// Like [`make_persist_data_task`], but optionally grants ACL permissions
/// on newly created datasets.
pub fn make_persist_data_task_with_acl(
    database: Arc<dyn IngestDb>,
    dataset_name: String,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    acl_db: Option<Arc<dyn AclDb>>,
) -> TypedTask<ProcessedInput, Data> {
    make_persist_data_task_with_acl_and_params(
        database,
        dataset_name,
        owner_id,
        tenant_id,
        acl_db,
        AddParamsInjection::default(),
    )
}

/// Pre-serialised [`AddParams`] payload injected into [`ProcessedInput`] by
/// the persist closure. Constructed once per pipeline so the cost of
/// serialising `node_set` is paid up-front, not per item.
///
/// Locked Decision 7 (LIB-06) — `AddParams` is wired in via the task
/// closure rather than a `RunSpec` / `TaskContext` extension.
#[derive(Debug, Clone, Default)]
struct AddParamsInjection {
    node_set_json: Option<String>,
    importance_weight: Option<f64>,
    target_dataset_id: Option<Uuid>,
}

/// Build a persist task whose closure also patches the [`ProcessedInput`]
/// with the [`AddParams`] fields (`node_set`, `importance_weight`) and
/// honours an optional `dataset_id` override.
fn make_persist_data_task_with_acl_and_params(
    database: Arc<dyn IngestDb>,
    dataset_name: String,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    acl_db: Option<Arc<dyn AclDb>>,
    add_params: AddParamsInjection,
) -> TypedTask<ProcessedInput, Data> {
    TypedTask::async_fn(move |processed: &ProcessedInput, _ctx| {
        let mut processed = processed.clone();
        // Decision 7 (LIB-06): inject add-specific params inside the task.
        if let Some(ref ns) = add_params.node_set_json {
            processed.node_set = Some(ns.clone());
        }
        if let Some(w) = add_params.importance_weight {
            processed.importance_weight = Some(w);
        }
        let override_ds = add_params.target_dataset_id;
        let database = Arc::clone(&database);
        let dataset_name = dataset_name.clone();
        let acl_db = acl_db.clone();
        Box::pin(async move {
            persist_data_with_acl(
                &processed,
                &*database,
                &dataset_name,
                owner_id,
                tenant_id,
                acl_db.as_deref(),
                override_ds,
            )
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
    build_add_pipeline_with_acl(
        storage,
        database,
        hash_algorithm,
        dataset_name,
        owner_id,
        tenant_id,
        None,
    )
}

/// Like [`build_add_pipeline`], but optionally grants ACL permissions on
/// newly created datasets.
pub fn build_add_pipeline_with_acl(
    storage: Arc<dyn StorageTrait>,
    database: Arc<dyn IngestDb>,
    hash_algorithm: HashAlgorithm,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    acl_db: Option<Arc<dyn AclDb>>,
) -> Pipeline {
    build_add_pipeline_internal(
        storage,
        database,
        hash_algorithm,
        dataset_name,
        owner_id,
        tenant_id,
        acl_db,
        AddParamsInjection::default(),
    )
}

/// Internal builder used by both [`build_add_pipeline_with_acl`] and the
/// executor-routed [`AddPipeline::add_with_params`]. Threads
/// [`AddParamsInjection`] into the persist task and attaches a
/// `data_id_fn` so the executor's `run_info["data"]` carrier is populated
/// for the
/// [`cognee_core::pipeline_run_registry::DbPipelineWatcher`] wired by
/// [`AddPipeline::with_pipeline_run_repo`] (gap 08-07).
#[allow(clippy::too_many_arguments)]
fn build_add_pipeline_internal(
    storage: Arc<dyn StorageTrait>,
    database: Arc<dyn IngestDb>,
    hash_algorithm: HashAlgorithm,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    acl_db: Option<Arc<dyn AclDb>>,
    add_params: AddParamsInjection,
) -> Pipeline {
    // Locked Decision 4 (LIB-06): the per-input `data_id_fn` operates on
    // the pipeline input (`DataInput`), which has no UUID until
    // `persist_data` runs. Return `None` here; the executor's run_info
    // `data_ids` stays empty (the watcher maps it to Python's `"None"`).
    // Gap-08 task 07 revisits this once the watcher is real.
    let data_id_fn: DataIdFn = Arc::new(|_v: Arc<dyn Value>| None);
    PipelineBuilder::new_with_task(
        "ingestion.add",
        make_process_input_task(Arc::clone(&storage), hash_algorithm, owner_id, tenant_id),
    )
    .add_task(make_persist_data_task_with_acl_and_params(
        database,
        dataset_name.to_string(),
        owner_id,
        tenant_id,
        acl_db,
        add_params,
    ))
    .with_name("ingestion")
    .with_data_id(data_id_fn)
    .build()
}

// ---------------------------------------------------------------------------
// AddPipeline — convenience wrapper
// ---------------------------------------------------------------------------

/// Ingest pipeline driven by the cognee-core task framework.
///
/// Routes [`add`](Self::add) / [`add_with_params`](Self::add_with_params)
/// through [`cognee_core::pipeline::execute`] so the executor's lifecycle
/// hooks fire and `TaskContext`-aware tasks can publish run-scoped payload
/// (LIB-06 / gap 08-07).
///
/// Backend handles required by [`cognee_core::TaskContextBuilder`]
/// — `thread_pool`, `graph_db`, `vector_db`, `database` — must be attached
/// via the chainable builders before calling `add()`. `AddPipeline::new`
/// does **not** populate them; calling `add()` on an under-configured
/// pipeline returns `IngestionError::MissingBackend { ... }`.
///
/// For composable pipeline-based execution (with concurrency, retry, etc.),
/// use [`build_add_pipeline`] + [`cognee_core::execute`] directly.
///
/// [`cognee_core::execute`]: cognee_core::execute
pub struct AddPipeline {
    storage: Arc<dyn StorageTrait>,
    database: Arc<dyn IngestDb>,
    hash_algorithm: HashAlgorithm,
    acl_db: Option<Arc<dyn AclDb>>,
    // ─── Executor-context handles (LIB-06) ────────────────────────────────
    thread_pool: Option<Arc<dyn CpuPool>>,
    graph_db: Option<Arc<dyn GraphDBTrait>>,
    vector_db: Option<Arc<dyn VectorDB>>,
    db_connection: Option<Arc<DatabaseConnection>>,
    // ─── Pipeline-run trail (gap 08-07) ───────────────────────────────────
    pipeline_run_repo: Option<Arc<dyn PipelineRunRepository>>,
}

impl AddPipeline {
    /// Create with the default MD5 hashing (Python-compatible).
    ///
    /// **Note:** `add()` routes through [`cognee_core::pipeline::execute`]
    /// and requires the four executor-context handles attached via
    /// [`with_thread_pool`](Self::with_thread_pool),
    /// [`with_graph_db`](Self::with_graph_db),
    /// [`with_vector_db`](Self::with_vector_db),
    /// [`with_database`](Self::with_database). A missing handle surfaces as
    /// `IngestionError::MissingBackend` at `add()` time.
    pub fn new(storage: Arc<dyn StorageTrait>, database: Arc<dyn IngestDb>) -> Self {
        Self {
            storage,
            database,
            hash_algorithm: HashAlgorithm::default(),
            acl_db: None,
            thread_pool: None,
            graph_db: None,
            vector_db: None,
            db_connection: None,
            pipeline_run_repo: None,
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
            acl_db: None,
            thread_pool: None,
            graph_db: None,
            vector_db: None,
            db_connection: None,
            pipeline_run_repo: None,
        }
    }

    /// Enable ACL permission grants on newly created datasets.
    ///
    /// When set, the pipeline grants all four permissions (read, write, delete,
    /// share) to the owner on each newly created dataset, matching Python's
    /// `create_authorized_dataset()` behavior.
    pub fn with_acl_db(mut self, acl_db: Arc<dyn AclDb>) -> Self {
        self.acl_db = Some(acl_db);
        self
    }

    /// Attach the CPU pool used by [`cognee_core::TaskContext`].
    pub fn with_thread_pool(mut self, pool: Arc<dyn CpuPool>) -> Self {
        self.thread_pool = Some(pool);
        self
    }

    /// Attach the graph database backend used by [`cognee_core::TaskContext`].
    pub fn with_graph_db(mut self, graph: Arc<dyn GraphDBTrait>) -> Self {
        self.graph_db = Some(graph);
        self
    }

    /// Attach the vector database backend used by [`cognee_core::TaskContext`].
    pub fn with_vector_db(mut self, vectors: Arc<dyn VectorDB>) -> Self {
        self.vector_db = Some(vectors);
        self
    }

    /// Attach the relational [`DatabaseConnection`] used by
    /// [`cognee_core::TaskContext`]. This is the same SeaORM handle the
    /// SQL-backed `IngestDb` is built on.
    pub fn with_database(mut self, db: Arc<DatabaseConnection>) -> Self {
        self.db_connection = Some(db);
        self
    }

    /// Attach the `PipelineRunRepository` used to persist the four-state
    /// `pipeline_runs` trail (gap 08-07).
    ///
    /// Embedded callers pass `Arc::new(NoopPipelineRunRepository::new())`
    /// (or simply omit this call — the pipeline defaults to no-op). CLI
    /// and HTTP callers pass `Arc::new(SeaOrmPipelineRunRepository::new(db))`
    /// so the rows surface in `/api/v1/activity/pipeline-runs`.
    pub fn with_pipeline_run_repo(mut self, repo: Arc<dyn PipelineRunRepository>) -> Self {
        self.pipeline_run_repo = Some(repo);
        self
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
        self.add_with_params(
            inputs,
            dataset_name,
            owner_id,
            tenant_id,
            &AddParams::default(),
        )
        .await
    }

    /// Like [`add`](Self::add), but accepts additional optional parameters.
    #[instrument(
        name = "ingestion.add_with_params",
        skip(self, inputs, params),
        fields(dataset_name, owner_id = %owner_id, inputs_count = inputs.len())
    )]
    pub async fn add_with_params(
        &self,
        inputs: Vec<DataInput>,
        dataset_name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
        params: &AddParams,
    ) -> Result<Vec<Data>, Box<dyn std::error::Error>> {
        // ── Resolve executor-context handles ─────────────────────────────
        let thread_pool = self
            .thread_pool
            .clone()
            .ok_or(IngestionError::MissingBackend {
                which: "thread_pool",
            })?;
        let graph_db = self
            .graph_db
            .clone()
            .ok_or(IngestionError::MissingBackend { which: "graph_db" })?;
        let vector_db = self
            .vector_db
            .clone()
            .ok_or(IngestionError::MissingBackend { which: "vector_db" })?;
        let db_connection = self
            .db_connection
            .clone()
            .ok_or(IngestionError::MissingBackend { which: "database" })?;

        // ── Pre-serialise add-params (Decision 7) ────────────────────────
        let node_set_json = params
            .node_set
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("Failed to serialize node_set: {e}"))?;
        let add_params_inj = AddParamsInjection {
            node_set_json,
            importance_weight: params.importance_weight,
            target_dataset_id: params.dataset_id,
        };

        // ── Build the typed pipeline ─────────────────────────────────────
        let pipeline = build_add_pipeline_internal(
            Arc::clone(&self.storage),
            Arc::clone(&self.database),
            self.hash_algorithm,
            dataset_name,
            owner_id,
            tenant_id,
            self.acl_db.clone(),
            add_params_inj,
        );

        // ── Build the TaskContext ────────────────────────────────────────
        // The executor re-derives `PipelineRunInfo.pipeline_id` from
        // `(pipeline.name, user_id, dataset_id)` — see
        // `cognee_core::pipeline::execute` and `deterministic_pipeline_id`.
        // We carry `pipeline.id` here as the placeholder; the watcher
        // observes the derived value.
        let pipeline_ctx = PipelineContext {
            pipeline_id: pipeline.id,
            pipeline_name: pipeline.name.clone().unwrap_or_default(),
            user_id: Some(owner_id),
            tenant_id,
            dataset_id: params.dataset_id,
            current_data: None,
            run_id: None,
            user_email: None,
            provenance_visited: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        };

        let (_cancel_handle, ctx) = TaskContextBuilder::new()
            .thread_pool(thread_pool)
            .database(db_connection)
            .graph_db(graph_db)
            .vector_db(vector_db)
            .pipeline_context(pipeline_ctx)
            .build()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
        let ctx = Arc::new(ctx);

        // ── Erase typed inputs ───────────────────────────────────────────
        let typed_inputs: Vec<Arc<dyn Value>> = inputs
            .into_iter()
            .map(|i| Arc::new(i) as Arc<dyn Value>)
            .collect();

        // ── Run the executor (gap 08-07 Decision 11: DbPipelineWatcher
        //    persists the four-state `pipeline_runs` trail; defaults to a
        //    no-op repo when the caller hasn't attached one). ───────────────
        let pipeline_run_repo = self
            .pipeline_run_repo
            .clone()
            .unwrap_or_else(cognee_database::NoopPipelineRunRepository::arc);
        let watcher = DbPipelineWatcher::new(pipeline_run_repo);
        let outputs = cognee_core::pipeline::execute(&pipeline, typed_inputs, ctx, &watcher)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

        extract_data_outputs(outputs)
    }
}

// ---------------------------------------------------------------------------
// Output extraction (Decision 9)
// ---------------------------------------------------------------------------

/// Downcast the executor's [`Arc<dyn Value>`] outputs back to the concrete
/// [`Data`] type the convenience function promises.
///
/// Returns [`IngestionError::OutputTypeMismatch`] when the downcast fails —
/// a programmer error indicating the pipeline's last task does not emit
/// `Data`.
fn extract_data_outputs(
    outputs: Vec<Arc<dyn Value>>,
) -> Result<Vec<Data>, Box<dyn std::error::Error>> {
    let mut data_vec = Vec::with_capacity(outputs.len());
    for o in outputs {
        // Explicit deref through `Arc` to reach the inner `dyn Value`, then
        // call `as_any` via vtable dispatch. Without this, method resolution
        // finds `<Arc<dyn Value> as Value>::as_any()` (via the blanket impl)
        // which downcasts to `Arc<dyn Value>` and never to `Data`. Mirrors
        // the pattern in `cognee_core::task::Task::borrow_input`.
        let d = (*o).as_any().downcast_ref::<Data>().cloned().ok_or(
            IngestionError::OutputTypeMismatch {
                expected: "Data",
                actual: "unknown",
            },
        )?;
        data_vec.push(d);
    }
    Ok(data_vec)
}

// ---------------------------------------------------------------------------
// IngestionError
// ---------------------------------------------------------------------------

/// Error returned by [`AddPipeline::add`] when a required executor-context
/// handle was not attached, or when the pipeline's typed output cannot be
/// downcast to [`Data`].
#[derive(Debug, thiserror::Error)]
pub enum IngestionError {
    /// A required `AddPipeline` builder field was not attached before
    /// calling `add()`.
    #[error("AddPipeline missing required backend: {which}")]
    MissingBackend { which: &'static str },
    /// The executor returned an output the pipeline cannot downcast to the
    /// expected concrete type.
    #[error("AddPipeline output type mismatch: expected {expected}, actual {actual}")]
    OutputTypeMismatch {
        expected: &'static str,
        actual: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_database::{connect, initialize, ops};
    use cognee_graph::MockGraphDB;
    use cognee_storage::{LocalStorage, MockStorage};
    use cognee_vector::MockVectorDB;
    use mockito::{Server, ServerGuard};
    use std::io::Write;
    use tempfile::NamedTempFile;

    async fn make_pipeline() -> (AddPipeline, Arc<cognee_database::DatabaseConnection>) {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let db = Arc::new(db);
        let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
        let pipeline = AddPipeline::new(storage, db.clone() as Arc<dyn IngestDb>)
            .with_thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
            .with_graph_db(Arc::new(MockGraphDB::new()))
            .with_vector_db(Arc::new(MockVectorDB::new()))
            .with_database(Arc::clone(&db));
        (pipeline, db)
    }

    async fn server_with_robots() -> ServerGuard {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/robots.txt")
            .with_status(404)
            .create_async()
            .await;
        server
    }

    #[tokio::test]
    async fn test_process_input_url_html_stores_text_with_source_metadata() {
        let mut server = server_with_robots().await;
        let html = "<html><head><title>Example</title></head><body><h1>Visible text</h1><script>hidden()</script></body></html>";
        let url = format!("{}/page.html", server.url());
        let _mock = server
            .mock("GET", "/page.html")
            .with_header("content-type", "text/html; charset=utf-8")
            .with_body(html)
            .create_async()
            .await;
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        let processed = process_input(
            &DataInput::Url(url.clone()),
            &storage,
            HashAlgorithm::Md5,
            Uuid::new_v4(),
            None,
        )
        .await
        .unwrap();

        let stored = storage.retrieve(&processed.storage_location).await.unwrap();
        let stored_text = String::from_utf8(stored).unwrap();
        assert!(stored_text.contains("Visible text"));
        assert!(!stored_text.contains("<html>"));
        assert_eq!(processed.stored_extension, "txt");
        assert_eq!(processed.stored_mime_type, "text/plain");
        assert_eq!(processed.original_extension, "html");
        assert_eq!(processed.original_mime_type, "text/html");
        assert_eq!(processed.loader_engine, "beautiful_soup_loader");
        assert_eq!(processed.original_location, url);
    }

    #[tokio::test]
    async fn test_persist_data_url_html_uses_stored_type_and_preserves_source_type() {
        let mut server = server_with_robots().await;
        let url = format!("{}/page", server.url());
        let _mock = server
            .mock("GET", "/page")
            .with_header("content-type", "application/xhtml+xml")
            .with_body("<html><body>XHTML body</body></html>")
            .create_async()
            .await;
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());
        let owner_id = Uuid::new_v4();

        let processed = process_input(
            &DataInput::Url(url),
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();
        let data = persist_data(&processed, &db, "url-html", owner_id, None)
            .await
            .unwrap();

        assert_eq!(data.extension, "txt");
        assert_eq!(data.mime_type, "text/plain");
        assert_eq!(data.original_extension.as_deref(), Some("html"));
        assert_eq!(
            data.original_mime_type.as_deref(),
            Some("application/xhtml+xml")
        );
        assert_eq!(data.loader_engine.as_deref(), Some("beautiful_soup_loader"));
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

    /// Regression guard for telemetry gap 05-02 (DataPoint provenance audit).
    ///
    /// Provenance task 05-03's `extract_content_hash_from_value` walks every
    /// input `Data` looking for the first non-empty `content_hash`. If any
    /// ingestion path leaves the column empty, downstream stamping silently
    /// regresses. This test asserts that every `DataInput` variant reachable
    /// from `process_data_input` produces a non-empty hex digest, and that
    /// the value round-trips through the SeaORM `Data` <-> `data::Model`
    /// conversion (the canonical DB read path).
    #[tokio::test]
    async fn test_content_hash_non_empty_across_variants_and_db_roundtrip() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        // 1. Text input.
        let text_data = pipeline
            .add(
                vec![DataInput::Text("Provenance audit text".to_string())],
                "audit_text",
                owner_id,
                None,
            )
            .await
            .unwrap();
        assert!(
            !text_data[0].content_hash.is_empty(),
            "Text input must populate content_hash"
        );

        // 2. File input.
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "Provenance audit file").unwrap();
        let file_path = temp_file.path().to_str().unwrap().to_string();
        let file_data = pipeline
            .add(
                vec![DataInput::FilePath(file_path)],
                "audit_file",
                owner_id,
                None,
            )
            .await
            .unwrap();
        assert!(
            !file_data[0].content_hash.is_empty(),
            "FilePath input must populate content_hash"
        );

        // 3. Binary input.
        let binary_data = pipeline
            .add(
                vec![DataInput::Binary {
                    name: "audit.bin".to_string(),
                    data: b"provenance audit binary".to_vec(),
                }],
                "audit_binary",
                owner_id,
                None,
            )
            .await
            .unwrap();
        assert!(
            !binary_data[0].content_hash.is_empty(),
            "Binary input must populate content_hash"
        );

        // 4. DataItem wrapper — must propagate the inner Text variant's hash.
        let wrapped = pipeline
            .add(
                vec![DataInput::DataItem {
                    data: Box::new(DataInput::Text("Wrapped audit text".to_string())),
                    label: "wrapped".to_string(),
                    external_metadata: None,
                }],
                "audit_wrapped",
                owner_id,
                None,
            )
            .await
            .unwrap();
        assert!(
            !wrapped[0].content_hash.is_empty(),
            "DataItem(Text) must populate content_hash"
        );

        // 5. Round-trip through the DB read path: the value `add()` returned
        //    must equal the value `get_data()` reads back via
        //    `From<data::Model> for Data`.
        let reread = ops::data::get_data(&db, text_data[0].id)
            .await
            .unwrap()
            .expect("data row exists immediately after add()");
        assert_eq!(
            reread.content_hash, text_data[0].content_hash,
            "content_hash must round-trip through SeaORM <-> Data conversion"
        );
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
                    external_metadata: None,
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

    // ── extract_name — Python parity ────────────────────────────────────

    #[test]
    fn extract_name_file_path_uses_stem_not_full_name() {
        // Python uses Path(file_path).stem which strips the extension.
        let input = DataInput::FilePath("documents/report.txt".into());
        let name = super::extract_name(&input, "abc123");
        assert_eq!(
            name, "report",
            "file path name should be stem (no extension)"
        );
    }

    #[test]
    fn extract_name_file_path_with_file_uri() {
        let input = DataInput::FilePath("file:///tmp/data/notes.pdf".into());
        let name = super::extract_name(&input, "abc123");
        assert_eq!(name, "notes");
    }

    #[test]
    fn extract_name_text_input_uses_hash() {
        let input = DataInput::Text("hello world".into());
        let name = super::extract_name(&input, "5eb63bbbe01eeed093cb22bb8f5acdc3");
        assert_eq!(name, "text_5eb63bbbe01eeed093cb22bb8f5acdc3");
    }

    // ── mime type override for text-loader extensions ──────────────────

    #[test]
    fn binary_md_file_gets_text_plain_mime() {
        let input = DataInput::Binary {
            name: "notes.md".to_string(),
            data: b"# Heading\nSome markdown".to_vec(),
        };
        let (_name, _ext, mime, _label) = super::extract_file_metadata(&input);
        assert_eq!(
            mime, "text/plain",
            ".md binary should produce text/plain, not text/markdown"
        );
    }

    #[test]
    fn file_path_md_gets_text_plain_mime() {
        let input = DataInput::FilePath("/tmp/notes.md".to_string());
        let (_name, _ext, mime, _label) = super::extract_file_metadata(&input);
        assert_eq!(
            mime, "text/plain",
            ".md file path should produce text/plain, not text/markdown"
        );
    }

    #[test]
    fn file_path_json_gets_text_plain_mime() {
        let input = DataInput::FilePath("/tmp/data.json".to_string());
        let (_name, _ext, mime, _label) = super::extract_file_metadata(&input);
        assert_eq!(
            mime, "text/plain",
            ".json file path should produce text/plain, not application/json"
        );
    }

    #[test]
    fn file_path_pdf_keeps_original_mime() {
        let input = DataInput::FilePath("/tmp/doc.pdf".to_string());
        let (_name, _ext, mime, _label) = super::extract_file_metadata(&input);
        assert_ne!(
            mime, "text/plain",
            ".pdf should NOT be overridden to text/plain"
        );
    }

    // ── external_metadata plumbing ────────────────────────────────────────

    #[tokio::test]
    async fn test_data_item_external_metadata_stored() {
        let (pipeline, _db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let meta_json = r#"{"source":"dlt","table_name":"orders"}"#.to_string();
        let result = pipeline
            .add(
                vec![DataInput::DataItem {
                    data: Box::new(DataInput::Text("dlt content".to_string())),
                    label: "dlt-label".to_string(),
                    external_metadata: Some(meta_json.clone()),
                }],
                "ds",
                owner_id,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].external_metadata.as_deref(),
            Some(meta_json.as_str()),
            "DataItem external_metadata must be stored in the Data record"
        );
    }

    #[tokio::test]
    async fn test_data_item_without_metadata() {
        let (pipeline, _db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let result = pipeline
            .add(
                vec![DataInput::DataItem {
                    data: Box::new(DataInput::Text("no metadata".to_string())),
                    label: "plain-label".to_string(),
                    external_metadata: None,
                }],
                "ds",
                owner_id,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].external_metadata, None,
            "DataItem with no external_metadata should produce None on Data"
        );
    }
}
