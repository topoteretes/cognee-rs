//! Shared async dataset-management operations: `list_datasets`, `list_data`,
//! `has_data`, `dataset_status`, `empty_dataset`, `delete_data`,
//! `delete_all_datasets`.
//!
//! These functions contain the pure-Rust async logic shared between every
//! language binding surface (C API, Neon JS, Python). Each function takes a
//! [`HandleState`] reference plus typed arguments, performs the operation
//! against `DatasetManager` from `cognee-lib`, and returns a
//! `serde_json::Value` result (or an [`SdkError`]).
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.
//!
//! ## Serde notes
//! - `Dataset`, `Data`, `DeleteResult` ARE `Serialize` → direct serde.
//! - `PipelineRunStatus` IS `Serialize` but `HashMap<Uuid, PipelineRunStatus>`
//!   has non-string JSON keys → convert to `HashMap<String, _>` before
//!   serialising.
//! - `has_data` returns `serde_json::Value::Bool(b)` (strict JSON bool).

use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use cognee_lib::api::{DatasetDb, DatasetManager};
use cognee_lib::delete::DeleteMode;

use crate::{HandleState, SdkError};

/// List all datasets for the current owner.
///
/// Returns a JSON array of `Dataset` objects.
pub async fn list_datasets(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let datasets = mgr
        .list_datasets(owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("list_datasets failed: {e}")))?;

    serde_json::to_value(&datasets)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize datasets: {e}")))
}

/// List all data items in a dataset.
///
/// `dataset_id_str` is a UUID string. Returns a JSON array of `Data` objects.
pub async fn list_data(
    state: &HandleState,
    dataset_id_str: &str,
) -> Result<serde_json::Value, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let items = mgr
        .list_data(dataset_id, owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("list_data failed: {e}")))?;

    serde_json::to_value(&items)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize data items: {e}")))
}

/// Check whether a dataset has any data.
///
/// `dataset_id_str` is a UUID string. Returns a JSON bool (`true` / `false`).
/// A non-existent dataset UUID returns `false` (COUNT=0, no error).
pub async fn has_data(
    state: &HandleState,
    dataset_id_str: &str,
) -> Result<serde_json::Value, SdkError> {
    let dataset_id = match Uuid::parse_str(dataset_id_str) {
        Ok(id) => id,
        // An unparseable id can never match a real dataset (all dataset ids are
        // UUIDs), so it is simply "not present" → false. This matches the
        // documented contract above and parity with an unknown-but-valid UUID.
        Err(_) => return Ok(serde_json::Value::Bool(false)),
    };
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let has = mgr
        .has_data(dataset_id, owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("has_data failed: {e}")))?;

    Ok(serde_json::Value::Bool(has))
}

/// Get the pipeline run status for a list of dataset UUIDs.
///
/// `ids_json` is a `serde_json::Value::Array` of UUID strings.
/// Returns a JSON object: `{"<uuid>": "<status-string>", …}`.
pub async fn dataset_status(
    state: &HandleState,
    ids_json: serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let ids: Vec<Uuid> = ids_json
        .as_array()
        .ok_or_else(|| SdkError::Validation("datasetIds must be a JSON array".to_string()))?
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

    // Convert HashMap<Uuid, HashMap<String, PipelineRunStatus>> →
    // HashMap<String, HashMap<String, _>> so outer JSON keys are valid strings.
    let string_keyed: HashMap<String, _> = statuses
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

    serde_json::to_value(&string_keyed)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize status map: {e}")))
}

/// Remove all data items from a dataset and delete the dataset record.
///
/// `dataset_id_str` is a UUID string. Returns a `DeleteResult` JSON object.
pub async fn empty_dataset(
    state: &HandleState,
    dataset_id_str: &str,
) -> Result<serde_json::Value, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let result = mgr
        .empty_dataset(dataset_id, owner_id, svc.delete_service.as_ref())
        .await
        .map_err(|e| SdkError::Runtime(format!("empty_dataset failed: {e}")))?;

    serde_json::to_value(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult: {e}")))
}

/// Delete a specific data item from a dataset.
///
/// `dataset_id_str` and `data_id_str` are UUID strings. `opts` is a JSON object
/// with optional boolean fields (camelCase keys):
/// - `"softDelete"` (default `false`)
/// - `"deleteDatasetIfEmpty"` (default `false`)
///
/// Returns a `DeleteResult` JSON object.
pub async fn delete_data(
    state: &HandleState,
    dataset_id_str: &str,
    data_id_str: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
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

    serde_json::to_value(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult: {e}")))
}

/// Delete all datasets for the current owner.
///
/// Returns a JSON array of `DeleteResult` objects.
pub async fn delete_all_datasets(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let results = mgr
        .delete_all(owner_id, svc.delete_service.as_ref())
        .await
        .map_err(|e| SdkError::Runtime(format!("delete_all failed: {e}")))?;

    serde_json::to_value(&results)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult[]: {e}")))
}
