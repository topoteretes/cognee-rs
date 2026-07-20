//! Shared async pipeline operations: `add`, `cognify`, `add_and_cognify`.
//!
//! These functions contain the pure-Rust async logic that is shared between
//! every language binding surface (C API, Neon JS, Python). Each function takes
//! a [`HandleState`] reference and `serde_json::Value` arguments, performs the
//! operation against the underlying cognee pipelines, and returns a
//! `serde_json::Value` result (or an [`SdkError`]).
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.
//!
//! ## Input marshalling
//!
//! `DataInput`'s derived serde is **externally tagged** (`{"Text":"…"}`),
//! which is **not** the `{ type, … }` discriminated union we expose to bindings,
//! so inputs are marshalled explicitly via [`crate::wire::marshal_inputs`].
//! Supported variants: `text`, `file`, `url`, `binary` (`name` required).
//! `s3` and recursive `dataItem` return [`SdkError::Unsupported`].
//!
//! ## Result marshalling
//!
//! `Data` is `Serialize` and crosses back directly. `CognifyResult` is **not**
//! `Serialize` (it carries non-serialisable internal fields), so its JSON is
//! hand-built via the shared [`crate::wire::cognify_result_json`] helper.

use std::collections::HashSet;

use serde_json::json;
use uuid::Uuid;

use cognee::cognify::cognify as cognee_cognify;
use cognee::database::ops;
use cognee::models::{Data, Dataset};

use crate::wire::{cognify_result_json, marshal_inputs};
use crate::{CogneeServices, HandleState, SdkError};

// ---------------------------------------------------------------------------
// opts parsing.
// ---------------------------------------------------------------------------

/// Parse an optional `tenant` UUID string out of an `opts` object.
pub fn opts_tenant(opts: &serde_json::Value) -> Result<Option<Uuid>, SdkError> {
    match opts.get("tenant").and_then(|v| v.as_str()) {
        Some(s) => Uuid::parse_str(s)
            .map(Some)
            .map_err(|e| SdkError::Validation(format!("invalid `tenant` UUID: {e}"))),
        None => Ok(None),
    }
}

/// Build a per-call `CognifyConfig` by cloning the cached config and applying
/// any `opts` overrides on top (rather than mutating the cached one).
pub fn cognify_config_with_opts(
    svc: &CogneeServices,
    opts: &serde_json::Value,
) -> cognee::cognify::CognifyConfig {
    let mut cfg = svc.cognify_config.clone();
    if let Some(n) = opts.get("chunkSize").and_then(|v| v.as_u64()) {
        cfg = cfg.with_chunk_size(n as usize);
    }
    if let Some(n) = opts.get("chunkOverlap").and_then(|v| v.as_u64()) {
        cfg = cfg.with_chunk_overlap(n as usize);
    }
    if let Some(b) = opts.get("summarization").and_then(|v| v.as_bool()) {
        cfg = cfg.with_summarization(b);
    }
    if let Some(b) = opts.get("temporalCognify").and_then(|v| v.as_bool()) {
        cfg = cfg.with_temporal_cognify(b);
    }
    if let Some(b) = opts.get("triplet").and_then(|v| v.as_bool()) {
        cfg = cfg.with_triplet_embeddings(b);
    }
    cfg
}

// ---------------------------------------------------------------------------
// Result marshalling helpers.
// ---------------------------------------------------------------------------

/// Serialise the `add` outcome into the `AddResult` JSON shape.
///
/// `AddPipeline::add` returns one [`Data`] per input — **including duplicates**
/// (the duplicate branch returns the pre-existing row). We therefore cannot
/// infer dedup from an empty result the way the plan assumed. Instead the caller
/// pre-scans the dataset's existing data ids and partitions the returned items
/// into newly-added vs deduplicated by id membership (`Data` ids are
/// content-addressed UUID5, so a re-added identical payload yields the same id).
pub fn add_result_json(
    newly_added: &[Data],
    duplicates: &[Data],
    dataset_name: &str,
) -> Result<serde_json::Value, SdkError> {
    let added = serde_json::to_value(newly_added)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize added data: {e}")))?;
    let dup = serde_json::to_value(duplicates)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize deduplicated data: {e}")))?;
    Ok(json!({
        "datasetName": dataset_name,
        "added": added,
        "addedCount": newly_added.len(),
        "deduplicated": dup,
        "deduplicatedCount": duplicates.len(),
    }))
}

/// The set of data ids already attached to the named dataset, or an empty set
/// when the dataset does not exist yet. Used to distinguish newly-added items
/// from duplicates in the `add` result.
pub async fn existing_data_ids(
    svc: &CogneeServices,
    name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<HashSet<Uuid>, SdkError> {
    let dataset = ops::datasets::get_dataset_by_name(&svc.database, name, owner_id, tenant_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("failed to resolve dataset '{name}': {e}")))?;
    let Some(dataset) = dataset else {
        return Ok(HashSet::new());
    };
    let data = ops::datasets::get_dataset_data(&svc.database, dataset.id)
        .await
        .map_err(|e| SdkError::Runtime(format!("failed to load data for dataset '{name}': {e}")))?;
    Ok(data.into_iter().map(|d| d.id).collect())
}

/// Partition the items returned by `add` into `(newly_added, duplicates)` using
/// the set of data ids that existed before the call.
///
/// `duplicates` holds items whose id already existed in the dataset before this
/// call; `newly_added` holds the rest.
pub fn partition_added(
    returned: Vec<Data>,
    pre_existing: &HashSet<Uuid>,
) -> (Vec<Data>, Vec<Data>) {
    returned
        .into_iter()
        .partition(|d| !pre_existing.contains(&d.id))
}

/// Resolve a dataset by name for the given owner/tenant, erroring if absent.
pub async fn resolve_dataset(
    svc: &CogneeServices,
    name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<Dataset, SdkError> {
    ops::datasets::get_dataset_by_name(&svc.database, name, owner_id, tenant_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("failed to resolve dataset '{name}': {e}")))?
        .ok_or_else(|| {
            SdkError::Validation(format!(
                "dataset '{name}' was not found for owner {owner_id}"
            ))
        })
}

/// Best-effort `User.email` lookup for cognify provenance stamping.
///
/// OSS build: the `users` table is owned by the closed cloud build, so this
/// always returns `None`. `cognify()` then uses `user_id.to_string()` as the
/// provenance stamp. The signature is preserved so call sites remain stable
/// when the closed build is swapped in (which will re-introduce the
/// DB-backed lookup).
pub async fn best_effort_user_email(_svc: &CogneeServices, _owner_id: Uuid) -> Option<String> {
    None
}

/// Shared cognify call: build the per-call config and invoke the 15-arg
/// `cognify(...)` free function in the exact positional order used by
/// `commands/cognify.rs`.
pub async fn run_cognify_on_items(
    svc: &CogneeServices,
    dataset: &Dataset,
    owner_id: Uuid,
    data_items: Vec<Data>,
    opts: &serde_json::Value,
) -> Result<cognee::cognify::CognifyResult, SdkError> {
    if data_items.is_empty() {
        return Ok(cognee::cognify::CognifyResult::empty());
    }

    let user_email = best_effort_user_email(svc, owner_id).await;
    let config = cognify_config_with_opts(svc, opts);

    cognee_cognify(
        data_items,
        dataset.id,
        Some(owner_id),
        user_email,
        dataset.tenant_id,
        svc.llm.clone(),
        svc.storage.clone(),
        svc.graph_db.clone(),
        svc.vector_db.clone(),
        svc.embedding_engine.clone(),
        svc.database.clone(),
        svc.pipeline_run_repo.clone(),
        svc.cpu_pool(),
        svc.ontology_resolver.clone(),
        &config,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("cognify failed: {e}")))
}

// ---------------------------------------------------------------------------
// Public top-level pipeline operations.
// ---------------------------------------------------------------------------

/// Run the add pipeline and return the `AddResult` JSON value.
///
/// `inputs_json` must be a single `{ type, … }` object or an array of them.
/// `opts` may be `serde_json::Value::Null` when no options were provided.
pub async fn add(
    state: &HandleState,
    inputs_json: serde_json::Value,
    dataset_name: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let inputs = marshal_inputs(&inputs_json)?;
    let tenant_id = opts_tenant(opts)?;

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    // Snapshot the dataset's existing data ids before the add so we can tell
    // newly-created items from duplicates (the pipeline returns both).
    let pre_existing = existing_data_ids(&svc, dataset_name, owner_id, tenant_id).await?;

    let returned = svc
        .add_pipeline
        .add(inputs, dataset_name, owner_id, tenant_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("add failed: {e}")))?;

    let (newly_added, duplicates) = partition_added(returned, &pre_existing);
    add_result_json(&newly_added, &duplicates, dataset_name)
}

/// Resolve the dataset by name, load its items, and run cognify.
///
/// `opts` may be `serde_json::Value::Null` when no options were provided.
pub async fn cognify(
    state: &HandleState,
    dataset_name: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let tenant_id = opts_tenant(opts)?;

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let dataset = resolve_dataset(&svc, dataset_name, owner_id, tenant_id).await?;

    let data_items = ops::datasets::get_dataset_data(&svc.database, dataset.id)
        .await
        .map_err(|e| {
            SdkError::Runtime(format!(
                "failed to load data for dataset '{dataset_name}': {e}"
            ))
        })?;

    let result = run_cognify_on_items(&svc, &dataset, owner_id, data_items, opts).await?;
    Ok(cognify_result_json(&result))
}

/// Sequential add → cognify on the freshly-added items.
///
/// If all inputs were duplicates, cognify is skipped entirely and a zeroed
/// `CogneeCognifyResult` is returned. `opts` may be `serde_json::Value::Null`
/// when no options were provided.
pub async fn add_and_cognify(
    state: &HandleState,
    inputs_json: serde_json::Value,
    dataset_name: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let inputs = marshal_inputs(&inputs_json)?;
    let tenant_id = opts_tenant(opts)?;

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let pre_existing = existing_data_ids(&svc, dataset_name, owner_id, tenant_id).await?;

    let returned = svc
        .add_pipeline
        .add(inputs, dataset_name, owner_id, tenant_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("add failed: {e}")))?;

    let (newly_added, duplicates) = partition_added(returned, &pre_existing);
    let add_json = add_result_json(&newly_added, &duplicates, dataset_name)?;

    if newly_added.is_empty() {
        // Everything was a duplicate: skip cognify, return a zeroed summary.
        return Ok(json!({
            "add": add_json,
            "cognify": cognify_result_json(&cognee::cognify::CognifyResult::empty()),
        }));
    }

    // Resolve the dataset for its id/tenant, but cognify the just-added items
    // directly (do not re-load the whole dataset).
    let dataset = resolve_dataset(&svc, dataset_name, owner_id, tenant_id).await?;
    let result = run_cognify_on_items(&svc, &dataset, owner_id, newly_added, opts).await?;

    Ok(json!({
        "add": add_json,
        "cognify": cognify_result_json(&result),
    }))
}
