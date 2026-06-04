//! Phase 5 — DatasetManager ops (#12).
//!
//! All exports follow the canonical pattern:
//!   `Arc::clone(&handle.state)` → `runtime().spawn` → `state.services().await?`
//!   → call DatasetManager API → `settle_with`.
//!
//! `DatasetManager::new(db: Arc<dyn DatasetDb>)` — `DatabaseConnection`
//! implements `DatasetDb` via blanket; cast per-call via
//! `Arc::clone(&svc.database) as Arc<dyn DatasetDb>`.
//!
//! ## Serde notes
//! - `Dataset` IS `Serialize` → direct serde.
//! - `Data` IS `Serialize` → direct serde.
//! - `DeleteResult` IS `Serialize` → direct serde.
//! - `PipelineRunStatus` IS `Serialize` but `HashMap<Uuid, PipelineRunStatus>`
//!   has non-string JSON keys → convert to `HashMap<String, PipelineRunStatus>`.

use std::sync::Arc;

use neon::prelude::*;
use uuid::Uuid;

use cognee_lib::api::{DatasetDb, DatasetManager};
use cognee_lib::delete::DeleteMode;

use crate::errors::{SdkError, throw_sdk_error};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;
use crate::sdk_memory::{js_to_value, parse_js, read_opts};

// ---------------------------------------------------------------------------
// cogneeListDatasets
// ---------------------------------------------------------------------------

/// `cogneeListDatasets(handle) -> Promise<Dataset[]>`
pub fn cognee_list_datasets(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_list_datasets(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_list_datasets(state: &crate::sdk::HandleState) -> Result<String, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let datasets = mgr
        .list_datasets(owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("list_datasets failed: {e}")))?;

    serde_json::to_string(&datasets)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize datasets: {e}")))
}

// ---------------------------------------------------------------------------
// cogneeListData
// ---------------------------------------------------------------------------

/// `cogneeListData(handle, datasetId) -> Promise<Data[]>`
pub fn cognee_list_data(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_list_data(&state, &dataset_id_str).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_list_data(
    state: &crate::sdk::HandleState,
    dataset_id_str: &str,
) -> Result<String, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let items = mgr
        .list_data(dataset_id, owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("list_data failed: {e}")))?;

    serde_json::to_string(&items)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize data items: {e}")))
}

// ---------------------------------------------------------------------------
// cogneeHasData
// ---------------------------------------------------------------------------

/// `cogneeHasData(handle, datasetId) -> Promise<boolean>`
pub fn cognee_has_data(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_has_data(&state, &dataset_id_str).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(has) => Ok(cx.boolean(has)),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_has_data(
    state: &crate::sdk::HandleState,
    dataset_id_str: &str,
) -> Result<bool, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    mgr.has_data(dataset_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("has_data failed: {e}")))
}

// ---------------------------------------------------------------------------
// cogneeDatasetStatus
// ---------------------------------------------------------------------------

/// `cogneeDatasetStatus(handle, datasetIds) -> Promise<Record<string, string>>`
///
/// `datasetIds` is a JSON array of UUID strings.
/// Returns `{ [uuidStr]: "INITIATED" | "STARTED" | "COMPLETED" | "ERRORED" }`.
pub fn cognee_dataset_status(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let ids_arg = cx.argument::<JsValue>(1)?;
    let ids_json = js_to_value(&mut cx, ids_arg)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_dataset_status(&state, ids_json).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_dataset_status(
    state: &crate::sdk::HandleState,
    ids_json: serde_json::Value,
) -> Result<String, SdkError> {
    let ids: Vec<Uuid> = ids_json
        .as_array()
        .ok_or_else(|| SdkError::Validation("datasetIds must be an array".to_string()))?
        .iter()
        .map(|v| {
            v.as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| {
                    SdkError::Validation("each datasetId must be a valid UUID string".to_string())
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let svc = state.services().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let statuses = mgr
        .get_status(&ids)
        .await
        .map_err(|e| SdkError::Runtime(format!("get_status failed: {e}")))?;

    // Convert HashMap<Uuid, PipelineRunStatus> → HashMap<String, PipelineRunStatus>
    // so the JSON keys are valid strings.
    let string_keyed: std::collections::HashMap<String, _> = statuses
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

    serde_json::to_string(&string_keyed)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize status map: {e}")))
}

// ---------------------------------------------------------------------------
// cogneeEmptyDataset
// ---------------------------------------------------------------------------

/// `cogneeEmptyDataset(handle, datasetId) -> Promise<DeleteResult>`
pub fn cognee_empty_dataset(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_empty_dataset(&state, &dataset_id_str).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_empty_dataset(
    state: &crate::sdk::HandleState,
    dataset_id_str: &str,
) -> Result<String, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let result = mgr
        .empty_dataset(dataset_id, owner_id, svc.delete_service.as_ref())
        .await
        .map_err(|e| SdkError::Runtime(format!("empty_dataset failed: {e}")))?;

    serde_json::to_string(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult: {e}")))
}

// ---------------------------------------------------------------------------
// cogneeDeleteData
// ---------------------------------------------------------------------------

/// `cogneeDeleteData(handle, datasetId, dataId, opts?) -> Promise<DeleteResult>`
///
/// opts: `{ softDelete?: boolean, deleteDatasetIfEmpty?: boolean }`
pub fn cognee_delete_data(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);
    let data_id_str = cx.argument::<JsString>(2)?.value(&mut cx);
    let opts = read_opts(&mut cx, 3)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_delete_data(&state, &dataset_id_str, &data_id_str, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_delete_data(
    state: &crate::sdk::HandleState,
    dataset_id_str: &str,
    data_id_str: &str,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let data_id = Uuid::parse_str(data_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid data id UUID: {e}")))?;
    let soft_delete = opts
        .get("softDelete")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let delete_dataset_if_empty = opts
        .get("deleteDatasetIfEmpty")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mode = if soft_delete {
        DeleteMode::Soft
    } else {
        DeleteMode::Hard
    };

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let result = mgr
        .delete_data(
            dataset_id,
            data_id,
            owner_id,
            mode,
            delete_dataset_if_empty,
            svc.delete_service.as_ref(),
        )
        .await
        .map_err(|e| SdkError::Runtime(format!("delete_data failed: {e}")))?;

    serde_json::to_string(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult: {e}")))
}

// ---------------------------------------------------------------------------
// cogneeDeleteAllDatasets
// ---------------------------------------------------------------------------

/// `cogneeDeleteAllDatasets(handle) -> Promise<DeleteResult[]>`
pub fn cognee_delete_all_datasets(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_delete_all_datasets(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_delete_all_datasets(state: &crate::sdk::HandleState) -> Result<String, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let results = mgr
        .delete_all(owner_id, svc.delete_service.as_ref())
        .await
        .map_err(|e| SdkError::Runtime(format!("delete_all failed: {e}")))?;

    serde_json::to_string(&results)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult[]: {e}")))
}
