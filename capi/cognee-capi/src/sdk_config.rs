//! Config surface for the C SDK tier (Phase 3).
//!
//! Exposes seven synchronous C functions that wrap `ConfigManager`:
//!
//! | Function                             | Wraps                                     |
//! |--------------------------------------|-------------------------------------------|
//! | `cg_sdk_config_set`                  | `ConfigManager::set(key, json_value)`     |
//! | `cg_sdk_config_set_str`              | convenience: encodes `value` as a JSON string |
//! | `cg_sdk_config_set_llm_config`       | `ConfigManager::set_llm_config`           |
//! | `cg_sdk_config_set_embedding_config` | `ConfigManager::set_embedding_config`     |
//! | `cg_sdk_config_set_vector_db_config` | `ConfigManager::set_vector_db_config`     |
//! | `cg_sdk_config_set_graph_db_config`  | `ConfigManager::set_graph_db_config`      |
//! | `cg_sdk_config_get`                  | read-back with secret fields blanked      |
//!
//! All functions are **synchronous** — config mutation is in-memory only. None
//! of them use `spawn_sdk_op` or the async callback pattern.
//!
//! ## Key names
//!
//! Key names match the Rust `Settings` field names exactly (snake_case), the
//! same names the TypeScript `configSet` uses (see `js/cognee-neon/src/config.rs`).
//! Common keys per group:
//!   - LLM: `llm_provider`, `llm_model`, `llm_api_key`, `llm_endpoint`,
//!     `llm_api_version`, `llm_temperature`, `llm_streaming`,
//!     `llm_max_completion_tokens`, `llm_max_retries`, `llm_max_parallel_requests`
//!   - Embedding: `embedding_provider`, `embedding_model`, `embedding_dimensions`,
//!     `embedding_endpoint`, `embedding_api_key`, `embedding_model_path`,
//!     `embedding_tokenizer_path`
//!   - Vector DB: `vector_db_provider`, `vector_db_url`, `vector_db_key`,
//!     `vector_db_host`, `vector_db_port`, `vector_db_name`
//!   - Graph DB: `graph_database_provider`, `graph_model`, `graph_file_path`
//!   - Chunking: `chunk_strategy`, `chunk_engine`, `chunk_size`, `chunk_overlap`
//!   - Paths: `system_root_directory`, `data_root_directory`,
//!     `cache_root_directory`, `logs_root_directory`
//!   - Ontology: `ontology_file_path`, `ontology_resolver`, `ontology_matching_strategy`
//!   - Misc: `monitoring_tool`, `classification_model`, `summarization_model`
//!
//! ## Error mapping (R2)
//!
//! `ConfigError::UnknownKey`  → `CG_ERR_UNKNOWN_CONFIG_KEY`  (17)
//! `ConfigError::TypeMismatch`→ `CG_ERR_CONFIG_TYPE_MISMATCH` (18)
//! Malformed JSON             → `CG_ERR_SDK_VALIDATION`       (14)
//!
//! ## Version bump / services rebuild
//!
//! Every successful `set` call increments `ConfigManager::version()`. The next
//! call to `HandleState::services()` (e.g. inside `cg_sdk_warm`) detects the
//! version advance and rebuilds the engine bundle — so a config change takes
//! effect on the very next warm.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::sync::Arc;

use cognee_lib::config::ConfigError;

use crate::error::{CgErrorCode, set_last_error};
use crate::sdk::CgSdk;
use crate::util::null_check;

// ── Secret-field blanking (matches js/cognee-neon/src/config.rs) ─────────────

/// Fields that must never be echoed back in `cg_sdk_config_get`.
///
/// This list mirrors `SECRET_FIELDS` in `js/cognee-neon/src/config.rs`.
/// `cognee_utils::redact` only catches secret-shaped substrings; a bare value
/// like `"llm_api_key": "abc123"` is NOT caught by it, so we use an explicit
/// allow-list instead.
const SECRET_FIELDS: &[&str] = &[
    "llm_api_key",
    "embedding_api_key",
    "vector_db_key",
    "vector_db_password",
    "graph_database_key",
    "graph_database_password",
    "db_password",
    "cache_password",
    "default_user_password",
    "otel_exporter_otlp_headers",
];

const REDACTED: &str = "***REDACTED***";

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Borrow a `&CgSdk` from a raw pointer after a null check, then run `f` on the
/// state's `ConfigManager` reference. Returns the `CgErrorCode` produced by `f`.
///
/// Writes to the thread-local last-error slot on failure so callers can use the
/// sync-style `cg_last_error_message()` pattern.
macro_rules! with_config {
    ($sdk:expr, $f:expr) => {{
        null_check!($sdk);
        let state = unsafe { &*$sdk };
        $f(&state.state)
    }};
}

/// Map a `ConfigError` to a `CgErrorCode`, recording the human-readable message
/// in the thread-local last-error slot.
fn config_error_to_code(e: ConfigError) -> CgErrorCode {
    let code = match &e {
        ConfigError::UnknownKey(_) => CgErrorCode::UnknownConfigKey,
        ConfigError::TypeMismatch { .. } => CgErrorCode::ConfigTypeMismatch,
    };
    set_last_error(e.to_string());
    code
}

/// Parse a `const char*` as a UTF-8 Rust `&str`. On failure sets the
/// last-error and returns the appropriate `CgErrorCode`.
///
/// Returns `Ok(s)` on success, `Err(code)` on null pointer or invalid UTF-8.
unsafe fn parse_cstr<'a>(ptr: *const c_char, param_name: &str) -> Result<&'a str, CgErrorCode> {
    if ptr.is_null() {
        set_last_error(format!("null pointer: {param_name}"));
        return Err(CgErrorCode::NullPointer);
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().map_err(|e| {
        set_last_error(format!("{param_name} is not valid UTF-8: {e}"));
        CgErrorCode::Utf8Error
    })
}

/// Parse a JSON object string into a `HashMap<String, serde_json::Value>`.
///
/// Returns `Err(SdkValidation)` for malformed JSON or non-object JSON.
fn parse_json_object(json: &str) -> Result<HashMap<String, serde_json::Value>, CgErrorCode> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        set_last_error(format!("JSON parse error: {e}"));
        CgErrorCode::SdkValidation
    })?;
    match value {
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        _ => {
            set_last_error("expected a JSON object");
            Err(CgErrorCode::SdkValidation)
        }
    }
}

// ── cg_sdk_config_set ─────────────────────────────────────────────────────────

/// Set a single configuration key to a JSON-encoded value.
///
/// `key` is a `Settings` field name (snake_case, e.g. `"llm_model"`).
/// `value_json` is any valid JSON value:
///   - string fields: `"\"openai\""` (a JSON string)
///   - numeric fields: `"0.7"`, `"4096"` (JSON numbers)
///   - boolean fields: `"true"` or `"false"` (JSON booleans)
///
/// Every successful call increments the config version and will cause
/// `cg_sdk_warm` to rebuild the service bundle on next invocation.
///
/// Returns `CG_ERR_UNKNOWN_CONFIG_KEY` (17) for unrecognised keys.
/// Returns `CG_ERR_CONFIG_TYPE_MISMATCH` (18) when the JSON type does not match.
/// Returns `CG_ERR_SDK_VALIDATION` (14) for malformed JSON.
/// Returns `CG_ERR_NULL_POINTER` (1) if `sdk`, `key`, or `value_json` is NULL.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` from `cg_sdk_new`.
/// `key` and `value_json` must be valid null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_config_set(
    sdk: *const CgSdk,
    key: *const c_char,
    value_json: *const c_char,
) -> CgErrorCode {
    with_config!(sdk, |state: &Arc<cognee_bindings_common::HandleState>| {
        let key_str = match unsafe { parse_cstr(key, "key") } {
            Ok(s) => s,
            Err(e) => return e,
        };
        let val_str = match unsafe { parse_cstr(value_json, "value_json") } {
            Ok(s) => s,
            Err(e) => return e,
        };
        let value: serde_json::Value = match serde_json::from_str(val_str) {
            Ok(v) => v,
            Err(e) => {
                set_last_error(format!("value_json parse error: {e}"));
                return CgErrorCode::SdkValidation;
            }
        };
        match state.cm.config().set(key_str, value) {
            Ok(()) => CgErrorCode::Ok,
            Err(e) => config_error_to_code(e),
        }
    })
}

// ── cg_sdk_config_set_str ─────────────────────────────────────────────────────

/// Set a string-typed configuration key from a plain C string (convenience).
///
/// Equivalent to calling `cg_sdk_config_set` with `value_json = "\"<value>\""`.
/// Covers ~80% of keys without requiring the caller to JSON-escape the value.
///
/// Returns the same error codes as `cg_sdk_config_set`.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` from `cg_sdk_new`.
/// `key` and `value` must be valid null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_config_set_str(
    sdk: *const CgSdk,
    key: *const c_char,
    value: *const c_char,
) -> CgErrorCode {
    with_config!(sdk, |state: &Arc<cognee_bindings_common::HandleState>| {
        let key_str = match unsafe { parse_cstr(key, "key") } {
            Ok(s) => s,
            Err(e) => return e,
        };
        let val_str = match unsafe { parse_cstr(value, "value") } {
            Ok(s) => s,
            Err(e) => return e,
        };
        let json_value = serde_json::Value::String(val_str.to_string());
        match state.cm.config().set(key_str, json_value) {
            Ok(()) => CgErrorCode::Ok,
            Err(e) => config_error_to_code(e),
        }
    })
}

// ── cg_sdk_config_set_llm_config ─────────────────────────────────────────────

/// Bulk-update LLM configuration from a JSON object.
///
/// `llm_config_json` must be a JSON object whose keys are any subset of the
/// LLM config keys: `llm_provider`, `llm_model`, `llm_api_key`, `llm_endpoint`,
/// `llm_api_version`, `llm_temperature`, `llm_streaming`,
/// `llm_max_completion_tokens`, `llm_max_retries`, `llm_max_parallel_requests`.
///
/// Returns the same error codes as `cg_sdk_config_set`.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` from `cg_sdk_new`.
/// `llm_config_json` must be a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_config_set_llm_config(
    sdk: *const CgSdk,
    llm_config_json: *const c_char,
) -> CgErrorCode {
    with_config!(sdk, |state: &Arc<cognee_bindings_common::HandleState>| {
        let json_str = match unsafe { parse_cstr(llm_config_json, "llm_config_json") } {
            Ok(s) => s,
            Err(e) => return e,
        };
        let map = match parse_json_object(json_str) {
            Ok(m) => m,
            Err(e) => return e,
        };
        match state.cm.config().set_llm_config(&map) {
            Ok(()) => CgErrorCode::Ok,
            Err(e) => config_error_to_code(e),
        }
    })
}

// ── cg_sdk_config_set_embedding_config ───────────────────────────────────────

/// Bulk-update embedding configuration from a JSON object.
///
/// `embedding_config_json` must be a JSON object whose keys are any subset of
/// the embedding config keys: `embedding_provider`, `embedding_model`,
/// `embedding_dimensions`, `embedding_endpoint`, `embedding_api_key`,
/// `embedding_model_path`, `embedding_tokenizer_path`.
///
/// Returns the same error codes as `cg_sdk_config_set`.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` from `cg_sdk_new`.
/// `embedding_config_json` must be a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_config_set_embedding_config(
    sdk: *const CgSdk,
    embedding_config_json: *const c_char,
) -> CgErrorCode {
    with_config!(sdk, |state: &Arc<cognee_bindings_common::HandleState>| {
        let json_str = match unsafe { parse_cstr(embedding_config_json, "embedding_config_json") } {
            Ok(s) => s,
            Err(e) => return e,
        };
        let map = match parse_json_object(json_str) {
            Ok(m) => m,
            Err(e) => return e,
        };
        match state.cm.config().set_embedding_config(&map) {
            Ok(()) => CgErrorCode::Ok,
            Err(e) => config_error_to_code(e),
        }
    })
}

// ── cg_sdk_config_set_vector_db_config ───────────────────────────────────────

/// Bulk-update vector DB configuration from a JSON object.
///
/// `vector_db_config_json` must be a JSON object whose keys are any subset of
/// the vector DB config keys: `vector_db_provider`, `vector_db_url`,
/// `vector_db_key`, `vector_db_host`, `vector_db_port`, `vector_db_name`.
///
/// Returns the same error codes as `cg_sdk_config_set`.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` from `cg_sdk_new`.
/// `vector_db_config_json` must be a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_config_set_vector_db_config(
    sdk: *const CgSdk,
    vector_db_config_json: *const c_char,
) -> CgErrorCode {
    with_config!(sdk, |state: &Arc<cognee_bindings_common::HandleState>| {
        let json_str = match unsafe { parse_cstr(vector_db_config_json, "vector_db_config_json") } {
            Ok(s) => s,
            Err(e) => return e,
        };
        let map = match parse_json_object(json_str) {
            Ok(m) => m,
            Err(e) => return e,
        };
        match state.cm.config().set_vector_db_config(&map) {
            Ok(()) => CgErrorCode::Ok,
            Err(e) => config_error_to_code(e),
        }
    })
}

// ── cg_sdk_config_set_graph_db_config ────────────────────────────────────────

/// Bulk-update graph DB configuration from a JSON object.
///
/// `graph_db_config_json` must be a JSON object whose keys are any subset of
/// the graph DB config keys: `graph_database_provider`, `graph_model`,
/// `graph_file_path`.
///
/// Returns the same error codes as `cg_sdk_config_set`.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` from `cg_sdk_new`.
/// `graph_db_config_json` must be a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_config_set_graph_db_config(
    sdk: *const CgSdk,
    graph_db_config_json: *const c_char,
) -> CgErrorCode {
    with_config!(sdk, |state: &Arc<cognee_bindings_common::HandleState>| {
        let json_str = match unsafe { parse_cstr(graph_db_config_json, "graph_db_config_json") } {
            Ok(s) => s,
            Err(e) => return e,
        };
        let map = match parse_json_object(json_str) {
            Ok(m) => m,
            Err(e) => return e,
        };
        match state.cm.config().set_graph_db_config(&map) {
            Ok(()) => CgErrorCode::Ok,
            Err(e) => config_error_to_code(e),
        }
    })
}

// ── cg_sdk_config_get ────────────────────────────────────────────────────────

/// Read back the current configuration as a JSON string.
///
/// Secret fields (`llm_api_key`, `embedding_api_key`, `vector_db_key`,
/// `vector_db_password`, `graph_database_key`, `graph_database_password`,
/// `db_password`, `cache_password`, `default_user_password`,
/// `otel_exporter_otlp_headers`) are replaced with `"***REDACTED***"` before
/// the JSON is serialized to the output string.
///
/// On success `*out_json` is set to a heap-allocated UTF-8 JSON string. The
/// caller must free it with `cg_string_destroy`.
///
/// Returns `CG_OK` on success, `CG_ERR_NULL_POINTER` if `sdk` or `out_json`
/// is NULL, `CG_ERR_RUNTIME` if serialization fails.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` from `cg_sdk_new`.
/// `out_json` must be a valid non-null pointer to a `char*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_config_get(
    sdk: *const CgSdk,
    out_json: *mut *mut c_char,
) -> CgErrorCode {
    null_check!(sdk);
    if out_json.is_null() {
        set_last_error("null pointer: out_json");
        return CgErrorCode::NullPointer;
    }
    let state = unsafe { &*sdk };

    // Serialize the current settings under a read lock.
    let mut value = {
        let settings = state.state.cm.settings();
        match serde_json::to_value(&*settings) {
            Ok(v) => v,
            Err(e) => {
                set_last_error(format!("failed to serialize settings: {e}"));
                return CgErrorCode::RuntimeError;
            }
        }
    };

    // Blank the secret fields in-place before returning to the C caller.
    if let serde_json::Value::Object(ref mut map) = value {
        for field in SECRET_FIELDS {
            if let Some(slot) = map.get_mut(*field) {
                *slot = serde_json::Value::String(REDACTED.to_string());
            }
        }
    }

    let json_str = match serde_json::to_string(&value) {
        Ok(s) => s,
        Err(e) => {
            set_last_error(format!("failed to serialize settings: {e}"));
            return CgErrorCode::RuntimeError;
        }
    };

    let c_string = match CString::new(json_str) {
        Ok(s) => s,
        Err(_) => {
            set_last_error("serialized settings JSON contained an unexpected null byte");
            return CgErrorCode::RuntimeError;
        }
    };

    unsafe { *out_json = c_string.into_raw() };
    CgErrorCode::Ok
}
