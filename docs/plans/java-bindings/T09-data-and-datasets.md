# T09 — Data ops (`forget`, `update`, `pruneData`, `pruneSystem`) + `datasets()`

## Objective

After this task the data-management ops and the `cognee.datasets()` sub-accessor
work through the async machinery: `forget`, `update`, `pruneData`, `pruneSystem`
on `Cognee`, and `list`, `listData`, `has`, `status`, `empty`, `deleteData`,
`deleteAll` on `datasets()`. The deterministic `add` → `datasets().list()` path
is tested without an LLM.

## Dependencies & preconditions

- **T05 done**; **T06 landed** (reuses `crate::args`, `spawn_future`, L3
  `Options`/`DataInput`/`CogneeData`, `CognifyResult`). Verify `bash
  java/scripts/check.sh` passes.
- Read `crates/bindings-common/src/ops/data.rs` and `.../ops/datasets.rs` for the
  exact signatures and the camelCase opts keys (note **owned vs `&`**: `forget`
  takes `target_json: Value` by value + `opts: &Value`; `update` takes
  `new_data_json: Value` by value; `dataset_status` takes `ids_json: Value` by
  value; the rest take `&str`/`&Value`).
- Read `ts/cognee-ts-neon/src/{sdk_data,sdk_datasets}.rs` for arg order.

## Context for this task — exact wire shapes

**Casing is mixed** (this drives the Java result modeling):
- `DeleteResult`, `Dataset`, `Data` are plain serde structs → **snake_case**
  keys (e.g. `deleted_data`, `owner_id`, `created_at`).
- `forget`/`update`/`pruneSystem` wrappers are hand-built → **camelCase** top
  level, but their nested `deleteResult`/`newData` are the snake_case structs.

Shapes:
- **`forget` target** (union): `{"kind":"item","dataId":"<uuid>","dataset":{"name":"…"}|{"id":"<uuid>"}}`
  / `{"kind":"dataset","dataset":{…}}` / `{"kind":"all"}`. Result:
  `{"target":…,"deleteResult":<DeleteResult>}`.
- **`update`**: `(dataId, newData, datasetName, opts?)`; opts (camelCase):
  `{datasetId?, tenant?, nodeSet?(string[]), preferredLoaders?(obj), incrementalLoading?(bool)}`.
  Result: `{"deletedDataId":"…","deleteResult":<DeleteResult>,"newData":[<Data>],"cognifyResult":<CognifyResult>|null}`.
- **`pruneData`**: no args, result `null`.
- **`pruneSystem` opts**: `{pruneGraph?, pruneVector?, pruneMetadata?, pruneCache?}`.
  Result (camelCase): `{"dataPruned":bool,"graphPruned":bool,"vectorPruned":bool,"metadataPruned":bool,"cachePruned":bool}`.
- **datasets:** `list`→`[<Dataset>]`; `listData(datasetId)`→`[<Data>]`;
  `has(datasetId)`→bool; `status(datasetIds[])`→`{"<uuid>":{"<pipeline>":"<status>"}}`;
  `empty(datasetId)`→`<DeleteResult>`; `deleteData(datasetId,dataId,opts?)`→`<DeleteResult>`
  (opts `{softDelete?, deleteDatasetIfEmpty?}`); `deleteAll()`→`[<DeleteResult>]`.

## Steps

### 1. Rust: create `java/cognee-java-jni/src/sdk_data.rs`

```rust
//! Data ops: forget, update, prune_data, prune_system.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::data;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::handle_ref;

/// `forget(handle, targetJson, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_forget<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    target_json: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let target = match arg_json(env, &target_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            data::forget(&state, target, &opts).await
        });
    })
}

/// `update(handle, dataId, newDataJson, datasetName, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_update<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    data_id: JString<'l>,
    new_data_json: JString<'l>,
    dataset_name: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let data_id = match arg_string(env, &data_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let new_data = match arg_json(env, &new_data_json) {
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
            data::update(&state, &data_id, new_data, &dataset, &opts).await
        });
    })
}

/// `pruneData(handle, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_pruneData<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        spawn_future(env, &future, async move { data::prune_data(&state).await });
    })
}

/// `pruneSystem(handle, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_pruneSystem<'l>(
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
            data::prune_system(&state, &opts).await
        });
    })
}
```

### 2. Rust: create `java/cognee-java-jni/src/sdk_datasets.rs`

Full `list_datasets` + `delete_data` as the two shapes; the rest follow the
identical pattern — implement each per the table.

```rust
//! Dataset ops: list, listData, has, status, empty, deleteData, deleteAll.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::datasets;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::handle_ref;

/// `listDatasets(handle, future)` — no extra args.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_listDatasets<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        spawn_future(env, &future, async move {
            datasets::list_datasets(&state).await
        });
    })
}

/// `deleteData(handle, datasetId, dataId, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_deleteData<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    dataset_id: JString<'l>,
    data_id: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let dataset_id = match arg_string(env, &dataset_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let data_id = match arg_string(env, &data_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            datasets::delete_data(&state, &dataset_id, &data_id, &opts).await
        });
    })
}
```

Implement the remaining five in the same file, each a copy of the closest shape
above with these exact bodies:

| Native fn (mangled suffix) | Args after `handle` | `spawn_future` body |
|---|---|---|
| `listData` | `dataset_id: JString` | `datasets::list_data(&state, &dataset_id).await` |
| `hasData` | `dataset_id: JString` | `datasets::has_data(&state, &dataset_id).await` |
| `datasetStatus` | `ids_json: JString` | `datasets::dataset_status(&state, ids).await` where `let ids = arg_json(...)?` (passed **by value**) |
| `emptyDataset` | `dataset_id: JString` | `datasets::empty_dataset(&state, &dataset_id).await` |
| `deleteAllDatasets` | *(none)* | `datasets::delete_all_datasets(&state).await` |

(`listData`/`hasData`/`emptyDataset` read `dataset_id` with `arg_string`;
`datasetStatus` reads `ids_json` with `arg_json` and passes the owned `Value`.)

Add `mod sdk_data;` and `mod sdk_datasets;` to `lib.rs`.

### 3. Java: result types

**`CogneeDataset.java`** (id/name are casing-agnostic; ignore the rest):

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeDataset(String id, String name) {}
```

**`DeleteResult.java`** — snake_case struct wrapped with camelCase accessors:

```java
package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.ArrayList;
import java.util.List;

/** Result of a delete/empty/forget op. Underlying keys are snake_case. */
public final class DeleteResult {
    private final JsonNode root;

    DeleteResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() {
        return root;
    }

    public int deletedData() { return root.path("deleted_data").asInt(); }
    public int deletedDatasets() { return root.path("deleted_datasets").asInt(); }
    public int deletedGraphNodes() { return root.path("deleted_graph_nodes").asInt(); }
    public int deletedVectorPoints() { return root.path("deleted_vector_points").asInt(); }
    public boolean prunedSessions() { return root.path("pruned_sessions").asBoolean(false); }

    public List<String> warnings() {
        List<String> out = new ArrayList<>();
        root.path("warnings").forEach(n -> out.add(n.asText()));
        return out;
    }
}
```

**`PruneResult.java`** (camelCase, clean record):

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

@JsonIgnoreProperties(ignoreUnknown = true)
public record PruneResult(
        boolean dataPruned,
        boolean graphPruned,
        boolean vectorPruned,
        boolean metadataPruned,
        boolean cachePruned) {}
```

**`ForgetResult.java`** and **`UpdateResult.java`** — camelCase wrappers over the
tree with typed accessors:

```java
package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

public final class ForgetResult {
    private final JsonNode root;

    ForgetResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() { return root; }
    public JsonNode target() { return root.path("target"); }
    public DeleteResult deleteResult() { return new DeleteResult(root.path("deleteResult")); }
}
```

```java
package ai.cognee;

import ai.cognee.internal.Json;
import com.fasterxml.jackson.databind.JsonNode;

public final class UpdateResult {
    private final JsonNode root;

    UpdateResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() { return root; }
    public String deletedDataId() { return root.path("deletedDataId").asText(); }
    public DeleteResult deleteResult() { return new DeleteResult(root.path("deleteResult")); }
    public JsonNode newData() { return root.path("newData"); }

    /** The re-cognify result, or null when nothing was re-cognified. */
    public CognifyResult cognifyResult() {
        JsonNode n = root.path("cognifyResult");
        return n.isObject() ? Json.fromNode(n, CognifyResult.class) : null;
    }
}
```

Add to `ai.cognee.internal.Json`:

```java
    public static <T> T fromNode(com.fasterxml.jackson.databind.JsonNode node, Class<T> type) {
        try {
            return MAPPER.treeToValue(node, type);
        } catch (Exception e) {
            throw new IllegalStateException("failed to convert JSON node to " + type, e);
        }
    }
```

### 4. Java: option builders `UpdateOptions.java`, `PruneSystemOptions.java`, `DeleteDataOptions.java`

```java
package ai.cognee;

import java.util.List;
import java.util.Map;

public final class UpdateOptions extends Options {
    public UpdateOptions datasetId(String id) { put("datasetId", id); return this; }
    public UpdateOptions tenant(String t) { put("tenant", t); return this; }
    public UpdateOptions nodeSet(List<String> s) { put("nodeSet", s); return this; }
    public UpdateOptions preferredLoaders(Map<String, String> m) { put("preferredLoaders", m); return this; }
    public UpdateOptions incrementalLoading(boolean b) { put("incrementalLoading", b); return this; }
}
```

```java
package ai.cognee;

public final class PruneSystemOptions extends Options {
    public PruneSystemOptions pruneGraph(boolean b) { put("pruneGraph", b); return this; }
    public PruneSystemOptions pruneVector(boolean b) { put("pruneVector", b); return this; }
    public PruneSystemOptions pruneMetadata(boolean b) { put("pruneMetadata", b); return this; }
    public PruneSystemOptions pruneCache(boolean b) { put("pruneCache", b); return this; }
}
```

```java
package ai.cognee;

public final class DeleteDataOptions extends Options {
    public DeleteDataOptions softDelete(boolean b) { put("softDelete", b); return this; }
    public DeleteDataOptions deleteDatasetIfEmpty(boolean b) { put("deleteDatasetIfEmpty", b); return this; }
}
```

### 5. Java: `ForgetTarget.java` builder

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonValue;
import java.util.Map;

/** Discriminated target for {@link Cognee#forget}. */
public final class ForgetTarget {
    private final Map<String, Object> fields;

    private ForgetTarget(Map<String, Object> fields) {
        this.fields = fields;
    }

    @JsonValue
    Map<String, Object> fields() {
        return fields;
    }

    public static ForgetTarget item(String dataId, ForgetTarget.DatasetRef dataset) {
        return new ForgetTarget(Map.of("kind", "item", "dataId", dataId, "dataset", dataset.map));
    }

    public static ForgetTarget dataset(ForgetTarget.DatasetRef dataset) {
        return new ForgetTarget(Map.of("kind", "dataset", "dataset", dataset.map));
    }

    public static ForgetTarget all() {
        return new ForgetTarget(Map.of("kind", "all"));
    }

    /** `{name:…}` or `{id:…}` dataset reference. */
    public static final class DatasetRef {
        final Map<String, Object> map;

        private DatasetRef(Map<String, Object> map) {
            this.map = map;
        }

        public static DatasetRef byName(String name) {
            return new DatasetRef(Map.of("name", name));
        }

        public static DatasetRef byId(String id) {
            return new DatasetRef(Map.of("id", id));
        }
    }
}
```

### 6. Java: `CogneeDatasets.java` sub-accessor

```java
package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;

/** Dataset-management operations. */
public final class CogneeDatasets {
    private final Cognee cognee;

    CogneeDatasets(Cognee cognee) {
        this.cognee = cognee;
    }

    public CompletableFuture<List<CogneeDataset>> list() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.listDatasets(cognee.handle(), f);
        return f.thenApply(json -> Json.fromJson(json, new TypeReference<List<CogneeDataset>>() {}));
    }

    public CompletableFuture<List<CogneeData>> listData(String datasetId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.listData(cognee.handle(), datasetId, f);
        return f.thenApply(json -> Json.fromJson(json, new TypeReference<List<CogneeData>>() {}));
    }

    public CompletableFuture<Boolean> has(String datasetId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.hasData(cognee.handle(), datasetId, f);
        return f.thenApply(json -> Json.fromJson(json, Boolean.class));
    }

    public CompletableFuture<Map<String, Object>> status(List<String> datasetIds) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.datasetStatus(cognee.handle(), Json.toJson(datasetIds), f);
        return f.thenApply(json -> Json.fromJson(json, new TypeReference<Map<String, Object>>() {}));
    }

    public CompletableFuture<DeleteResult> empty(String datasetId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.emptyDataset(cognee.handle(), datasetId, f);
        return f.thenApply(json -> new DeleteResult(Json.tree(json)));
    }

    public CompletableFuture<DeleteResult> deleteData(String datasetId, String dataId) {
        return deleteData(datasetId, dataId, null);
    }

    public CompletableFuture<DeleteResult> deleteData(
            String datasetId, String dataId, DeleteDataOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.deleteData(cognee.handle(), datasetId, dataId, Options.jsonOf(opts), f);
        return f.thenApply(json -> new DeleteResult(Json.tree(json)));
    }

    public CompletableFuture<List<DeleteResult>> deleteAll() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.deleteAllDatasets(cognee.handle(), f);
        return f.thenApply(json -> {
            List<DeleteResult> out = new ArrayList<>();
            for (JsonNode n : Json.tree(json)) {
                out.add(new DeleteResult(n));
            }
            return out;
        });
    }
}
```

### 7. Extend `Native.java`

```java
    // data ops
    public static native void forget(long handle, String targetJson, String optsJson,
            CompletableFuture<String> future);
    public static native void update(long handle, String dataId, String newDataJson,
            String datasetName, String optsJson, CompletableFuture<String> future);
    public static native void pruneData(long handle, CompletableFuture<String> future);
    public static native void pruneSystem(long handle, String optsJson,
            CompletableFuture<String> future);
    // dataset ops
    public static native void listDatasets(long handle, CompletableFuture<String> future);
    public static native void listData(long handle, String datasetId, CompletableFuture<String> future);
    public static native void hasData(long handle, String datasetId, CompletableFuture<String> future);
    public static native void datasetStatus(long handle, String datasetIdsJson,
            CompletableFuture<String> future);
    public static native void emptyDataset(long handle, String datasetId,
            CompletableFuture<String> future);
    public static native void deleteData(long handle, String datasetId, String dataId,
            String optsJson, CompletableFuture<String> future);
    public static native void deleteAllDatasets(long handle, CompletableFuture<String> future);
```

### 8. Add ops + `datasets()` accessor to `Cognee.java`

```java
    private CogneeDatasets datasets;

    public synchronized CogneeDatasets datasets() {
        if (datasets == null) {
            datasets = new CogneeDatasets(this);
        }
        return datasets;
    }

    public CompletableFuture<ForgetResult> forget(ForgetTarget target) {
        return forget(target, null);
    }

    public CompletableFuture<ForgetResult> forget(ForgetTarget target, String tenant) {
        String optsJson = tenant == null ? "null"
                : ai.cognee.internal.Json.toJson(java.util.Map.of("tenant", tenant));
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.forget(handle(), ai.cognee.internal.Json.toJson(target), optsJson, f);
        return f.thenApply(json -> new ForgetResult(ai.cognee.internal.Json.tree(json)));
    }

    public CompletableFuture<UpdateResult> update(
            String dataId, java.util.List<DataInput> newData, String datasetName, UpdateOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.update(handle(), dataId, ai.cognee.internal.Json.toJson(newData), datasetName,
                Options.jsonOf(opts), f);
        return f.thenApply(json -> new UpdateResult(ai.cognee.internal.Json.tree(json)));
    }

    public CompletableFuture<Void> pruneData() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.pruneData(handle(), f);
        return f.thenApply(s -> null);
    }

    public CompletableFuture<PruneResult> pruneSystem(PruneSystemOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.pruneSystem(handle(), Options.jsonOf(opts), f);
        return f.thenApply(json -> ai.cognee.internal.Json.fromJson(json, PruneResult.class));
    }

    public CompletableFuture<PruneResult> pruneSystem() {
        return pruneSystem(null);
    }
```

### 9. Test `java/src/test/java/ai/cognee/DatasetsTest.java`

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class DatasetsTest {
    @Test
    void addThenListIsDeterministic(@TempDir Path dir) {
        try (Cognee cognee = new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()))) {
            cognee.add(List.of(DataInput.text("x")), "ds").join();
            List<CogneeDataset> ds = cognee.datasets().list().join();
            assertTrue(ds.stream().anyMatch(d -> "ds".equals(d.name())));
            assertEquals(Boolean.TRUE, cognee.datasets()
                    .has(ds.stream().filter(d -> "ds".equals(d.name())).findFirst().get().id())
                    .join());
        }
    }
}
```

## Verification

1. `cargo build`/`clippy` (shim crate) → clean.
2. `bash java/scripts/check.sh` → `DatasetsTest` passes.
3. `scripts/check_all.sh` → green.

## Out of scope

- Sessions/admin/notebooks → **T10**. Visualization/statics → **T11**.
- Fully typing `Data`/`DeleteResult` beyond the accessors above → extend when a
  consumer needs a field; `raw()` covers the rest.
