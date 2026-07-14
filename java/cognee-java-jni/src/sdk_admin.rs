//! Admin/user/notebook ops.

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jlong};

use cognee_bindings_common::ops::admin;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::checked_handle;

/// `getOrCreateDefaultUser(handle, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_getOrCreateDefaultUser<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        spawn_future(env, &future, async move {
            admin::run_get_or_create_default_user(&state).await
        });
    })
}

/// `resetPipelineRunStatus(handle, datasetId, pipelineName, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_resetPipelineRunStatus<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    dataset_id: JString<'l>,
    pipeline_name: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let dataset_id = match arg_string(env, &dataset_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let pipeline_name = match arg_string(env, &pipeline_name) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            admin::run_reset_pipeline_run_status(&state, &dataset_id, &pipeline_name).await
        });
    })
}

/// `resetDatasetPipelineRunStatus(handle, datasetId, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_resetDatasetPipelineRunStatus<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    dataset_id: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let dataset_id = match arg_string(env, &dataset_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            admin::run_reset_dataset_pipeline_run_status(&state, &dataset_id).await
        });
    })
}

/// `listNotebooks(handle, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_listNotebooks<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        spawn_future(env, &future, async move {
            admin::run_list_notebooks(&state).await
        });
    })
}

/// `createNotebook(handle, name, cellsJson, deletable, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_createNotebook<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    name: JString<'l>,
    cells_json: JString<'l>,
    deletable: jboolean,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let name = match arg_string(env, &name) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        // Default absent cells to an empty JSON array (run_create_notebook wants an array).
        let cells = match arg_json(env, &cells_json) {
            Ok(serde_json::Value::Null) => serde_json::Value::Array(vec![]),
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let deletable = deletable != 0;
        spawn_future(env, &future, async move {
            admin::run_create_notebook(&state, name, cells, deletable).await
        });
    })
}

/// `updateNotebook(handle, id, patchJson, future)` — patch `{name?, cells?}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_updateNotebook<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    id: JString<'l>,
    patch_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let id = match arg_string(env, &id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let patch = match arg_json(env, &patch_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            admin::run_update_notebook(&state, &id, patch).await
        });
    })
}

/// `deleteNotebook(handle, id, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_deleteNotebook<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    id: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let id = match arg_string(env, &id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            admin::run_delete_notebook(&state, &id).await
        });
    })
}
