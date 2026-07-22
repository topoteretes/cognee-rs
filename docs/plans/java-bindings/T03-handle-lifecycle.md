# T03 — Handle lifecycle: `nativeNew`/`nativeDestroy`, `Cognee` (`AutoCloseable` + `Cleaner`)

## Objective

After this task a Java caller can construct and destroy an SDK handle:
`try (Cognee c = new Cognee(Map.of("data_root_directory", "...")))  { ... }`
round-trips; the native `HandleState` is created synchronously (no DB I/O, no
`owner_id` resolution — matching neon `cogneeNew`), freed on `close()` (idempotent)
with a `Cleaner` leak backstop, and use-after-close throws `IllegalStateException`.
This task also introduces the base `ai.cognee.CogneeException(code, message)` and
the Rust error-throw helper, because a failed constructor must surface a coded
error.

## Dependencies & preconditions

- **T01 done** (skeleton compiles, `Native.version()` works).
- **T02 done** (`java/scripts/check.sh` exists and is wired). Verify:
  `bash java/scripts/check.sh` passes (or SKIPs cleanly without a JDK).
- Read `crates/bindings-common/src/handle.rs`: `HandleState::from_settings`
  (sync, no I/O), `from_env`, and `services()`/`owner_id()` (async — **not** used
  here). Note the neon 3-way settings overlay in `ts/cognee-ts-neon/src/sdk.rs`
  (`cognee_new`): defaults < env < provided object, merged at the
  `serde_json::Value` level, keys are canonical snake_case `Settings` field names.
- Read `crates/bindings-common/src/error.rs`: `SdkError::code()` strings.

## Context for this task

**Handle representation (decision #6).** `nativeNew` returns
`Box::into_raw(Box::new(Arc<HandleState>)) as jlong`. Ops (later tasks) deref
this pointer to `&Arc<HandleState>` and `Arc::clone` before spawning, so a
concurrent `destroy` cannot dangle an in-flight task. `nativeDestroy` reclaims
the `Box`, dropping that one `Arc` (in-flight tasks keep their own clones alive).

**Settings overlay (mirror neon `cognee_new`).** A null/empty settings string →
env-only `Settings`. Otherwise parse a JSON **object** whose keys are canonical
snake_case `Settings` field names, serialize the env `Settings` to a value,
merge only the provided keys on top, and deserialize back to `Settings`. This
preserves env/default values for absent keys (do NOT re-deserialize a partial
object with `serde(default)` — that resets absent fields). L3 passes the
constructor `Map`/`String` straight through as JSON; **no camelCase translation
in v1** (design §3: snake_case at the boundary; the design §4 example uses
`Map.of("data_root_directory", ...)` and `set("llm_model", ...)`).

**Errors.** A construction failure (`invalid settings JSON`, `not an object`,
`invalid settings`) is an `SdkError::Validation`. It is thrown synchronously as
`ai.cognee.CogneeException` carrying `code()`/message via the JNI up-call
(constructed through the exception's `(String,String)` constructor and
`env.throw`). Panics still map to `java.lang.RuntimeException` (T01 guards).

## Steps

### 1. Create `java/cognee-java-jni/src/errors.rs`

```rust
//! JNI error helpers: map an `SdkError`/code+message onto a thrown
//! `ai.cognee.CogneeException(String code, String message)`.

use jni::JNIEnv;
use jni::objects::{JString, JThrowable};

use cognee_bindings_common::SdkError;

/// Throw `ai.cognee.CogneeException(code, message)` on the current thread.
///
/// Best-effort: if constructing/throwing the typed exception itself fails, fall
/// back to a plain `RuntimeException` so an error is always surfaced.
pub(crate) fn throw_cognee_exception(env: &mut JNIEnv, code: &str, message: &str) {
    let built = (|| -> jni::errors::Result<()> {
        let code_j: JString = env.new_string(code)?;
        let msg_j: JString = env.new_string(message)?;
        let exc = env.new_object(
            "ai/cognee/CogneeException",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[(&code_j).into(), (&msg_j).into()],
        )?;
        env.throw(JThrowable::from(exc))
    })();
    if built.is_err() {
        let _ = env.throw_new("java/lang/RuntimeException", message);
    }
}

/// Throw a `CogneeException` from an `SdkError` (uses its stable `code()`).
pub(crate) fn throw_sdk_error(env: &mut JNIEnv, err: SdkError) {
    throw_cognee_exception(env, err.code(), &err.to_string());
}
```

> jni 0.21 provides `From<&JString> for JValue`, so `(&code_j).into()` builds the
> constructor args. If trait resolution complains, use
> `&[JValue::Object(&code_j), JValue::Object(&msg_j)]` (a `JString` derefs to
> `JObject`) and `use jni::objects::JValue;`. Record any change in the
> Deviations log.

### 2. Create `java/cognee-java-jni/src/handle.rs`

```rust
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
        _ => return Err(SdkError::Validation("settings must be a JSON object".into())),
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
```

### 3. Extend `java/cognee-java-jni/src/lib.rs`

Add the module declarations:

```rust
mod errors;
mod handle;
mod runtime;
```

(Keep `runtime` — it will be used from T05 on. If `mod runtime;` is currently
flagged dead, that is fine; do not delete it.)

### 4. Create `java/src/main/java/ai/cognee/CogneeException.java`

```java
package ai.cognee;

/**
 * Unchecked exception carrying a stable machine-readable {@code code()} shared
 * with the other cognee bindings (JS {@code e.code}, C {@code CgErrorCode}).
 * The code string is the contract; branch on it, not on the message.
 */
public class CogneeException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    private final String code;

    public CogneeException(String code, String message) {
        super(message);
        this.code = code;
    }

    /** Stable machine-readable error code (e.g. {@code "VALIDATION_ERROR"}). */
    public String code() {
        return code;
    }
}
```

### 5. Create `java/src/main/java/ai/cognee/internal/Json.java`

Shared Jackson bridge used by every L3 class.

```java
package ai.cognee.internal;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;

/** Shared JSON marshalling for the cognee Java SDK (internal). */
public final class Json {
    private static final ObjectMapper MAPPER = new ObjectMapper();

    private Json() {}

    /** Serialize any value to a JSON string; {@code null} → the string "null". */
    public static String toJson(Object value) {
        try {
            return value == null ? "null" : MAPPER.writeValueAsString(value);
        } catch (Exception e) {
            throw new IllegalArgumentException("failed to serialize to JSON", e);
        }
    }

    public static <T> T fromJson(String json, Class<T> type) {
        try {
            return MAPPER.readValue(json, type);
        } catch (Exception e) {
            throw new IllegalStateException("failed to deserialize JSON: " + json, e);
        }
    }

    public static <T> T fromJson(String json, TypeReference<T> type) {
        try {
            return MAPPER.readValue(json, type);
        } catch (Exception e) {
            throw new IllegalStateException("failed to deserialize JSON: " + json, e);
        }
    }
}
```

### 6. Extend `java/src/main/java/ai/cognee/internal/Native.java`

Add the two native declarations (append inside the class, after `version()`):

```java
    /** Create a native handle from a settings JSON string (or null for env). */
    public static native long newHandle(String settingsJson);

    /** Free a native handle. Safe with 0; called at most once per handle. */
    public static native void destroy(long handle);
```

### 7. Create `java/src/main/java/ai/cognee/Cognee.java`

```java
package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import java.lang.ref.Cleaner;
import java.util.Map;

/**
 * The cognee Java SDK entry point. Construct with optional settings (canonical
 * snake_case {@code Settings} field names), then drive the pipeline. Holds a
 * native handle; call {@link #close()} to release it (a {@link Cleaner} is a
 * leak backstop, but {@code close()} is the primary path).
 */
public final class Cognee implements AutoCloseable {
    private static final Cleaner CLEANER = Cleaner.create();

    /** Mutable holder so the Cleaner can null the handle after freeing it. */
    private static final class Handle implements Runnable {
        private long ptr;

        Handle(long ptr) {
            this.ptr = ptr;
        }

        @Override
        public void run() {
            if (ptr != 0) {
                Native.destroy(ptr);
                ptr = 0;
            }
        }
    }

    private final Handle handleHolder;
    private final Cleaner.Cleanable cleanable;
    private volatile boolean closed = false;

    /** Construct from environment/default settings. */
    public Cognee() {
        this((String) null);
    }

    /** Construct from a settings map (canonical snake_case keys). */
    public Cognee(Map<String, ?> settings) {
        this(settings == null ? null : Json.toJson(settings));
    }

    /** Construct from a settings JSON string (or {@code null} for env-only). */
    public Cognee(String settingsJson) {
        long ptr = Native.newHandle(settingsJson); // throws CogneeException on bad settings
        this.handleHolder = new Handle(ptr);
        this.cleanable = CLEANER.register(this, this.handleHolder);
    }

    /** The native handle for internal op calls. Throws if closed. */
    public long handle() {
        if (closed) {
            throw new IllegalStateException("Cognee handle is closed");
        }
        return handleHolder.ptr;
    }

    @Override
    public void close() {
        if (closed) {
            return;
        }
        closed = true;
        cleanable.clean(); // runs Handle.run() exactly once → Native.destroy
    }
}
```

> `handle()` is `public` because the L3 op files added in later tasks live in the
> same `ai.cognee` package and call it; it is not part of the documented API
> (mark it `@hidden` in Javadoc in T12, or keep it package-private and place all
> op classes in `ai.cognee`). Keep all L3 op logic in package `ai.cognee`.

### 8. Create `java/src/test/java/ai/cognee/CogneeLifecycleTest.java`

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.nio.file.Path;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CogneeLifecycleTest {
    @Test
    void constructCloseRoundTrips(@TempDir Path dir) {
        Cognee cognee = new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()));
        assertDoesNotThrow(cognee::handle);
        cognee.close();
        cognee.close(); // idempotent
        assertThrows(IllegalStateException.class, cognee::handle);
    }

    @Test
    void envOnlyConstruction() {
        try (Cognee cognee = new Cognee()) {
            assertDoesNotThrow(cognee::handle);
        }
    }

    @Test
    void invalidSettingsThrowsCogneeException() {
        CogneeException ex =
                assertThrows(CogneeException.class, () -> new Cognee("[\"not an object\"]"));
        org.junit.jupiter.api.Assertions.assertEquals("VALIDATION_ERROR", ex.code());
    }
}
```

## Verification

1. `cargo build --manifest-path java/cognee-java-jni/Cargo.toml` → clean.
2. `cargo clippy --manifest-path java/cognee-java-jni/Cargo.toml --all-targets -- -D warnings`
   → clean.
3. `bash java/scripts/check.sh` → the three `CogneeLifecycleTest` cases and
   `NativeLoadTest` pass.
4. `scripts/check_all.sh` → green.

## Out of scope

- `warm()`, `ownerId()`, and anything async → **T05** (needs the up-call
  machinery). Do not call `HandleState::services()`/`owner_id()` here.
- The `CogneeException` subclasses (`CogneeConfigException`,
  `CogneeValidationException`) and config native methods → **T04**.
- camelCase→snake_case key translation in the constructor → post-v1 (v1 is
  snake_case only).
