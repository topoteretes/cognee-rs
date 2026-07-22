# T04 — Config surface (`set` / `setStr` / 4 bulk / `get`) + `ConfigError` mapping

## Objective

After this task `cognee.config()` exposes the v1 config surface (design decision
A3.1): generic `set(key, value)`, `setStr(key, value)`, the four bulk setters
(`setLlmConfig`, `setEmbeddingConfig`, `setVectorDbConfig`, `setGraphDbConfig`),
and `get()` returning the redacted settings snapshot. Config mutation is
synchronous (in-memory, cheap — no runtime, no async). A `ConfigError` surfaces
as `ai.cognee.CogneeException` with the stable code `UNKNOWN_CONFIG_KEY` or
`CONFIG_TYPE_MISMATCH`, matching the other bindings.

## Dependencies & preconditions

- **T03 done.** Verify:
  - `bash java/scripts/check.sh` passes (lifecycle tests green).
  - `ai.cognee.CogneeException` and `ai.cognee.internal.Json` exist.
  - `crate::handle::handle_ref` and `crate::errors::throw_cognee_exception` exist.
- Read `ts/cognee-ts-neon/src/config.rs`: `config_set` (generic), the
  `bulk_setter!` macro over `set_llm_config`/`set_embedding_config`/
  `set_vector_db_config`/`set_graph_db_config`, `get_config` (serialize
  `cm.settings()`, then `redact_config_json`), and `throw_config_error`
  (`UnknownKey → "UNKNOWN_CONFIG_KEY"`, `TypeMismatch → "CONFIG_TYPE_MISMATCH"`).
- Confirm `crates/lib/src/config.rs` still defines `ConfigError::{UnknownKey,
  TypeMismatch{key,reason}}` and `ConfigManager::{set, set_llm_config,
  set_embedding_config, set_vector_db_config, set_graph_db_config}` (grep for
  `pub fn set` / `pub enum ConfigError`).

## Context for this task

**A3.1 (from `docs/tools/bindings.md`).** Java v1 ships the same surface as C /
Python: **generic `set` + `set_str` + 4 bulk setters + `get`**. The ~40 granular
typed setters (JS-only sugar) are a post-v1 mechanical addition and are **out of
scope**. Keys are canonical snake_case `Settings` field names.

**Sync, no runtime.** Config setters are synchronous like neon's (they bump the
config version, which version-invalidates the cached services on the next op).
The native methods take `long handle` + JSON strings and either return normally
(`void`) / a JSON string (`get`), or throw synchronously.

**Value marshalling.** The generic `set(key, value)` receives the value as a
JSON string (L3 `Json.toJson(value)`), which the Rust side parses to
`serde_json::Value` and hands to `ConfigManager::set`. `setStr(key, str)` is the
same path with a JSON string value. Bulk setters receive a JSON object string,
parsed to `HashMap<String, serde_json::Value>`.

**`get()`** mirrors neon `get_config`: serialize `cm.settings()` to a value,
`redact_config_json` it (blanks secret fields), return the JSON string. L3
deserializes to `Map<String, Object>`. (snake_case keys in v1.)

**Error model (design §5, minimal).** The code string is the contract; v1 does
**not** add `CogneeException` subclasses. Both `SdkError` codes (T03) and
`ConfigError` codes flow through the same base `CogneeException(code, message)`.

## Steps

### 1. Create `java/cognee-java-jni/src/config.rs`

```rust
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
            throw_cognee_exception(env, "VALIDATION_ERROR", &format!("{what} string was invalid"));
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
        // SAFETY: `handle` is a live handle (Java closed-guard).
        let state = unsafe { handle_ref(handle) };
        let Some(key) = read_string(env, &key, "config key") else { return };
        let Some(value_json) = read_string(env, &value_json, "config value") else { return };
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
                // SAFETY: live handle (Java closed-guard).
                let state = unsafe { handle_ref(handle) };
                let Some(json) = read_string(env, &map_json, "config map") else { return };
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

bulk_setter!(Java_ai_cognee_internal_Native_configSetLlmConfig, set_llm_config);
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
        // SAFETY: live handle (Java closed-guard).
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
```

### 2. Extend `java/cognee-java-jni/src/lib.rs`

Add `mod config;` to the module list.

### 3. Extend `java/src/main/java/ai/cognee/internal/Native.java`

Append the six declarations:

```java
    public static native void configSet(long handle, String key, String valueJson);

    public static native void configSetLlmConfig(long handle, String mapJson);

    public static native void configSetEmbeddingConfig(long handle, String mapJson);

    public static native void configSetVectorDbConfig(long handle, String mapJson);

    public static native void configSetGraphDbConfig(long handle, String mapJson);

    public static native String getConfig(long handle);
```

### 4. Create `java/src/main/java/ai/cognee/CogneeConfig.java`

```java
package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import com.fasterxml.jackson.core.type.TypeReference;
import java.util.Map;

/**
 * Synchronous configuration surface (design decision A3.1): generic {@link #set},
 * {@link #setStr}, four bulk setters, and {@link #get}. Keys are canonical
 * snake_case {@code Settings} field names. A type error throws
 * {@link CogneeException} with {@code code() == "CONFIG_TYPE_MISMATCH"}; an
 * unknown key throws with {@code "UNKNOWN_CONFIG_KEY"}.
 */
public final class CogneeConfig {
    private final Cognee cognee;

    CogneeConfig(Cognee cognee) {
        this.cognee = cognee;
    }

    /** Set any config key to any JSON-serializable value. */
    public void set(String key, Object value) {
        Native.configSet(cognee.handle(), key, Json.toJson(value));
    }

    /** Convenience for string-valued keys (identical to {@link #set}). */
    public void setStr(String key, String value) {
        set(key, value);
    }

    public void setLlmConfig(Map<String, ?> values) {
        Native.configSetLlmConfig(cognee.handle(), Json.toJson(values));
    }

    public void setEmbeddingConfig(Map<String, ?> values) {
        Native.configSetEmbeddingConfig(cognee.handle(), Json.toJson(values));
    }

    public void setVectorDbConfig(Map<String, ?> values) {
        Native.configSetVectorDbConfig(cognee.handle(), Json.toJson(values));
    }

    public void setGraphDbConfig(Map<String, ?> values) {
        Native.configSetGraphDbConfig(cognee.handle(), Json.toJson(values));
    }

    /** Read-back of the current settings (secret fields blanked, snake_case keys). */
    public Map<String, Object> get() {
        String json = Native.getConfig(cognee.handle());
        return Json.fromJson(json, new TypeReference<Map<String, Object>>() {});
    }
}
```

### 5. Add the `config()` accessor to `Cognee.java`

Add a lazily-created, cached accessor:

```java
    private CogneeConfig config;

    /** The synchronous configuration surface. */
    public synchronized CogneeConfig config() {
        if (config == null) {
            config = new CogneeConfig(this);
        }
        return config;
    }
```

### 6. Create `java/src/test/java/ai/cognee/CogneeConfigTest.java`

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.nio.file.Path;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CogneeConfigTest {
    private Cognee handle(Path dir) {
        return new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()));
    }

    @Test
    void setAndGetRoundTrip(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            cognee.config().set("llm_model", "gpt-4o-mini");
            cognee.config().setLlmConfig(Map.of("provider", "openai"));
            Map<String, Object> snapshot = cognee.config().get();
            assertEquals("gpt-4o-mini", snapshot.get("llm_model"));
        }
    }

    @Test
    void typeMismatchSurfacesCode(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeException ex = assertThrows(
                    CogneeException.class,
                    () -> cognee.config().set("chunk_size", "not-a-number"));
            assertEquals("CONFIG_TYPE_MISMATCH", ex.code());
        }
    }

    @Test
    void unknownKeySurfacesCode(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeException ex = assertThrows(
                    CogneeException.class,
                    () -> cognee.config().set("no_such_key", "x"));
            assertEquals("UNKNOWN_CONFIG_KEY", ex.code());
        }
    }
}
```

> If `get()` returns the LLM model under a different snake_case key than
> `llm_model` (verify against `crates/lib/src/config.rs` `Settings` field names —
> the neon `get_config` serializes those field names directly), adjust the
> assertion to the actual field name and note it in the Deviations log.

## Verification

1. `cargo build --manifest-path java/cognee-java-jni/Cargo.toml` → clean.
2. `cargo clippy --manifest-path java/cognee-java-jni/Cargo.toml --all-targets -- -D warnings`
   → clean.
3. `bash java/scripts/check.sh` → `CogneeConfigTest` (3 cases) + prior tests pass.
4. `scripts/check_all.sh` → green.

## Out of scope

- The ~40 granular typed setters (`setLlmModel`, …) → post-v1 (A3.1). v1 is
  generic + bulk + get only.
- Async ops, `warm()`, `ownerId()`, the up-call machinery → **T05**.
- `CogneeException` subclasses → not in v1 (the code string is the contract).
- camelCase key translation in `get()`/`set()` → post-v1.
