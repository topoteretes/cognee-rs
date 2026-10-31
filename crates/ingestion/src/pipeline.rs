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
use cognee_core::task::{TaskError, Value};
use cognee_core::{Pipeline, PipelineBuilder, PipelineContext, TaskContextBuilder, TypedTask};
use cognee_database::{AclDb, IngestDb, PipelineRunRepository, RelationalDb};
use cognee_graph::GraphDBTrait;
#[cfg(feature = "audio-loader")]
use cognee_llm::Transcriber;
use cognee_models::{Data, DataInput, DataPoint, Dataset, Document};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;

use crate::content_hasher::HashAlgorithm;
use crate::dataset_locks::DatasetLocks;
use crate::id_generation::{generate_data_id, generate_dataset_id};
use crate::loader_registry::get_loader_name;
use crate::loaders::{LoaderOutput, LoaderRegistry};
use crate::url_resolver::UrlMetadata;
#[cfg(feature = "html-loader")]
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
    /// List of node-set names for graph organisation and access control
    /// grouping. Stored as a JSON array in `Data.node_set` and under
    /// `external_metadata["node_set"]`, where cognify reads it back. Every
    /// name must be non-empty (Python's `validate_node_set_names`); an empty
    /// list means no node set, exactly like `None`.
    pub node_set: Option<Vec<String>>,

    /// Target an existing dataset by UUID instead of name.
    pub dataset_id: Option<Uuid>,

    /// Maps MIME types or file extensions to preferred loader names.
    pub preferred_loaders: Option<HashMap<String, String>>,

    /// Importance weight (0.0 to 1.0) for relevance scoring.
    pub importance_weight: Option<f64>,

    /// When `true`, skip content that is already present in the dataset
    /// (deduplication by content hash). Matches Python's `incremental_loading`
    /// parameter. Defaults to `false`; callers that want Python-parity should
    /// set this to `true`.
    pub incremental_loading: bool,
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
    /// MD5 of the EXTRACTED-text file (Python parity, `ingest_data.py:195`).
    /// Equals `content_hash` only when extracted text == raw bytes (plain text).
    pub raw_content_hash: String,
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
    pub raw_source_uri: Option<String>,
    pub owner_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub external_metadata: Option<String>,
    /// Node-set names for graph organisation — the add call's
    /// [`AddParams::node_set`]. [`persist_data`] writes them to the
    /// `Data.node_set` column and under `external_metadata["node_set"]`; an
    /// empty list is the same as `None`.
    pub node_set: Option<Vec<String>>,
    /// Importance weight for ranking (0.0 to 1.0).
    pub importance_weight: Option<f64>,
    /// The data row already exists but its stored text was gone, so it was
    /// extracted and stored again: [`persist_data`] points the row at it.
    pub replaces_stored_text: bool,
}

/// Doc types whose loader calls a model at add time. Running it again costs a
/// paid request and can change the text, so a re-add reuses what is stored.
const REUSES_STORED_TEXT: &[&str] = &["audio"];

// ---------------------------------------------------------------------------
// Task 1 implementation: DataInput → ProcessedInput
// ---------------------------------------------------------------------------

/// Process a single [`DataInput`]: resolve URLs, stream to storage, compute
/// content hash, and extract all metadata needed to create a [`Data`] record.
///
/// This is the first step of the ingest pipeline (Task 1).
pub async fn process_input(
    input: &DataInput,
    storage: &dyn StorageTrait,
    hash_algorithm: HashAlgorithm,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<ProcessedInput, Box<dyn std::error::Error>> {
    process_input_with_registry(
        input,
        storage,
        hash_algorithm,
        owner_id,
        tenant_id,
        &LoaderRegistry::default_registry(),
        None,
    )
    .await
}

/// [`process_input`] with the loaders of `registry` instead of
/// [`LoaderRegistry::default_registry`].
///
/// With `database`, audio whose data row already holds its transcript is not
/// transcribed again (see [`stored_text`]).
#[instrument(
    name = "ingestion.process_input",
    skip(input, storage, registry, database)
)]
async fn process_input_with_registry(
    input: &DataInput,
    storage: &dyn StorageTrait,
    hash_algorithm: HashAlgorithm,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    registry: &LoaderRegistry,
    database: Option<&dyn IngestDb>,
) -> Result<ProcessedInput, Box<dyn std::error::Error>> {
    use tokio::sync::Mutex;

    // Before anything is fetched or stored, as Python's `ingest_data` checks
    // a DataItem's `external_metadata["node_set"]` up front.
    validate_item_node_set(input)?;

    let (effective_input, resolved_url_metadata, resolved_label, data_item_metadata) =
        resolve_input_for_processing(input).await?;

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
    ) = if let Some(metadata) = resolved_url_metadata.as_ref() {
        let fname = format!("text_placeholder.{}", metadata.stored_extension);
        (
            fname,
            metadata.stored_extension.clone(),
            metadata.stored_mime_type.clone(),
            metadata.source_extension.clone(),
            metadata.source_mime_type.clone(),
            resolved_label,
            metadata.loader_engine.clone(),
        )
    } else {
        let (fname, ext, mime, lbl) = extract_file_metadata(input);
        let loader = get_loader_name(&ext).to_string();
        (fname, ext.clone(), mime.clone(), ext, mime, lbl, loader)
    };

    // S3 ingestion is not yet implemented (decision D3): the loader-dispatch
    // path below must NOT silently store raw S3 bytes. Surface a clear error
    // instead. The `original_*` metadata above still carries the placeholder.
    if matches!(unwrap_data_item(input), DataInput::S3Path(_)) {
        return Err(Box::new(IngestionError::S3IngestionUnavailable));
    }

    let raw_source_uri = if let Some(metadata) = resolved_url_metadata.as_ref()
        && is_html_url_metadata(metadata)
    {
        let raw_file_name = format!("source_placeholder.{}", metadata.source_extension);
        let raw_location = storage.store(&metadata.raw_bytes, &raw_file_name).await?;
        Some(storage_location_to_uri(storage.base_path(), &raw_location))
    } else {
        None
    };

    // Rebindable stored-file metadata. For non-URL inputs the loader runs at
    // ADD time (Python parity, ingest_data.py:103) and the stored artifact
    // becomes extracted text, so these are overridden to txt/text/plain below.
    let mut stored_extension = stored_extension;
    let mut stored_mime_type = stored_mime_type;
    let mut loader_engine = loader_engine;
    // Set when a re-add reuses the stored text, whose row already has its URI.
    let mut reused_data_uri = None;
    let mut replaces_stored_text = false;

    // Buffer the (effective) input bytes. For URL inputs `effective_input` is
    // the already-extracted text; for everything else it is the raw original
    // bytes, which we hash (UNCHANGED — sacred) and then feed to the loader.
    let size_counter: Arc<Mutex<i64>> = Arc::new(Mutex::new(0i64));
    let raw_bytes: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let size_clone = size_counter.clone();
    let raw_bytes_clone = raw_bytes.clone();

    effective_input
        .process_by_chunks(move |chunk| {
            let size = size_clone.clone();
            let bytes = raw_bytes_clone.clone();
            let chunk_owned = chunk.to_vec();
            async move {
                *size.lock().await += chunk_owned.len() as i64;
                bytes.lock().await.extend_from_slice(&chunk_owned);
                Ok::<(), Box<dyn std::error::Error>>(())
            }
        })
        .await?;

    // Finalise hash — content-only, no owner_id (Python compatible). For
    // non-URL inputs this is the MD5 of the RAW original bytes; `data_id` is
    // derived from it. Both are SACRED and must not change.
    let collected = Arc::try_unwrap(raw_bytes)
        .map_err(|_| "Failed to unwrap bytes")?
        .into_inner();
    let content_hash =
        crate::content_hasher::ContentHasher::hash_content(&collected, hash_algorithm);
    let data_size = Arc::try_unwrap(size_counter)
        .map_err(|_| "Failed to unwrap size counter")?
        .into_inner();

    let data_id = generate_data_id(&content_hash, owner_id, tenant_id);

    // Store the artifact + compute `raw_content_hash` (MD5 of the stored
    // extracted-text file — Python parity, ingest_data.py:195).
    let (storage_location, raw_content_hash) = if resolved_url_metadata.is_some() {
        // URL path: the loader already ran at fetch time, so `collected` IS the
        // extracted text. Store it verbatim under the metadata-derived name and
        // extension (preserving the existing URL behaviour, incl. non-HTML
        // URLs that keep their source extension). `raw_content_hash` is the MD5
        // of that stored file, which equals `content_hash` here.
        let location = storage.store(&collected, &file_name).await?;
        (location, content_hash.clone())
    } else {
        // Non-URL path: run the document loader at ADD time and store the
        // EXTRACTED text as `text_<content_hash>.txt` (Python text_loader.py:76
        // names the file with the ORIGINAL file's content hash).
        let doc_type = cognee_models::doc_type_for_extension(&original_extension)
            .unwrap_or("text")
            .to_string();

        // Binary/non-text blob fallback (pre-task-17 raw-storage parity).
        //
        // Rust classifies by extension only, so a file with no (or an unknown)
        // extension falls back to `doc_type = "text"` and the text loader. The
        // text loader rejects non-UTF-8 bytes, which would make `add` fail for
        // generic binary blobs that have no specific loader — a regression
        // introduced by running loaders at ADD time (task 17).
        //
        // Match the pre-task-17 behaviour: when the would-be loader is the
        // *text* loader but the bytes are not valid UTF-8 text, store the RAW
        // bytes verbatim instead of running text extraction. This only affects
        // the generic-text fallback path: files routed to a real loader by
        // their extension (text/csv/html/pdf, which are actually-text content;
        // audio when a transcriber is attached; otherwise image/audio, which
        // fail with `UnsupportedDocumentType`) are unchanged.
        let is_text_doc_type = doc_type == "text";
        if is_text_doc_type && std::str::from_utf8(&collected).is_err() {
            let stored_name = format!("text_{content_hash}.bin");
            let location = storage.store(&collected, &stored_name).await?;
            // Raw bytes are stored verbatim, so `raw_content_hash` equals the
            // (content-only) `content_hash` of those same bytes.
            (location, content_hash.clone())
        } else {
            // Checked before a loader is needed: audio transcribed before is
            // reused even when no transcriber is attached now.
            let stored = if REUSES_STORED_TEXT.contains(&doc_type.as_str()) {
                stored_text(database, storage, data_id, &loader_engine).await?
            } else {
                StoredText::Absent
            };
            if let StoredText::Reusable {
                uri,
                raw_content_hash,
                extension,
                mime_type,
            } = stored
            {
                stored_extension = extension;
                stored_mime_type = mime_type;
                reused_data_uri = Some(uri.clone());
                (uri, raw_content_hash)
            } else {
                replaces_stored_text = matches!(stored, StoredText::Lost);
                let loader = registry.get(&doc_type).ok_or_else(|| {
                    Box::new(IngestionError::UnsupportedDocumentType {
                        document_type: doc_type.clone(),
                    }) as Box<dyn std::error::Error>
                })?;

                // Minimal Document descriptor for the loader (only metadata fields
                // the loaders read; the bytes are passed separately).
                let descriptor = build_loader_descriptor(
                    data_id,
                    &extract_name(input, &content_hash),
                    &original_extension,
                    &original_mime_type,
                );

                let extracted_text = match loader.extract(&collected, &descriptor).await? {
                    LoaderOutput::Text(t) => t,
                    LoaderOutput::Rows(rows) => rows.join("\n\n"),
                    LoaderOutput::SingleChunk { text, .. } => text,
                };
                let extracted_bytes = extracted_text.into_bytes();

                let stored_name = format!("text_{content_hash}.txt");
                let location = storage.store(&extracted_bytes, &stored_name).await?;

                // Stored-file metadata now describes the extracted text.
                stored_extension = "txt".to_string();
                stored_mime_type = "text/plain".to_string();
                loader_engine = loader.engine_name().to_string();

                let raw_content_hash = crate::content_hasher::ContentHasher::hash_content(
                    &extracted_bytes,
                    hash_algorithm,
                );
                (location, raw_content_hash)
            }
        }
    };

    // Compute derived fields that previously lived in add()
    let raw_data_uri = reused_data_uri
        .unwrap_or_else(|| storage_location_to_uri(storage.base_path(), &storage_location));
    let name = extract_name(input, &content_hash);
    let original_location = if let Some(uri) = raw_source_uri.clone() {
        uri
    } else {
        match input {
            DataInput::Text(_) => raw_data_uri.clone(),
            _ => extract_original_location(input),
        }
    };
    let external_metadata =
        merge_external_metadata(data_item_metadata, resolved_url_metadata.as_ref())?;

    Ok(ProcessedInput {
        content_hash,
        raw_content_hash,
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
        raw_source_uri,
        owner_id,
        tenant_id,
        external_metadata,
        node_set: None,
        importance_weight: None,
        replaces_stored_text,
    })
}

/// What a re-add finds of the data row's stored text.
enum StoredText {
    /// No data row yet, or no database to look in.
    Absent,
    /// The row's text, written by the same loader and still in storage.
    Reusable {
        uri: String,
        raw_content_hash: String,
        extension: String,
        mime_type: String,
    },
    /// The row exists, but its text is gone from storage or was not written by
    /// this loader: it is extracted again and the row pointed at the new file.
    Lost,
}

/// The stored text of the data row `data_id`, as `loader_engine` wrote it.
///
/// Re-adding content keeps its row as it is (see
/// [`persist_data_with_acl_and_locks`]), so transcribing again would only cost
/// another paid request, whose text can differ, and leave a second stored file
/// no row points at. Re-adds in one `add` and in sequence see the row; two
/// concurrent adds of the same new file can both transcribe it, since the
/// per-`data_id` lock is taken in persist.
async fn stored_text(
    database: Option<&dyn IngestDb>,
    storage: &dyn StorageTrait,
    data_id: Uuid,
    loader_engine: &str,
) -> Result<StoredText, Box<dyn std::error::Error>> {
    let Some(database) = database else {
        return Ok(StoredText::Absent);
    };
    let Some(row) = database.get_data(data_id).await? else {
        return Ok(StoredText::Absent);
    };
    let (Some(engine), Some(raw_content_hash)) = (row.loader_engine, row.raw_content_hash) else {
        return Ok(StoredText::Lost);
    };
    if engine != loader_engine || !storage.exists(&row.raw_data_location).await? {
        return Ok(StoredText::Lost);
    }
    Ok(StoredText::Reusable {
        uri: row.raw_data_location,
        raw_content_hash,
        extension: row.extension,
        mime_type: row.mime_type,
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
    persist_data_with_acl_and_locks(
        processed,
        database,
        dataset_name,
        owner_id,
        tenant_id,
        None,
        None,
        None,
    )
    .await
}

/// Like [`persist_data`], but optionally grants all four ACL permissions
/// (read, write, delete, share) to the owner when a new dataset is created.
///
/// Unsynchronised: equivalent to
/// [`persist_data_with_acl_and_locks`] with no locks. Kept at its original
/// seven-argument shape because it is public API re-exported from both
/// `cognee_ingestion` and `cognee`, and no existing caller should have to
/// change to say "I am the only writer" — which is what `None` means and what
/// every single-caller embedder (the CLI, the library facade) wants. Serving
/// concurrent requests? Use [`persist_data_with_acl_and_locks`].
pub async fn persist_data_with_acl(
    processed: &ProcessedInput,
    database: &dyn IngestDb,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    acl_db: Option<&dyn AclDb>,
    target_dataset_id: Option<Uuid>,
) -> Result<Data, Box<dyn std::error::Error>> {
    persist_data_with_acl_and_locks(
        processed,
        database,
        dataset_name,
        owner_id,
        tenant_id,
        acl_db,
        target_dataset_id,
        None,
    )
    .await
}

/// Like [`persist_data_with_acl`], but serializes the dataset
/// create-and-grant against concurrent writers of the same dataset identity.
///
/// When `acl_db` is `Some`, the owner is ensured as a principal and receives
/// all permissions on newly created datasets, matching Python's
/// `create_authorized_dataset()` behavior.
///
/// When `target_dataset_id` is `Some`, the pipeline looks up the dataset by UUID
/// instead of name, allowing callers to target a specific existing dataset.
///
/// When `dataset_locks` is `Some`, the by-name "look it up, create it if
/// missing, grant the owner" sequence runs under the lock for that dataset
/// identity, serializing it against every other writer of the same identity in
/// this process — most importantly `POST /v1/datasets`, whose compensating
/// rollback would otherwise be able to delete the dataset out from under data
/// this call just attached to it. See [`crate::dataset_locks`]. `None` keeps
/// the previous unsynchronised behaviour, which is what every single-threaded
/// caller (the CLI, the library facade) wants.
#[instrument(
    name = "ingestion.persist_data_with_acl",
    skip(processed, database, acl_db, dataset_locks),
    fields(data_id = %processed.data_id)
)]
#[allow(
    clippy::too_many_arguments,
    reason = "every argument is one independent dimension of \"persist this item\" \
              — the store, the identity, and three orthogonal opt-ins; bundling \
              them into a struct would only move the same list one level out"
)]
pub async fn persist_data_with_acl_and_locks(
    processed: &ProcessedInput,
    database: &dyn IngestDb,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    acl_db: Option<&dyn AclDb>,
    target_dataset_id: Option<Uuid>,
    dataset_locks: Option<&DatasetLocks>,
) -> Result<Data, Box<dyn std::error::Error>> {
    // The node set is written here, where the row is built, rather than by
    // the `AddPipeline` task closure — so every entry point that hands a
    // `ProcessedInput` with a node set to a `persist_data*` function gets the
    // same two writes, and so does the re-add path below. Python stores
    // `None`, not `[]`, for an empty set (`json.dumps(...) if effective_node_set
    // else None`), so an empty list is treated as no node set.
    //
    // Checked before the dataset is resolved: a rejected name must not leave
    // behind an empty dataset (with its owner grants) that nothing attached
    // to. `AddPipeline::add_with_params` already refused the call up front;
    // this is for direct callers of the `persist_data*` entry points.
    let node_set = processed
        .node_set
        .as_deref()
        .filter(|names| !names.is_empty());
    if let Some(names) = node_set {
        validate_node_set_names(names)?;
    }
    let node_set_column = node_set
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| format!("Failed to serialize node_set: {e}"))?;

    // Take the identity lock before looking anything up, on *both* resolution
    // paths.
    //
    // The by-id path needs it just as much as the by-name one, which is not
    // obvious: it creates nothing and grants nothing, so there is no
    // insert-to-grant window of its own. But the window this guards is not
    // about what *this* call creates — it is about attaching data to a row
    // somebody else is about to roll back. A caller can reach a half-created
    // row by id: `uuid5(name, owner, tenant)` is derivable by anyone who knows
    // the name, so the id needs no listing to be observable mid-window. (Until
    // SDK-637 the listing handed it over too, because `GET /v1/datasets` read
    // through to ownership when the ACL returned nothing; it now does that only
    // with no `AclDb` wired, which is still every OSS deployment.) Resolving by
    // id without the lock would walk straight into the case the by-name lock
    // exists to prevent.
    //
    // Acquired here rather than inside the branches so the guard outlives both
    // the resolve and the grant below — releasing between them would reopen
    // exactly the window this closes.
    let dataset_guard = match dataset_locks {
        Some(locks) => Some(match target_dataset_id {
            Some(ds_id) => locks.lock(ds_id).await,
            None => {
                crate::dataset_locks::lock_dataset_identity(
                    locks,
                    dataset_name,
                    owner_id,
                    tenant_id,
                )
                .await
            }
        }),
        None => None,
    };

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
            .get_dataset_by_name_in_tenant(dataset_name, owner_id, tenant_id)
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

    // The dataset now exists with its grants committed, so the window is shut
    // and nothing below can be invalidated by a concurrent create: a create
    // that arrives from here on finds the row and short-circuits, and its
    // rollback only ever deletes a row it wrote itself. Release rather than
    // holding to the end of the function — the remaining work is this item's
    // own data write, and keeping the lock would serialize every concurrent
    // `add` into one dataset behind a single file at a time.
    drop(dataset_guard);

    let data_id = processed.data_id;

    // Serialize the read-modify-write of this data row against every other
    // `persist_data*` call for the same `data_id` in this process. The re-add
    // refresh below derives the `external_metadata` it writes from the row it
    // read here; without the lock two concurrent re-adds of the same content
    // with different node sets could interleave read-read-write-write, so the
    // later write would merge into a stale snapshot — dropping whatever the
    // other wrote to the metadata — and the two calls would each return a row
    // the store no longer holds. Under the lock every refresh reads the row
    // the previous one wrote, and the `node_set` column and the metadata key
    // it writes are always derived from that one read.
    //
    // In-process only, like `DatasetLocks` (and keyed the same way): two
    // processes re-adding the same content to a shared database can still
    // race, and the last writer wins on `external_metadata` — a whole-value
    // write, so the column and the key still agree with each other, but a
    // metadata change the other process made between this read and this write
    // is lost. Closing that needs a row lock (`SELECT ... FOR UPDATE`) or a
    // compare-and-swap on `updated_at`, which neither SQLite nor the
    // `IngestDb` trait offers today; it is not worth a cross-process lock for
    // a refresh whose inputs are the caller's own re-add.
    let _data_guard = data_id_locks().lock(data_id).await;

    if let Some(mut existing_data) = database.get_data(data_id).await? {
        database.attach_data_to_dataset(dataset.id, data_id).await?;
        // Python's `ingest_data` update branch rewrites the existing row's
        // `node_set` and `external_metadata` from the re-add's values. Only
        // the node set is refreshed here (the rest of the row stays as
        // stored), and only when the call supplies one: a re-add without a
        // node set leaves the stored tags alone. The write is skipped when
        // the row already carries this set, which is the common re-add
        // (memify's session persist re-adds under one fixed set). "Carries"
        // is decided on the parsed JSON, not the stored text: a row Python
        // wrote spells the same set `["a", "b"]` (json.dumps spacing) and may
        // order metadata keys differently, and must not be rewritten on every
        // re-add.
        //
        // The write is a targeted update of the two columns, not a full-row
        // write of the snapshot read above: a `token_count`, `last_accessed`
        // or `pipeline_status` a concurrent cognify or search wrote in between
        // has to survive.
        //
        // The cognify completion marker is deliberately left in place, so an
        // incremental cognify will not re-process the item and the graph keeps
        // the tags it was built with — the same as Python, where a plain
        // `add()` skips already-added content (`run_tasks_data_item_incremental`)
        // and `ingest_data` resets `pipeline_status` only when the content
        // changed. Clearing the marker here would not be a clean re-tag
        // either: the next cognify would run a full LLM re-extraction of the
        // item on top of its existing memory, and whatever the old extraction
        // produced that the new one does not reproduce (entities, edges,
        // summaries) would stay in the graph and vector stores. In both SDKs
        // `update()` is the way to re-tag cognified content: it removes the
        // item's memory and re-cognifies it (Python resets the marker on the
        // pinned row; Rust adds the replacement first, then deletes the old
        // item — or, when the replacement is this same content-addressed row,
        // drops only its memory and completion marker).
        if processed.replaces_stored_text {
            // The row's stored text was gone and has been extracted again. Point
            // the row at the new file so it stops referring to a missing one. The
            // cognify marker stays, as above: the graph keeps what it was built
            // from, and `update()` re-cognifies.
            let updated_at = database
                .update_data_stored_text(
                    data_id,
                    &processed.raw_data_uri,
                    &processed.raw_content_hash,
                    &processed.stored_extension,
                    &processed.stored_mime_type,
                    &processed.loader_engine,
                )
                .await?;
            info!(data_id = %data_id, "stored text replaced on re-add");
            existing_data.raw_data_location = processed.raw_data_uri.clone();
            existing_data.raw_content_hash = Some(processed.raw_content_hash.clone());
            existing_data.extension = processed.stored_extension.clone();
            existing_data.mime_type = processed.stored_mime_type.clone();
            existing_data.loader_engine = Some(processed.loader_engine.clone());
            existing_data.updated_at = Some(updated_at);
        }
        if let Some(names) = node_set {
            let external_metadata = with_node_set_in_external_metadata(
                existing_data.external_metadata.as_deref(),
                names,
            )
            .map_err(|e| format!("Failed to record node_set in external_metadata: {e}"))?;
            if !same_json(
                existing_data.node_set.as_deref(),
                node_set_column.as_deref(),
            ) || !same_json(
                existing_data.external_metadata.as_deref(),
                external_metadata.as_deref(),
            ) {
                // A row deleted since the read above fails the write with
                // `DatabaseError::NotFound`, which propagates: the call does
                // not report a row that no longer exists.
                let updated_at = database
                    .update_data_node_set(
                        data_id,
                        node_set_column.as_deref(),
                        external_metadata.as_deref(),
                    )
                    .await?;
                info!(data_id = %data_id, "node_set refreshed on re-add");
                // Patch the snapshot with exactly what was written rather than
                // reading the row back: the three columns are the only ones the
                // refresh touches, and `updated_at` is the value stored.
                existing_data.node_set = node_set_column;
                existing_data.external_metadata = external_metadata;
                existing_data.updated_at = Some(updated_at);
            }
        }
        info!(data_id = %data_id, is_duplicate = true, "input processed");
        return Ok(existing_data);
    }

    let external_metadata = match node_set {
        Some(names) => {
            with_node_set_in_external_metadata(processed.external_metadata.as_deref(), names)
                .map_err(|e| format!("Failed to record node_set in external_metadata: {e}"))?
        }
        None => processed.external_metadata.clone(),
    };

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
    .raw_content_hash(processed.raw_content_hash.clone())
    .data_size(processed.data_size);
    if let Some(tid) = processed.tenant_id {
        data_builder = data_builder.tenant_id(tid);
    }
    if let Some(ref lbl) = processed.label {
        data_builder = data_builder.label(lbl.clone());
    }
    if let Some(meta) = external_metadata {
        data_builder = data_builder.external_metadata(meta);
    }
    if let Some(column) = node_set_column {
        data_builder = data_builder.node_set(column);
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

/// The process-wide per-`data_id` locks [`persist_data_with_acl_and_locks`]
/// holds across its read-modify-write of one data row.
///
/// Process-wide rather than an `AddPipeline` option: the race it closes —
/// two concurrent re-adds of the same content — needs no server to happen
/// (any embedder can run two `add`s at once), and every `persist_data*` entry
/// point must take the same lock for it to mean anything. Contention is only
/// ever between calls for the same `data_id`. A registry of its own, separate
/// from any [`DatasetLocks`] a caller wires for datasets.
fn data_id_locks() -> &'static DatasetLocks {
    static LOCKS: std::sync::LazyLock<DatasetLocks> = std::sync::LazyLock::new(DatasetLocks::new);
    &LOCKS
}

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

/// Unwrap a [`DataInput::DataItem`] to its underlying input, returning other
/// variants unchanged. Used to detect the underlying variant (e.g. `S3Path`)
/// regardless of `DataItem` wrapping.
fn unwrap_data_item(input: &DataInput) -> &DataInput {
    match input {
        DataInput::DataItem { data, .. } => unwrap_data_item(data),
        other => other,
    }
}

/// Build a minimal [`Document`] descriptor passed to a [`DocumentLoader`] at
/// ADD time. Only the metadata fields loaders read are populated; the content
/// bytes are passed separately to `extract`.
fn build_loader_descriptor(
    data_id: Uuid,
    name: &str,
    extension: &str,
    mime_type: &str,
) -> Document {
    let doc_type = cognee_models::doc_type_for_extension(extension)
        .unwrap_or("text")
        .to_string();
    let mut base = DataPoint::new("Document", None);
    base.id = data_id;
    Document {
        base,
        document_type: doc_type,
        name: name.to_string(),
        raw_data_location: String::new(),
        mime_type: mime_type.to_string(),
        extension: extension.to_string(),
        data_id,
        external_metadata: None,
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
        // TODO(COG-4456): replace placeholder metadata when S3 ingestion is implemented.
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

async fn resolve_input_for_processing(
    input: &DataInput,
) -> Result<
    (
        DataInput,
        Option<UrlMetadata>,
        Option<String>,
        Option<String>,
    ),
    Box<dyn std::error::Error>,
> {
    match input {
        DataInput::Url(url) => {
            #[cfg(feature = "html-loader")]
            {
                let resolved = resolve_url_input(url).await?;
                Ok((resolved.input, Some(resolved.metadata), None, None))
            }
            #[cfg(not(feature = "html-loader"))]
            {
                let _ = url;
                Err(Box::new(IngestionError::UrlIngestionUnavailable))
            }
        }
        DataInput::DataItem {
            data,
            label,
            external_metadata,
        } => {
            if let DataInput::Url(_url) = data.as_ref() {
                #[cfg(feature = "html-loader")]
                {
                    let resolved = resolve_url_input(_url).await?;
                    Ok((
                        resolved.input,
                        Some(resolved.metadata),
                        Some(label.clone()),
                        external_metadata.clone(),
                    ))
                }
                #[cfg(not(feature = "html-loader"))]
                {
                    Err(Box::new(IngestionError::UrlIngestionUnavailable))
                }
            } else {
                Ok((
                    input.clone(),
                    None,
                    Some(label.clone()),
                    external_metadata.clone(),
                ))
            }
        }
        _ => Ok((input.clone(), None, None, None)),
    }
}

fn is_html_url_metadata(metadata: &UrlMetadata) -> bool {
    metadata.essence == "text/html" || metadata.essence == "application/xhtml+xml"
}

/// What a stored or supplied `external_metadata` string holds, as the two
/// merges below read it.
///
/// Python's `DataItem.external_metadata` is `dict | None`, and `ingest_data`
/// merges it with `if item_external_metadata: ext_metadata.update(...)` — so
/// `None` and `{}` are nothing to merge. The Rust `DataInput::DataItem`
/// carries the metadata as a JSON string, which can also spell "nothing" as
/// the literal `null` or a blank string; both are read as [`Self::Absent`]
/// rather than kept verbatim under `data_item_external_metadata_raw`.
enum ItemMetadata {
    Absent,
    Object(serde_json::Map<String, serde_json::Value>),
    /// Valid JSON that is not an object, or text that is not JSON at all;
    /// kept verbatim under `data_item_external_metadata_raw` when merged.
    Raw(String),
}

fn classify_item_metadata(metadata: Option<&str>) -> ItemMetadata {
    let Some(metadata) = metadata else {
        return ItemMetadata::Absent;
    };
    if metadata.trim().is_empty() {
        return ItemMetadata::Absent;
    }
    match serde_json::from_str::<serde_json::Value>(metadata) {
        Ok(serde_json::Value::Null) => ItemMetadata::Absent,
        Ok(serde_json::Value::Object(object)) => ItemMetadata::Object(object),
        _ => ItemMetadata::Raw(metadata.to_string()),
    }
}

/// The map a merge starts from: the item's own keys, or its raw text under
/// `data_item_external_metadata_raw`, or nothing.
fn item_metadata_map(metadata: ItemMetadata) -> serde_json::Map<String, serde_json::Value> {
    match metadata {
        ItemMetadata::Absent => serde_json::Map::new(),
        ItemMetadata::Object(object) => object,
        ItemMetadata::Raw(raw) => serde_json::Map::from_iter([(
            "data_item_external_metadata_raw".to_string(),
            serde_json::Value::String(raw),
        )]),
    }
}

/// Whether two stored JSON texts spell the same value.
///
/// Parsed comparison, so `["a","b"]` (serde_json) equals `["a", "b"]`
/// (Python's `json.dumps`) and two objects with the same keys in another
/// order are one value. Text that is not JSON is compared as text, and
/// `None` equals only `None`.
fn same_json(stored: Option<&str>, candidate: Option<&str>) -> bool {
    match (stored, candidate) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            if a == b {
                return true;
            }
            match (
                serde_json::from_str::<serde_json::Value>(a),
                serde_json::from_str::<serde_json::Value>(b),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        }
        _ => false,
    }
}

/// Check everything an add call is about to store the way Python's
/// `ingest_data` does before anything is stored: the call's `node_set` and
/// every `DataItem`'s `external_metadata["node_set"]`.
///
/// [`AddPipeline::add_with_params`] runs this first, so a bad name fails the
/// whole call rather than one item mid-run. It is public for callers that do
/// irreversible work *before* they add — `cognee::api::update` deletes the
/// document it replaces first, and must refuse a node set the add would
/// refuse while the old data is still there.
pub fn validate_add_inputs(
    inputs: &[DataInput],
    node_set: Option<&[String]>,
) -> Result<(), IngestionError> {
    if let Some(names) = node_set {
        validate_node_set_names(names)?;
    }
    for input in inputs {
        validate_item_node_set(input)?;
    }
    Ok(())
}

/// Port of Python's `validate_node_set_names` for a list the type system has
/// already shaped: every name must be non-empty once trimmed.
///
/// Python also rejects a value that is not a list and entries that are not
/// strings; [`AddParams::node_set`] cannot carry either, so only the
/// non-empty rule is left to check here. The remaining Python check — that
/// the name survives `NodeSet.id_for` without a `UnicodeEncodeError` — has no
/// Rust counterpart: a `String` is always valid UTF-8.
pub fn validate_node_set_names(names: &[String]) -> Result<(), IngestionError> {
    for name in names {
        if name.trim().is_empty() {
            return Err(IngestionError::InvalidNodeSet {
                reason: format!(
                    "node_set entries must be non-empty strings, got {name:?} in {names:?}"
                ),
            });
        }
    }
    Ok(())
}

/// Port of Python's `validate_node_set_names` for a `node_set` value found in
/// a data item's `external_metadata`, which arrives as arbitrary JSON.
///
/// `null` means no node set; anything else must be an array of non-empty
/// strings. A bare string is rejected rather than read as one name, so there
/// is one accepted shape.
fn validate_node_set_value(value: &serde_json::Value) -> Result<(), IngestionError> {
    let serde_json::Value::Array(entries) = value else {
        if value.is_null() {
            return Ok(());
        }
        return Err(IngestionError::InvalidNodeSet {
            reason: format!("node_set must be a list of names, got {value}"),
        });
    };
    for entry in entries {
        if entry.as_str().is_none_or(|name| name.trim().is_empty()) {
            return Err(IngestionError::InvalidNodeSet {
                reason: format!(
                    "node_set entries must be non-empty strings, got {entry} in {value}"
                ),
            });
        }
    }
    Ok(())
}

/// Check a data item's own `external_metadata["node_set"]` the way Python's
/// `ingest_data` does before anything is stored:
/// `validate_node_set_names((data_item.external_metadata or {}).get("node_set"))`.
///
/// Metadata without the key, or that is not a JSON object, has nothing to
/// check — [`merge_external_metadata`] keeps such text raw rather than
/// reading keys from it.
pub fn validate_item_node_set(input: &DataInput) -> Result<(), IngestionError> {
    let DataInput::DataItem {
        external_metadata, ..
    } = input
    else {
        return Ok(());
    };
    if let ItemMetadata::Object(object) = classify_item_metadata(external_metadata.as_deref())
        && let Some(value) = object.get("node_set")
    {
        validate_node_set_value(value)?;
    }
    Ok(())
}

/// Record the add call's `node_set` under `external_metadata["node_set"]`.
///
/// Python's `ingest_data` does `ext_metadata["node_set"] = effective_node_set`
/// for every item when the set is non-empty, after merging the item's own
/// metadata — so the call's node set wins over a `node_set` key the item
/// carried. Cognify reads the node set back from there, not from the
/// `Data.node_set` column (`classify_documents.update_node_set`, ported in
/// `cognee_models::document`), so without the key a document — and every
/// chunk, summary and entity derived from it — is never tagged, and a
/// `node_name` search never finds it.
///
/// The caller passes a non-empty set; metadata that is not a JSON object is
/// kept under `data_item_external_metadata_raw`, as [`merge_external_metadata`]
/// keeps it, and `null` or blank metadata is read as none
/// ([`classify_item_metadata`]).
fn with_node_set_in_external_metadata(
    external_metadata: Option<&str>,
    node_set: &[String],
) -> Result<Option<String>, serde_json::Error> {
    let mut merged = item_metadata_map(classify_item_metadata(external_metadata));
    merged.insert("node_set".to_string(), serde_json::json!(node_set));
    serde_json::to_string(&serde_json::Value::Object(merged)).map(Some)
}

fn merge_external_metadata(
    data_item_metadata: Option<String>,
    url_metadata: Option<&UrlMetadata>,
) -> Result<Option<String>, serde_json::Error> {
    let Some(metadata) = url_metadata else {
        // Nothing to merge with: the item's text is stored verbatim, unless
        // it spells "no metadata".
        return Ok(data_item_metadata.filter(|item_metadata| {
            !matches!(
                classify_item_metadata(Some(item_metadata)),
                ItemMetadata::Absent
            )
        }));
    };

    let data_item_metadata = classify_item_metadata(data_item_metadata.as_deref());
    let user_metadata_object = match &data_item_metadata {
        ItemMetadata::Object(object) => Some(serde_json::Value::Object(object.clone())),
        _ => None,
    };
    let mut merged = item_metadata_map(data_item_metadata);
    let mut has_conflict = false;

    let url_fields = [
        ("source", serde_json::json!("url")),
        ("url", serde_json::json!(metadata.requested_url.clone())),
        ("final_url", serde_json::json!(metadata.final_url.clone())),
        (
            "content_type",
            serde_json::json!(metadata.content_type.clone()),
        ),
    ];
    for (key, value) in url_fields {
        if merged.contains_key(key) {
            has_conflict = true;
        }
        merged.insert(key.to_string(), value);
    }
    if let Some(title) = &metadata.title {
        if merged.contains_key("title") {
            has_conflict = true;
        }
        merged.insert("title".to_string(), serde_json::json!(title));
    }
    if has_conflict && let Some(user_metadata) = user_metadata_object {
        merged.insert("data_item_external_metadata".to_string(), user_metadata);
    }

    serde_json::to_string(&serde_json::Value::Object(merged)).map(Some)
}

/// Derive a human-readable name for the stored Data record.
fn extract_name(input: &DataInput, content_hash: &str) -> String {
    match input {
        DataInput::Text(_) => format!("text_{content_hash}"),
        DataInput::FilePath(path) => {
            let clean_path = path.strip_prefix("file://").unwrap_or(path);
            Path::new(clean_path)
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        }
        DataInput::Url(_) => format!("text_{content_hash}"),
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
                format!("file://{path}")
            }
        }
        DataInput::Url(url) => url.clone(),
        DataInput::S3Path(path) => path.clone(),
        DataInput::Binary { name, .. } => format!("binary://{name}"),
        DataInput::DataItem { data, .. } => extract_original_location(data),
    }
}

// ---------------------------------------------------------------------------
// Task 1 wrapper: DataInput → ProcessedInput
// ---------------------------------------------------------------------------

/// Lift a `process_input` error into a [`TaskError`], keeping the type where
/// it is possible to keep it.
///
/// `process_input` returns a plain `Box<dyn Error>`, which does not satisfy
/// `TaskError`'s `Send + Sync`, so the general case has to flatten to a string.
/// `IngestionError` itself IS `Send + Sync`, so recover it first and let it
/// cross intact: the executor then keeps it as `ExecutionError::TaskFailed`'s
/// `#[source]`, which is what lets the HTTP layer classify
/// `UnsupportedDocumentType` as 415 instead of 500.
///
/// This used to flatten unconditionally, so `downcast_ref` found nothing at any
/// level of the source chain and no caller could tell a client error from a
/// server one.
///
/// Split out of the closure so it can be tested without a storage backend or a
/// particular loader feature set — the end-to-end assertion in
/// `test_unsupported_loader_type_errors_at_add` is gated on the pdf features
/// being *off*, which is not the configuration a workspace test run uses.
fn preserve_ingestion_error(e: Box<dyn std::error::Error>) -> TaskError {
    match e.downcast::<IngestionError>() {
        Ok(typed) => typed as TaskError,
        Err(other) => format!("{other}").into(),
    }
}

/// Build a [`TypedTask`] that streams a [`DataInput`] to storage, hashes its
/// content, and returns a self-contained [`ProcessedInput`].
pub fn make_process_input_task(
    storage: Arc<dyn StorageTrait>,
    hash_algorithm: HashAlgorithm,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> TypedTask<DataInput, ProcessedInput> {
    make_process_input_task_with_registry(
        storage,
        hash_algorithm,
        owner_id,
        tenant_id,
        Arc::new(LoaderRegistry::default_registry()),
        None,
    )
}

/// [`make_process_input_task`] with the loaders of `registry`, and with
/// `database` to reuse the stored text of content added before.
fn make_process_input_task_with_registry(
    storage: Arc<dyn StorageTrait>,
    hash_algorithm: HashAlgorithm,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    registry: Arc<LoaderRegistry>,
    database: Option<Arc<dyn IngestDb>>,
) -> TypedTask<DataInput, ProcessedInput> {
    TypedTask::async_fn(move |input: &DataInput, _ctx| {
        let input = input.clone();
        let storage = Arc::clone(&storage);
        let registry = Arc::clone(&registry);
        let database = database.clone();
        Box::pin(async move {
            process_input_with_registry(
                &input,
                &*storage,
                hash_algorithm,
                owner_id,
                tenant_id,
                &registry,
                database.as_deref(),
            )
            .await
            .map(Box::new)
            .map_err(preserve_ingestion_error)
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

/// The [`AddParams`] fields the persist closure injects into each
/// [`ProcessedInput`]. Constructed once per pipeline.
///
/// Locked Decision 7 (LIB-06) — `AddParams` is wired in via the task
/// closure rather than a `RunSpec` / `TaskContext` extension.
#[derive(Debug, Clone, Default)]
struct AddParamsInjection {
    /// Written to the `Data.node_set` column and `external_metadata["node_set"]`
    /// by [`persist_data_with_acl_and_locks`].
    node_set: Option<Vec<String>>,
    importance_weight: Option<f64>,
    target_dataset_id: Option<Uuid>,
    /// Identity locks serializing dataset create-and-grant against concurrent
    /// writers, or `None` for the unsynchronised single-caller default. Wired
    /// by embedders that serve concurrent requests — see
    /// [`AddPipeline::with_dataset_locks`].
    dataset_locks: Option<Arc<DatasetLocks>>,
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
        if let Some(ref node_set) = add_params.node_set {
            processed.node_set = Some(node_set.clone());
        }
        if let Some(w) = add_params.importance_weight {
            processed.importance_weight = Some(w);
        }
        let override_ds = add_params.target_dataset_id;
        let dataset_locks = add_params.dataset_locks.clone();
        let database = Arc::clone(&database);
        let dataset_name = dataset_name.clone();
        let acl_db = acl_db.clone();
        Box::pin(async move {
            persist_data_with_acl_and_locks(
                &processed,
                &*database,
                &dataset_name,
                owner_id,
                tenant_id,
                acl_db.as_deref(),
                override_ds,
                dataset_locks.as_deref(),
            )
            .await
            .map(Box::new)
            .map_err(preserve_ingestion_error)
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
        Arc::new(LoaderRegistry::default_registry()),
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
    registry: Arc<LoaderRegistry>,
) -> Pipeline {
    // Locked Decision 4 (LIB-06): the per-input `data_id_fn` operates on
    // the pipeline input (`DataInput`), which has no UUID until
    // `persist_data` runs. Return `None` here; the executor's run_info
    // `data_ids` stays empty (the watcher maps it to Python's `"None"`).
    // Gap-08 task 07 revisits this once the watcher is real.
    let data_id_fn: DataIdFn = Arc::new(|_v: Arc<dyn Value>| None);
    PipelineBuilder::new_with_task(
        "ingestion.add",
        make_process_input_task_with_registry(
            Arc::clone(&storage),
            hash_algorithm,
            owner_id,
            tenant_id,
            registry,
            Some(Arc::clone(&database)),
        ),
    )
    .add_task(make_persist_data_task_with_acl_and_params(
        database,
        dataset_name.to_string(),
        owner_id,
        tenant_id,
        acl_db,
        add_params,
    ))
    // Persisted pipeline-run name; must match Python (add.py:319) and the
    // canonical "add_pipeline" name DatasetManager::get_status queries.
    .with_name("add_pipeline")
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
    db_connection: Option<Arc<dyn RelationalDb>>,
    // ─── Pipeline-run trail (gap 08-07) ───────────────────────────────────
    pipeline_run_repo: Option<Arc<dyn PipelineRunRepository>>,
    // ─── Concurrency (SDK-636) ────────────────────────────────────────────
    dataset_locks: Option<Arc<DatasetLocks>>,
    // ─── Media loaders at ADD time ────────────────────────────────────────
    #[cfg(feature = "audio-loader")]
    transcriber: Option<Arc<dyn Transcriber>>,
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
        Self::new_with_algorithm(storage, database, HashAlgorithm::default())
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
            // Python's add takes the process's one dataset lock registry;
            // `with_dataset_locks` swaps in a host's own.
            dataset_locks: Some(DatasetLocks::global()),
            #[cfg(feature = "audio-loader")]
            transcriber: None,
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

    /// Serialize dataset create-and-grant against other writers of the same
    /// dataset identity in this process.
    ///
    /// Only embedders that serve concurrent requests need this — an HTTP
    /// server whose `POST /v1/datasets` handler shares the same
    /// [`DatasetLocks`] registry. Without it an `add` can resolve a dataset
    /// row that a concurrent create is about to roll back, and the data it
    /// writes is left pointing at a dataset that no longer exists. See
    /// [`crate::dataset_locks`].
    ///
    /// Left unset the pipeline behaves exactly as before, which is what the
    /// CLI and the library facade want: one caller, nothing to serialize
    /// against.
    pub fn with_dataset_locks(mut self, locks: Arc<DatasetLocks>) -> Self {
        self.dataset_locks = Some(locks);
        self
    }

    /// The dataset lock registry wired with [`Self::with_dataset_locks`], if
    /// any. An operation that holds a dataset across `add` calls (`improve`)
    /// must hold it in this registry, so the `add` rides on its hold.
    pub fn dataset_locks(&self) -> Option<&Arc<DatasetLocks>> {
        self.dataset_locks.as_ref()
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

    /// Attach the relational [`RelationalDb`] used by
    /// [`cognee_core::TaskContext`]. This is the same SeaORM handle the
    /// SQL-backed `IngestDb` is built on.
    pub fn with_database(mut self, db: Arc<dyn RelationalDb>) -> Self {
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

    /// Run the audio loader at ADD time, transcribing with `transcriber`.
    ///
    /// Without it an audio input fails with
    /// [`IngestionError::UnsupportedDocumentType`], as no audio loader is
    /// registered (Python transcribes at add time too, `ingest_data.py:103`).
    #[cfg(feature = "audio-loader")]
    pub fn with_transcriber(mut self, transcriber: Arc<dyn Transcriber>) -> Self {
        self.transcriber = Some(transcriber);
        self
    }

    /// The loaders `add` runs: the default registry, plus the audio loader
    /// when a transcriber is attached.
    fn loader_registry(&self) -> LoaderRegistry {
        let registry = LoaderRegistry::default_registry();
        #[cfg(feature = "audio-loader")]
        if let Some(transcriber) = &self.transcriber {
            return registry.with_audio(Arc::clone(transcriber));
        }
        registry
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

        // ── Validate node sets before anything is stored ─────────────────
        // Python's `ingest_data` checks the call's `node_set` and every
        // DataItem's `external_metadata["node_set"]` first, so a bad name
        // fails the whole call rather than one item mid-run. (`process_input`
        // and `persist_data*` check their own input again — each is a public
        // entry point of its own, and the repeat is one metadata parse per
        // item.)
        validate_add_inputs(&inputs, params.node_set.as_deref())?;

        // ── Add-params injected per item (Decision 7) ────────────────────
        let add_params_inj = AddParamsInjection {
            node_set: params.node_set.clone(),
            importance_weight: params.importance_weight,
            target_dataset_id: params.dataset_id,
            dataset_locks: self.dataset_locks.clone(),
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
            Arc::new(self.loader_registry()),
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
    /// A `DataInput::Url` was supplied but the `html-loader` feature (which
    /// provides URL crawling/fetching) is not enabled in this build.
    #[error("URL ingestion requires the `html-loader` feature to be enabled")]
    UrlIngestionUnavailable,
    /// A `DataInput::S3Path` was supplied. S3 ingestion is not yet implemented
    /// (decision D3); the loader-dispatch path must not store raw S3 bytes.
    #[error("S3 ingestion is not yet implemented")]
    S3IngestionUnavailable,
    /// No loader is registered for the document type derived from the input's
    /// extension (the relevant loader feature is disabled in this build).
    #[error("Unsupported document type at ingest: {document_type}")]
    UnsupportedDocumentType { document_type: String },
    /// A node set — the add call's or one in a data item's
    /// `external_metadata["node_set"]` — is not a list of non-empty names.
    /// Port of Python's `InvalidNodeSetError`, a 400-class client error.
    #[error("Invalid node_set: {reason}")]
    InvalidNodeSet { reason: String },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use cognee_database::{connect, initialize, ops};
    use cognee_graph::MockGraphDB;
    #[cfg(feature = "html-loader")]
    use cognee_storage::LocalStorage;
    use cognee_storage::MockStorage;
    use cognee_vector::MockVectorDB;
    #[cfg(feature = "html-loader")]
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
            .with_database(db.clone());
        (pipeline, db)
    }

    #[cfg(feature = "html-loader")]
    async fn server_with_robots() -> ServerGuard {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/robots.txt")
            .with_status(404)
            .create_async()
            .await;
        server
    }

    #[cfg(feature = "html-loader")]
    #[tokio::test]
    async fn test_process_input_url_html_stores_text_with_source_metadata() {
        let mut server = server_with_robots().await;
        let html = "<html><head><title>Example</title><style>.x{display:none}</style></head><body><h1>Visible text</h1><script>hidden()</script></body></html>";
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

        let raw_source_uri = processed.raw_source_uri.as_ref().unwrap();
        let raw_html = storage.retrieve(raw_source_uri).await.unwrap();
        assert_eq!(raw_html, html.as_bytes());
        assert!(raw_source_uri.ends_with(".html"));
        assert!(processed.raw_data_uri.ends_with(".txt"));
        assert_ne!(processed.raw_data_uri, *raw_source_uri);

        let stored = storage.retrieve(&processed.raw_data_uri).await.unwrap();
        let stored_text = String::from_utf8(stored).unwrap();
        assert!(stored_text.contains("Visible text"));
        assert!(!stored_text.contains("<html>"));
        assert!(!stored_text.contains("hidden()"));
        assert!(!stored_text.contains("display:none"));
        assert_eq!(processed.stored_extension, "txt");
        assert_eq!(processed.stored_mime_type, "text/plain");
        assert_eq!(processed.original_extension, "html");
        assert_eq!(processed.original_mime_type, "text/html");
        assert_eq!(processed.loader_engine, "beautiful_soup_loader");
        assert_eq!(processed.original_location, *raw_source_uri);

        let metadata: serde_json::Value =
            serde_json::from_str(processed.external_metadata.as_ref().unwrap()).unwrap();
        assert_eq!(metadata["source"], "url");
        assert_eq!(metadata["url"], url);
        assert_eq!(metadata["final_url"], url);
        assert_eq!(metadata["content_type"], "text/html; charset=utf-8");
        assert_eq!(metadata["title"], "Example");
    }

    #[cfg(feature = "html-loader")]
    #[tokio::test]
    async fn test_persist_data_url_html_uses_text_payload_and_raw_html_original_location() {
        let mut server = server_with_robots().await;
        let url = format!("{}/page", server.url());
        let html = "<html><head><title>XHTML</title></head><body>XHTML body</body></html>";
        let _mock = server
            .mock("GET", "/page")
            .with_header("content-type", "application/xhtml+xml")
            .with_body(html)
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
        assert!(data.raw_data_location.ends_with(".txt"));
        assert!(data.original_data_location.ends_with(".html"));
        assert_ne!(data.raw_data_location, data.original_data_location);
        assert_eq!(
            storage
                .retrieve(&data.original_data_location)
                .await
                .unwrap(),
            html.as_bytes()
        );
        assert_eq!(data.original_extension.as_deref(), Some("html"));
        assert_eq!(
            data.original_mime_type.as_deref(),
            Some("application/xhtml+xml")
        );
        assert_eq!(data.loader_engine.as_deref(), Some("beautiful_soup_loader"));
    }

    #[cfg(feature = "html-loader")]
    #[tokio::test]
    async fn test_data_item_url_merges_metadata_and_preserves_label() {
        let mut server = server_with_robots().await;
        let url = format!("{}/wrapped", server.url());
        let _mock = server
            .mock("GET", "/wrapped")
            .with_header("content-type", "text/html")
            .with_body("<html><head><title>Wrapped</title></head><body>Wrapped body</body></html>")
            .create_async()
            .await;
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());
        let owner_id = Uuid::new_v4();
        let processed = process_input(
            &DataInput::DataItem {
                data: Box::new(DataInput::Url(url.clone())),
                label: "wrapped-label".to_string(),
                external_metadata: Some(r#"{"custom":"keep","rank":7}"#.to_string()),
            },
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();
        let data = persist_data(&processed, &db, "wrapped-url", owner_id, None)
            .await
            .unwrap();

        assert_eq!(data.label.as_deref(), Some("wrapped-label"));
        assert_eq!(data.extension, "txt");
        assert!(data.original_data_location.ends_with(".html"));
        let metadata: serde_json::Value =
            serde_json::from_str(data.external_metadata.as_ref().unwrap()).unwrap();
        assert_eq!(metadata["custom"], "keep");
        assert_eq!(metadata["rank"], 7);
        assert_eq!(metadata["source"], "url");
        assert_eq!(metadata["url"], url);
        assert_eq!(metadata["final_url"], url);
        assert_eq!(metadata["content_type"], "text/html");
        assert_eq!(metadata["title"], "Wrapped");
    }

    #[cfg(feature = "html-loader")]
    #[tokio::test]
    async fn test_data_item_url_invalid_or_non_object_metadata_preserved_under_raw_field() {
        let mut server = server_with_robots().await;
        let cases = [
            ("/invalid-meta", "not-json"),
            ("/non-object-meta", r#"["not","an","object"]"#),
        ];

        for (path, user_metadata) in cases {
            let url = format!("{}{}", server.url(), path);
            let _mock = server
                .mock("GET", path)
                .with_header("content-type", "text/html")
                .with_body("<html><body>Metadata body</body></html>")
                .create_async()
                .await;
            let temp_dir = tempfile::tempdir().unwrap();
            let storage = LocalStorage::new(temp_dir.path().to_path_buf());
            let owner_id = Uuid::new_v4();

            let processed = process_input(
                &DataInput::DataItem {
                    data: Box::new(DataInput::Url(url.clone())),
                    label: "invalid-meta".to_string(),
                    external_metadata: Some(user_metadata.to_string()),
                },
                &storage,
                HashAlgorithm::Md5,
                owner_id,
                None,
            )
            .await
            .unwrap();

            let metadata: serde_json::Value =
                serde_json::from_str(processed.external_metadata.as_ref().unwrap()).unwrap();
            assert_eq!(metadata["data_item_external_metadata_raw"], user_metadata);
            assert_eq!(metadata["source"], "url");
            assert_eq!(metadata["url"], url);
        }
    }

    #[cfg(feature = "html-loader")]
    #[tokio::test]
    async fn test_non_html_url_inputs_do_not_store_raw_source_copy() {
        let mut server = server_with_robots().await;
        let cases = [
            ("/plain", "text/plain", "plain body", "txt", "text/plain"),
            (
                "/json",
                "application/json",
                r#"{"hello":"world"}"#,
                "json",
                "application/json",
            ),
            ("/csv", "text/csv", "a,b\n1,2\n", "csv", "text/csv"),
            (
                "/pdf",
                "application/pdf",
                "%PDF-1.7\n",
                "pdf",
                "application/pdf",
            ),
        ];

        for (path, content_type, body, expected_ext, expected_mime) in cases {
            let url = format!("{}{}", server.url(), path);
            let _mock = server
                .mock("GET", path)
                .with_header("content-type", content_type)
                .with_body(body)
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

            assert_eq!(processed.raw_source_uri, None);
            assert_eq!(processed.original_location, url);
            assert_eq!(processed.stored_extension, expected_ext);
            assert_eq!(processed.stored_mime_type, expected_mime);
            assert_eq!(
                storage.retrieve(&processed.raw_data_uri).await.unwrap(),
                body.as_bytes()
            );
        }
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

        let datasets = cognee_database::IngestDb::list_datasets_by_owner(&*db, owner_id)
            .await
            .unwrap();
        assert_eq!(datasets.len(), 1);
        let ds_data = cognee_database::DeleteDb::get_dataset_data(&*db, datasets[0].id)
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

        let datasets = cognee_database::IngestDb::list_datasets_by_owner(&*db, owner_id)
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

        let datasets = cognee_database::IngestDb::list_datasets_by_owner(&*db, owner_id)
            .await
            .unwrap();
        assert_eq!(datasets.len(), 1);
        let ds_data = cognee_database::DeleteDb::get_dataset_data(&*db, datasets[0].id)
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

        let dataset = cognee_database::IngestDb::get_dataset_by_name_in_tenant(
            &*db,
            "test_dataset",
            owner_id,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let ds_data = cognee_database::DeleteDb::get_dataset_data(&*db, dataset.id)
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

        let datasets = cognee_database::IngestDb::list_datasets_by_owner(&*db, owner_id)
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

        let datasets = cognee_database::IngestDb::list_datasets_by_owner(&*db, owner_id)
            .await
            .unwrap();
        assert_eq!(datasets.len(), 1);
        let ds_data = cognee_database::DeleteDb::get_dataset_data(&*db, datasets[0].id)
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
        let datasets = cognee_database::IngestDb::list_datasets_by_owner(&*db, owner_id)
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
        // After running the loader at ADD time, the stored artifact is the
        // extracted text and `loader_engine` reflects the loader that ran.
        // A `.txt` file runs the always-available text loader.
        let (pipeline, _db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "content").unwrap();
        let txt_path = temp_file.path().with_extension("txt");
        std::fs::copy(temp_file.path(), &txt_path).unwrap();

        let result = pipeline
            .add(
                vec![DataInput::FilePath(txt_path.to_str().unwrap().to_string())],
                "ds",
                owner_id,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result[0].loader_engine.as_deref(), Some("text_loader"));
        assert_eq!(result[0].extension, "txt");
        let _ = std::fs::remove_file(&txt_path);
    }

    /// With a loader feature disabled, ADD must surface a clear error rather
    /// than silently store raw bytes. Under default features no `pdf` loader is
    /// registered, so a `.pdf` input errors at ingest (Python parity gotcha).
    #[cfg(not(any(feature = "pdf-pdfium", feature = "pdf-pure-rust")))]
    #[tokio::test]
    async fn test_unsupported_loader_type_errors_at_add() {
        let (pipeline, _db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "%PDF-1.7").unwrap();
        let pdf_path = temp_file.path().with_extension("pdf");
        std::fs::copy(temp_file.path(), &pdf_path).unwrap();

        let result = pipeline
            .add(
                vec![DataInput::FilePath(pdf_path.to_str().unwrap().to_string())],
                "ds",
                owner_id,
                None,
            )
            .await;

        assert!(
            result.is_err(),
            "pdf input must error when the pdf loader feature is off"
        );

        // The VARIANT must survive the trip out, not just the failure. The task
        // wrapper used to flatten every error to a string, so `downcast_ref`
        // found nothing at any level of the source chain and callers could not
        // tell a client error from a server one — which is why `POST /add`
        // reported an unsupported type as 500. `make_process_input_task` now
        // passes `IngestionError` through typed, and `ExecutionError::TaskFailed`
        // keeps it as `#[source]`; this pins that path end to end.
        let err = result.expect_err("checked is_err above");
        let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&*err);
        let mut found = false;
        while let Some(e) = current {
            if matches!(
                e.downcast_ref::<IngestionError>(),
                Some(IngestionError::UnsupportedDocumentType { .. })
            ) {
                found = true;
                break;
            }
            current = e.source();
        }
        assert!(
            found,
            "`IngestionError::UnsupportedDocumentType` must be recoverable from the \
             error chain (the HTTP layer classifies on it to return 415); got: {err}"
        );

        let _ = std::fs::remove_file(&pdf_path);
    }

    /// Feature-independent guard for the typed-error passthrough that
    /// `POST /api/v1/add`'s 415 depends on. The end-to-end version of this
    /// (`test_unsupported_loader_type_errors_at_add`) only compiles when the
    /// pdf features are OFF, which is not how a workspace run resolves them,
    /// so on its own it would leave this unguarded in CI.
    #[test]
    fn preserve_ingestion_error_keeps_the_variant_recoverable() {
        let boxed: Box<dyn std::error::Error> = Box::new(IngestionError::UnsupportedDocumentType {
            document_type: "pdf".to_string(),
        });
        let lifted = preserve_ingestion_error(boxed);
        assert!(
            matches!(
                lifted.downcast_ref::<IngestionError>(),
                Some(IngestionError::UnsupportedDocumentType { document_type }) if document_type == "pdf"
            ),
            "the variant must survive the lift into TaskError; got: {lifted}"
        );
    }

    /// The other half of the contract: anything that is not an
    /// `IngestionError` still has to cross, since it cannot be moved into a
    /// `Send + Sync` box. It flattens to its message, and must not be
    /// mistaken for a typed error afterwards.
    #[test]
    fn preserve_ingestion_error_flattens_everything_else() {
        let boxed: Box<dyn std::error::Error> = Box::new(std::io::Error::other("disk on fire"));
        let lifted = preserve_ingestion_error(boxed);
        assert_eq!(lifted.to_string(), "disk on fire");
        assert!(lifted.downcast_ref::<IngestionError>().is_none());
    }

    /// Positive counterpart to `test_unsupported_loader_type_errors_at_add`.
    ///
    /// #102 was a `pdf` loader that no shipped binding registered, so `.pdf`
    /// input hit `UnsupportedDocumentType` at dispatch. The negative test above
    /// pins the disabled case and `cfg`s out wherever a backend is on, which
    /// left the enabled case with no coverage at all — so a future edit to a
    /// binding `default` list could silently reinstate the bug.
    ///
    /// This asserts registration rather than driving an `add`, deliberately.
    /// Going through the pipeline would exercise the *extraction* backend,
    /// which is a different thing from what broke, and on a cold cache the
    /// pdfium backend would have to download a ~7 MB libpdfium. A registration
    /// assertion is backend-agnostic, needs no PDF fixture and no network, and
    /// pins exactly what #102 was.
    ///
    /// (This comment used to give a second reason — that the task wrapper
    /// erased the error type, making a variant assertion vacuous. That is no
    /// longer true: `make_process_input_task` passes `IngestionError` through
    /// typed, and the negative test above now asserts the variant. The
    /// remaining reasons still stand.)
    ///
    /// The cold-cache abort this comment used to describe — `bind_pdfium_silent`
    /// building and dropping a tokio runtime inside async — is fixed; the
    /// download now runs on `spawn_blocking`, and
    /// `tests/pdf_pdfium_cold_cache.rs` pins that it errors rather than aborts.
    #[cfg(any(feature = "pdf-pdfium", feature = "pdf-pure-rust"))]
    #[test]
    fn test_pdf_loader_is_registered_when_a_backend_is_enabled() {
        let registry = crate::loaders::LoaderRegistry::default_registry();
        let loader = registry.get("pdf");
        assert!(
            loader.is_some(),
            "a pdf backend is compiled in, so `default_registry` must register a \
             `pdf` loader; without it `.pdf` input fails at dispatch with \
             `UnsupportedDocumentType`, which is #102"
        );
        assert_eq!(
            loader.map(|l| l.engine_name()),
            Some("pypdf_loader"),
            "the registered pdf loader should be `PdfLoader` (Python-parity engine name)"
        );
    }

    /// Text-path no-regression: the stored artifact is byte-identical to the
    /// input, `raw_content_hash == content_hash`, `extension == "txt"`, and the
    /// content hash / data_id match the pinned Python-compatible values.
    #[tokio::test]
    async fn test_text_path_no_regression_hashes_and_stored_bytes() {
        use cognee_storage::LocalStorage;

        // Pinned values for the fixed string (MD5 of the bytes, verified by the
        // content_hasher unit tests: md5("hello world") == HELLO_WORLD_MD5).
        const HELLO_WORLD: &str = "hello world";
        const HELLO_WORLD_MD5: &str = "5eb63bbbe01eeed093cb22bb8f5acdc3";

        let temp_dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());
        let owner_id = Uuid::new_v4();

        let processed = process_input(
            &DataInput::Text(HELLO_WORLD.to_string()),
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();

        // content_hash + data_id derived from RAW bytes — SACRED, must not change.
        assert_eq!(processed.content_hash, HELLO_WORLD_MD5);
        assert_eq!(
            processed.data_id,
            generate_data_id(HELLO_WORLD_MD5, owner_id, None),
            "data_id must remain uuid5(content_hash + owner + tenant)"
        );

        // For plain text the extracted text equals the raw bytes.
        assert_eq!(
            processed.raw_content_hash, processed.content_hash,
            "plain-text raw_content_hash must equal content_hash"
        );
        assert_eq!(processed.stored_extension, "txt");
        assert_eq!(processed.stored_mime_type, "text/plain");
        assert_eq!(processed.loader_engine, "text_loader");

        // Stored file content is byte-identical to the input.
        let stored = storage.retrieve(&processed.raw_data_uri).await.unwrap();
        assert_eq!(stored, HELLO_WORLD.as_bytes());
    }

    /// CSV path (feature-gated): the stored artifact is the EXTRACTED text
    /// (rows formatted as Python does), `extension == "txt"`, the stored file
    /// name ends in `text_<content_hash>.txt`, `original_extension == "csv"`,
    /// and `raw_content_hash != content_hash`.
    #[cfg(feature = "csv-loader")]
    #[tokio::test]
    async fn test_csv_path_stores_extracted_text() {
        use cognee_storage::LocalStorage;

        let csv = "name,age\nAlice,30\nBob,25\n";
        let temp_dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());
        let owner_id = Uuid::new_v4();

        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "{csv}").unwrap();
        let csv_path = temp_file.path().with_extension("csv");
        std::fs::copy(temp_file.path(), &csv_path).unwrap();

        let processed = process_input(
            &DataInput::FilePath(csv_path.to_str().unwrap().to_string()),
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();

        // content_hash is the MD5 of the RAW csv bytes (unchanged).
        assert_eq!(
            processed.content_hash,
            crate::content_hasher::ContentHasher::hash_content(csv.as_bytes(), HashAlgorithm::Md5)
        );
        // Stored artifact is extracted text, not raw csv.
        assert_eq!(processed.stored_extension, "txt");
        assert_eq!(processed.stored_mime_type, "text/plain");
        assert_eq!(processed.original_extension, "csv");
        assert_eq!(processed.loader_engine, "csv_loader");
        assert_ne!(
            processed.raw_content_hash, processed.content_hash,
            "extracted csv text differs from raw bytes"
        );

        let stored = storage.retrieve(&processed.raw_data_uri).await.unwrap();
        let stored_text = String::from_utf8(stored).unwrap();
        assert!(stored_text.contains("name: Alice, age: 30"));
        assert!(!stored_text.contains("name,age"));

        // raw_content_hash is the MD5 of the stored extracted text.
        assert_eq!(
            processed.raw_content_hash,
            crate::content_hasher::ContentHasher::hash_content(
                stored_text.as_bytes(),
                HashAlgorithm::Md5
            )
        );
        let _ = std::fs::remove_file(&csv_path);
    }

    /// S3 ingestion (decision D3) must error at ingest, never store raw bytes.
    #[tokio::test]
    async fn test_s3_input_errors_at_add() {
        use cognee_storage::LocalStorage;

        let temp_dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(temp_dir.path().to_path_buf());

        let result = process_input(
            &DataInput::S3Path("s3://bucket/key.txt".to_string()),
            &storage,
            HashAlgorithm::Md5,
            Uuid::new_v4(),
            None,
        )
        .await;

        assert!(result.is_err(), "S3 input must error at ingest");
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

    #[test]
    fn node_set_is_recorded_in_external_metadata_like_python() {
        let set = vec!["alpha".to_string(), "beta".to_string()];
        let parse = |s: Option<String>| -> serde_json::Value {
            serde_json::from_str(&s.expect("metadata written")).expect("json")
        };

        // No metadata: Python's `get_external_metadata_dict` starts from `{}`.
        // JSON `null` and a blank string are no metadata either, not text to
        // keep raw (Python: `if item_external_metadata:` skips `None`).
        for absent in [None, Some("null"), Some(""), Some("  \n")] {
            assert_eq!(
                parse(with_node_set_in_external_metadata(absent, &set).unwrap()),
                serde_json::json!({"node_set": ["alpha", "beta"]}),
                "metadata {absent:?}"
            );
        }

        // The item's own keys survive; the call's node set replaces an item
        // `node_set` key, as `ext_metadata["node_set"] = ...` does.
        assert_eq!(
            parse(
                with_node_set_in_external_metadata(
                    Some("{\"source\":\"crm\",\"node_set\":[\"stale\"]}"),
                    &set
                )
                .unwrap()
            ),
            serde_json::json!({"source": "crm", "node_set": ["alpha", "beta"]})
        );

        // Metadata that is not an object is kept raw, as `merge_external_metadata` keeps it.
        assert_eq!(
            parse(with_node_set_in_external_metadata(Some("not json"), &set).unwrap()),
            serde_json::json!({
                "data_item_external_metadata_raw": "not json",
                "node_set": ["alpha", "beta"],
            })
        );
    }

    /// `add(node_set=[...])` must tag what cognify builds from the data: the
    /// node set reaches the `Document` (and from there every chunk) only
    /// through `external_metadata["node_set"]`. Before this, it reached the
    /// `Data.node_set` column alone, so no document was ever tagged and a
    /// `node_name` search never found data added with a node set.
    #[tokio::test]
    async fn add_with_node_set_tags_the_document_cognify_builds() {
        let (pipeline, _db) = make_pipeline().await;
        let params = AddParams {
            node_set: Some(vec!["alpha".to_string()]),
            ..AddParams::default()
        };
        let data = pipeline
            .add_with_params(
                vec![DataInput::Text(
                    "Alice maintains the billing service.".into(),
                )],
                "node_set_ds",
                Uuid::new_v4(),
                None,
                &params,
            )
            .await
            .expect("add");
        assert_eq!(data.len(), 1);
        let metadata: serde_json::Value = serde_json::from_str(
            data[0]
                .external_metadata
                .as_deref()
                .expect("external_metadata written"),
        )
        .expect("json");
        assert_eq!(metadata["node_set"], serde_json::json!(["alpha"]));
        assert_eq!(data[0].node_set.as_deref(), Some("[\"alpha\"]"));

        let documents = cognee_models::classify_documents(&data);
        assert_eq!(documents.len(), 1);
        let sets = documents[0]
            .base
            .belongs_to_set
            .as_ref()
            .expect("the document is tagged with its node set");
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0]["name"], "alpha");
        assert_eq!(documents[0].base.source_node_set.as_deref(), Some("alpha"));
    }

    fn metadata_json(data: &Data) -> serde_json::Value {
        serde_json::from_str(
            data.external_metadata
                .as_deref()
                .expect("external_metadata written"),
        )
        .expect("json")
    }

    /// Python's `ingest_data` update branch rewrites `node_set` and
    /// `external_metadata` on a re-add of existing content; the Rust dedup
    /// path used to return the stored row untouched, so content added once
    /// without a node set could never be tagged afterwards.
    #[tokio::test]
    async fn re_add_with_node_set_refreshes_the_stored_row() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();
        let input = || vec![DataInput::Text("Re-added content".into())];
        let with_set = |names: &[&str]| AddParams {
            node_set: Some(names.iter().map(|n| n.to_string()).collect()),
            ..AddParams::default()
        };

        let first = pipeline
            .add(input(), "re_add_ds", owner_id, None)
            .await
            .expect("first add");
        assert_eq!(first[0].node_set, None);
        assert_eq!(first[0].external_metadata, None);

        // A re-add that supplies a node set tags the existing row.
        let second = pipeline
            .add_with_params(input(), "re_add_ds", owner_id, None, &with_set(&["alpha"]))
            .await
            .expect("re-add with node set");
        assert_eq!(second[0].id, first[0].id);
        assert_eq!(second[0].node_set.as_deref(), Some("[\"alpha\"]"));
        assert_eq!(
            metadata_json(&second[0]),
            serde_json::json!({"node_set": ["alpha"]})
        );
        let stored = cognee_database::IngestDb::get_data(&*db, first[0].id)
            .await
            .unwrap()
            .expect("row exists");
        assert_eq!(stored.node_set.as_deref(), Some("[\"alpha\"]"));
        assert_eq!(
            metadata_json(&stored),
            serde_json::json!({"node_set": ["alpha"]})
        );
        assert!(stored.updated_at.is_some(), "the refresh stamps updated_at");

        // A re-add without a node set leaves the stored tags alone.
        let third = pipeline
            .add(input(), "re_add_ds", owner_id, None)
            .await
            .expect("re-add without node set");
        assert_eq!(third[0].node_set.as_deref(), Some("[\"alpha\"]"));

        // A re-add with another set replaces it, as `ext_metadata["node_set"] =
        // effective_node_set` does, and the row is still one row.
        let fourth = pipeline
            .add_with_params(
                input(),
                "re_add_ds",
                owner_id,
                None,
                &with_set(&["beta", "gamma"]),
            )
            .await
            .expect("re-add with another node set");
        assert_eq!(fourth[0].node_set.as_deref(), Some("[\"beta\",\"gamma\"]"));
        assert_eq!(
            metadata_json(&fourth[0]),
            serde_json::json!({"node_set": ["beta", "gamma"]})
        );
        let dataset = cognee_database::IngestDb::get_dataset_by_name_in_tenant(
            &*db,
            "re_add_ds",
            owner_id,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let rows = cognee_database::DeleteDb::get_dataset_data(&*db, dataset.id)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    /// Python: `node_set=json.dumps(effective_node_set) if effective_node_set
    /// else None` — an empty list is stored as NULL, not `"[]"`, and no
    /// `node_set` key is written.
    #[tokio::test]
    async fn empty_node_set_is_stored_as_null_like_python() {
        let (pipeline, _db) = make_pipeline().await;
        let params = AddParams {
            node_set: Some(vec![]),
            ..AddParams::default()
        };
        let data = pipeline
            .add_with_params(
                vec![DataInput::DataItem {
                    data: Box::new(DataInput::Text("Empty node set".into())),
                    label: "item".into(),
                    external_metadata: Some(r#"{"source":"crm"}"#.into()),
                }],
                "empty_set_ds",
                Uuid::new_v4(),
                None,
                &params,
            )
            .await
            .expect("add");
        assert_eq!(data[0].node_set, None);
        assert_eq!(
            metadata_json(&data[0]),
            serde_json::json!({"source": "crm"}),
            "no node_set key for an empty set"
        );
    }

    /// Port of Python's `validate_node_set_names`: the call's names and a
    /// DataItem's `external_metadata["node_set"]` are checked before
    /// anything is stored, and the whole call fails.
    #[tokio::test]
    async fn invalid_node_sets_are_rejected_before_anything_is_stored() {
        let (pipeline, db) = make_pipeline().await;
        let owner_id = Uuid::new_v4();

        let err = pipeline
            .add_with_params(
                vec![DataInput::Text("Blank name".into())],
                "invalid_set_ds",
                owner_id,
                None,
                &AddParams {
                    node_set: Some(vec!["alpha".into(), "  ".into()]),
                    ..AddParams::default()
                },
            )
            .await
            .expect_err("a blank name is rejected");
        assert!(
            matches!(
                err.downcast_ref::<IngestionError>(),
                Some(IngestionError::InvalidNodeSet { .. })
            ),
            "typed error, got: {err}"
        );

        for bad in [
            r#"{"node_set": "alpha"}"#,
            r#"{"node_set": ["alpha", 1]}"#,
            r#"{"node_set": [""]}"#,
        ] {
            let err = pipeline
                .add(
                    vec![DataInput::DataItem {
                        data: Box::new(DataInput::Text("Bad item key".into())),
                        label: "item".into(),
                        external_metadata: Some(bad.into()),
                    }],
                    "invalid_set_ds",
                    owner_id,
                    None,
                )
                .await
                .expect_err("a bad item node_set is rejected");
            assert!(
                matches!(
                    err.downcast_ref::<IngestionError>(),
                    Some(IngestionError::InvalidNodeSet { .. })
                ),
                "metadata {bad}: typed error, got: {err}"
            );
        }

        // Nothing was stored: the dataset was never created.
        assert!(
            cognee_database::IngestDb::get_dataset_by_name_in_tenant(
                &*db,
                "invalid_set_ds",
                owner_id,
                None
            )
            .await
            .unwrap()
            .is_none()
        );

        // `null` means no node set, as `validate_node_set_names(None)` does.
        let data = pipeline
            .add(
                vec![DataInput::DataItem {
                    data: Box::new(DataInput::Text("Null item key".into())),
                    label: "item".into(),
                    external_metadata: Some(r#"{"node_set": null}"#.into()),
                }],
                "null_set_ds",
                owner_id,
                None,
            )
            .await
            .expect("null is accepted");
        assert_eq!(data[0].node_set, None);
    }

    #[test]
    fn validate_node_set_value_follows_python() {
        assert!(validate_node_set_value(&serde_json::json!(null)).is_ok());
        assert!(validate_node_set_value(&serde_json::json!([])).is_ok());
        assert!(validate_node_set_value(&serde_json::json!(["a", "b c"])).is_ok());
        for bad in [
            serde_json::json!("a"),
            serde_json::json!({"a": 1}),
            serde_json::json!(1),
            serde_json::json!(["a", null]),
            serde_json::json!(["a", " "]),
        ] {
            assert!(validate_node_set_value(&bad).is_err(), "{bad}");
        }
        assert!(validate_node_set_names(&["a".into()]).is_ok());
        assert!(validate_node_set_names(&[]).is_ok());
        assert!(validate_node_set_names(&["a".into(), String::new()]).is_err());
    }

    /// JSON `null` and a blank string are "no metadata", for both merges.
    #[test]
    fn json_null_item_metadata_is_no_metadata() {
        for absent in [None, Some("null".to_string()), Some(String::new())] {
            assert_eq!(merge_external_metadata(absent, None).unwrap(), None);
        }
        assert_eq!(
            merge_external_metadata(Some(r#"{"a":1}"#.into()), None).unwrap(),
            Some(r#"{"a":1}"#.to_string()),
            "an object is stored verbatim"
        );
        assert_eq!(
            merge_external_metadata(Some("not json".into()), None).unwrap(),
            Some("not json".to_string()),
            "other text is stored verbatim, as before"
        );
        assert!(matches!(
            classify_item_metadata(Some("[1]")),
            ItemMetadata::Raw(raw) if raw == "[1]"
        ));
    }

    /// A caller of the public `persist_data` with `ProcessedInput::node_set`
    /// set gets both writes — the merge lives where the row is built, not in
    /// the `AddPipeline` task closure.
    #[tokio::test]
    async fn persist_data_writes_node_set_to_column_and_metadata() {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let storage = MockStorage::new();
        let owner_id = Uuid::new_v4();

        let mut processed = process_input(
            &DataInput::DataItem {
                data: Box::new(DataInput::Text("Direct persist".into())),
                label: "item".into(),
                external_metadata: Some(r#"{"source":"crm","node_set":["stale"]}"#.into()),
            },
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();
        processed.node_set = Some(vec!["alpha".into()]);

        let data = persist_data(&processed, &db, "direct_ds", owner_id, None)
            .await
            .unwrap();
        assert_eq!(data.node_set.as_deref(), Some("[\"alpha\"]"));
        assert_eq!(
            metadata_json(&data),
            serde_json::json!({"source": "crm", "node_set": ["alpha"]})
        );
        let documents = cognee_models::classify_documents(std::slice::from_ref(&data));
        assert_eq!(documents[0].base.source_node_set.as_deref(), Some("alpha"));

        // Blank names are refused on this entry point too.
        processed.node_set = Some(vec![String::new()]);
        let err = persist_data(&processed, &db, "direct_ds", owner_id, None)
            .await
            .expect_err("blank name");
        assert!(matches!(
            err.downcast_ref::<IngestionError>(),
            Some(IngestionError::InvalidNodeSet { .. })
        ));
    }

    /// A direct `persist_data*` caller whose node set is refused must not
    /// leave an empty dataset behind: the names are checked before the
    /// dataset is resolved or created (and before the owner is granted on it).
    #[tokio::test]
    async fn persist_data_rejects_the_node_set_before_creating_the_dataset() {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let storage = MockStorage::new();
        let owner_id = Uuid::new_v4();
        let acl = cognee_test_utils::MockAclDb::new();

        let mut processed = process_input(
            &DataInput::Text("Orphan check".into()),
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();
        processed.node_set = Some(vec!["alpha".into(), " ".into()]);

        let err = persist_data_with_acl(
            &processed,
            &db,
            "orphan_ds",
            owner_id,
            None,
            Some(&acl),
            None,
        )
        .await
        .expect_err("blank name");
        assert!(matches!(
            err.downcast_ref::<IngestionError>(),
            Some(IngestionError::InvalidNodeSet { .. })
        ));

        assert!(
            cognee_database::IngestDb::get_dataset_by_name_in_tenant(
                &db,
                "orphan_ds",
                owner_id,
                None
            )
            .await
            .unwrap()
            .is_none(),
            "no dataset was created for the refused call"
        );
        assert!(
            cognee_database::IngestDb::get_data(&db, processed.data_id)
                .await
                .unwrap()
                .is_none(),
            "no row was created either"
        );
        let generated_id = generate_dataset_id("orphan_ds", owner_id, None);
        assert!(
            !cognee_database::AclDb::has_permission(&acl, owner_id, generated_id, "write")
                .await
                .unwrap(),
            "no owner grant was written for the dataset that was never created"
        );
    }

    /// "Already carries this set" is decided on the parsed JSON: a row Python
    /// wrote spells the set with `json.dumps` spacing (`["a", "b"]`), and
    /// re-adding it under the same set must not rewrite the row.
    #[tokio::test]
    async fn re_add_under_the_same_set_leaves_a_python_written_row_untouched() {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let storage = MockStorage::new();
        let owner_id = Uuid::new_v4();

        let mut processed = process_input(
            &DataInput::Text("Python-written row".into()),
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();

        // The row as Python's `ingest_data` stores it for node_set=["a", "b"]:
        // `json.dumps(effective_node_set)` in the column, and the metadata
        // dict (a JSON column) rendered with the same spacing.
        let python_column = r#"["a", "b"]"#;
        let python_metadata = r#"{"source": "crm", "node_set": ["a", "b"]}"#;
        let row = Data::builder(
            processed.data_id,
            processed.name.clone(),
            processed.raw_data_uri.clone(),
            processed.original_location.clone(),
            "txt",
            "text/plain",
            processed.content_hash.clone(),
            owner_id,
        )
        .node_set(python_column)
        .external_metadata(python_metadata)
        .build();
        cognee_database::IngestDb::create_data(&db, row)
            .await
            .unwrap();

        processed.node_set = Some(vec!["a".into(), "b".into()]);
        let data = persist_data(&processed, &db, "python_row_ds", owner_id, None)
            .await
            .unwrap();
        assert_eq!(data.node_set.as_deref(), Some(python_column));
        assert_eq!(data.external_metadata.as_deref(), Some(python_metadata));

        let stored = cognee_database::IngestDb::get_data(&db, processed.data_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.node_set.as_deref(),
            Some(python_column),
            "the Python spelling is kept byte for byte"
        );
        assert_eq!(stored.external_metadata.as_deref(), Some(python_metadata));
        assert_eq!(stored.updated_at, None, "the row was not rewritten");

        // A different set is still a refresh.
        processed.node_set = Some(vec!["c".into()]);
        let data = persist_data(&processed, &db, "python_row_ds", owner_id, None)
            .await
            .unwrap();
        assert_eq!(data.node_set.as_deref(), Some(r#"["c"]"#));
        assert_eq!(
            metadata_json(&data),
            serde_json::json!({"source": "crm", "node_set": ["c"]})
        );
        assert!(data.updated_at.is_some());
    }

    /// An `IngestDb` that, after every `get_data`, writes the columns other
    /// writers own — the way a concurrent cognify (token count, completion
    /// marker) or search (last access) lands between the pipeline's read of
    /// the row and its refresh.
    struct WritesBetweenReadAndRefresh {
        inner: Arc<cognee_database::DatabaseConnection>,
        dataset_id: Uuid,
        hook: AfterRead,
    }

    /// What [`WritesBetweenReadAndRefresh`] does between the pipeline's read
    /// of the row and its refresh.
    enum AfterRead {
        /// Write `token_count` and the cognify completion marker.
        WriteOtherColumns,
        /// Delete the row once it has been attached (the dataset link goes
        /// with it, `ON DELETE CASCADE`).
        DeleteRow,
        /// Track how many calls are between their read and their refresh at
        /// once, yielding after the read so an unserialized second call would
        /// get its own read in first.
        CountOverlap {
            in_flight: Arc<std::sync::atomic::AtomicUsize>,
            max_in_flight: Arc<std::sync::atomic::AtomicUsize>,
        },
    }

    #[async_trait::async_trait]
    impl IngestDb for WritesBetweenReadAndRefresh {
        async fn get_dataset(
            &self,
            id: Uuid,
        ) -> Result<Option<cognee_models::Dataset>, cognee_database::DatabaseError> {
            IngestDb::get_dataset(&*self.inner, id).await
        }
        async fn create_dataset(
            &self,
            dataset: cognee_models::Dataset,
        ) -> Result<cognee_models::Dataset, cognee_database::DatabaseError> {
            IngestDb::create_dataset(&*self.inner, dataset).await
        }
        async fn list_datasets_by_owner(
            &self,
            owner_id: Uuid,
        ) -> Result<Vec<cognee_models::Dataset>, cognee_database::DatabaseError> {
            IngestDb::list_datasets_by_owner(&*self.inner, owner_id).await
        }
        async fn get_data(&self, id: Uuid) -> Result<Option<Data>, cognee_database::DatabaseError> {
            use std::sync::atomic::Ordering;
            let snapshot = IngestDb::get_data(&*self.inner, id).await?;
            if snapshot.is_some() {
                match &self.hook {
                    AfterRead::WriteOtherColumns => {
                        ops::data::update_data_token_count(&self.inner, id, 42).await?;
                        ops::data::mark_cognify_pipeline_status_complete(
                            &self.inner,
                            id,
                            self.dataset_id,
                        )
                        .await?;
                    }
                    AfterRead::DeleteRow => {}
                    AfterRead::CountOverlap {
                        in_flight,
                        max_in_flight,
                    } => {
                        let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        max_in_flight.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                }
            }
            Ok(snapshot)
        }
        async fn create_data(&self, d: Data) -> Result<Data, cognee_database::DatabaseError> {
            IngestDb::create_data(&*self.inner, d).await
        }
        async fn update_data_node_set(
            &self,
            data_id: Uuid,
            node_set: Option<&str>,
            external_metadata: Option<&str>,
        ) -> Result<chrono::DateTime<chrono::Utc>, cognee_database::DatabaseError> {
            let written =
                IngestDb::update_data_node_set(&*self.inner, data_id, node_set, external_metadata)
                    .await;
            if let AfterRead::CountOverlap { in_flight, .. } = &self.hook {
                in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
            written
        }
        async fn update_data_stored_text(
            &self,
            data_id: Uuid,
            raw_data_location: &str,
            raw_content_hash: &str,
            extension: &str,
            mime_type: &str,
            loader_engine: &str,
        ) -> Result<chrono::DateTime<chrono::Utc>, cognee_database::DatabaseError> {
            IngestDb::update_data_stored_text(
                &*self.inner,
                data_id,
                raw_data_location,
                raw_content_hash,
                extension,
                mime_type,
                loader_engine,
            )
            .await
        }
        async fn attach_data_to_dataset(
            &self,
            dataset_id: Uuid,
            data_id: Uuid,
        ) -> Result<(), cognee_database::DatabaseError> {
            IngestDb::attach_data_to_dataset(&*self.inner, dataset_id, data_id).await?;
            if matches!(self.hook, AfterRead::DeleteRow) {
                ops::data::delete_data(&self.inner, data_id).await?;
            }
            Ok(())
        }
        async fn update_last_accessed(
            &self,
            data_ids: &[Uuid],
            timestamp: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), cognee_database::DatabaseError> {
            IngestDb::update_last_accessed(&*self.inner, data_ids, timestamp).await
        }
        async fn get_latest_pipeline_status(
            &self,
            pipeline_name: &str,
            dataset_id: Uuid,
        ) -> Result<Option<cognee_database::PipelineRunStatus>, cognee_database::DatabaseError>
        {
            IngestDb::get_latest_pipeline_status(&*self.inner, pipeline_name, dataset_id).await
        }
    }

    /// The re-add refresh writes `node_set` and `external_metadata` only. A
    /// full-row write of the snapshot read earlier used to roll back whatever
    /// another writer put on the row in between.
    #[tokio::test]
    async fn re_add_refresh_keeps_columns_written_after_its_read() {
        let db = Arc::new(connect("sqlite::memory:").await.unwrap());
        initialize(&db).await.unwrap();
        let storage = MockStorage::new();
        let owner_id = Uuid::new_v4();

        let mut processed = process_input(
            &DataInput::Text("Raced row".into()),
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();
        let first = persist_data(&processed, &*db, "raced_ds", owner_id, None)
            .await
            .unwrap();
        assert_eq!(first.token_count, -1);
        assert_eq!(first.pipeline_status, None);
        let dataset_id = generate_dataset_id("raced_ds", owner_id, None);

        let racing = WritesBetweenReadAndRefresh {
            inner: db.clone(),
            dataset_id,
            hook: AfterRead::WriteOtherColumns,
        };
        processed.node_set = Some(vec!["alpha".into()]);
        let refreshed = persist_data(&processed, &racing, "raced_ds", owner_id, None)
            .await
            .unwrap();
        assert_eq!(refreshed.node_set.as_deref(), Some(r#"["alpha"]"#));

        let stored = cognee_database::IngestDb::get_data(&*db, processed.data_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.node_set.as_deref(), Some(r#"["alpha"]"#));
        assert_eq!(
            metadata_json(&stored),
            serde_json::json!({"node_set": ["alpha"]})
        );
        assert_eq!(
            stored.token_count, 42,
            "a token count written after the read survives the refresh"
        );
        let status: serde_json::Value =
            serde_json::from_str(stored.pipeline_status.as_deref().expect("marker kept")).unwrap();
        assert_eq!(
            status["cognify_pipeline"][ops::data::pipeline_status_dataset_key(dataset_id)],
            serde_json::json!(ops::data::DATA_ITEM_PROCESSING_COMPLETED),
            "the cognify completion marker written after the read survives — and is \
             deliberately not cleared by a node-set change (Python parity; `update()` \
             is the re-tag path)"
        );
        // The returned row is the read snapshot patched with exactly what the
        // refresh wrote — no read-back. The refreshed columns and the
        // `updated_at` stamp equal the stored ones; columns other writers own
        // are as of the read (the row is not re-selected after every refresh).
        assert_eq!(refreshed.node_set, stored.node_set);
        assert_eq!(refreshed.external_metadata, stored.external_metadata);
        assert!(refreshed.updated_at.is_some());
        assert_eq!(refreshed.updated_at, stored.updated_at);
        assert_eq!(refreshed.token_count, -1, "as of the read");
    }

    /// Review finding (#306): a row deleted between the pipeline's read and
    /// its refresh used to be reported from the stale snapshot as a success.
    /// The refresh's `NotFound` now propagates instead.
    #[tokio::test]
    async fn re_add_refresh_of_a_row_deleted_after_the_read_is_an_error() {
        let db = Arc::new(connect("sqlite::memory:").await.unwrap());
        initialize(&db).await.unwrap();
        let storage = MockStorage::new();
        let owner_id = Uuid::new_v4();

        let mut processed = process_input(
            &DataInput::Text("Vanishing row".into()),
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();
        persist_data(&processed, &*db, "vanish_ds", owner_id, None)
            .await
            .unwrap();

        let racing = WritesBetweenReadAndRefresh {
            inner: db.clone(),
            dataset_id: generate_dataset_id("vanish_ds", owner_id, None),
            hook: AfterRead::DeleteRow,
        };
        processed.node_set = Some(vec!["alpha".into()]);
        let err = persist_data(&processed, &racing, "vanish_ds", owner_id, None)
            .await
            .expect_err("a vanished row is not reported as refreshed");
        assert!(err.to_string().contains("not found"), "got {err}");
        assert!(
            cognee_database::IngestDb::get_data(&*db, processed.data_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Review finding (#306): two concurrent re-adds of the same content used
    /// to interleave read-read-write-write, each merging its node set into the
    /// same stale metadata. The read-modify-write is now serialized per
    /// `data_id`, so no two calls are ever between their read and their write
    /// at once, and the stored row is the last writer's — column and metadata
    /// key agreeing.
    #[tokio::test]
    async fn concurrent_re_adds_of_one_row_are_serialized() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let db = Arc::new(connect("sqlite::memory:").await.unwrap());
        initialize(&db).await.unwrap();
        let storage = MockStorage::new();
        let owner_id = Uuid::new_v4();

        let processed = process_input(
            &DataInput::Text("Contended row".into()),
            &storage,
            HashAlgorithm::Md5,
            owner_id,
            None,
        )
        .await
        .unwrap();
        persist_data(&processed, &*db, "contended_ds", owner_id, None)
            .await
            .unwrap();

        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let racing = WritesBetweenReadAndRefresh {
            inner: db.clone(),
            dataset_id: generate_dataset_id("contended_ds", owner_id, None),
            hook: AfterRead::CountOverlap {
                in_flight: Arc::clone(&in_flight),
                max_in_flight: Arc::clone(&max_in_flight),
            },
        };
        let mut with_a = processed.clone();
        with_a.node_set = Some(vec!["a".into()]);
        let mut with_b = processed.clone();
        with_b.node_set = Some(vec!["b".into()]);

        let (a, b) = tokio::join!(
            persist_data(&with_a, &racing, "contended_ds", owner_id, None),
            persist_data(&with_b, &racing, "contended_ds", owner_id, None),
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(
            max_in_flight.load(Ordering::SeqCst),
            1,
            "the two re-adds' read-modify-writes must not overlap"
        );
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);

        // The later writer's row is the stored one, column and key agreeing.
        let stored = cognee_database::IngestDb::get_data(&*db, processed.data_id)
            .await
            .unwrap()
            .unwrap();
        let last = if a.updated_at >= b.updated_at { &a } else { &b };
        assert_eq!(stored.node_set, last.node_set);
        assert_eq!(stored.external_metadata, last.external_metadata);
        let column: serde_json::Value =
            serde_json::from_str(stored.node_set.as_deref().unwrap()).unwrap();
        assert_eq!(metadata_json(&stored)["node_set"], column);
    }
}
