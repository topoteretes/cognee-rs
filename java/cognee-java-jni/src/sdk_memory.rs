//! Memory ops: remember, remember_entry, memify, improve.

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::memory;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::checked_handle;

/// `remember(handle, inputsJson, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_remember<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    inputs_json: JString<'l>,
    dataset_name: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let inputs = match arg_json(env, &inputs_json) {
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
            memory::run_remember(&state, inputs, &dataset, &opts).await
        });
    })
}

/// `rememberEntry(handle, entryJson, datasetName, sessionId, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_rememberEntry<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    entry_json: JString<'l>,
    dataset_name: JString<'l>,
    session_id: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let entry = match arg_json(env, &entry_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let dataset = match arg_string(env, &dataset_name) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let session = match arg_string(env, &session_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            memory::run_remember_entry(&state, entry, &dataset, &session, &opts).await
        });
    })
}

/// `memify(handle, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_memify<'l>(
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
            memory::run_memify_op(&state, &opts).await
        });
    })
}

/// `improve(handle, optsJson, future)` — opts must contain `datasetName`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_improve<'l>(
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
            memory::run_improve(&state, &opts).await
        });
    })
}
