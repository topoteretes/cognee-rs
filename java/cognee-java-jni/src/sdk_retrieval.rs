//! Retrieval ops: search, recall.

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::retrieval;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::checked_handle;

/// `search(handle, query, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_search<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    query: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let query = match arg_string(env, &query) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            retrieval::search(&state, &query, &opts).await
        });
    })
}

/// `recall(handle, query, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_recall<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    query: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let query = match arg_string(env, &query) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            retrieval::recall(&state, &query, &opts).await
        });
    })
}
