//! Admin/user ops.

use jni::JNIEnv;
use jni::objects::{JClass, JObject};
use jni::sys::jlong;

use cognee_bindings_common::ops::admin;

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
