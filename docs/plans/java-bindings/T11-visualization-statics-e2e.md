# T11 — Visualization ops + static setup methods + LLM-gated E2E

## Objective

After this task the binding is functionally complete: `visualize` /
`visualizeToFile` (feature-gated, but always callable — they return
`FEATURE_NOT_BUILT` when the feature is off), the static module-level setup
methods `Cognee.setupLogging()` / `Cognee.initOtlp()` / `Cognee.initTelemetry()`
/ `Cognee.version()`, and an LLM-gated end-to-end test (`warm → add → cognify →
search`) that skips cleanly without `OPENAI_URL`/`OPENAI_TOKEN`.

## Dependencies & preconditions

- **T06 done** and **T07 done** (the E2E exercises add/cognify/search). Verify
  `bash java/scripts/check.sh` passes.
- Read `crates/bindings-common/src/ops/visualization.rs`: `visualize(state,
  opts: Option<&Value>) -> Result<String, SdkError>`, `visualize_to_file(state,
  opts) -> Result<String, SdkError>`. Both return `FeatureNotBuilt` (code
  `FEATURE_NOT_BUILT`) when the `visualization` feature is off — so the JNI
  wrapper needs **no `#[cfg]`** (unlike neon's cfg-gated wrapper); the
  bindings-common layer handles the gating.
- Read `ts/cognee-ts-neon/src/logging.rs`, `.../telemetry_analytics.rs`, and
  `.../telemetry_otlp.rs` (+ `.../default_subscriber.rs`) — the statics port
  from these.

## Context for this task

- **Uniform result contract for viz:** `visualize`/`visualize_to_file` return a
  `String`; the wrapper maps it to `serde_json::Value::String(...)` so it flows
  through the same `spawn_future` path and L3 deserializes it back to a `String`.
- **Statics are module-level** (no handle). Java exposes them as `static` methods
  on `Cognee`.
- **OTEL service name default:** `cognee.java-binding` (mirrors neon's
  `cognee.node-binding`).
- **Analytics policy (design §3, decision 11):** arm unless `TELEMETRY_DISABLED`
  set, `ENV ∈ {test,dev}`, or `COGNEE_HOST_SDK` set — via
  `cognee_telemetry::env::{arm_binding_emission, is_disabled}`.

## Steps

### 1. Rust: create `java/cognee-java-jni/src/sdk_visualization.rs`

```rust
//! Visualization ops: visualize (HTML string), visualize_to_file (path).
//! No #[cfg] — bindings-common returns FEATURE_NOT_BUILT when the feature is off.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::visualization;

use crate::args::arg_json;
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::handle_ref;

/// `visualize(handle, optsJson, future)` — completes with the HTML string.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_visualize<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            visualization::visualize(&state, Some(&opts))
                .await
                .map(serde_json::Value::String)
        });
    })
}

/// `visualizeToFile(handle, optsJson, future)` — completes with the written path.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_visualizeToFile<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            visualization::visualize_to_file(&state, Some(&opts))
                .await
                .map(serde_json::Value::String)
        });
    })
}
```

Add `mod sdk_visualization;` to `lib.rs`.

### 2. Rust: create `java/cognee-java-jni/src/sdk_static.rs` — logging + telemetry

Port `logging.rs` and `telemetry_analytics.rs` verbatim (they are self-contained).
`initOtlp` ports `telemetry_otlp.rs` + `default_subscriber.rs` from neon.

```rust
//! Module-level statics: logging, OTLP telemetry, product analytics.

use std::sync::{Mutex, OnceLock};

use jni::JNIEnv;
use jni::objects::JClass;
use jni::sys::jboolean;

use crate::guard_void;

// --- setupLogging (port of cognee-ts-neon/src/logging.rs) ---

static LOG_GUARDS: OnceLock<Mutex<Option<cognee_logging::LogGuards>>> = OnceLock::new();

/// `setupLogging()` — env-driven, idempotent.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_setupLogging<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    guard_void(&mut env, |env| {
        let slot = LOG_GUARDS.get_or_init(|| Mutex::new(None));
        // lock poison is unrecoverable
        let mut lock = slot.lock().expect("lock poison is unrecoverable");
        if lock.is_some() {
            return; // idempotent
        }
        match cognee_logging::LoggingConfig::from_env() {
            Ok(cfg) => {
                let guards = cognee_logging::init_logging(
                    cfg,
                    std::iter::empty::<cognee_logging::BoxedLayer>(),
                );
                *lock = Some(guards);
            }
            Err(e) => {
                crate::errors::throw_cognee_exception(
                    env,
                    "RUNTIME_ERROR",
                    &format!("invalid logging config: {e}"),
                );
            }
        }
    })
}

// --- initTelemetry / analytics arming (port of telemetry_analytics.rs) ---

static ANALYTICS_ARMED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

fn arm_analytics() -> bool {
    let slot = ANALYTICS_ARMED.get_or_init(|| Mutex::new(None));
    // lock poison is unrecoverable
    let mut lock = slot.lock().expect("lock poison is unrecoverable");
    if let Some(armed) = *lock {
        return armed;
    }
    cognee_telemetry::env::arm_binding_emission();
    let armed = !cognee_telemetry::env::is_disabled();
    *lock = Some(armed);
    armed
}

/// `initTelemetry() -> boolean` — arm product analytics per the per-binding
/// policy (ON unless TELEMETRY_DISABLED / ENV∈{test,dev} / COGNEE_HOST_SDK).
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_initTelemetry<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jboolean {
    // Boolean return: default `false` (0) on panic.
    crate::guard_jlong(&mut env, |_env| arm_analytics() as jni::sys::jlong) as jboolean
}
```

> `guard_jlong` reused for the boolean return keeps the panic sentinel uniform
> (0 == false). If clippy objects to the cast chain, add a small `guard_jboolean`
> helper in `lib.rs` mirroring `guard_jlong` and record it in the Deviations log.

**`initOtlp`** ports `ts/cognee-ts-neon/src/telemetry_otlp.rs` (+ its
`default_subscriber.rs` dependency). Read those two files and reproduce their
logic in `sdk_static.rs`, replacing the neon `FunctionContext`/`cx.throw_error`
entry shape with a JNI entry:

```rust
/// `initOtlp()` — install OTLP export from env (idempotent). Service-name
/// default `cognee.java-binding`. Ports telemetry_otlp.rs + default_subscriber.rs.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_initOtlp<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    guard_void(&mut env, |env| {
        // Port of telemetry_otlp::setup_telemetry:
        //  - OnceLock<Mutex<Option<TelemetryGuard>>> for the guard
        //  - apply_default_service_name("cognee.java-binding") when OTEL_SERVICE_NAME unset
        //  - EnvSettingsView::from_env(); if !is_tracing_enabled -> store noop guard, return
        //  - init_telemetry::<Registry>(&settings) -> install the layer via the default
        //    subscriber's reload handle (port default_subscriber::install() + OTEL_RELOAD_HANDLE)
        //  - on init error, throw_cognee_exception(env, "RUNTIME_ERROR", ...)
        // See ts/cognee-ts-neon/src/{telemetry_otlp.rs, default_subscriber.rs}.
        let _ = env; // remove once the port is filled in
        todo!("port telemetry_otlp.rs + default_subscriber.rs; do not leave todo!() in the final code")
    })
}
```

> Do NOT ship a `todo!()`. If the default-subscriber/reload-handle port proves
> larger than this task's budget, implement `initOtlp` as the **minimal faithful
> subset**: set the default service name, check `is_tracing_enabled`, and if
> enabled install the OTEL layer as a standalone global subscriber via
> `tracing_subscriber` (no hot-reload) — and record that simplification in the
> Deviations log. `setupLogging` and `initTelemetry` must be complete regardless.

Add `mod sdk_static;` to `lib.rs`.

### 3. Make `Native.version()` public and add the static/viz declarations

In `Native.java`, change `static native String version();` to
`public static native String version();`, and append:

```java
    // visualization
    public static native void visualize(long handle, String optsJson,
            CompletableFuture<String> future);
    public static native void visualizeToFile(long handle, String optsJson,
            CompletableFuture<String> future);
    // module-level statics
    public static native void setupLogging();
    public static native void initOtlp();
    public static native boolean initTelemetry();
```

### 4. Java: `VisualizeOptions.java` + viz ops + statics on `Cognee`

```java
package ai.cognee;

public final class VisualizeOptions extends Options {
    /** Output path for {@code visualizeToFile} (ignored by {@code visualize}). */
    public VisualizeOptions destinationPath(String path) { put("destinationPath", path); return this; }
}
```

Add to `Cognee.java`:

```java
    // --- visualization ---
    public CompletableFuture<String> visualize() {
        return visualize(null);
    }

    public CompletableFuture<String> visualize(VisualizeOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.visualize(handle(), Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, String.class));
    }

    public CompletableFuture<String> visualizeToFile(VisualizeOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.visualizeToFile(handle(), Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, String.class));
    }

    // --- module-level statics ---
    /** Initialize file logging from env vars (idempotent). */
    public static void setupLogging() {
        Native.setupLogging();
    }

    /** Install OpenTelemetry OTLP export from env vars (idempotent). */
    public static void initOtlp() {
        Native.initOtlp();
    }

    /** Arm product-analytics emission (per the opt-out policy); returns whether
     *  analytics are effective for this process. */
    public static boolean initTelemetry() {
        return Native.initTelemetry();
    }

    /** The native/SDK version string. */
    public static String version() {
        return Native.version();
    }
```

### 5. Test `java/src/test/java/ai/cognee/StaticsTest.java`

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

class StaticsTest {
    @Test
    void staticsAreIdempotentAndSafe() {
        assertNotNull(Cognee.version());
        assertDoesNotThrow(Cognee::setupLogging);
        assertDoesNotThrow(Cognee::setupLogging); // idempotent
        assertDoesNotThrow(Cognee::initOtlp);
        assertDoesNotThrow(Cognee::initTelemetry);
    }
}
```

### 6. Test `java/src/test/java/ai/cognee/EndToEndIT.java` (LLM-gated)

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class EndToEndIT {
    @Test
    void warmAddCognifySearch(@TempDir Path dir) {
        String url = System.getenv("OPENAI_URL");
        String token = System.getenv("OPENAI_TOKEN");
        assumeTrue(url != null && !url.isEmpty() && token != null && !token.isEmpty(),
                "OPENAI_URL/OPENAI_TOKEN not set — skipping LLM E2E");

        try (Cognee cognee = new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()))) {
            cognee.config().setLlmConfig(Map.of(
                    "llm_provider", "openai", "llm_api_key", token, "llm_endpoint", url));
            cognee.warm().join();
            cognee.add(List.of(DataInput.text(
                    "Alan Turing was a mathematician who founded computer science.")),
                    "ds").join();
            CognifyResult c = cognee.cognify("ds").join();
            assertNotNull(c);
            SearchResponse r = cognee.search("Who founded computer science?",
                    new SearchOptions().searchType(SearchType.GRAPH_COMPLETION)).join();
            assertNotNull(r.raw());
        }
    }
}
```

> The LLM config keys were verified against `crates/lib/src/config.rs`
> `set_llm_config` (lines ~1814-1842): the accepted keys are `llm_provider`,
> `llm_model`, `llm_api_key`, `llm_endpoint`, … (each prefixed with `llm_`);
> unknown keys return `ConfigError::UnknownKey`. The boilerplate above uses the
> correct prefixed keys (`llm_provider`/`llm_api_key`/`llm_endpoint`), matching
> the existing `CogneeConfigTest` usage. Name the class `*IT` so it is clearly an
> integration test; it still runs under surefire and skips via `assumeTrue`.

## Verification

1. `cargo build`/`clippy` (shim crate) → clean; **no `todo!()` remains** in
   `sdk_static.rs` (`grep -n 'todo!' java/cognee-java-jni/src/*.rs` finds nothing).
2. `bash java/scripts/check.sh` → `StaticsTest` passes; `EndToEndIT` passes with
   credentials or skips (Assumption) without them.
3. With credentials present, `bash java/scripts/check.sh` runs the E2E green.
4. `scripts/check_all.sh` → green.

## Out of scope

- Docs / examples / README / Javadoc site → **T12**.
- Prebuild classifier jars → **T13**.
- OTLP hot-reload sophistication beyond neon's port → keep parity with neon; do
  not innovate.
