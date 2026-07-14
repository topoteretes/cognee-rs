# T07 — Retrieval ops: `search`, `recall` + `SearchType` enum + result types

## Objective

After this task `cognee.search(query, opts)` and `cognee.recall(query, opts)`
work through the async machinery, with a `SearchType` enum whose values are the
exact wire strings and `SearchOptions`/`RecallOptions` builders that send the
exact camelCase opts keys. Because `SearchResponse` is an open-ended, deeply
nested structure (a 7-variant tagged union `result`, arbitrary `payload`
values), search/recall results are exposed as thin wrappers over the Jackson
tree with typed convenience accessors and a `raw()` escape hatch — exactly the
design's guidance for open-ended payloads (§4).

## Dependencies & preconditions

- **T05 done** (async machinery). Verify `bash java/scripts/check.sh` passes.
- **T06 done or in parallel:** this task reuses `crate::args`,
  `crate::future::spawn_future`, and the L3 `Options` base. If T06 has not
  landed, create `Options.java` / `args.rs` per T06 §1/§3 first (coordinate the
  shared files). Confirm `Options.java` and `args.rs` exist.
- Read `crates/bindings-common/src/ops/retrieval.rs`: `search(state, query,
  opts)`, `recall(state, query, opts)`, `parse_search_type` (the 15 valid
  SCREAMING_SNAKE_CASE values), and `build_search_request`/`build_scope_input`
  (the opts keys).
- Read `crates/search/src/types/search_result.rs`: `SearchResponse` serializes
  with **snake_case** field names (`search_type`, `result`, `context`, `graphs`,
  `diagnostics`, `datasets`, `only_context`, `use_combined_context`, `verbose`);
  `result` is a `{kind, data}` tagged union.

## Context for this task — exact wire shapes

**`SearchType`** — 15 values, wire = the enum constant name exactly:
`SUMMARIES, CHUNKS, RAG_COMPLETION, TRIPLET_COMPLETION, GRAPH_COMPLETION,
GRAPH_SUMMARY_COMPLETION, CYPHER, NATURAL_LANGUAGE, GRAPH_COMPLETION_COT,
GRAPH_COMPLETION_CONTEXT_EXTENSION, FEELING_LUCKY, FEEDBACK, TEMPORAL,
CODING_RULES, CHUNKS_LEXICAL`. Default `GRAPH_COMPLETION`.

**`search` opts (camelCase)** — `searchType`, `datasets` (string[]), `datasetIds`
(uuid string[]), `topK` (int), `systemPrompt`, `sessionId`, `nodeType`,
`nodeName` (string[]), `onlyContext` (bool), `useCombinedContext` (bool),
`verbose` (bool), `saveInteraction` (bool, defaults true server-side),
`autoFeedbackDetection` (bool). A `userId` key is ignored server-side.

**`recall` opts (camelCase)** — `searchType`, `datasets` (string[]), `topK`
(int, default 10), `autoRoute` (bool, default false), `sessionId`, `scope`
(a string or string[]; valid snake_case scope values: `auto`, `graph`,
`session`, `trace`, `graph_context`, `all`).

**`search` result** = the snake_case `SearchResponse` JSON. **`recall` result**
= hand-built camelCase `{"items":[...],"searchTypeUsed":"..."|null,
"autoRouted":bool,"searchResponse":<SearchResponse>|null}`.

## Steps

### 1. Rust: create `java/cognee-java-jni/src/sdk_retrieval.rs`

Follow the T06 wrapper pattern exactly.

```rust
//! Retrieval ops: search, recall.

use std::sync::Arc;

use jni::JNIEnv;
use jni::objects::{JClass, JObject, JString};
use jni::sys::jlong;

use cognee_bindings_common::ops::retrieval;

use crate::args::{arg_json, arg_string};
use crate::errors::throw_sdk_error;
use crate::future::spawn_future;
use crate::guard_void;
use crate::handle::handle_ref;

/// `search(handle, query, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_search<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    query: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let query = match arg_string(env, &query) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            retrieval::search(&state, &query, &opts).await
        });
    })
}

/// `recall(handle, query, optsJson, future)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_cognee_internal_Native_recall<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    handle: jlong,
    query: JString<'l>,
    opts_json: JString<'l>,
    future: JObject<'l>,
) {
    guard_void(&mut env, |env| {
        let state = unsafe { Arc::clone(handle_ref(handle)) };
        let query = match arg_string(env, &query) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        let opts = match arg_json(env, &opts_json) {
            Ok(v) => v,
            Err(e) => return throw_sdk_error(env, e),
        };
        spawn_future(env, &future, async move {
            retrieval::recall(&state, &query, &opts).await
        });
    })
}
```

Add `mod sdk_retrieval;` to `lib.rs`.

### 2. Java: extend `ai.cognee.internal.Json` with a tree parser

Add to `Json.java`:

```java
    public static com.fasterxml.jackson.databind.JsonNode tree(String json) {
        try {
            return MAPPER.readTree(json);
        } catch (Exception e) {
            throw new IllegalStateException("failed to parse JSON tree: " + json, e);
        }
    }
```

### 3. Java: `SearchType.java`

```java
package ai.cognee;

/** Search strategy; enum constant names are the exact wire values. */
public enum SearchType {
    SUMMARIES,
    CHUNKS,
    RAG_COMPLETION,
    TRIPLET_COMPLETION,
    GRAPH_COMPLETION,
    GRAPH_SUMMARY_COMPLETION,
    CYPHER,
    NATURAL_LANGUAGE,
    GRAPH_COMPLETION_COT,
    GRAPH_COMPLETION_CONTEXT_EXTENSION,
    FEELING_LUCKY,
    FEEDBACK,
    TEMPORAL,
    CODING_RULES,
    CHUNKS_LEXICAL;

    /** Wire string (identical to {@link #name()}). */
    public String wire() {
        return name();
    }

    public static SearchType fromWire(String wire) {
        return valueOf(wire);
    }
}
```

### 4. Java: option builders `SearchOptions.java`, `RecallOptions.java`

```java
package ai.cognee;

import java.util.List;

public final class SearchOptions extends Options {
    public SearchOptions searchType(SearchType t) { put("searchType", t.wire()); return this; }
    public SearchOptions datasets(List<String> d) { put("datasets", d); return this; }
    public SearchOptions datasetIds(List<String> ids) { put("datasetIds", ids); return this; }
    public SearchOptions topK(int n) { put("topK", n); return this; }
    public SearchOptions systemPrompt(String p) { put("systemPrompt", p); return this; }
    public SearchOptions sessionId(String s) { put("sessionId", s); return this; }
    public SearchOptions nodeType(String t) { put("nodeType", t); return this; }
    public SearchOptions nodeName(List<String> n) { put("nodeName", n); return this; }
    public SearchOptions onlyContext(boolean b) { put("onlyContext", b); return this; }
    public SearchOptions useCombinedContext(boolean b) { put("useCombinedContext", b); return this; }
    public SearchOptions verbose(boolean b) { put("verbose", b); return this; }
    public SearchOptions saveInteraction(boolean b) { put("saveInteraction", b); return this; }
    public SearchOptions autoFeedbackDetection(boolean b) { put("autoFeedbackDetection", b); return this; }
}
```

```java
package ai.cognee;

import java.util.List;

public final class RecallOptions extends Options {
    public RecallOptions searchType(SearchType t) { put("searchType", t.wire()); return this; }
    public RecallOptions datasets(List<String> d) { put("datasets", d); return this; }
    public RecallOptions topK(int n) { put("topK", n); return this; }
    public RecallOptions autoRoute(boolean b) { put("autoRoute", b); return this; }
    public RecallOptions sessionId(String s) { put("sessionId", s); return this; }
    /** A single scope, e.g. "graph". */
    public RecallOptions scope(String scope) { put("scope", scope); return this; }
    /** Multiple scopes, e.g. ["graph","session"]. */
    public RecallOptions scope(List<String> scopes) { put("scope", scopes); return this; }
}
```

### 5. Java: result wrappers `SearchResponse.java`, `RecallResult.java`

`SearchResponse` wraps the (snake_case) tree with typed accessors + `raw()`:

```java
package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/**
 * A search result. The underlying payload is open-ended (the {@code result}
 * field is a tagged union and item {@code payload}s are arbitrary), so this
 * exposes the parsed tree via {@link #raw()} plus typed accessors for the
 * stable top-level fields.
 */
public final class SearchResponse {
    private final JsonNode root;

    SearchResponse(JsonNode root) {
        this.root = root;
    }

    /** The full parsed response tree (snake_case keys, as produced by the core). */
    public JsonNode raw() {
        return root;
    }

    public SearchType searchType() {
        return SearchType.fromWire(root.path("search_type").asText());
    }

    /** The `{kind, data}` result union node. */
    public JsonNode result() {
        return root.path("result");
    }

    public boolean onlyContext() {
        return root.path("only_context").asBoolean(false);
    }

    public boolean useCombinedContext() {
        return root.path("use_combined_context").asBoolean(false);
    }

    public boolean verbose() {
        return root.path("verbose").asBoolean(false);
    }
}
```

`RecallResult` (camelCase top level, nested snake_case `searchResponse`):

```java
package ai.cognee;

import com.fasterxml.jackson.databind.JsonNode;

/** A recall result. Top-level keys are camelCase; {@link #searchResponse()} is
 *  the nested (open-ended) search response, or null. */
public final class RecallResult {
    private final JsonNode root;

    RecallResult(JsonNode root) {
        this.root = root;
    }

    public JsonNode raw() {
        return root;
    }

    /** The recalled memory items (open-ended array). */
    public JsonNode items() {
        return root.path("items");
    }

    /** The effective search type, or null when unset. */
    public SearchType searchTypeUsed() {
        JsonNode n = root.path("searchTypeUsed");
        return n.isTextual() ? SearchType.fromWire(n.asText()) : null;
    }

    public boolean autoRouted() {
        return root.path("autoRouted").asBoolean(false);
    }

    /** The nested search response, or null if absent. */
    public SearchResponse searchResponse() {
        JsonNode n = root.path("searchResponse");
        return n.isObject() ? new SearchResponse(n) : null;
    }
}
```

### 6. Extend `Native.java`

```java
    public static native void search(long handle, String query, String optsJson,
            CompletableFuture<String> future);

    public static native void recall(long handle, String query, String optsJson,
            CompletableFuture<String> future);
```

### 7. Add ops to `Cognee.java`

```java
    public CompletableFuture<SearchResponse> search(String query) {
        return search(query, null);
    }

    public CompletableFuture<SearchResponse> search(String query, SearchOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.search(handle(), query, Options.jsonOf(opts), f);
        return f.thenApply(json -> new SearchResponse(ai.cognee.internal.Json.tree(json)));
    }

    public CompletableFuture<RecallResult> recall(String query) {
        return recall(query, null);
    }

    public CompletableFuture<RecallResult> recall(String query, RecallOptions opts) {
        CompletableFuture<String> f = new CompletableFuture<>();
        Native.recall(handle(), query, Options.jsonOf(opts), f);
        return f.thenApply(json -> new RecallResult(ai.cognee.internal.Json.tree(json)));
    }
```

### 8. Test `java/src/test/java/ai/cognee/SearchTypeTest.java`

The wire-value mapping is testable without an LLM (the live search round-trip is
covered by T11's E2E). Deserialization of a canned `SearchResponse` is also
tested offline.

```java
package ai.cognee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import ai.cognee.internal.Json;
import org.junit.jupiter.api.Test;

class SearchTypeTest {
    @Test
    void wireValuesAreConstantNames() {
        assertEquals("GRAPH_COMPLETION", SearchType.GRAPH_COMPLETION.wire());
        assertEquals(SearchType.CHUNKS_LEXICAL, SearchType.fromWire("CHUNKS_LEXICAL"));
        assertEquals(15, SearchType.values().length);
    }

    @Test
    void searchResponseParsesCannedJson() {
        String canned = "{\"search_type\":\"GRAPH_COMPLETION\",\"result\":{\"kind\":\"Text\","
                + "\"data\":\"hello\"},\"only_context\":false,\"use_combined_context\":false,"
                + "\"verbose\":true}";
        SearchResponse r = new SearchResponse(Json.tree(canned));
        assertEquals(SearchType.GRAPH_COMPLETION, r.searchType());
        assertTrue(r.verbose());
        assertEquals("Text", r.result().path("kind").asText());
    }
}
```

## Verification

1. `cargo build --manifest-path java/cognee-java-jni/Cargo.toml` → clean.
2. `cargo clippy --manifest-path java/cognee-java-jni/Cargo.toml --all-targets -- -D warnings`
   → clean.
3. `bash java/scripts/check.sh` → `SearchTypeTest` passes.
4. `scripts/check_all.sh` → green.

## Out of scope

- Live search/recall against an LLM → **T11** E2E (LLM-gated).
- Fully typing the `SearchOutput` union / item payloads → post-v1 (the `raw()`
  tree is the v1 contract for these open-ended fields).
- `NATURAL_LANGUAGE` / `CYPHER` returning `[]` on the default ladybug backend is
  expected core behaviour (not a binding concern).
