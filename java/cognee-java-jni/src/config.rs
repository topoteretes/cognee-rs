//! Config surface: generic `set`, `set_str`, 4 bulk setters, `get`.
//! Synchronous — mirrors `ts/cognee-ts-neon/src/config.rs` without neon types.

use std::collections::HashMap;

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jlong, jstring};

use cognee_lib::config::ConfigError;

use crate::errors::throw_cognee_exception;
use crate::handle::handle_ref;
use crate::{guard_jstring, guard_void};

/// Map a `ConfigError` onto a thrown `CogneeException` with its stable code.
fn throw_config_error(env: &mut JNIEnv, err: ConfigError) {
    let code = match err {
        ConfigError::UnknownKey(_) => "UNKNOWN_CONFIG_KEY",
        ConfigError::TypeMismatch { .. } => "CONFIG_TYPE_MISMATCH",
    };
    throw_cognee_exception(env, code, &err.to_string());
}

/// Read a `JString` argument into a `String`, throwing `VALIDATION_ERROR` on failure.
/// Returns `None` when it threw (caller must early-return).
fn read_string(env: &mut JNIEnv, s: &JString, what: &str) -> Option<String> {
    match env.get_string(s) {
        Ok(v) => Some(v.into()),
        Err(_) => {
            throw_cognee_exception(
                env,
                "VALIDATION_ERROR",
                &format!("{what} string was invalid"),
            );
            None
        }
    }
}

/// `configSet(handle, key, valueJson)` — generic `ConfigManager::set`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_configSet<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    key: JString<'l>,
    value_json: JString<'l>,
) {
    guard_void(&mut env, |env| {
        // SAFETY: `handle` is live for this synchronous call — Java's `dispatch`
        // holds the op/close read lock (blocking `destroy`) and fences
        // reachability (blocking the Cleaner) around it. See `handle::handle_ref`.
        let state = unsafe { handle_ref(handle) };
        let Some(key) = read_string(env, &key, "config key") else {
            return;
        };
        let Some(value_json) = read_string(env, &value_json, "config value") else {
            return;
        };
        let value: serde_json::Value = match serde_json::from_str(&value_json) {
            Ok(v) => v,
            Err(e) => {
                throw_cognee_exception(
                    env,
                    "VALIDATION_ERROR",
                    &format!("invalid config value JSON: {e}"),
                );
                return;
            }
        };
        if let Err(e) = state.cm.config().set(&key, value) {
            throw_config_error(env, e);
        }
    })
}

/// One JNI export per bulk setter. `$fn` is the full mangled name.
macro_rules! bulk_setter {
    ($fn:ident, $method:ident) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn $fn<'l>(
            mut env: JNIEnv<'l>,
            _class: JClass<'l>,
            handle: jlong,
            map_json: JString<'l>,
        ) {
            guard_void(&mut env, |env| {
                // SAFETY: live for this synchronous call via Java's `dispatch`
                // (op/close read lock + reachabilityFence). See `handle_ref`.
                let state = unsafe { handle_ref(handle) };
                let Some(json) = read_string(env, &map_json, "config map") else {
                    return;
                };
                let map: HashMap<String, serde_json::Value> = match serde_json::from_str(&json) {
                    Ok(m) => m,
                    Err(e) => {
                        throw_cognee_exception(
                            env,
                            "VALIDATION_ERROR",
                            &format!("config map must be a JSON object: {e}"),
                        );
                        return;
                    }
                };
                if let Err(e) = state.cm.config().$method(&map) {
                    throw_config_error(env, e);
                }
            })
        }
    };
}

bulk_setter!(
    Java_ai_cognee_internal_Native_configSetLlmConfig,
    set_llm_config
);
bulk_setter!(
    Java_ai_cognee_internal_Native_configSetEmbeddingConfig,
    set_embedding_config
);
bulk_setter!(
    Java_ai_cognee_internal_Native_configSetVectorDbConfig,
    set_vector_db_config
);
bulk_setter!(
    Java_ai_cognee_internal_Native_configSetGraphDbConfig,
    set_graph_db_config
);

/// `getConfig(handle) -> String` — redacted settings snapshot (secrets blanked).
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_getConfig<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
) -> jstring {
    guard_jstring(&mut env, |env| {
        // SAFETY: live for this synchronous call via Java's `dispatch`
        // (op/close read lock + reachabilityFence). See `handle_ref`.
        let state = unsafe { handle_ref(handle) };
        let mut value = {
            let settings = state.cm.settings();
            match serde_json::to_value(&*settings) {
                Ok(v) => v,
                Err(e) => {
                    throw_cognee_exception(
                        env,
                        "RUNTIME_ERROR",
                        &format!("failed to serialize settings: {e}"),
                    );
                    return std::ptr::null_mut();
                }
            }
        };
        cognee_bindings_common::redact_config_json(&mut value);
        match env.new_string(value.to_string()) {
            Ok(s) => s.into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    })
}
