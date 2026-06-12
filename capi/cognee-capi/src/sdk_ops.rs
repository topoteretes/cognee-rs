//! Phase 4 — core pipeline ops: `add`, `cognify`, `add_and_cognify`.
//!
//! Each export follows the Phase-2 canonical pattern: clone the
//! `Arc<HandleState>` into `spawn_sdk_op`, obtain a `CogneeServices` via
//! `state.services().await?`, call the `cognee-lib` API with the bundled
//! `Arc<dyn …>` handles, marshal the result back to JSON, and deliver it
//! through the callback.
//!
//! ## Input marshalling
//!
//! `DataInput`'s derived serde is **externally tagged** (`{"Text":"…"}`),
//! which is **not** the `{ type, … }` discriminated union we expose to C, so
//! inputs are marshalled explicitly by matching on `type` (no
//! `serde_json::from_value`). Supported variants: `text`, `file`, `url`,
//! `binary` (`name` required). `s3` and recursive `dataItem` return
//! `CG_ERR_UNSUPPORTED` (15).
//!
//! ## Result marshalling
//!
//! `Data` is `Serialize` and crosses back directly. `CognifyResult` is **not**
//! `Serialize` (it carries non-serialisable internal fields), so its JSON is
//! hand-built via the shared `cognee_bindings_common::wire::cognify_result_json`
//! helper.
//!
//! ## add-specific helpers
//!
//! `add_result_json`, `partition_added`, `existing_data_ids`, `resolve_dataset`,
//! `best_effort_user_email`, `opts_tenant`, and `cognify_config_with_opts` are
//! ported verbatim from `js/cognee-neon/src/sdk_ops.rs` (per phase-4 spec: they
//! are NOT in `bindings-common`).

use std::ffi::{CStr, CString, c_char};
use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use cognee_bindings_common::wire::{cognify_result_json, marshal_inputs};
use cognee_bindings_common::{CogneeServices, HandleState, SdkError};
use cognee_lib::cognify::cognify;
use cognee_lib::database::{UserDb, ops};
use cognee_lib::models::{Data, Dataset};

use crate::error::CgErrorCode;
use crate::runtime::global_runtime;
use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};

// ---------------------------------------------------------------------------
// UTF-8 helper: parse a raw C string, delivering errors via the deferred
// callback pattern (R1).  Returns `None` if parsing fails (caller should
// return immediately).  `ud_raw` is the `user_data as usize` stash.
// ---------------------------------------------------------------------------

/// Attempt to parse a (non-null) C string pointer into an owned `String`.
///
/// On success returns `Some(owned)`.  On UTF-8 error, fires the callback on a
/// spawned thread (R1) and returns `None` — the caller must return immediately.
///
/// `ud_raw` carries `user_data as usize` so the closure is `Send`.
fn parse_c_str_or_fire(
    ptr: *const c_char,
    field_name: &'static str,
    callback: CgSdkResultCallback,
    ud_raw: usize,
) -> Option<String> {
    // Guard against null pointers for required (non-optional) string params.
    if ptr.is_null() {
        let rt = global_runtime()?;
        let msg_text = format!("{field_name} must not be null");
        rt.handle().spawn(async move {
            let msg = CString::new(msg_text).unwrap_or_else(|_| {
                CString::new("argument must not be null").expect("literal has no null bytes")
            });
            // SAFETY: ud_raw was a valid *mut c_void at capture time.
            unsafe {
                callback(
                    CgErrorCode::NullPointer,
                    std::ptr::null(),
                    msg.as_ptr(),
                    ud_raw as *mut std::ffi::c_void,
                )
            };
        });
        return None;
    }
    match unsafe { CStr::from_ptr(ptr) }.to_str() {
        Ok(s) => Some(s.to_owned()),
        Err(_) => {
            // Deliver via a spawned OS thread to honour R1.
            let rt = global_runtime()?;
            let msg_text = format!("{field_name} is not valid UTF-8");
            rt.handle().spawn(async move {
                let msg = CString::new(msg_text).unwrap_or_else(|_| {
                    CString::new("argument is not valid UTF-8").expect("literal has no null bytes")
                });
                // SAFETY: ud_raw was a valid *mut c_void at capture time.
                unsafe {
                    callback(
                        CgErrorCode::Utf8Error,
                        std::ptr::null(),
                        msg.as_ptr(),
                        ud_raw as *mut std::ffi::c_void,
                    )
                };
            });
            None
        }
    }
}

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
/// (the duplicate branch returns the pre-existing row). We therefore cannot
/// infer dedup from an empty result the way the plan assumed. Instead the caller
/// pre-scans the dataset's existing data ids and partitions the returned items
/// into newly-added vs deduplicated by id membership (`Data` ids are
/// content-addressed UUID5, so a re-added identical payload yields the same id).
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
/// call; `newly_added` holds the rest.
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
// Core async logic (SDK-tier agnostic helpers).
// ---------------------------------------------------------------------------

/// Run the add pipeline and return the `AddResult` JSON value.
async fn run_add(
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
async fn run_cognify(
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
async fn run_add_and_cognify(
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

// ---------------------------------------------------------------------------
// C-exported functions.
// ---------------------------------------------------------------------------

/// Add data to the named dataset.
///
/// `inputs_json` is a `CogneeDataInput` object **or** array (see wire shapes
/// in the header).  `dataset_name` is the target dataset name (will be
/// auto-created if absent).  `opts_json` may be `NULL` or a JSON object with
/// an optional `"tenant"` key (UUID string).
///
/// Async (D4, R1): the callback fires on a tokio worker thread, never
/// synchronously from this call.
///
/// On success `result_json` is a `CogneeAddResult` JSON object:
/// `{"datasetName":"…","added":[…],"addedCount":N,"deduplicated":[…],"deduplicatedCount":M}`
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `inputs_json` and
/// `dataset_name` must be valid null-terminated UTF-8 strings.
/// `opts_json` may be NULL.  `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_add(
    sdk: *const CgSdk,
    inputs_json: *const c_char,
    dataset_name: *const c_char,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    // Stash user_data as usize so error-path closures are Send (same pattern
    // as cg_sdk_warm / cg_sdk_owner_id in sdk.rs).
    let ud_raw = user_data as usize;

    // Parse string arguments before spawning (pointers are only valid during
    // this call).
    let inputs_str = match parse_c_str_or_fire(inputs_json, "inputs_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let dataset_str = match parse_c_str_or_fire(dataset_name, "dataset_name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
            Some(s) => Some(s),
            None => return,
        }
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        // Parse inputs JSON.
        let inputs_val: serde_json::Value = serde_json::from_str(&inputs_str)
            .map_err(|e| SdkError::Validation(format!("inputs_json parse error: {e}")))?;
        // Parse opts JSON (default to null if absent).
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        run_add(&state, inputs_val, &dataset_str, &opts_val).await
    });
}

/// Run the cognify pipeline on an existing dataset.
///
/// `dataset_name` is the name of a dataset that must already exist (created by
/// a prior `cg_sdk_add` call).  `opts_json` may be `NULL` or a JSON object with
/// optional keys: `tenant` (UUID string), `chunkSize` (integer), `chunkOverlap`
/// (integer), `summarization` (boolean), `temporalCognify` (boolean),
/// `triplet` (boolean).
///
/// Async (D4, R1): the callback fires on a tokio worker thread.
///
/// On success `result_json` is a `CogneeCognifyResult` JSON object:
/// `{"chunks":N,"entities":N,"edges":N,"summaries":N,"embeddings":N,"alreadyCompleted":false,"priorPipelineRunId":null}`
///
/// # Safety
/// `sdk` and `dataset_name` must be valid non-null pointers to null-terminated
/// UTF-8 strings.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_cognify(
    sdk: *const CgSdk,
    dataset_name: *const c_char,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let dataset_str = match parse_c_str_or_fire(dataset_name, "dataset_name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
            Some(s) => Some(s),
            None => return,
        }
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        run_cognify(&state, &dataset_str, &opts_val).await
    });
}

/// Add data and immediately cognify — a single combined op.
///
/// Equivalent to `cg_sdk_add` followed by `cg_sdk_cognify`, but with the
/// optimisation that cognify operates only on the **newly-added** items (items
/// that were already present are skipped).  If all inputs were duplicates,
/// cognify is skipped entirely and a zeroed `CogneeCognifyResult` is returned.
///
/// On success `result_json` is:
/// `{"add":CogneeAddResult,"cognify":CogneeCognifyResult}`
///
/// # Safety
/// Same as `cg_sdk_add`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_add_and_cognify(
    sdk: *const CgSdk,
    inputs_json: *const c_char,
    dataset_name: *const c_char,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let inputs_str = match parse_c_str_or_fire(inputs_json, "inputs_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let dataset_str = match parse_c_str_or_fire(dataset_name, "dataset_name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
            Some(s) => Some(s),
            None => return,
        }
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let inputs_val: serde_json::Value = serde_json::from_str(&inputs_str)
            .map_err(|e| SdkError::Validation(format!("inputs_json parse error: {e}")))?;
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        run_add_and_cognify(&state, inputs_val, &dataset_str, &opts_val).await
    });
}
