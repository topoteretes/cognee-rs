//! Admin/user ops.

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::admin;

use crate::args::arg_string;
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
