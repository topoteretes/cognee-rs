//! Shared async data-management operations: `forget`, `update`, `prune_data`,
//! `prune_system`.
//!
//! These functions contain the pure-Rust async logic that is shared between
//! every language binding surface (C API, Neon JS, Python). Each function takes
//! a [`HandleState`] reference and `serde_json::Value` arguments, performs the
//! operation against the underlying cognee-lib APIs, and returns a
//! `serde_json::Value` result (or an [`SdkError`]).
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.
//!
//! ## Wire shapes
//!
//! ### `forget` target (camelCase keys required)
//!
//! ```json
//! {"kind":"item","dataId":"<uuid>","dataset":{"name":"…"}|{"id":"<uuid>"}}
//! {"kind":"dataset","dataset":{"name":"…"}|{"id":"<uuid>"}}
//! {"kind":"all"}
//! ```
//!
//! ### `prune_system` opts (camelCase keys required, all optional)
//!
//! ```json
//! {"pruneGraph":bool,"pruneVector":bool,"pruneMetadata":bool,"pruneCache":bool}
//! ```
//!
//! ### Result shapes (all camelCase)
//!
//! `forget`:       `{"target":"…","deleteResult":{…}}`
//! `update`:       `{"deletedDataId":"…","deleteResult":{…},"newData":[…],"cognifyResult":{…}}`
//! `prune_data`:   `null`
//! `prune_system`: `{"dataPruned":bool,"graphPruned":bool,"vectorPruned":bool,
//!                   "metadataPruned":bool,"cachePruned":bool}`

use serde_json::json;
use uuid::Uuid;

use cognee_lib::add::generate_dataset_id;
use cognee_lib::api::{
    DatasetRef, ForgetTarget, PruneTarget, forget as cognee_forget,
    prune_data as cognee_prune_data, prune_system as cognee_prune_system, update as cognee_update,
};
use cognee_lib::database::IngestDb;

use crate::wire::{cognify_result_json, marshal_inputs};
use crate::{HandleState, SdkError};

// ---------------------------------------------------------------------------
// opts helpers
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

// ---------------------------------------------------------------------------
// ForgetTarget marshalling (shared by all binding surfaces).
// ---------------------------------------------------------------------------

/// Marshal a discriminated-union JSON value into a [`ForgetTarget`].
///
/// The `kind` field selects the variant; wire keys must already be camelCase
/// (`"dataId"`, not `"data_id"`).
pub fn marshal_forget_target(value: &serde_json::Value) -> Result<ForgetTarget, SdkError> {
    let obj = value
        .as_object()
        .ok_or_else(|| SdkError::Validation("forget target must be an object".to_string()))?;
    let kind = obj.get("kind").and_then(|v| v.as_str()).ok_or_else(|| {
        SdkError::Validation("forget target is missing a string `kind`".to_string())
    })?;

    match kind {
        "item" => {
            let data_id_str = obj.get("dataId").and_then(|v| v.as_str()).ok_or_else(|| {
                SdkError::Validation("item target requires a `dataId` UUID string".to_string())
            })?;
            let data_id = Uuid::parse_str(data_id_str)
                .map_err(|e| SdkError::Validation(format!("invalid `dataId` UUID: {e}")))?;
            let dataset = marshal_dataset_ref(obj.get("dataset"))?;
            Ok(ForgetTarget::Item { data_id, dataset })
        }
        "dataset" => {
            let dataset = marshal_dataset_ref(obj.get("dataset"))?;
            Ok(ForgetTarget::Dataset { dataset })
        }
        "all" => Ok(ForgetTarget::All),
        other => Err(SdkError::Validation(format!(
            "unknown forget target kind `{other}`. Valid: item, dataset, all"
        ))),
    }
}

/// Marshal `{"name":"…"}` or `{"id":"…"}` into a [`DatasetRef`].
pub fn marshal_dataset_ref(value: Option<&serde_json::Value>) -> Result<DatasetRef, SdkError> {
    let obj = value
        .and_then(|v| v.as_object())
        .ok_or_else(|| SdkError::Validation("dataset reference must be an object".to_string()))?;

    if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
        return Ok(DatasetRef::Name(name.to_string()));
    }
    if let Some(id_str) = obj.get("id").and_then(|v| v.as_str()) {
        let id = Uuid::parse_str(id_str)
            .map_err(|e| SdkError::Validation(format!("invalid dataset `id` UUID: {e}")))?;
        return Ok(DatasetRef::Id(id));
    }
    Err(SdkError::Validation(
        "dataset reference must have either `name` or `id`".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Public top-level data operations.
// ---------------------------------------------------------------------------

/// Delete data from the knowledge graph.
///
/// `target_json` is a discriminated union on `"kind"` (camelCase keys).
/// `opts` may be `serde_json::Value::Null` when no options were provided.
///
/// Returns `{"target":"…","deleteResult":{…}}`.
pub async fn forget(
    state: &HandleState,
    target_json: serde_json::Value,
    _opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    // _opts is reserved for future tenant support — same decision as capi/neon.
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let target = marshal_forget_target(&target_json)?;

    let db_ref: &dyn IngestDb = svc.database.as_ref();

    let result = cognee_forget(target, owner_id, svc.delete_service.as_ref(), Some(db_ref))
        .await
        .map_err(|e| SdkError::Runtime(format!("forget failed: {e}")))?;

    let delete_result_json = serde_json::to_value(&result.delete_result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult: {e}")))?;

    Ok(json!({
        "target": result.target,
        "deleteResult": delete_result_json,
    }))
}

/// Replace a data item with new content and re-cognify.
///
/// `data_id_str` is a UUID string. `new_data_json` is a `{ type, … }` object
/// or array. `opts` may be `serde_json::Value::Null`.
///
/// Optional `opts` fields (camelCase):
/// - `"datasetId"`: explicit dataset UUID string (falls back to name-derived ID)
/// - `"tenant"`: tenant UUID string
/// - `"nodeSet"`: JSON array of node identifier strings
/// - `"preferredLoaders"`: JSON object mapping MIME type / extension → loader name
/// - `"incrementalLoading"`: bool (default `false`)
///
/// Returns `{"deletedDataId":"…","deleteResult":{…},"newData":[…],"cognifyResult":{…}}`.
pub async fn update(
    state: &HandleState,
    data_id_str: &str,
    new_data_json: serde_json::Value,
    dataset_name: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let data_id = Uuid::parse_str(data_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid `dataId` UUID: {e}")))?;
    let tenant_id = opts_tenant(opts)?;
    let new_data = marshal_inputs(&new_data_json)?;

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    // Resolve `dataset_id`: prefer an explicit UUID from opts; fall back to
    // the deterministic name-derived ID used by all other bindings.
    let dataset_id = opts
        .get("datasetId")
        .and_then(|v| v.as_str())
        .map(|s| {
            Uuid::parse_str(s)
                .map_err(|e| SdkError::Validation(format!("invalid `datasetId` UUID: {e}")))
        })
        .transpose()?
        .unwrap_or_else(|| generate_dataset_id(dataset_name, owner_id, tenant_id));

    // Parse optional extra params from opts.
    let node_set: Option<Vec<String>> = opts.get("nodeSet").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect()
    });
    let preferred_loaders: Option<std::collections::HashMap<String, String>> = opts
        .get("preferredLoaders")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        });
    let incremental_loading = opts
        .get("incrementalLoading")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let result = cognee_update(
        data_id,
        new_data,
        dataset_id,
        dataset_name,
        owner_id,
        tenant_id,
        node_set,
        preferred_loaders,
        incremental_loading,
        None, // acl_db: callers that want ACL enforcement should use the HTTP handler
        svc.delete_service.as_ref(),
        svc.add_pipeline.as_ref(),
        svc.llm.clone(),
        svc.storage.clone(),
        svc.graph_db.clone(),
        svc.vector_db.clone(),
        svc.embedding_engine.clone(),
        Some(svc.database.clone()),
        svc.ontology_resolver.clone(),
        &svc.cognify_config,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("update failed: {e}")))?;

    let delete_result_json = serde_json::to_value(&result.delete_result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize delete_result: {e}")))?;
    let new_data_val = serde_json::to_value(&result.new_data)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize new_data: {e}")))?;
    let cognify_result_val = result
        .cognify_result
        .as_ref()
        .map(cognify_result_json)
        .unwrap_or(serde_json::Value::Null);

    Ok(json!({
        "deletedDataId": result.deleted_data_id.to_string(),
        "deleteResult": delete_result_json,
        "newData": new_data_val,
        "cognifyResult": cognify_result_val,
    }))
}

/// Remove all files from data storage.
///
/// Returns `serde_json::Value::Null` on success (void op, wire shape "null").
pub async fn prune_data(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    cognee_prune_data(svc.storage.as_ref())
        .await
        .map_err(|e| SdkError::Runtime(format!("prune_data failed: {e}")))?;
    // Void op — callers that expose this as a Python None or JS undefined
    // should check for Null and convert accordingly.
    Ok(serde_json::Value::Null)
}

/// Selective backend cleanup (graph, vector, session cache, optional metadata).
///
/// `opts` may be `serde_json::Value::Null`; all fields default to Python's
/// `PruneTarget::default_system()` values when absent.
///
/// Returns `{"dataPruned":bool,"graphPruned":bool,"vectorPruned":bool,
///            "metadataPruned":bool,"cachePruned":bool}`.
pub async fn prune_system(
    state: &HandleState,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;

    let defaults = PruneTarget::default_system();
    let target = PruneTarget {
        graph: opts
            .get("pruneGraph")
            .and_then(|v| v.as_bool())
            .unwrap_or(defaults.graph),
        vector: opts
            .get("pruneVector")
            .and_then(|v| v.as_bool())
            .unwrap_or(defaults.vector),
        metadata: opts
            .get("pruneMetadata")
            .and_then(|v| v.as_bool())
            .unwrap_or(defaults.metadata),
        cache: opts
            .get("pruneCache")
            .and_then(|v| v.as_bool())
            .unwrap_or(defaults.cache),
    };

    let result = cognee_prune_system(
        &target,
        Some(svc.graph_db.as_ref()),
        Some(svc.vector_db.as_ref()),
        Some(svc.session_store.as_ref()),
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("prune_system failed: {e}")))?;

    Ok(json!({
        "dataPruned": result.data_pruned,
        "graphPruned": result.graph_pruned,
        "vectorPruned": result.vector_pruned,
        "metadataPruned": result.metadata_pruned,
        "cachePruned": result.cache_pruned,
    }))
}
