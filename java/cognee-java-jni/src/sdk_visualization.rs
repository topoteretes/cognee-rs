//! Visualization ops: visualize (HTML string), visualize_to_file (path).
//! No #[cfg] — bindings-common returns FEATURE_NOT_BUILT when the feature is off.

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::visualization;

use crate::args::arg_json;
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::checked_handle;

/// `visualize(handle, optsJson, future)` — completes with the HTML string.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_visualize<'l>(
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
            visualization::visualize(&state, Some(&opts))
                .await
                .map(serde_json::Value::String)
        });
    })
}

/// `visualizeToFile(handle, optsJson, future)` — completes with the written path.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_visualizeToFile<'l>(
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
            visualization::visualize_to_file(&state, Some(&opts))
                .await
                .map(serde_json::Value::String)
        });
    })
}
