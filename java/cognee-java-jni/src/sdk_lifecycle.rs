//! Async lifecycle ops: `warm`, `ownerId`.

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
