# T08 — Memory ops: `remember`, `rememberEntry`, `memify`, `improve`

## Objective

After this task the four memory ops work through the async machinery with typed
options and results. `remember`/`rememberEntry` return `RememberResult` (a
`raw()` wrapper — its JSON is snake_case for Python-SDK parity), `memify` returns
a typed `MemifyResult`, `improve` returns a typed `ImproveResult`.

## Dependencies & preconditions

- **T05 done**; **T06 landed** (reuses `crate::args`, `spawn_future`, L3
  `Options`, `DataInput`). Verify `bash java/scripts/check.sh` passes and
  `DataInput.java`, `Options.java`, `args.rs` exist.
- Read `crates/bindings-common/src/ops/memory.rs`: `run_remember(state,
  inputs_json, dataset_name, opts)`, `run_remember_entry(state, entry_json,
  dataset_name, session_id, opts)`, `run_memify_op(state, opts)`,
  `run_improve(state, opts)`; the camelCase opts/entry keys documented in its
  module header; and `memify_result_json`.
- Read `ts/cognee-ts-neon/src/sdk_memory.rs` for the neon arg order.

## Context for this task — exact wire shapes (from `ops/memory.rs`)

- **`remember` opts:** `{sessionId?, selfImprovement?, tenant?}`. Result:
  `RememberResult` serialized directly → **snake_case keys** (deliberate,
  Python-SDK parity; issue #46).
- **`rememberEntry` entry** (discriminated union on `type`):
  - `{"type":"qa","question","answer","context?","feedbackText?","feedbackScore?","usedGraphElementIds?"}`
  - `{"type":"trace","originFunction","status?","methodParams?","methodReturnValue?","memoryQuery?","memoryContext?","errorMessage?","generateFeedbackWithLlm?"}`
  - `{"type":"feedback","qaId","feedbackText?","feedbackScore?"}`

  Signature: `(entry, datasetName, sessionId, opts?{tenant?})`.
- **`memify` opts:** `{tripletBatchSize?, nodeTypeFilter?, nodeNameFilter?(string[]),
  nodeNameFilterOperator?}`. Result (`memify_result_json`, camelCase):
  `{"tripletCount":N,"indexedCount":N,"batchCount":N,"alreadyCompleted":bool,
  "priorPipelineRunId":"<uuid>"|null}`.
- **`improve` opts (`datasetName` REQUIRED):** `{datasetName, sessionIds?(string[]),
  nodeName?(string[]), feedbackAlpha?(double, default 0.1), tenant?}`. Result
  (camelCase): `{"stagesRun":[...],"memifyResult":<MemifyResult>|null,
  "feedbackEntriesProcessed":N,"feedbackEntriesApplied":N,"sessionsPersisted":N,
  "edgesSynced":N}`.

## Steps

### 1. Rust: create `java/cognee-java-jni/src/sdk_memory.rs`

Follow the T06 wrapper pattern.

```rust
//! Memory ops: remember, remember_entry, memify, improve.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::memory;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::handle_ref;

/// `remember(handle, inputsJson, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_remember<'l>(
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
            memory::run_remember(&state, inputs, &dataset, &opts).await
        });
    })
}

/// `rememberEntry(handle, entryJson, datasetName, sessionId, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_rememberEntry<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    entry_json: JString<'l>,
    dataset_name: JString<'l>,
    session_id: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let entry = match arg_json(env, &entry_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let dataset = match arg_string(env, &dataset_name) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let session = match arg_string(env, &session_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            memory::run_remember_entry(&state, entry, &dataset, &session, &opts).await
        });
    })
}

/// `memify(handle, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_memify<'l>(
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
            memory::run_memify_op(&state, &opts).await
        });
    })
}

/// `improve(handle, optsJson, future)` — opts must contain `datasetName`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_improve<'l>(
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
            memory::run_improve(&state, &opts).await
        });
    })
}
```

Add `mod sdk_memory;` to `lib.rs`.

### 2. Java: `MemoryEntry.java` (discriminated-union builder)

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonValue;
import java.util.LinkedHashMap;
import java.util.Map;

/** A single typed memory entry for {@link Cognee#rememberEntry}. */
public final class MemoryEntry {
    private final Map<String, Object> fields = new LinkedHashMap<>();

    private MemoryEntry(String type) {
        fields.put("type", type);
    }

    @JsonValue
    Map<String, Object> fields() {
        return fields;
    }

    private MemoryEntry put(String key, Object value) {
        if (value != null) {
            fields.put(key, value);
        }
        return this;
    }

    // --- qa ---
    public static MemoryEntry qa(String question, String answer) {
        return new MemoryEntry("qa").put("question", question).put("answer", answer);
    }

    public MemoryEntry context(String c) { return put("context", c); }
    public MemoryEntry usedGraphElementIds(Map<String, ?> m) { return put("usedGraphElementIds", m); }

    // --- trace ---
    public static MemoryEntry trace(String originFunction) {
        return new MemoryEntry("trace").put("originFunction", originFunction);
    }

    public MemoryEntry status(String s) { return put("status", s); }
    public MemoryEntry memoryQuery(String q) { return put("memoryQuery", q); }
    public MemoryEntry memoryContext(String c) { return put("memoryContext", c); }
    public MemoryEntry methodParams(Object o) { return put("methodParams", o); }
    public MemoryEntry methodReturnValue(Object o) { return put("methodReturnValue", o); }
    public MemoryEntry errorMessage(String e) { return put("errorMessage", e); }
    public MemoryEntry generateFeedbackWithLlm(boolean b) { return put("generateFeedbackWithLlm", b); }

    // --- feedback ---
    public static MemoryEntry feedback(String qaId) {
        return new MemoryEntry("feedback").put("qaId", qaId);
    }

    // shared optional feedback fields (qa + feedback)
    public MemoryEntry feedbackText(String t) { return put("feedbackText", t); }
    public MemoryEntry feedbackScore(int s) { return put("feedbackScore", s); }
}
```

### 3. Java: option builders

```java
package ai.cognee;

public final class RememberOptions extends Options {
    public RememberOptions sessionId(String s) { put("sessionId", s); return this; }
    public RememberOptions selfImprovement(boolean b) { put("selfImprovement", b); return this; }
    public RememberOptions tenant(String t) { put("tenant", t); return this; }
}
```

```java
package ai.cognee;

import java.util.List;

public final class MemifyOptions extends Options {
    public MemifyOptions tripletBatchSize(int n) { put("tripletBatchSize", n); return this; }
    public MemifyOptions nodeTypeFilter(String s) { put("nodeTypeFilter", s); return this; }
    public MemifyOptions nodeNameFilter(List<String> names) { put("nodeNameFilter", names); return this; }
    public MemifyOptions nodeNameFilterOperator(String op) { put("nodeNameFilterOperator", op); return this; }
}
```

```java
package ai.cognee;

import java.util.List;

/** {@code datasetName} is required. */
public final class ImproveOptions extends Options {
    public ImproveOptions(String datasetName) {
        put("datasetName", datasetName);
    }

    public ImproveOptions sessionIds(List<String> ids) { put("sessionIds", ids); return this; }
    public ImproveOptions nodeName(List<String> names) { put("nodeName", names); return this; }
    public ImproveOptions feedbackAlpha(double a) { put("feedbackAlpha", a); return this; }
    public ImproveOptions tenant(String t) { put("tenant", t); return this; }
}
```

### 4. Java: result types

`RememberResult` wraps its (snake_case, open-ended) tree:

```java
package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/** Result of {@code remember}/{@code rememberEntry}. Keys are snake_case
 *  (Python-SDK parity); exposed via {@link #raw()}. */
public final class RememberResult {
    private final JsonNode root;

    RememberResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() {
        return root;
    }
}
```

`MemifyResult` and `ImproveResult` are camelCase and fully typed:

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

@JsonIgnoreProperties(ignoreUnknown = true)
public record MemifyResult(
        long tripletCount,
        long indexedCount,
        long batchCount,
        boolean alreadyCompleted,
        String priorPipelineRunId) {}
```

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import java.util.List;

@JsonIgnoreProperties(ignoreUnknown = true)
public record ImproveResult(
        List<String> stagesRun,
        MemifyResult memifyResult,
        long feedbackEntriesProcessed,
        long feedbackEntriesApplied,
        long sessionsPersisted,
        long edgesSynced) {}
```

> If any numeric field's JSON type surprises Jackson (e.g. a float where a `long`
> is declared), change the field type to `double`/`Long` and note it in the
> Deviations log. `memifyResult` may be `null` — that maps to a `null` record
> component cleanly.

### 5. Extend `Native.java`

```java
    public static native void remember(long handle, String inputsJson, String datasetName,
            String optsJson, CompletableFuture<String> future);

    public static native void rememberEntry(long handle, String entryJson, String datasetName,
            String sessionId, String optsJson, CompletableFuture<String> future);

    public static native void memify(long handle, String optsJson, CompletableFuture<String> future);

    public static native void improve(long handle, String optsJson, CompletableFuture<String> future);
```

### 6. Add ops to `Cognee.java`

```java
    public CompletableFuture<RememberResult> remember(
            java.util.List<DataInput> inputs, String datasetName, RememberOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.remember(handle(), ai.cognee.internal.Json.toJson(inputs), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(json -> new RememberResult(ai.cognee.internal.Json.tree(json)));
    }

    public CompletableFuture<RememberResult> rememberEntry(
            MemoryEntry entry, String datasetName, String sessionId, String tenant) {
        String optsJson = tenant == null ? "null"
                : ai.cognee.internal.Json.toJson(java.util.Map.of("tenant", tenant));
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.rememberEntry(handle(), ai.cognee.internal.Json.toJson(entry), datasetName,
                sessionId, optsJson, f);
        return f.thenApply(json -> new RememberResult(ai.cognee.internal.Json.tree(json)));
    }

    public CompletableFuture<MemifyResult> memify(MemifyOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.memify(handle(), Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, MemifyResult.class));
    }

    public CompletableFuture<MemifyResult> memify() {
        return memify(null);
    }

    public CompletableFuture<ImproveResult> improve(ImproveOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.improve(handle(), opts.toJson(), f); // opts required (datasetName)
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, ImproveResult.class));
    }
```

### 7. Test `java/src/test/java/ai/cognee/MemoryMarshallingTest.java`

Marshalling/deserialization is unit-testable offline; live memify/improve/remember
are LLM/graph-dependent (covered opportunistically by T11 if credentials exist).

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;

import ai.cognee.internal.Json;
import com.fasterxml.jackson.databind.JsonNode;
import org.junit.jupiter.api.Test;

class MemoryMarshallingTest {
    @Test
    void memoryEntryQaSerializes() {
        MemoryEntry e = MemoryEntry.qa("q?", "a.").context("ctx").feedbackScore(3);
        JsonNode n = Json.tree(Json.toJson(e));
        assertEquals("qa", n.path("type").asText());
        assertEquals("q?", n.path("question").asText());
        assertEquals(3, n.path("feedbackScore").asInt());
    }

    @Test
    void memifyResultDeserializes() {
        String canned = "{\"tripletCount\":5,\"indexedCount\":5,\"batchCount\":1,"
                + "\"alreadyCompleted\":false,\"priorPipelineRunId\":null}";
        MemifyResult r = Json.fromJson(canned, MemifyResult.class);
        assertEquals(5, r.tripletCount());
        assertEquals(1, r.batchCount());
    }

    @Test
    void improveOptionsRequiresDatasetName() {
        ImproveOptions o = new ImproveOptions("ds").feedbackAlpha(0.2);
        JsonNode n = Json.tree(o.toJson());
        assertEquals("ds", n.path("datasetName").asText());
    }
}
```

## Verification

1. `cargo build`/`clippy` for the shim crate → clean.
2. `bash java/scripts/check.sh` → `MemoryMarshallingTest` passes.
3. `scripts/check_all.sh` → green.

## Out of scope

- Live remember/memify/improve round-trips → **T11** (LLM/graph-gated).
- Fully typing `RememberResult` → post-v1 (`raw()` snake_case tree is the v1
  contract; extend when the struct's fields are pinned).
