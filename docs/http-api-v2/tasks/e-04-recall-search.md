# E-04 — `POST /api/v1/recall`

| | |
|---|---|
| Wire path | `POST /api/v1/recall` |
| Status | **Done (commit 9981e79)** — `RecallPayloadDTO` extended with `sessionId` (camelCase + snake_case alias) and `scope` (null/string/list); custom `deserialize_with` delegates to `cognee_search::recall_scope::normalize_scope`. Handler re-routed from `SearchOrchestrator::search` to inline four-source fan-out via `cognee_search::recall_scope::{search_session, search_trace, fetch_graph_context, run_graph}` mirroring Python `recall.py:373-531` (cycle workaround per Decision 18). `ComponentHandles` extended with `session_store` + `session_manager` slots. Response emits Python's flat-list-with-`_source` wire shape. Reviewer-amended: graph_context content key fixed `snapshot` → `content` per Python `recall.py:314`. 6 DTO unit tests + 5 new integration tests + 3 cross-SDK harness tests. **No new wire divergence**. |
| Depends on | **LIB-07** (commit 7d25c0b — recall scope widening) and **LIB-08** (commit f98cac7 — lift to `cognee-search`). Per Decisions 17 + 18 the work was split so E-04 retains strict Python parity AND can reach the primitives without a cycle violation. |
| Effort | ~0.5 day for DTO + handler + tests. |
| Owner crate | `cognee-http-server` |

## 1. Goal

Reverse the deliberate "do not add session_id" guard in [`crates/http-server/src/dto/recall.rs:29-30`](../../../crates/http-server/src/dto/recall.rs#L29-L30) and add the two v2-defining parameters to the recall request: `session_id` (string) and `scope` (string OR list of strings, expanded via `cognee_search::recall_scope::normalize_scope`). Re-route the handler from `SearchOrchestrator::search` to `cognee_lib::api::recall::recall(...)` so the four-source fan-out (`graph` / `session` / `trace` / `graph_context`) — owned by LIB-07 and reachable through LIB-08's lift — runs end-to-end.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `RecallPayloadDTO` | `cognee/api/v1/recall/routers/get_recall_router.py` | 23–48 |
| `POST ""` handler | same | 78–145 |
| `normalize_scope` | `cognee/memory/entries.py` | 81–115 |

Request body (additions only, all other fields already match):

```json
{
  ...existing v1 fields...,
  "session_id": "abc123",                      // optional
  "scope": "graph_context"                     // OR ["graph", "session"] OR "all"
}
```

`scope` semantics:
- `null` or `"auto"` → session-first if `session_id` present, else `graph`.
- `"all"` → expands to `["graph", "session", "trace", "graph_context"]`.
- Otherwise validated against the same allowlist (`graph`, `session`, `trace`, `graph_context`). Unknown → `ValueError` → 422 in Python; Rust must match.

## 3. Current Rust state (verified 2026-04-30)

- Handler `post_recall` at [`crates/http-server/src/routers/recall.rs:117`](../../../crates/http-server/src/routers/recall.rs#L117). It explicitly **bypasses** `cognee_lib::api::recall::recall` and calls `SearchOrchestrator::search` directly (Python-parity comment at L94 says "must NOT call the library-level cognee_lib::api::recall::recall, which would diverge from the Python HTTP contract"). **However**, this comment is now misleading: in Python `get_recall_router.py:115` the HTTP handler **does** import and call `from cognee.api.v1.recall import recall as cognee_recall`. The strict-parity stance in the comment was correct under v1 (when the lib `recall()` did less than the orchestrator); under v2 the comment must be re-evaluated since Python's lib `recall()` is what owns the `scope` fan-out.
- `SearchRequest.session_id: Option<String>` exists at [`crates/search/src/types/search_request.rs:20`](../../../crates/search/src/types/search_request.rs#L20); the `post_recall` handler hard-codes it to `None` at [`recall.rs:146`](../../../crates/http-server/src/routers/recall.rs#L146). `SearchOrchestrator::search` honors `session_id` for session history loading, feedback flows, and trace/QA persistence (see uses at `crates/search/src/orchestration/search_orchestrator.rs:299/319/351`). It does NOT implement `trace` / `graph_context` source fan-out.
- `crates/search/src/query_router.rs` is a **query-type classifier** (auto-routing between `GraphCompletion` / `RagCompletion` / `Cypher` / etc. based on keyword heuristics), **NOT** a scope-source fan-out router. There is no `RecallScope` enum and no `normalize_scope` function anywhere in `crates/`. The previous version of this task doc cited this file in error.
- `cognee_lib::api::recall::recall` ([`crates/lib/src/api/recall.rs:69`](../../../crates/lib/src/api/recall.rs#L69)) implements only the legacy session-first short-circuit (session OR graph). It accepts `session_id` and `auto_route`, but does **NOT** accept a `scope` parameter and does **NOT** perform the four-source (`session` / `graph` / `trace` / `graph_context`) fan-out that Python's `cognee/api/v1/recall/recall.py:373-475` performs.

### Library scope resolved by Decisions 17 + 18

Python's HTTP `POST /recall` calls `cognee_recall(...)` which performs scope-resolved source fan-out across up to four sources (`graph`, `session`, `trace`, `graph_context`) and merges results. The Rust HTTP handler today calls `SearchOrchestrator::search` directly and gets only graph-source results.

**Decision 17 (2026-04-30)** split the work into two tasks so E-04 retains strict Python parity:

- **[LIB-07](lib-07-recall-scope-widening.md)** (prerequisite) — widen `cognee_lib::api::recall::recall()` to accept `scope: Option<Vec<RecallScope>>`, add `RecallScope` enum + `normalize_scope()` helper + `_search_session` / `_search_trace` / `_fetch_graph_context` private helpers + tests. Pure library work, no HTTP changes. **Landed at commit 7d25c0b.**
- **E-04** (this task) — once LIB-07 is in, re-route the HTTP handler from `SearchOrchestrator::search` to `cognee_lib::api::recall::recall(...)` (matching Python `get_recall_router.py:115`), plumb `session_id` + the normalized `scope` through, and add the corresponding DTO fields. **No new wire divergence** — full parity with Python's four-source fan-out.

**Decision 18 (2026-04-30)** added a follow-up architectural fix:

- **[LIB-08](lib-08-recall-scope-lift.md)** (prerequisite) — the http-server↔lib cycle constraint at [`crates/http-server/Cargo.toml:35-37`](../../../crates/http-server/Cargo.toml#L35-L37) means `cognee-http-server` cannot import from `cognee-lib`, so E-04 cannot consume LIB-07's `RecallScope` / `normalize_scope` / `ScopeInput` / `RecallItem` / `RecallSource` directly. LIB-08 lifted these primitives plus the four `pub` source helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) to `crates/search/src/recall_scope.rs`. `cognee-lib::api::recall::*` re-exports preserve the public API. `normalize_scope` returns `SearchError::InvalidInput` at the new location (error message string byte-identical to Python). **Landed at commit f98cac7.** E-04 now imports them via `use cognee_search::recall_scope::*` — no re-implementation, no cycle.

## 4. Implementation steps

> **Decisions 17 + 18 (2026-04-30)**: this task assumes LIB-07 (commit 7d25c0b) and LIB-08 (commit f98cac7) have already landed. Do NOT attempt to implement source fan-out in the HTTP layer; do NOT re-implement `RecallScope` / `normalize_scope` in `cognee-http-server` (E-01's `WireRememberStatus` pattern was explicitly rejected here in favor of Option α).

1. **Extend the DTO** at [`crates/http-server/src/dto/recall.rs:36`](../../../crates/http-server/src/dto/recall.rs#L36) with `session_id` and `scope`. Add `use cognee_search::recall_scope::{RecallScope, ScopeInput, normalize_scope};` at the top of the file:
   ```rust
   #[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
   #[serde(rename_all = "camelCase")]
   pub struct RecallPayloadDTO {
       ...existing fields...
       #[serde(default, alias = "session_id")]
       pub session_id: Option<String>,

       #[serde(default, deserialize_with = "deserialize_scope")]
       pub scope: Option<Vec<RecallScope>>,
   }
   ```
   The custom `deserialize_with` accepts `null | string | list<string>`, builds a `ScopeInput`, dispatches into `cognee_search::recall_scope::normalize_scope` (returns `Result<Vec<RecallScope>, SearchError>`), and surfaces unknowns via `serde::de::Error::custom(err.to_string())` — the `Display` impl of `SearchError::InvalidInput("Unknown recall scope(s): [...]. Valid values: [...]")` is byte-identical to Python. Per Decision 10 the wire is camelCase (`sessionId`); the `alias` accepts snake_case too. **Reuse LIB-08's `RecallScope`, `ScopeInput`, and `normalize_scope` directly** — do NOT re-implement them in the http-server crate.

2. **Remove the guard** in the same file. Delete the comment "Do NOT add `session_id` or `auto_route`" at lines 29–30 and the negative test `test_recall_dto_does_not_accept_session_id` at lines 96–105. Replace with positive tests (see §5).

3. **Re-route the handler** at [`crates/http-server/src/routers/recall.rs:117`](../../../crates/http-server/src/routers/recall.rs#L117). Critical constraint: `cognee-http-server` **cannot** call `cognee_lib::api::recall::recall(...)` directly because [`crates/http-server/Cargo.toml:35-37`](../../../crates/http-server/Cargo.toml#L35-L37) forbids the http-server → lib direction (the same cycle that forced E-01's `WireRememberStatus`). Instead, **inline the four-source fan-out** in the handler using the `pub` helpers lifted to `cognee_search::recall_scope`:

   ```rust
   use cognee_search::recall_scope::{
       RecallScope, RecallItem, fetch_graph_context, run_graph, search_session, search_trace,
   };
   ```

   The handler must replicate the same `auto`-resolution + iteration + `auto_fallthrough` short-circuit logic that `cognee_lib::api::recall::recall()` performs at [`crates/lib/src/api/recall.rs:78-197`](../../../crates/lib/src/api/recall.rs#L78-L197). This is **strict Python parity** — the algorithm is byte-identical to Python `recall.py:373-531`; the implementation just lives in a different crate.

   Remove the misleading parity comment at [`recall.rs:94`](../../../crates/http-server/src/routers/recall.rs#L94) ("must NOT call the library-level cognee_lib::api::recall::recall"). The new (correct) parity comment: "calls `cognee_search::recall_scope::*` helpers directly because the http-server → lib cycle constraint (Cargo.toml:35-37) forbids importing `cognee_lib::api::recall::recall`. The fan-out logic mirrors Python `recall.py:373-531` byte-for-byte."

   **Note for the implementation agent**: the alternative — exposing the `recall()` orchestration as a `pub fn` in `cognee-search` — is out of scope for E-04 (would be LIB-09). Inlining keeps E-04's diff to the http-server crate only and matches the Decision 18 spec (the four `pub` helpers are sufficient).

4. **Plumb `session_id` and `scope` through** to the per-source helpers, mirroring `cognee_lib::api::recall::recall()`'s body:
   ```rust
   // 1. Resolve scope to concrete sources (auto-mode logic from recall.rs:78-100).
   let normalized = payload.scope.unwrap_or_else(|| vec![RecallScope::Auto]);
   let auto_mode = normalized.as_slice() == [RecallScope::Auto];
   let (sources, auto_fallthrough) = if auto_mode {
       match (payload.session_id.as_deref(), payload.datasets.as_ref(), payload.search_type.into()) {
           (Some(_), None, None) => (vec![RecallScope::Session, RecallScope::Graph], true),
           (Some(_), _, _)       => (vec![RecallScope::Session, RecallScope::Graph], false),
           (None, _, _)          => (vec![RecallScope::Graph], false),
       }
   } else { (normalized, false) };

   // 2. Iterate (recall.rs:151-197). Call search_session / search_trace /
   //    fetch_graph_context / run_graph with the appropriate args. Concatenate
   //    Vec<RecallItem> across sources.
   ```
   No `SearchRequest` widening required — `run_graph` builds its own `SearchRequest` from the args.

   **Component handles**: the helpers need `session_store: Option<&dyn SessionStore>` and `session_manager: Option<&SessionManager>`. **`ComponentHandles` does NOT have these slots today** ([`crates/http-server/src/components.rs:26-67`](../../../crates/http-server/src/components.rs#L26-L67)). Add two new fields:
   ```rust
   pub session_store: Option<Arc<dyn cognee_session::SessionStore>>,
   pub session_manager: Option<Arc<cognee_session::SessionManager>>,
   ```
   Both default to `None` so existing test fixtures and `AppState::build_with_db` keep compiling unchanged. The helpers gracefully return `Ok(vec![])` when these are `None` (matches Python's `is_available` short-circuit at `recall.py:170-171`). `cognee-http-server` already depends on `cognee-session` ([`crates/http-server/Cargo.toml:107`](../../../crates/http-server/Cargo.toml#L107)), so no new dependency is needed.

5. **Adapt the response shape** — Python's recall response is a flat list of dicts each with a `_source` key (`recall.py:208/278/315/495-498`). Today the Rust handler returns `Vec<RecallResultDTO>` (shape `{searchResult, datasetId, datasetName}`) via `flatten_search_response`. The two are **not** wire-compatible. Map each `RecallItem` (`{source, content, score}` from `cognee_search::recall_scope::RecallItem`) to a flat JSON object:
   ```jsonc
   // For source=graph items whose content is itself a SearchResult dict, the
   // graph result fields appear at the top level next to "_source": "graph"
   // (Python recall.py:489-498 -- `r["_source"] = "graph"` mutates in place).
   // For source=session/trace, the dict fields (question, answer, etc.) appear
   // at top level next to "_source": "session" / "trace".
   // For source=graph_context, "_source": "graph_context" + the snapshot under
   // a content field (Python recall.py:312 returns the snapshot dict tagged).
   ```
   Add a new `RecallV2ResultDTO` (or extend `RecallResultDTO`) that serializes as `serde_json::Value` and inject `_source` per Python parity. Document the per-source shape in a doc-comment that cites Python `recall.py:191-208` (session), `recall.py:252-278` (trace), `recall.py:289-315` (graph_context), `recall.py:455-498` (graph). Cross-SDK parity test asserts byte equality on this shape.

6. **Validation** for unknown scope values, per Decision 7:
   - Status code: **`400`** (Python overrides FastAPI's default 422 globally).
   - Body shape produced by the existing `ValidatedJson` extractor at [`crates/http-server/src/middleware/validation.rs:88-101`](../../../crates/http-server/src/middleware/validation.rs#L88-L101):
     ```json
     {
       "detail": [{
         "loc": ["body"],
         "msg": "Unknown recall scope(s): [\"foo\"]. Valid values: [\"all\", \"auto\", \"graph\", \"graph_context\", \"session\", \"trace\"]",
         "type": "value_error.json_parse"
       }],
       "body": <raw input echo>
     }
     ```
   - Known deltas vs Python's exact envelope (`loc=["body","scope"]`, `type="value_error"`) are the existing v1-envelope gap and apply equally to all body-field validation paths in the Rust port — out of scope for E-04, tracked separately. Test asserts the **current Rust shape** with substring-match on `msg`.

7. **No new wire divergence**. With LIB-07 + LIB-08 landed, all four scope sources work end-to-end. The `scope` happy paths AND the `400` validation envelope match Python within the documented envelope-shape gap. The `_source` injection on each item is the literal Python shape.

## 5. Tests

- Update `crates/http-server/src/dto/recall.rs` tests:
  - `recall_dto_accepts_session_id` (replaces the deleted `test_recall_dto_does_not_accept_session_id` negative test).
  - `recall_dto_accepts_scope_as_string` — input `"scope": "graph"` round-trips through `normalize_scope` to `Some(vec![RecallScope::Graph])`.
  - `recall_dto_accepts_scope_as_list` — input `"scope": ["graph", "session"]` → `Some(vec![Graph, Session])`.
  - `recall_dto_scope_all_expands_to_four_sources` — input `"scope": "all"` → `Some(vec![Graph, Session, Trace, GraphContext])`.
  - `recall_dto_scope_null_normalizes_to_auto` — input `"scope": null` → `Some(vec![Auto])`.
  - `recall_dto_scope_unknown_returns_serde_error` — input `"scope": "foo"` → `serde_json::from_str` returns `Err`, error message contains `"Unknown recall scope(s)"`.
- Update `crates/http-server/tests/test_recall.rs`:
  - `post_recall_passes_session_id_to_library` — POST `{"query":"x","session_id":"s1"}`, assert the library `recall()` call observed `session_id == Some("s1")` (use a capture-mode test fixture or mock).
  - `post_recall_passes_scope_to_library` — POST `{"query":"x","scope":"graph"}`, assert the library `recall()` call observed `scope == Some(vec![Graph])`.
  - `post_recall_scope_all_runs_four_sources` — POST `{"query":"x","scope":"all"}`, assert the library was called with all four scopes; uses LIB-07's mocks where applicable.
  - `unknown_scope_returns_400_with_validation_envelope` — POST `{"query":"x","scope":"foo"}` and assert:
    - Status `400` (NOT 422).
    - Body has `detail` array of length 1.
    - `body.detail[0].loc` equals `["body"]`.
    - `body.detail[0].msg` contains the `"Unknown recall scope(s)"` substring.
    - `body.detail[0].type` equals `"value_error.json_parse"` (current Rust shape).
    - Top-level `body.body` echoes the raw input JSON.
- Cross-SDK parity in `e2e-cross-sdk/harness/test_http_v2_recall.py` (NEW file):
  - `test_session_id_passthrough` — send `{"query": "...", "session_id": "s1", "scope": "auto"}` to both servers; structurally diff the result lists. Now passes end-to-end (post-LIB-07).
  - `test_scope_all_four_sources_match` — send `{"query":"...","scope":"all"}` to both servers after seeding all four backends; structurally diff per-source results.
  - `test_unknown_scope_returns_400` — send `{"query":"x","scope":"foo"}` to both servers; assert both return 400. Compare with substring tolerance for `msg` and skip `type` per the documented envelope-shape gap.

## 6. Acceptance criteria

- [x] `RecallPayloadDTO` accepts `sessionId` (camelCase) with `session_id` snake-case alias, per Decision 10.
- [x] `RecallPayloadDTO` accepts `scope` as `null | string | list<string>` with normalization via `deserialize_with` delegating to `cognee_search::recall_scope::normalize_scope`.
- [x] Handler re-routed from `SearchOrchestrator::search` to inline four-source fan-out using `cognee_search::recall_scope::{search_session, search_trace, fetch_graph_context, run_graph}` (mirrors Python `recall.py:373-531`; cycle constraint forbids calling `cognee_lib::api::recall::recall` directly).
- [x] `ComponentHandles` extended with `session_store` + `session_manager` slots (both `Option<Arc<...>>`, default `None`).
- [x] `session_id` and `scope` plumbed through to the helpers.
- [x] Response items emit Python's flat-dict-with-`_source` wire shape (NOT the `RecallResultDTO {searchResult, datasetId, datasetName}` envelope). Graph context content under `content` key (reviewer fix per `recall.py:314`).
- [x] `scope: null` normalizes to `[Auto]`.
- [x] `scope: "all"` expands to all four sources.
- [x] Unknown scope returns **400** (not 422) with the Rust validation envelope (`loc=["body"]`, `type="value_error.json_parse"`, `msg` contains `"Unknown recall scope(s)"`).
- [x] Integration tests assert byte-shape on the `400` envelope under the current Rust shape.
- [x] Cross-SDK parity test added for `session_id` and `scope=all` happy paths.
- [x] **The negative-test guardrail is gone** (no comment in the codebase still claims `session_id` should not be on the DTO).
- [x] **No new wire divergence** introduced (Decisions 17 + 18 — no D-2 created).

## 7. References

- [Python recall handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L78)
- [Python `normalize_scope`](https://github.com/topoteretes/cognee/blob/main/cognee/memory/entries.py#L81)
- [Python `recall()` library function](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/recall.py#L317) — algorithm to mirror.
- [LIB-07 task doc](lib-07-recall-scope-widening.md) — library widening (commit 7d25c0b).
- [LIB-08 task doc](lib-08-recall-scope-lift.md) — primitives lifted to `cognee-search` (commit f98cac7).
- [Rust `recall_scope` module](../../../crates/search/src/recall_scope.rs) — `RecallScope` / `RecallItem` / `normalize_scope` / four `pub` helpers.
- [Rust library `recall()`](../../../crates/lib/src/api/recall.rs) — reference body the HTTP handler must mirror inline (cycle prevents calling it directly).
- [Rust DTO](../../../crates/http-server/src/dto/recall.rs)
