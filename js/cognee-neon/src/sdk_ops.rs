//! Phase 3 — pipeline ops: `add`, `cognify`, `add-and-cognify`.
//!
//! Each export follows the Phase-1 canonical pattern: clone the
//! `Arc<HandleState>` into `runtime().spawn`, obtain a `CogneeServices` via
//! `state.services().await?`, call the `cognee-lib` API with the bundled
//! `Arc<dyn …>` handles (exactly mirroring the CLI command builders — see
//! `crates/cli/src/commands/{add_and_cognify,cognify}.rs` — which are the
//! authoritative reference), marshal the result back to JS, and settle the
//! promise.
//!
//! ## Input marshalling
//!
//! `DataInput`'s derived serde is **externally tagged** (`{"Text":"…"}`),
//! which is **not** the `{ type, … }` discriminated union we expose to TS, so
//! inputs are marshalled explicitly by matching on `type` (no
//! `serde_json::from_value`). Supported variants: `text`, `file`, `url`,
//! `binary` (`name` required). `url`/`s3` are not wired end-to-end (documented
//! in `docs/not-implemented.md`); the recursive `DataItem` variant is out of
//! scope for v1.
//!
//! ## Result marshalling
//!
//! `Data` is `Serialize` and crosses back directly. `CognifyResult` is **not**
//! `Serialize` (it carries non-serialisable internal fields), so its JSON is
//! hand-built from the same `.len()` counts the CLI prints.

use std::sync::Arc;

use neon::prelude::*;
use serde_json::json;
use uuid::Uuid;

use cognee_lib::cognify::cognify;
use cognee_lib::database::{UserDb, ops};
use cognee_lib::models::{Data, Dataset};

use crate::errors::{SdkError, throw_sdk_error};
use crate::json::{cognify_result_json, js_to_value, marshal_inputs, parse_js};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;
use crate::services::CogneeServices;


// ---------------------------------------------------------------------------
// opts parsing (owner / tenant overrides; cognify config overrides).
// ---------------------------------------------------------------------------

/// Parse an optional `tenant` UUID string out of an `opts` object.
fn opts_tenant(opts: &serde_json::Value) -> Result<Option<Uuid>, SdkError> {
    match opts.get("tenant").and_then(|v| v.as_str()) {
        Some(s) => Uuid::parse_str(s)
            .map(Some)
            .map_err(|e| SdkError::Validation(format!("invalid `tenant` UUID: {e}"))),
        None => Ok(None),
    }
}

/// Build a per-call `CognifyConfig` by cloning the cached config and applying
/// any `opts` overrides on top (rather than mutating the cached one).
fn cognify_config_with_opts(
    svc: &CogneeServices,
    opts: &serde_json::Value,
) -> cognee_lib::cognify::CognifyConfig {
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
// Result marshalling.
// ---------------------------------------------------------------------------

/// Serialise the `add` outcome into the `AddResult` JSON shape.
///
/// `AddPipeline::add` returns one [`Data`] per input — **including duplicates**
/// (the duplicate branch returns the pre-existing row; see
/// `crates/ingestion/src/pipeline.rs`). We therefore cannot infer dedup from an
/// empty result the way the plan assumed. Instead the caller pre-scans the
/// dataset's existing data ids and partitions the returned items into
/// newly-added vs deduplicated by id membership (`Data` ids are content-addressed
/// UUID5, so a re-added identical payload yields the same id).
fn add_result_json(
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
        // `added` holds only the items that were newly created by this call;
        // `deduplicated` holds the items that already existed (an empty `added`
        // array means every submitted item was a duplicate).
        "added": added,
        "addedCount": newly_added.len(),
        "deduplicated": dup,
        "deduplicatedCount": duplicates.len(),
    }))
}

/// The set of data ids already attached to the named dataset, or an empty set
/// when the dataset does not exist yet. Used to distinguish newly-added items
/// from duplicates in the `add` result.
async fn existing_data_ids(
    svc: &CogneeServices,
    name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<std::collections::HashSet<Uuid>, SdkError> {
    let dataset = ops::datasets::get_dataset_by_name(&svc.database, name, owner_id, tenant_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("failed to resolve dataset '{name}': {e}")))?;
    let Some(dataset) = dataset else {
        return Ok(std::collections::HashSet::new());
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
/// call; `newly_added` holds the rest. The `addedCount === 0` ⇒ "everything was
/// a pre-existing duplicate" contract is exact. The only imprecision is *within*
/// a single batch: if the same payload is submitted twice in one `add` call,
/// both copies share a content-addressed id that was absent from the pre-scan,
/// so both land in `newly_added` (inflating `addedCount` by the in-batch dup
/// count). This never misclassifies a true pre-existing duplicate and never
/// reports a false empty `added`.
fn partition_added(
    returned: Vec<Data>,
    pre_existing: &std::collections::HashSet<Uuid>,
) -> (Vec<Data>, Vec<Data>) {
    returned
        .into_iter()
        .partition(|d| !pre_existing.contains(&d.id))
}

/// Resolve a dataset by name for the given owner/tenant, erroring if absent.
async fn resolve_dataset(
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
async fn best_effort_user_email(svc: &CogneeServices, owner_id: Uuid) -> Option<String> {
    let db = Arc::clone(&svc.database);
    let user_db = db.as_ref() as &dyn UserDb;
    user_db
        .get_user(owner_id)
        .await
        .ok()
        .flatten()
        .map(|u| u.email)
}

// ---------------------------------------------------------------------------
// Native exports.
// ---------------------------------------------------------------------------

/// `cogneeAdd(handle, dataInput, datasetName, opts?) -> Promise<AddResult>`
pub fn cognee_add(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let data_arg = cx.argument::<JsValue>(1)?;
    let inputs_json = js_to_value(&mut cx, data_arg)?;
    let dataset_name = cx.argument::<JsString>(2)?.value(&mut cx);
    let opts = match cx.argument_opt(3) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_add(&state, inputs_json, &dataset_name, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(value) => parse_js(&mut cx, &value.to_string()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// Run the add pipeline and return the `AddResult` JSON.
async fn run_add(
    state: &crate::sdk::HandleState,
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

/// `cogneeCognify(handle, dataset, opts?) -> Promise<CognifyResult>`
pub fn cognee_cognify(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let dataset_name = cx.argument::<JsString>(1)?.value(&mut cx);
    let opts = match cx.argument_opt(2) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_cognify(&state, &dataset_name, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(value) => parse_js(&mut cx, &value.to_string()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// Resolve the dataset by name, load its items, and run cognify.
async fn run_cognify(
    state: &crate::sdk::HandleState,
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

/// `cogneeAddAndCognify(handle, dataInput, datasetName, opts?) -> Promise<{ add, cognify }>`
///
/// One native call: add first, then cognify the just-added `Vec<Data>` directly
/// (mirroring `commands/add_and_cognify.rs`). If `add` returns an empty vec
/// (everything was a duplicate), cognify is skipped and a zeroed summary is
/// returned.
pub fn cognee_add_and_cognify(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let data_arg = cx.argument::<JsValue>(1)?;
    let inputs_json = js_to_value(&mut cx, data_arg)?;
    let dataset_name = cx.argument::<JsString>(2)?.value(&mut cx);
    let opts = match cx.argument_opt(3) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_add_and_cognify(&state, inputs_json, &dataset_name, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(value) => parse_js(&mut cx, &value.to_string()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// Sequential add → cognify on the freshly-added items.
async fn run_add_and_cognify(
    state: &crate::sdk::HandleState,
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
            "cognify": cognify_result_json(&cognee_lib::cognify::CognifyResult::empty()),
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

/// Shared cognify call: build the per-call config and invoke the 15-arg
/// `cognify(...)` free function in the exact positional order used by
/// `commands/cognify.rs`.
async fn run_cognify_on_items(
    svc: &CogneeServices,
    dataset: &Dataset,
    owner_id: Uuid,
    data_items: Vec<Data>,
    opts: &serde_json::Value,
) -> Result<cognee_lib::cognify::CognifyResult, SdkError> {
    if data_items.is_empty() {
        return Ok(cognee_lib::cognify::CognifyResult::empty());
    }

    let user_email = best_effort_user_email(svc, owner_id).await;
    let config = cognify_config_with_opts(svc, opts);

    cognify(
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
