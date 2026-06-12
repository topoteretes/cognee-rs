# Phase 5 — Retrieval: search / recall

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** the round-trip closes: `add → cognify → search`/`recall` from C, over all 15
search types. Reference: `js/cognee-neon/src/sdk_retrieval.rs` + `js/src/types.ts`
(`CogneeSearchOptions`, `CogneeRecallOptions`, `RecallScopeString`, `SearchTypeString`).

## Prerequisites

Phase 4 (search needs cognified data).

## Exported functions

Async-only (D4), Phase-2 conventions:

| Function | Inputs | Result JSON |
|---|---|---|
| `cg_sdk_search` | `query`, `opts_json` | `SearchResponse` (pass-through `serde_json::to_string`, same shape as TS) |
| `cg_sdk_recall` | `query`, `opts_json` | `{items[], searchTypeUsed, autoRouted, searchResponse}` |

`opts_json` fields (identical to TS, camelCase):

- search: `searchType` (one of the 15 `SCREAMING_SNAKE_CASE` strings), `datasets[]`,
  `datasetIds[]` (UUID strings), `topK`, `systemPrompt`, `sessionId`, `nodeType`,
  `nodeName[]`, `onlyContext`, `useCombinedContext`, `verbose`, `saveInteraction`
  (default `true` when absent, matching Python SDK behavior), `autoFeedbackDetection`.
  **Note:** `userId` from opts is ignored — `user_id` in `SearchRequest` is always set
  from the handle's `owner_id` so dataset-name resolution works (same as neon reference).
- recall: `searchType`, `datasets[]`, `topK` (default `10` when absent), `autoRoute`
  (default `false` when absent), `sessionId`,
  `scope` (string or string array: `auto|graph|session|trace|graph_context`).
  **Note:** there is no `all` scope variant — `RecallScope` has five variants
  (`Auto`, `Graph`, `Session`, `Trace`, `GraphContext`).

## Inherit TS decisions verbatim

- `SearchType` parsed via `serde_json::from_value(Value::String(s))` with the
  `SCREAMING_SNAKE_CASE` serde attribute — the same path as the HTTP server, guaranteed sync.
  Invalid string → `CG_ERR_SDK_VALIDATION` (code 14) listing all 15 valid values.
- `ScopeInput::Single/Many` built directly from strings; empty array → `None` passed to
  `recall()`, which applies its `Auto` default.
- `RecallResult` does **not** derive `Serialize`; JSON is hand-built with camelCase keys:
  `items` (serialize via `serde_json::to_value`), `searchTypeUsed` (nullable),
  `autoRouted` (bool), `searchResponse` (nullable).
- `SearchResponse` **does** derive `Serialize` — pass through `serde_json::to_string` directly.
  No `cognee_bindings_common::wire` helper is needed for search/recall (unlike cognify).

## Import paths

```rust
use cognee_lib::api::{ScopeInput, normalize_scope, recall};
use cognee_lib::search::{SearchRequest, SearchType};
```

`ScopeInput` and `normalize_scope` are re-exported from `cognee_lib::api::recall`
(which re-exports them from `cognee_search::recall_scope`). Both `recall` and
`normalize_scope` are in scope via `cognee_lib::api::recall`.

## Session borrow pattern for `recall()`

`CogneeServices` holds `session_store: Arc<dyn SessionStore>` and
`session_manager: Arc<SessionManager>`. The `recall()` free function takes
`Option<&dyn SessionStore>` and `Option<&SessionManager>`. Clone the Arcs before
the async block then call `.as_ref()` inside:

```rust
let session_store_ref = Arc::clone(&svc.session_store);
let session_manager_ref = Arc::clone(&svc.session_manager);
// … move into async block …
recall(…, Some(session_store_ref.as_ref()), Some(session_manager_ref.as_ref()), scope_opt).await
```

## Tasks

1. **`capi/cognee-capi/src/sdk_retrieval.rs`** — two async ops via the shared facade.
   - `run_search`: `state.services().await?` → `state.owner_id().await?` →
     `build_search_request(query, opts, owner_id)?` → `svc.search_orchestrator.search(&req).await`
     → `serde_json::to_string(&response)`.
   - `run_recall`: services + owner_id → parse opts fields
     (`query_type`, `datasets`, `top_k`, `auto_route`, `session_id`, `scope_input`) →
     `normalize_scope(scope_input)` → `recall(…)` → hand-build JSON.
   - `parse_search_type(s: &str) -> Result<SearchType, SdkError>` helper (returns
     `SdkError::Validation` with all 15 valid values on error).
   - `build_search_request(query, opts, owner_id)` helper (mirrors neon reference exactly).
   - `build_scope_input(opts)` helper — returns `Ok(None)` for absent/null; `Err` for
     non-string non-array types.
   - C-exported `cg_sdk_search` and `cg_sdk_recall` follow the Phase-4 boilerplate:
     null-check `sdk`, parse C strings via `parse_c_str_or_fire`, `spawn_sdk_op`.
     Both `query` and `opts_json` should be handled the same as `dataset_name` /
     `opts_json` in `cg_sdk_cognify` (`opts_json` NULL → `serde_json::Value::Null`).
2. **Register in `capi/cognee-capi/src/lib.rs`**: add `pub mod sdk_retrieval;`
   (following the `pub mod sdk_ops;` entry at line 26).
3. **Tier-A smoke `capi/examples/sdk_retrieval_smoke.c`** — mock-backed, no LLM needed:
   - invalid `searchType` string → callback fires `CG_ERR_SDK_VALIDATION`.
   - all 15 valid `searchType` strings accepted (parsed without error via a dedicated
     string-mapping sub-test that does not actually execute a live search).
   - valid `cg_sdk_search` call against empty mock store → `CG_OK`, well-formed JSON array.
   - valid `cg_sdk_recall` call with each scope variant → `CG_OK`, well-formed JSON object
     with `items`, `searchTypeUsed`, `autoRouted`, `searchResponse` keys.
4. **Wire `sdk_retrieval_smoke` into `capi/examples/CMakeLists.txt`** — follow the
   `sdk_config_smoke` pattern (lines 31–35).
5. **Wire `sdk_retrieval_smoke` into `capi/scripts/check.sh`** — add a Phase 5 Tier-A
   section after the Phase 4 block (line ~110), running with `MOCK_EMBEDDING=true`.
6. **Tier-B example `capi/examples/example_sdk_add_cognify_search.c`** — the flagship C
   example (mirrors `js/examples/add-cognify-search.ts`): env-driven config,
   add → cognify → `cg_sdk_search` (`GRAPH_COMPLETION`) → `cg_sdk_recall`; SKIPs cleanly
   without credentials (D12 SKIP guard, same pattern as `example_sdk_add_cognify.c`).
7. **Wire `example_sdk_add_cognify_search` into `capi/examples/CMakeLists.txt`** — follow
   the `example_sdk_add_cognify` pattern (lines 42–48).
8. **Wire `example_sdk_add_cognify_search` into `capi/scripts/check.sh`** — add a Phase 5
   Tier-B block after the Phase 4 Tier-B block (line ~126), gated on
   `OPENAI_URL`/`OPENAI_TOKEN` (same pattern as `example_sdk_add_cognify`).
9. **Regenerate `capi/include/cognee_sdk.h`** via cbindgen (run
   `capi/scripts/gen_sdk_header.sh` or the equivalent `cbindgen` invocation) and commit
   the updated header with the two new exported function signatures.

## Exit criteria

- [ ] all 15 search-type strings accepted (Tier-A string-mapping check, mirroring TS's
      locked `SearchType ↔ string` test)
- [ ] recall with all 5 scopes (`auto`, `graph`, `session`, `trace`, `graph_context`)
      + session routing verified in Tier-A smoke
- [ ] live `add → cognify → search → recall` round-trip from C verified (Tier-B,
      gated in capi-check per D12)
- [ ] `cognee_sdk.h` regenerated and committed with the two new signatures
