//! Async lifecycle ops: `warm`, `ownerId`, `flush`.

use cognee_bindings_common::ops;
use jni::JNIEnv;
use jni::objects::{JClass, JObject};
use jni::sys::jlong;

use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::checked_handle;

/// `warm(handle, future)` — force `services()` to build (async), surfacing
/// config/connection errors and resolving `owner_id`. Completes with `null`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_warm<'l>(
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
            state.services().await.map(|_| serde_json::Value::Null)
        });
    })
}

/// `ownerId(handle, future)` — resolve the email-derived owner id (warms lazily).
/// Completes with the UUID string (JSON-encoded).
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_ownerId<'l>(
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
            state
                .owner_id()
                .await
                .map(|id| serde_json::Value::String(id.to_string()))
        });
    })
}

/// `flush(handle, future)` — checkpoint what is buffered, without closing.
/// Completes with `{"flushed":bool,"warm":bool,"ms":u64}`.
///
/// Safe to call on a UI lifecycle callback: it never builds the engine (a
/// never-warmed handle returns immediately), never closes anything, and reports
/// a skipped checkpoint in the payload rather than as a failure. The work itself
/// runs on the SDK runtime, not on the calling thread — the Java side gets a
/// future and is free to not wait for it.
///
/// `"flushed"` is the outcome, not the dispatch: it is `true` only when the
/// store really is checkpointed, and `false` when the checkpoint was skipped
/// (a read in flight, a read-only store) or failed. The future still completes
/// normally in both cases — a caller that wants to know must read the field.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_flush<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let Some(state) = checked_handle(env, handle, &future) else {
            return;
        };
        spawn_future(
            env,
            &future,
            async move { ops::lifecycle::flush(&state).await },
        );
    })
}
