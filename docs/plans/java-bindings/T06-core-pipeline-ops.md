# T06 — Core pipeline ops: `add`, `cognify`, `addAndCognify` + typed results

## Objective

After this task the three core pipeline ops work end-to-end through the async
machinery, returning typed Java records. `add(...)` runs deterministically
against sqlite in a temp dir with no LLM. **This task establishes the canonical
op-wrapping pattern (Rust wrapper + Java method + typed record + `Options`
builder + `DataInput` builder) that T07–T11 reuse verbatim.**

## Dependencies & preconditions

- **T05 done.** Verify `bash java/scripts/check.sh` passes and
  `crate::future::spawn_future`, `crate::errors::throw_sdk_error`,
  `crate::handle::handle_ref`, and the L3 `Cognee.handle()` accessor all exist.
- Read `crates/bindings-common/src/ops/pipeline.rs`: `add(state, inputs_json,
  dataset_name, opts) -> Result<Value, SdkError>`, `cognify(state, dataset_name,
  opts)`, `add_and_cognify(state, inputs_json, dataset_name, opts)`.
- Read `crates/bindings-common/src/wire.rs`: `marshal_inputs` (accepts a single
  `{type,…}` object or an array), the input variants, and `cognify_result_json`.
- Read `ts/cognee-ts-neon/src/sdk_ops.rs` for the neon arg-extraction order
  (`handle, dataInput, datasetName, opts?`).

## Context for this task — exact wire shapes (from `wire.rs` / `pipeline.rs`)

**`DataInput`** (a single object, or an array of them):

```json
{"type":"text","text":"..."}
{"type":"file","path":"/abs/or/rel/path"}
{"type":"url","url":"https://..."}
{"type":"binary","bytes":"<base64>","name":"file.pdf"}
```

`bytes` may be base64, a JSON byte array, or a Node `Buffer` projection — Java
sends base64. `binary` requires `name` (MIME detection). `s3`/`dataItem` →
`SdkError::Unsupported`.

**`opts` (camelCase)** — `add`: `{tenant?}`. `cognify`:
`{tenant?, chunkSize?, chunkOverlap?, summarization?, temporalCognify?, triplet?}`.

**`AddResult` JSON** (from `add_result_json`):

```json
{"datasetName":"...","added":[<Data>...],"addedCount":N,
 "deduplicated":[<Data>...],"deduplicatedCount":N}
```

**`CognifyResult` JSON** (from `cognify_result_json`):

```json
{"chunks":N,"entities":N,"edges":N,"summaries":N,"embeddings":N,
 "alreadyCompleted":bool,"priorPipelineRunId":"<uuid>"|null}
```

**`addAndCognify` JSON**: `{"add":<AddResult>,"cognify":<CognifyResult>}`.

## Steps

### 1. Create `java/cognee-java-jni/src/args.rs` (shared arg helpers)

```rust
//! Shared JNI argument helpers used by every op wrapper.

use jni::JNIEnv;
use jni::objects::JString;

use cognee_bindings_common::SdkError;

/// Read a required string argument.
pub(crate) fn arg_string(env: &mut JNIEnv, s: &JString) -> Result<String, SdkError> {
    if s.is_null() {
        return Err(SdkError::Validation("required string argument was null".into()));
    }
    env.get_string(s)
        .map(|v| v.into())
        .map_err(|_| SdkError::Validation("invalid string argument".into()))
}

/// Read an optional JSON-string argument into a `Value`; null/empty/"null" → `Null`.
pub(crate) fn arg_json(env: &mut JNIEnv, s: &JString) -> Result<serde_json::Value, SdkError> {
    if s.is_null() {
        return Ok(serde_json::Value::Null);
    }
    let raw: String = env
        .get_string(s)
        .map(|v| v.into())
        .map_err(|_| SdkError::Validation("invalid JSON string argument".into()))?;
    let t = raw.trim();
    if t.is_empty() || t == "null" {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(t).map_err(|e| SdkError::Validation(format!("invalid JSON argument: {e}")))
}
```

Add `mod args;` to `lib.rs`.

### 2. Create `java/cognee-java-jni/src/sdk_ops.rs` — WORKED EXAMPLE + pattern

The **canonical wrapper pattern** (memorize this shape; every op wrapper is a
copy with different args and a different `pipeline::*`/`ops::*` call):

```rust
//! Pipeline ops: add, cognify, add-and-cognify.
//!
//! Every wrapper: guard → clone the handle Arc → parse JNI args (sync-throw on
//! malformed JSON / null) → `spawn_future` the shared op body. The op body's
//! `Ok(Value)` completes the future with the JSON string; `Err(SdkError)`
//! completes it exceptionally with a CogneeException.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::pipeline;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::handle_ref;

/// `add(handle, inputsJson, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_add<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    inputs_json: JString<'l>,
    dataset_name: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        // SAFETY: live handle (Java closed-guard); clone before moving into spawn.
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let inputs = match arg_json(env, &inputs_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let dataset = match arg_string(env, &dataset_name) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            pipeline::add(&state, inputs, &dataset, &opts).await
        });
    })
}

/// `cognify(handle, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_cognify<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    dataset_name: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let dataset = match arg_string(env, &dataset_name) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            pipeline::cognify(&state, &dataset, &opts).await
        });
    })
}

/// `addAndCognify(handle, inputsJson, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_addAndCognify<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    inputs_json: JString<'l>,
    dataset_name: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let inputs = match arg_json(env, &inputs_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let dataset = match arg_string(env, &dataset_name) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            pipeline::add_and_cognify(&state, inputs, &dataset, &opts).await
        });
    })
}
```

Add `mod sdk_ops;` to `lib.rs`.

### 3. Java L3 — shared builders (created once here, reused by T07–T11)

**`java/src/main/java/ai/cognee/Options.java`** — base for every `*Options` builder:

```java
package ai.cognee;

import ai.cognee.internal.Json;
import java.util.LinkedHashMap;
import java.util.Map;

/** Base for typed option builders. Serializes to the camelCase {@code opts} JSON. */
public abstract class Options {
    protected final Map<String, Object> values = new LinkedHashMap<>();

    protected void put(String key, Object value) {
        if (value != null) {
            values.put(key, value);
        }
    }

    /** The JSON this builder sends across the boundary. */
    public String toJson() {
        return Json.toJson(values);
    }

    /** JSON for an options builder, or {@code "null"} when none was given. */
    static String jsonOf(Options opts) {
        return opts == null ? "null" : opts.toJson();
    }
}
```

**`java/src/main/java/ai/cognee/DataInput.java`**:

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonValue;
import java.util.Base64;
import java.util.Map;

/** A discriminated input for {@code add}/{@code addAndCognify}/{@code remember}/{@code update}. */
public final class DataInput {
    private final Map<String, Object> fields;

    private DataInput(Map<String, Object> fields) {
        this.fields = fields;
    }

    /** The `{type,…}` object Jackson serializes for this input. */
    @JsonValue
    Map<String, Object> fields() {
        return fields;
    }

    public static DataInput text(String text) {
        return new DataInput(Map.of("type", "text", "text", text));
    }

    public static DataInput file(String path) {
        return new DataInput(Map.of("type", "file", "path", path));
    }

    public static DataInput url(String url) {
        return new DataInput(Map.of("type", "url", "url", url));
    }

    /** Binary input; {@code name} drives MIME detection. Bytes are sent base64. */
    public static DataInput binary(byte[] bytes, String name) {
        String b64 = Base64.getEncoder().encodeToString(bytes);
        return new DataInput(Map.of("type", "binary", "bytes", b64, "name", name));
    }
}
```

### 4. Java L3 — option builders for this task

**`AddOptions.java`**:

```java
package ai.cognee;

public final class AddOptions extends Options {
    public AddOptions tenant(String tenant) {
        put("tenant", tenant);
        return this;
    }
}
```

**`CognifyOptions.java`**:

```java
package ai.cognee;

public final class CognifyOptions extends Options {
    public CognifyOptions tenant(String tenant) { put("tenant", tenant); return this; }
    public CognifyOptions chunkSize(int n) { put("chunkSize", n); return this; }
    public CognifyOptions chunkOverlap(int n) { put("chunkOverlap", n); return this; }
    public CognifyOptions summarization(boolean b) { put("summarization", b); return this; }
    public CognifyOptions temporalCognify(boolean b) { put("temporalCognify", b); return this; }
    public CognifyOptions triplet(boolean b) { put("triplet", b); return this; }
}
```

### 5. Java L3 — typed result records

All records use `@JsonIgnoreProperties(ignoreUnknown = true)` so extra fields
never break deserialization. Field names match the camelCase JSON exactly.

**`CogneeData.java`** (minimal Data projection; extend as needed):

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeData(String id, String name) {}
```

**`AddResult.java`**:

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import java.util.List;

@JsonIgnoreProperties(ignoreUnknown = true)
public record AddResult(
        String datasetName,
        List<CogneeData> added,
        int addedCount,
        List<CogneeData> deduplicated,
        int deduplicatedCount) {}
```

**`CognifyResult.java`**:

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

@JsonIgnoreProperties(ignoreUnknown = true)
public record CognifyResult(
        int chunks,
        int entities,
        int edges,
        int summaries,
        int embeddings,
        boolean alreadyCompleted,
        String priorPipelineRunId) {}
```

**`AddAndCognifyResult.java`**:

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

@JsonIgnoreProperties(ignoreUnknown = true)
public record AddAndCognifyResult(AddResult add, CognifyResult cognify) {}
```

### 6. Extend `Native.java`

```java
    public static native void add(long handle, String inputsJson, String datasetName,
            String optsJson, CompletableFuture<String> future);

    public static native void cognify(long handle, String datasetName, String optsJson,
            CompletableFuture<String> future);

    public static native void addAndCognify(long handle, String inputsJson, String datasetName,
            String optsJson, CompletableFuture<String> future);
```

### 7. Add the ops to `Cognee.java`

```java
    // --- add ---
    public CompletableFuture<AddResult> add(java.util.List<DataInput> inputs, String datasetName) {
        return add(inputs, datasetName, null);
    }

    public CompletableFuture<AddResult> add(
            java.util.List<DataInput> inputs, String datasetName, AddOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.add(handle(), ai.cognee.internal.Json.toJson(inputs), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, AddResult.class));
    }

    public CompletableFuture<AddResult> add(DataInput input, String datasetName, AddOptions opts) {
        return add(java.util.List.of(input), datasetName, opts);
    }

    public CompletableFuture<AddResult> add(String text, String datasetName) {
        return add(DataInput.text(text), datasetName, null);
    }

    // --- cognify ---
    public CompletableFuture<CognifyResult> cognify(String datasetName) {
        return cognify(datasetName, null);
    }

    public CompletableFuture<CognifyResult> cognify(String datasetName, CognifyOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.cognify(handle(), datasetName, Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, CognifyResult.class));
    }

    // --- addAndCognify ---
    public CompletableFuture<AddAndCognifyResult> addAndCognify(
            java.util.List<DataInput> inputs, String datasetName, CognifyOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.addAndCognify(handle(), ai.cognee.internal.Json.toJson(inputs), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, AddAndCognifyResult.class));
    }
```

### 8. Test `java/src/test/java/ai/cognee/CogneeAddTest.java`

`add` is deterministic without an LLM (mirrors `test_add_parity.py`).

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class CogneeAddTest {
    @Test
    void addReturnsTypedResult(@TempDir Path dir) {
        try (Cognee cognee = new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()))) {
            AddResult r = cognee.add(List.of(DataInput.text("hello cognee")), "ds").join();
            assertEquals("ds", r.datasetName());
            assertEquals(1, r.addedCount());
            // Re-adding the identical payload is a content-addressed duplicate.
            AddResult r2 = cognee.add(List.of(DataInput.text("hello cognee")), "ds").join();
            assertEquals(0, r2.addedCount());
            assertEquals(1, r2.deduplicatedCount());
            assertTrue(r2.deduplicated().size() == 1);
        }
    }
}
```

## Verification

1. `cargo build --manifest-path java/cognee-java-jni/Cargo.toml` → clean.
2. `cargo clippy --manifest-path java/cognee-java-jni/Cargo.toml --all-targets -- -D warnings`
   → clean.
3. `bash java/scripts/check.sh` → `CogneeAddTest` passes (no `-Xcheck:jni` abort).
4. `scripts/check_all.sh` → green.

## Out of scope

- `cognify`/`addAndCognify` LLM round-trip tests → **T11** (LLM-gated E2E).
- `search`/`recall` → **T07**; memory ops → **T08**; data/datasets → **T09**.
- Full `CogneeData` field coverage → extend the record when a consumer needs a
  field; `@JsonIgnoreProperties(ignoreUnknown=true)` keeps it forward-compatible.
