//! Dataset ops: list, listData, has, status, empty, deleteData, deleteAll.

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::datasets;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::checked_handle;

/// `listDatasets(handle, future)` — no extra args.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_listDatasets<'l>(
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
            datasets::list_datasets(&state).await
        });
    })
}

/// `listData(handle, datasetId, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_listData<'l>(
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
            datasets::list_data(&state, &dataset_id).await
        });
    })
}

/// `hasData(handle, datasetId, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_hasData<'l>(
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
            datasets::has_data(&state, &dataset_id).await
        });
    })
}

/// `datasetStatus(handle, datasetIdsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_datasetStatus<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    ids_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let ids = match arg_json(env, &ids_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            datasets::dataset_status(&state, ids).await
        });
    })
}

/// `emptyDataset(handle, datasetId, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_emptyDataset<'l>(
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
            datasets::empty_dataset(&state, &dataset_id).await
        });
    })
}

/// `deleteData(handle, datasetId, dataId, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_deleteData<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    dataset_id: JString<'l>,
    data_id: JString<'l>,
    opts_json: JString<'l>,
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
        let data_id = match arg_string(env, &data_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            datasets::delete_data(&state, &dataset_id, &data_id, &opts).await
        });
    })
}

/// `deleteAllDatasets(handle, future)` — no extra args.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_deleteAllDatasets<'l>(
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
            datasets::delete_all_datasets(&state).await
        });
    })
}
