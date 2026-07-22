# T10 — Session, user/admin, and notebook ops

## Objective

After this task `cognee.sessions()`, `cognee.users()` (getOrCreateDefault + the
two pipeline-run resets — see the divergence note below), and
`cognee.notebooks()` work through the async machinery.

## Dependencies & preconditions

- **T05 done**; **T06 landed** (reuses `crate::args`, `spawn_future`, L3
  `Options`, `Json`). Verify `bash java/scripts/check.sh` passes.
- Read `crates/bindings-common/src/ops/sessions.rs` and `.../ops/admin.rs` for
  exact signatures. Note `run_create_notebook(state, name: String, cells: Value,
  deletable: bool)` takes `name` owned and a `bool`.
- Read `ts/cognee-ts-neon/src/sdk_admin.rs` for arg order (all admin + session +
  notebook ops live in that one neon file).

**Divergence (recorded in README §2):** the design §4 split resets into an
`admin()` accessor; the TS reference puts `getOrCreateDefault`,
`resetPipelineRunStatus`, and `resetDatasetPipelineRunStatus` all on `users`.
This task follows the TS reality — Java exposes `cognee.users()` with those three
and has **no** `admin()` accessor. (The Rust file is still named `sdk_admin.rs`.)

## Context for this task — exact wire shapes

- **sessions:** `getSession(sessionId, opts{lastN?})`→`[<SessionQAEntry>]`;
  `addFeedback(sessionId, qaId, opts{feedbackText?, feedbackScore?})`→bool;
  `deleteFeedback(sessionId, qaId)`→bool; `getGraphContext(sessionId)`→string|null;
  `setGraphContext(sessionId, context)`→null.
- **users/admin:** `getOrCreateDefaultUser()`→`<User>`;
  `resetPipelineRunStatus(datasetId, pipelineName)`→null;
  `resetDatasetPipelineRunStatus(datasetId)`→null.
- **notebooks:** `listNotebooks()`→`[<Notebook>]`;
  `createNotebook(name, cells(array), deletable(bool))`→`<Notebook>`;
  `updateNotebook(id, patch{name?, cells?})`→`<Notebook>`|null;
  `deleteNotebook(id)`→bool.

## Steps

### 1. Rust: create `java/cognee-java-jni/src/sdk_sessions.rs`

Worked examples for the two non-trivial shapes (opts + boolean result):

```rust
//! Session ops: getSession, addFeedback, deleteFeedback, get/setGraphContext.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::sessions;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::handle_ref;

/// `getSession(handle, sessionId, optsJson, future)` — opts `{lastN?}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_getSession<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    session_id: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let session = match arg_string(env, &session_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            sessions::run_get_session(&state, &session, &opts).await
        });
    })
}

/// `addFeedback(handle, sessionId, qaId, optsJson, future)`
/// — opts `{feedbackText?, feedbackScore?}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_addFeedback<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    session_id: JString<'l>,
    qa_id: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let session = match arg_string(env, &session_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let qa = match arg_string(env, &qa_id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            sessions::run_add_feedback(&state, &session, &qa, &opts).await
        });
    })
}
```

Implement the remaining three in the same file (same pattern):

| Native fn | Args after `handle` | `spawn_future` body |
|---|---|---|
| `deleteFeedback` | `session_id, qa_id: JString` | `sessions::run_delete_feedback(&state, &session, &qa).await` |
| `getGraphContext` | `session_id: JString` | `sessions::run_get_graph_context(&state, &session).await` |
| `setGraphContext` | `session_id, context: JString` | `sessions::run_set_graph_context(&state, &session, &context).await` |

### 2. Rust: create `java/cognee-java-jni/src/sdk_admin.rs`

Worked examples for the notebook create (with a `jboolean`) and update (JSON
patch); the rest are one-liners per the table.

```rust
//! Admin/user/notebook ops.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jlong};

use cognee_bindings_common::ops::admin;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::handle_ref;

/// `createNotebook(handle, name, cellsJson, deletable, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_createNotebook<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    name: JString<'l>,
    cells_json: JString<'l>,
    deletable: jboolean,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let name = match arg_string(env, &name) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        // Default absent cells to an empty JSON array (run_create_notebook wants an array).
        let cells = match arg_json(env, &cells_json) {
            Ok(serde_json::Value::Null) => serde_json::Value::Array(vec![]),
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let deletable = deletable != 0;
        spawn_future(env, &future, async move {
            admin::run_create_notebook(&state, name, cells, deletable).await
        });
    })
}

/// `updateNotebook(handle, id, patchJson, future)` — patch `{name?, cells?}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_updateNotebook<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    id: JString<'l>,
    patch_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let id = match arg_string(env, &id) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let patch = match arg_json(env, &patch_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            admin::run_update_notebook(&state, &id, patch).await
        });
    })
}
```

Implement the remaining five one-liners in the same file:

| Native fn | Args after `handle` | `spawn_future` body |
|---|---|---|
| `getOrCreateDefaultUser` | *(none)* | `admin::run_get_or_create_default_user(&state).await` |
| `resetPipelineRunStatus` | `dataset_id, pipeline_name: JString` | `admin::run_reset_pipeline_run_status(&state, &dataset_id, &pipeline_name).await` |
| `resetDatasetPipelineRunStatus` | `dataset_id: JString` | `admin::run_reset_dataset_pipeline_run_status(&state, &dataset_id).await` |
| `listNotebooks` | *(none)* | `admin::run_list_notebooks(&state).await` |
| `deleteNotebook` | `id: JString` | `admin::run_delete_notebook(&state, &id).await` |

Add `mod sdk_sessions;` and `mod sdk_admin;` to `lib.rs`.

### 3. Extend `Native.java`

```java
    // sessions
    public static native void getSession(long handle, String sessionId, String optsJson,
            CompletableFuture<String> future);
    public static native void addFeedback(long handle, String sessionId, String qaId,
            String optsJson, CompletableFuture<String> future);
    public static native void deleteFeedback(long handle, String sessionId, String qaId,
            CompletableFuture<String> future);
    public static native void getGraphContext(long handle, String sessionId,
            CompletableFuture<String> future);
    public static native void setGraphContext(long handle, String sessionId, String context,
            CompletableFuture<String> future);
    // users / admin
    public static native void getOrCreateDefaultUser(long handle, CompletableFuture<String> future);
    public static native void resetPipelineRunStatus(long handle, String datasetId,
            String pipelineName, CompletableFuture<String> future);
    public static native void resetDatasetPipelineRunStatus(long handle, String datasetId,
            CompletableFuture<String> future);
    // notebooks
    public static native void listNotebooks(long handle, CompletableFuture<String> future);
    public static native void createNotebook(long handle, String name, String cellsJson,
            boolean deletable, CompletableFuture<String> future);
    public static native void updateNotebook(long handle, String id, String patchJson,
            CompletableFuture<String> future);
    public static native void deleteNotebook(long handle, String id,
            CompletableFuture<String> future);
```

### 4. Java: result types + `CogneeNotebook`, `CogneeUser`

```java
package ai.cognee;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;

@JsonIgnoreProperties(ignoreUnknown = true)
public record CogneeUser(String id, String email) {}
```

```java
package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/** A notebook. {@code cells} are open-ended, exposed via the tree. */
public final class CogneeNotebook {
    private final JsonNode root;

    CogneeNotebook(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() { return root; }
    public String id() { return root.path("id").asText(); }
    public String name() { return root.path("name").asText(); }
    public JsonNode cells() { return root.path("cells"); }
}
```

### 5. Java: sub-accessors

**`CogneeSessions.java`**:

```java
package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import com.fasterxml.jackson.core.type.TypeReference;
import java.util.List;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.concurrent.CompletableFuture;

public final class CogneeSessions {
    private final Cognee cognee;

    CogneeSessions(Cognee cognee) {
        this.cognee = cognee;
    }

    public CompletableFuture<List<Map<String, Object>>> get(String sessionId) {
        return get(sessionId, null);
    }

    /** {@code lastN} limits the number of returned QA entries (null = all). */
    public CompletableFuture<List<Map<String, Object>>> get(String sessionId, Integer lastN) {
        String opts = lastN == null ? "null" : Json.toJson(Map.of("lastN", lastN));
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.getSession(cognee.handle(), sessionId, opts, f);
        return f.thenApply(json ->
                Json.fromJson(json, new TypeReference<List<Map<String, Object>>>() {}));
    }

    public CompletableFuture<Boolean> addFeedback(
            String sessionId, String qaId, String feedbackText, Integer feedbackScore) {
        Map<String, Object> opts = new LinkedHashMap<>();
        if (feedbackText != null) opts.put("feedbackText", feedbackText);
        if (feedbackScore != null) opts.put("feedbackScore", feedbackScore);
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.addFeedback(cognee.handle(), sessionId, qaId, Json.toJson(opts), f);
        return f.thenApply(json -> Json.fromJson(json, Boolean.class));
    }

    public CompletableFuture<Boolean> deleteFeedback(String sessionId, String qaId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.deleteFeedback(cognee.handle(), sessionId, qaId, f);
        return f.thenApply(json -> Json.fromJson(json, Boolean.class));
    }

    /** Returns the stored graph context, or null if none. */
    public CompletableFuture<String> getGraphContext(String sessionId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.getGraphContext(cognee.handle(), sessionId, f);
        // The op completes with a JSON string ("..." or null).
        return f.thenApply(json -> Json.fromJson(json, String.class));
    }

    public CompletableFuture<Void> setGraphContext(String sessionId, String context) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.setGraphContext(cognee.handle(), sessionId, context, f);
        return f.thenApply(s -> null);
    }
}
```

**`CogneeUsers.java`**:

```java
package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import java.util.concurrent.CompletableFuture;

public final class CogneeUsers {
    private final Cognee cognee;

    CogneeUsers(Cognee cognee) {
        this.cognee = cognee;
    }

    public CompletableFuture<CogneeUser> getOrCreateDefault() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.getOrCreateDefaultUser(cognee.handle(), f);
        return f.thenApply(json -> Json.fromJson(json, CogneeUser.class));
    }

    public CompletableFuture<Void> resetPipelineRunStatus(String datasetId, String pipelineName) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.resetPipelineRunStatus(cognee.handle(), datasetId, pipelineName, f);
        return f.thenApply(s -> null);
    }

    public CompletableFuture<Void> resetDatasetPipelineRunStatus(String datasetId) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.resetDatasetPipelineRunStatus(cognee.handle(), datasetId, f);
        return f.thenApply(s -> null);
    }
}
```

**`CogneeNotebooks.java`**:

```java
package ai.cognee;

import ai.cognee.internal.Json;
import ai.cognee.internal.Native;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;

public final class CogneeNotebooks {
    private final Cognee cognee;

    CogneeNotebooks(Cognee cognee) {
        this.cognee = cognee;
    }

    public CompletableFuture<List<CogneeNotebook>> list() {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.listNotebooks(cognee.handle(), f);
        return f.thenApply(json -> {
            List<CogneeNotebook> out = new ArrayList<>();
            for (JsonNode n : Json.tree(json)) {
                out.add(new CogneeNotebook(n));
            }
            return out;
        });
    }

    public CompletableFuture<CogneeNotebook> create(String name) {
        return create(name, null, true);
    }

    public CompletableFuture<CogneeNotebook> create(String name, List<?> cells, boolean deletable) {
        String cellsJson = cells == null ? "null" : Json.toJson(cells);
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.createNotebook(cognee.handle(), name, cellsJson, deletable, f);
        return f.thenApply(json -> new CogneeNotebook(Json.tree(json)));
    }

    /** Returns the updated notebook, or null if not found. */
    public CompletableFuture<CogneeNotebook> update(String id, String name, List<?> cells) {
        Map<String, Object> patch = new LinkedHashMap<>();
        if (name != null) patch.put("name", name);
        if (cells != null) patch.put("cells", cells);
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.updateNotebook(cognee.handle(), id, Json.toJson(patch), f);
        return f.thenApply(json -> {
            JsonNode n = Json.tree(json);
            return n.isNull() ? null : new CogneeNotebook(n);
        });
    }

    public CompletableFuture<Boolean> delete(String id) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.deleteNotebook(cognee.handle(), id, f);
        return f.thenApply(json -> Json.fromJson(json, Boolean.class));
    }
}
```

### 6. Add accessors to `Cognee.java`

```java
    private CogneeSessions sessions;
    private CogneeUsers users;
    private CogneeNotebooks notebooks;

    public synchronized CogneeSessions sessions() {
        if (sessions == null) sessions = new CogneeSessions(this);
        return sessions;
    }

    public synchronized CogneeUsers users() {
        if (users == null) users = new CogneeUsers(this);
        return users;
    }

    public synchronized CogneeNotebooks notebooks() {
        if (notebooks == null) notebooks = new CogneeNotebooks(this);
        return notebooks;
    }
```

### 7. Test `java/src/test/java/ai/cognee/SessionsAdminTest.java`

Deterministic parts (no LLM): default user, notebook CRUD, and a session's
graph-context round-trip all work against sqlite in a temp dir.

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Path;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class SessionsAdminTest {
    private Cognee handle(Path dir) {
        return new Cognee(Map.of(
                "data_root_directory", dir.resolve("data").toString(),
                "system_root_directory", dir.resolve("sys").toString()));
    }

    @Test
    void defaultUserResolves(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeUser u = cognee.users().getOrCreateDefault().join();
            assertNotNull(u.id());
        }
    }

    @Test
    void notebookCrud(@TempDir Path dir) {
        try (Cognee cognee = handle(dir)) {
            CogneeNotebook nb = cognee.notebooks().create("nb1").join();
            assertEquals("nb1", nb.name());
            assertTrue(cognee.notebooks().delete(nb.id()).join());
        }
    }
}
```

> If notebook create/list requires a warmed handle, call `cognee.warm().join()`
> first. If `getOrCreateDefault`/notebooks need an LLM on this build, gate the
> test on `OPENAI_URL`/`OPENAI_TOKEN` (see T11's skip pattern) and record it in
> the Deviations log.

## Verification

1. `cargo build`/`clippy` (shim crate) → clean.
2. `bash java/scripts/check.sh` → `SessionsAdminTest` passes (or skips cleanly if
   an op needs credentials on this build).
3. `scripts/check_all.sh` → green.

## Out of scope

- Visualization + static setup methods + LLM-gated E2E → **T11**.
- Fully typing `SessionQAEntry`/`Notebook`/`User` beyond the accessors → extend
  when a consumer needs a field.
