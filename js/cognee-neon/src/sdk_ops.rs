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

use base64::Engine as _;
use neon::prelude::*;
use serde_json::json;
use uuid::Uuid;

use cognee_lib::cognify::cognify;
use cognee_lib::database::{UserDb, ops};
use cognee_lib::models::{Data, DataInput, Dataset};

use crate::errors::{SdkError, throw_sdk_error};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;
use crate::services::CogneeServices;

// ---------------------------------------------------------------------------
// JS <-> JSON helpers (JSON.stringify / JSON.parse round-trip).
// ---------------------------------------------------------------------------

/// Stringify a JS value via the global `JSON.stringify`.
fn stringify_js<'cx>(
    cx: &mut FunctionContext<'cx>,
    val: Handle<'cx, JsValue>,
) -> NeonResult<String> {
    let global = cx.global_object();
    let json: Handle<JsObject> = global.get(cx, "JSON")?;
    let stringify: Handle<JsFunction> = json.get(cx, "stringify")?;
    let result: Handle<JsValue> = stringify.call_with(cx).arg(val).apply(cx)?;
    let s = result.downcast_or_throw::<JsString, _>(cx)?;
    Ok(s.value(cx))
}

/// Parse a JSON string into a JS value via the global `JSON.parse`.
///
/// Generic over `Context` so it works both in a `FunctionContext` and inside a
/// promise's `settle_with` callback (which hands back a `TaskContext`).
fn parse_js<'cx, C: Context<'cx>>(cx: &mut C, json: &str) -> JsResult<'cx, JsValue> {
    let global = cx.global_object();
    let json_obj: Handle<JsObject> = global.get(cx, "JSON")?;
    let parse: Handle<JsFunction> = json_obj.get(cx, "parse")?;
    let arg = cx.string(json);
    parse.call_with(cx).arg(arg).apply(cx)
}

/// Convert a JS value into a `serde_json::Value`.
fn js_to_value<'cx>(
    cx: &mut FunctionContext<'cx>,
    val: Handle<'cx, JsValue>,
) -> NeonResult<serde_json::Value> {
    let json = stringify_js(cx, val)?;
    serde_json::from_str::<serde_json::Value>(&json)
        .or_else(|e| cx.throw_error(format!("invalid JSON value: {e}")))
}

// ---------------------------------------------------------------------------
// Input marshalling: discriminated union (`{ type, … }`) -> DataInput.
// ---------------------------------------------------------------------------

/// Marshal a single `{ type, … }` JSON object into a [`DataInput`].
///
/// `Buffer`/`Uint8Array` arguments stringify to a `{ "type": "Buffer", "data":
/// [..] }` object via `JSON.stringify`; `binary.bytes` therefore accepts either
/// that shape, a plain byte array, or a base64 string. `name` is required for
/// the `binary` variant (the Rust `Binary { data, name }` variant uses it for
/// MIME detection).
fn marshal_one(value: &serde_json::Value) -> Result<DataInput, SdkError> {
    let obj = value
        .as_object()
        .ok_or_else(|| SdkError::Validation("each data input must be an object".to_string()))?;
    let ty = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SdkError::Validation("data input is missing a string `type`".to_string()))?;

    match ty {
        "text" => {
            let text = obj.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
                SdkError::Validation("text input requires a `text` string".into())
            })?;
            Ok(DataInput::Text(text.to_string()))
        }
        "file" => {
            let path = obj.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                SdkError::Validation("file input requires a `path` string".into())
            })?;
            Ok(DataInput::FilePath(path.to_string()))
        }
        "url" => {
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SdkError::Validation("url input requires a `url` string".into()))?;
            Ok(DataInput::Url(url.to_string()))
        }
        "binary" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    SdkError::Validation(
                        "binary input requires a `name` string (used for MIME detection)".into(),
                    )
                })?
                .to_string();
            let data = marshal_bytes(obj.get("bytes"))?;
            Ok(DataInput::Binary { data, name })
        }
        "s3" => Err(SdkError::Unsupported(
            "s3 inputs are not yet supported (DataInput::S3Path is a stub)".into(),
        )),
        "dataItem" => Err(SdkError::Unsupported(
            "the recursive `dataItem` input variant is out of scope for v1".into(),
        )),
        other => Err(SdkError::Validation(format!(
            "unknown data input type `{other}`"
        ))),
    }
}

/// Decode `bytes` for a binary input: a base64 string, a plain JSON array of
/// byte values, or a Node `Buffer`/`Uint8Array` JSON projection
/// (`{ type: "Buffer", data: [..] }`).
fn marshal_bytes(bytes: Option<&serde_json::Value>) -> Result<Vec<u8>, SdkError> {
    let bytes =
        bytes.ok_or_else(|| SdkError::Validation("binary input requires `bytes`".to_string()))?;

    match bytes {
        serde_json::Value::String(s) => base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|e| SdkError::Validation(format!("invalid base64 `bytes`: {e}"))),
        serde_json::Value::Array(arr) => decode_byte_array(arr),
        serde_json::Value::Object(obj) => {
            // Node Buffer/Uint8Array stringifies to { type: "Buffer", data: [..] }.
            let data = obj.get("data").and_then(|v| v.as_array()).ok_or_else(|| {
                SdkError::Validation(
                    "binary `bytes` object must carry a numeric `data` array".to_string(),
                )
            })?;
            decode_byte_array(data)
        }
        _ => Err(SdkError::Validation(
            "binary `bytes` must be a base64 string, a byte array, or a Buffer".to_string(),
        )),
    }
}

/// Convert a JSON array of integers in `0..=255` into `Vec<u8>`.
fn decode_byte_array(arr: &[serde_json::Value]) -> Result<Vec<u8>, SdkError> {
    arr.iter()
        .map(|v| {
            v.as_u64()
                .filter(|n| *n <= 255)
                .map(|n| n as u8)
                .ok_or_else(|| {
                    SdkError::Validation("binary `bytes` array must contain bytes 0..=255".into())
                })
        })
        .collect()
}

/// Marshal the `dataInput` argument — a single item **or** an array of items —
/// into `Vec<DataInput>`.
fn marshal_inputs(value: &serde_json::Value) -> Result<Vec<DataInput>, SdkError> {
    match value {
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                return Err(SdkError::Validation(
                    "dataInput array must not be empty".to_string(),
                ));
            }
            items.iter().map(marshal_one).collect()
        }
        other => marshal_one(other).map(|input| vec![input]),
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

/// Hand-build the `CognifyResult` JSON from its `.len()` counts (it is **not**
/// `Serialize`).
fn cognify_result_json(result: &cognee_lib::cognify::CognifyResult) -> serde_json::Value {
    json!({
        "chunks": result.chunks.len(),
        "entities": result.entities.len(),
        "edges": result.edges.len(),
        "summaries": result.summaries.len(),
        "embeddings": result.embeddings.len(),
        "alreadyCompleted": result.already_completed,
        "priorPipelineRunId": result.prior_pipeline_run_id.map(|id| id.to_string()),
    })
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
