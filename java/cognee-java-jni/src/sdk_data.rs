//! Data ops: forget, update, prune_data, prune_system.

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::data;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::checked_handle;

/// `forget(handle, targetJson, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_forget<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    target_json: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let target = match arg_json(env, &target_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            data::forget(&state, target, &opts).await
        });
    })
}

/// `update(handle, dataId, newDataJson, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_update<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    data_id: JString<'l>,
    new_data_json: JString<'l>,
    dataset_name: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let data_id = match arg_string(env, &data_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let new_data = match arg_json(env, &new_data_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let dataset = match arg_string(env, &dataset_name) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            data::update(&state, &data_id, new_data, &dataset, &opts).await
        });
    })
}

/// `pruneData(handle, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_pruneData<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        spawn_future(env, &future, async move { data::prune_data(&state).await });
    })
}

/// `pruneSystem(handle, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_pruneSystem<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            data::prune_system(&state, &opts).await
        });
    })
}
