//! Session ops: getSession, addFeedback, deleteFeedback, get/setGraphContext.

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::sessions;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::checked_handle;

/// `getSession(handle, sessionId, optsJson, future)` — opts `{lastN?}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_getSession<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    session_id: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
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
            sessions::run_get_session(&state, &session, &opts).await
        });
    })
}

/// `addFeedback(handle, sessionId, qaId, optsJson, future)`
/// — opts `{feedbackText?, feedbackScore?}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_addFeedback<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    session_id: JString<'l>,
    qa_id: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let session = match arg_string(env, &session_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let qa = match arg_string(env, &qa_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            sessions::run_add_feedback(&state, &session, &qa, &opts).await
        });
    })
}

/// `deleteFeedback(handle, sessionId, qaId, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_deleteFeedback<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    session_id: JString<'l>,
    qa_id: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let session = match arg_string(env, &session_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let qa = match arg_string(env, &qa_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            sessions::run_delete_feedback(&state, &session, &qa).await
        });
    })
}

/// `getGraphContext(handle, sessionId, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_getGraphContext<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    session_id: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let session = match arg_string(env, &session_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            sessions::run_get_graph_context(&state, &session).await
        });
    })
}

/// `setGraphContext(handle, sessionId, context, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_setGraphContext<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    session_id: JString<'l>,
    context: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        let session = match arg_string(env, &session_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let context = match arg_string(env, &context) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            sessions::run_set_graph_context(&state, &session, &context).await
        });
    })
}
