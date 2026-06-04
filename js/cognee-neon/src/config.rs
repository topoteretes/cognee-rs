//! Config surface (Phase 2).
//!
//! Exposes `ConfigManager`'s setters (granular + bulk + generic) and a
//! `getConfig` read-back to TypeScript. All functions take the
//! `CogneeHandle` `JsBox` and reach the `ConfigManager` via
//! `handle.state.cm.config()` (the `cm: Arc<ComponentManager>` lives on
//! `HandleState`; `ComponentManager::config()` returns `&ConfigManager`).
//!
//! Each setter bumps the config version, which version-invalidates the cached
//! [`crate::services::CogneeServices`] (`HandleState::services()` is keyed on
//! `cm.config().version()`), so a config change rebuilds the engines on the
//! next op — no manual re-wiring.
//!
//! Setters are synchronous (config mutation is cheap, in-memory). The granular
//! setters are infallible (`-> void`); the generic `set` and the bulk setters
//! are fallible and **throw** the mapped [`ConfigError`]
//! (`UnknownKey` / `TypeMismatch`) as a JS `Error` with a `code` field.

use std::collections::HashMap;

use neon::prelude::*;

use cognee_lib::config::ConfigError;

use crate::sdk::CogneeHandle;

/// Secret/credential fields that must never be echoed back through
/// `getConfig`. Blanked to `"***REDACTED***"` before serialization.
///
/// `cognee_utils::redact` only matches secret-*shaped* substrings (`sk-…`,
/// `Bearer …`, `password=…`); a bare value like `"llm_api_key": "abc123"` is
/// NOT caught by it (and `redact` is not re-exported by `cognee_lib`), so we
/// use an explicit, deterministic blanklist instead.
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

// ---------------------------------------------------------------------------
// JS <-> serde_json marshalling (JSON.stringify / JSON.parse round-trip).
// ---------------------------------------------------------------------------

/// Stringify a JS value via the global `JSON.stringify`.
fn stringify_js<'cx>(
    cx: &mut FunctionContext<'cx>,
    val: Handle<'cx, JsValue>,
) -> NeonResult<String> {
    let global = cx.global_object();
    let json: Handle<JsObject> = global.get(cx, "JSON")?;
    let stringify: Handle<JsFunction> = json.get(cx, "stringify")?;
    let result: Handle<JsValue> = stringify.call_with(cx).arg(val).apply(cx)?;
    let s = result.downcast_or_throw::<JsString, _>(cx)?;
    Ok(s.value(cx))
}

/// Parse a JSON string into a JS value via the global `JSON.parse`.
fn parse_js<'cx>(cx: &mut FunctionContext<'cx>, json: &str) -> JsResult<'cx, JsValue> {
    let global = cx.global_object();
    let json_obj: Handle<JsObject> = global.get(cx, "JSON")?;
    let parse: Handle<JsFunction> = json_obj.get(cx, "parse")?;
    let arg = cx.string(json);
    parse.call_with(cx).arg(arg).apply(cx)
}

/// Convert a JS value (object) into a `serde_json::Value`.
fn js_to_value<'cx>(
    cx: &mut FunctionContext<'cx>,
    val: Handle<'cx, JsValue>,
) -> NeonResult<serde_json::Value> {
    let json = stringify_js(cx, val)?;
    serde_json::from_str::<serde_json::Value>(&json)
        .or_else(|e| cx.throw_error(format!("invalid JSON value: {e}")))
}

/// Convert a JS object argument into a `HashMap<String, serde_json::Value>` for
/// the bulk setters.
fn js_to_map<'cx>(
    cx: &mut FunctionContext<'cx>,
    val: Handle<'cx, JsValue>,
) -> NeonResult<HashMap<String, serde_json::Value>> {
    match js_to_value(cx, val)? {
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        _ => cx.throw_error("expected a config object"),
    }
}

/// Throw a [`ConfigError`] as a JS `Error` carrying a `code` field, mirroring
/// the `errors.rs` / `throw_sdk_error` convention.
fn throw_config_error<'cx, T>(cx: &mut FunctionContext<'cx>, err: ConfigError) -> NeonResult<T> {
    let code = match err {
        ConfigError::UnknownKey(_) => "UNKNOWN_CONFIG_KEY",
        ConfigError::TypeMismatch { .. } => "CONFIG_TYPE_MISMATCH",
    };
    let msg = err.to_string();
    let js_err = cx.error(msg)?;
    let code_val = cx.string(code);
    js_err
        .downcast_or_throw::<JsObject, _>(cx)?
        .set(cx, "code", code_val)?;
    cx.throw(js_err)
}

// ---------------------------------------------------------------------------
// Granular setters (sync, infallible -> undefined).
// ---------------------------------------------------------------------------

/// Read the first arg as a `CogneeHandle` and the second as a string, then run
/// `f` with the `ConfigManager`-bound string setter.
macro_rules! string_setter {
    ($name:ident, $method:ident) => {
        pub fn $name(mut cx: FunctionContext) -> JsResult<JsUndefined> {
            let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
            let value = cx.argument::<JsString>(1)?.value(&mut cx);
            handle.state.cm.config().$method(&value);
            Ok(cx.undefined())
        }
    };
}

/// Like [`string_setter!`] but the second arg is a JS number coerced to `u32`.
macro_rules! u32_setter {
    ($name:ident, $method:ident) => {
        pub fn $name(mut cx: FunctionContext) -> JsResult<JsUndefined> {
            let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
            let value = cx.argument::<JsNumber>(1)?.value(&mut cx) as u32;
            handle.state.cm.config().$method(value);
            Ok(cx.undefined())
        }
    };
}

/// Like [`string_setter!`] but the second arg is a JS number coerced to `u16`.
macro_rules! u16_setter {
    ($name:ident, $method:ident) => {
        pub fn $name(mut cx: FunctionContext) -> JsResult<JsUndefined> {
            let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
            let value = cx.argument::<JsNumber>(1)?.value(&mut cx) as u16;
            handle.state.cm.config().$method(value);
            Ok(cx.undefined())
        }
    };
}

/// Like [`string_setter!`] but the second arg is a JS number (`f64`).
macro_rules! f64_setter {
    ($name:ident, $method:ident) => {
        pub fn $name(mut cx: FunctionContext) -> JsResult<JsUndefined> {
            let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
            let value = cx.argument::<JsNumber>(1)?.value(&mut cx);
            handle.state.cm.config().$method(value);
            Ok(cx.undefined())
        }
    };
}

/// Like [`string_setter!`] but the second arg is a JS boolean.
macro_rules! bool_setter {
    ($name:ident, $method:ident) => {
        pub fn $name(mut cx: FunctionContext) -> JsResult<JsUndefined> {
            let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
            let value = cx.argument::<JsBoolean>(1)?.value(&mut cx);
            handle.state.cm.config().$method(value);
            Ok(cx.undefined())
        }
    };
}

// LLM
string_setter!(config_set_llm_provider, set_llm_provider);
string_setter!(config_set_llm_model, set_llm_model);
string_setter!(config_set_llm_api_key, set_llm_api_key);
string_setter!(config_set_llm_endpoint, set_llm_endpoint);
string_setter!(config_set_llm_api_version, set_llm_api_version);
f64_setter!(config_set_llm_temperature, set_llm_temperature);
bool_setter!(config_set_llm_streaming, set_llm_streaming);
u32_setter!(
    config_set_llm_max_completion_tokens,
    set_llm_max_completion_tokens
);
u32_setter!(config_set_llm_max_retries, set_llm_max_retries);
u32_setter!(
    config_set_llm_max_parallel_requests,
    set_llm_max_parallel_requests
);

// Embedding
string_setter!(config_set_embedding_provider, set_embedding_provider);
string_setter!(config_set_embedding_model, set_embedding_model);
u32_setter!(config_set_embedding_dimensions, set_embedding_dimensions);
string_setter!(config_set_embedding_endpoint, set_embedding_endpoint);
string_setter!(config_set_embedding_api_key, set_embedding_api_key);
string_setter!(config_set_embedding_model_path, set_embedding_model_path);
string_setter!(
    config_set_embedding_tokenizer_path,
    set_embedding_tokenizer_path
);

// Vector DB
string_setter!(config_set_vector_db_provider, set_vector_db_provider);
string_setter!(config_set_vector_db_url, set_vector_db_url);
string_setter!(config_set_vector_db_key, set_vector_db_key);
string_setter!(config_set_vector_db_host, set_vector_db_host);
u16_setter!(config_set_vector_db_port, set_vector_db_port);
string_setter!(config_set_vector_db_name, set_vector_db_name);

// Graph DB
string_setter!(
    config_set_graph_database_provider,
    set_graph_database_provider
);
string_setter!(config_set_graph_model, set_graph_model);
string_setter!(config_set_graph_file_path, set_graph_file_path);

// Chunking
string_setter!(config_set_chunk_strategy, set_chunk_strategy);
string_setter!(config_set_chunk_engine, set_chunk_engine);
u32_setter!(config_set_chunk_size, set_chunk_size);
u32_setter!(config_set_chunk_overlap, set_chunk_overlap);

// Paths
string_setter!(config_set_system_root_directory, set_system_root_directory);
string_setter!(config_set_data_root_directory, set_data_root_directory);
string_setter!(config_set_cache_root_directory, set_cache_root_directory);
string_setter!(config_set_logs_root_directory, set_logs_root_directory);

// Ontology
string_setter!(config_set_ontology_file_path, set_ontology_file_path);
string_setter!(config_set_ontology_resolver, set_ontology_resolver);
string_setter!(
    config_set_ontology_matching_strategy,
    set_ontology_matching_strategy
);

// Other
string_setter!(config_set_monitoring_tool, set_monitoring_tool);
string_setter!(config_set_classification_model, set_classification_model);
string_setter!(config_set_summarization_model, set_summarization_model);

// ---------------------------------------------------------------------------
// Generic + bulk setters (sync, fallible -> throw ConfigError).
// ---------------------------------------------------------------------------

/// `configSet(handle, key, value)` — forwards to `ConfigManager::set`.
pub fn config_set(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let key = cx.argument::<JsString>(1)?.value(&mut cx);
    let value_arg = cx.argument::<JsValue>(2)?;
    let value = js_to_value(&mut cx, value_arg)?;
    match handle.state.cm.config().set(&key, value) {
        Ok(()) => Ok(cx.undefined()),
        Err(e) => throw_config_error(&mut cx, e),
    }
}

/// Run a bulk-setter closure with a marshalled `HashMap`, surfacing
/// `ConfigError` as a thrown JS error.
macro_rules! bulk_setter {
    ($name:ident, $method:ident) => {
        pub fn $name(mut cx: FunctionContext) -> JsResult<JsUndefined> {
            let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
            let obj = cx.argument::<JsValue>(1)?;
            let map = js_to_map(&mut cx, obj)?;
            match handle.state.cm.config().$method(&map) {
                Ok(()) => Ok(cx.undefined()),
                Err(e) => throw_config_error(&mut cx, e),
            }
        }
    };
}

bulk_setter!(config_set_llm_config, set_llm_config);
bulk_setter!(config_set_embedding_config, set_embedding_config);
bulk_setter!(config_set_vector_db_config, set_vector_db_config);
bulk_setter!(config_set_graph_db_config, set_graph_db_config);

// ---------------------------------------------------------------------------
// getConfig (sync read-back, secrets blanked).
// ---------------------------------------------------------------------------

/// `getConfig(handle) -> object` — a JSON snapshot of the current `Settings`
/// with the known secret fields blanked (see [`SECRET_FIELDS`]).
pub fn get_config(mut cx: FunctionContext) -> JsResult<JsValue> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;

    let mut value = {
        let settings = handle.state.cm.settings();
        serde_json::to_value(&*settings)
            .or_else(|e| cx.throw_error(format!("failed to serialize settings: {e}")))?
    };

    // Blank the secret fields in-place before crossing back into JS.
    if let serde_json::Value::Object(ref mut map) = value {
        for field in SECRET_FIELDS {
            if let Some(slot) = map.get_mut(*field) {
                *slot = serde_json::Value::String(REDACTED.to_string());
            }
        }
    }

    let json = serde_json::to_string(&value)
        .or_else(|e| cx.throw_error(format!("failed to serialize settings: {e}")))?;
    parse_js(&mut cx, &json)
}
