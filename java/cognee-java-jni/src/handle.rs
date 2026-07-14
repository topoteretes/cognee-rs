//! Handle lifecycle: `newHandle(settingsJson) -> long` and `destroy(long)`.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jlong;

use cognee_bindings_common::{HandleState, SdkError};
use cognee_lib::config::{ConfigManager, Settings};

use crate::errors::throw_sdk_error;
use crate::{guard_jlong, guard_void};

/// Borrow a `jlong` handle as `&Arc<HandleState>`.
///
/// # Safety
/// `ptr` must be a value returned by `newHandle` that has not been destroyed.
/// The Java layer guarantees this via its closed-guard + `Cleaner` run-once.
pub(crate) unsafe fn handle_ref<'a>(ptr: jlong) -> &'a Arc<HandleState> {
    unsafe { &*(ptr as *const Arc<HandleState>) }
}

/// Build `Settings` with the neon 3-way overlay: defaults < env < provided.
fn build_settings(settings_json: &str) -> Result<Settings, SdkError> {
    let base = ConfigManager::from_env().read().clone();
    let trimmed = settings_json.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(base);
    }
    let provided = serde_json::from_str::<serde_json::Value>(trimmed)
        .map_err(|e| SdkError::Validation(format!("invalid settings JSON: {e}")))?;
    let map = match provided {
        serde_json::Value::Object(m) => m,
        _ => {
            return Err(SdkError::Validation(
                "settings must be a JSON object".into(),
            ));
        }
    };
    let mut merged = serde_json::to_value(&base)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize base settings: {e}")))?;
    if let serde_json::Value::Object(ref mut base_map) = merged {
        for (k, v) in map {
            base_map.insert(k, v);
        }
    }
    serde_json::from_value::<Settings>(merged)
        .map_err(|e| SdkError::Validation(format!("invalid settings: {e}")))
}

/// `ai.cognee.internal.Native.newHandle(String settingsJson) -> long`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_newHandle<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    settings_json: JString<'l>,
) -> jlong {
    guard_jlong(&mut env, |env| {
        let json = if settings_json.is_null() {
            String::new()
        } else {
            match env.get_string(&settings_json) {
                // JNI modified-UTF-8 is handled by get_string (design §10).
                Ok(s) => s.into(),
                Err(_) => {
                    throw_sdk_error(
                        env,
                        SdkError::Validation("settings string was not valid".into()),
                    );
                    return 0;
                }
            }
        };
        match build_settings(&json) {
            Ok(settings) => {
                let state = Arc::new(HandleState::from_settings(settings));
                Box::into_raw(Box::new(state)) as jlong
            }
            Err(e) => {
                throw_sdk_error(env, e);
                0
            }
        }
    })
}

/// `ai.cognee.internal.Native.destroy(long handle)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_destroy<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
) {
    guard_void(&mut env, |_env| {
        if handle != 0 {
            // SAFETY: `handle` came from `newHandle`; destroy runs at most once
            // (Java closed-guard + Cleaner run-once).
            unsafe {
                drop(Box::from_raw(handle as *mut Arc<HandleState>));
            }
        }
    })
}
