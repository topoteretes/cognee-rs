//! Pipeline ops: add, cognify, add-and-cognify.
//!
//! Every wrapper: guard → clone the handle Arc → parse JNI args (sync-throw on
//! malformed JSON / null) → `spawn_future` the shared op body. The op body's
//! `Ok(Value)` completes the future with the JSON string; `Err(SdkError)`
//! completes it exceptionally with a CogneeException.

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::pipeline;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::checked_handle;

/// `add(handle, inputsJson, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_add<'l>(
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
            pipeline::add(&state, inputs, &dataset, &opts).await
        });
    })
}

/// `cognify(handle, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_cognify<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    dataset_name: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
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
            pipeline::cognify(&state, &dataset, &opts).await
        });
    })
}

/// `addAndCognify(handle, inputsJson, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_addAndCognify<'l>(
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
            pipeline::add_and_cognify(&state, inputs, &dataset, &opts).await
        });
    })
}
