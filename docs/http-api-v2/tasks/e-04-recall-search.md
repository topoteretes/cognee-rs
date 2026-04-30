# E-04 — `POST /api/v1/recall`

| | |
|---|---|
| Wire path | `POST /api/v1/recall` |
| Status | **Blocked on LIB-07** — DTO + handler work is ready, but the library widening that owns the four-source fan-out lives in [LIB-07](lib-07-recall-scope-widening.md) per **Decision 17** (2026-04-30). |
| Depends on | **LIB-07** (`recall()` scope widening — `RecallScope` enum, `normalize_scope()`, source helpers). Per Decision 17 the work was split so E-04 retains strict Python parity. |
| Effort | ~0.5 day for DTO + handler + tests once LIB-07 has landed. |
| Owner crate | `cognee-http-server` |

## 1. Goal

Reverse the deliberate "do not add session_id" guard in [`crates/http-server/src/dto/recall.rs:29-30`](../../../crates/http-server/src/dto/recall.rs#L29-L30) and add the two v2-defining parameters to the recall request: `session_id` (string) and `scope` (string OR list of strings, expanded via a new `normalize_scope` helper colocated with the DTO — see §3 "Library scope of E-04" for why it lives in the DTO module rather than `cognee_search`).

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

### Library scope resolved by Decision 17

Python's HTTP `POST /recall` calls `cognee_recall(...)` which performs scope-resolved source fan-out across up to four sources (`graph`, `session`, `trace`, `graph_context`) and merges results. The Rust HTTP handler today calls `SearchOrchestrator::search` directly and gets only graph-source results.

**Decision 17 (2026-04-30)** split the work into two tasks so E-04 retains strict Python parity:

- **[LIB-07](lib-07-recall-scope-widening.md)** (prerequisite) — widen `cognee_lib::api::recall::recall()` to accept `scope: Option<Vec<RecallScope>>`, add `RecallScope` enum + `normalize_scope()` helper + `_search_session` / `_search_trace` / `_fetch_graph_context` private helpers + 14 tests. Pure library work, no HTTP changes. **Must land before E-04.**
- **E-04** (this task) — once LIB-07 is in, re-route the HTTP handler from `SearchOrchestrator::search` to `cognee_lib::api::recall::recall(...)` (matching Python `get_recall_router.py:115`), plumb `session_id` + the normalized `scope` through, and add the corresponding DTO fields. **No new wire divergence** — full parity with Python's four-source fan-out.

## 4. Implementation steps

> **Decision 17 (2026-04-30)**: this task assumes LIB-07 has already landed. Do NOT attempt to implement source fan-out in the HTTP layer.

1. **Extend the DTO** at [`crates/http-server/src/dto/recall.rs:36`](../../../crates/http-server/src/dto/recall.rs#L36) with `session_id` and `scope`:
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
   The custom `deserialize_with` accepts `null | string | list<string>`, dispatches into `cognee_lib::api::recall::normalize_scope` (landed at LIB-07), and surfaces unknowns via `serde::de::Error::custom("Unknown recall scope(s): [...]. Valid values: [...]")`. Per Decision 10 the wire is camelCase (`sessionId`); the `alias` accepts snake_case too. **Reuse LIB-07's `RecallScope` and `normalize_scope` directly** — do NOT re-implement them in the http-server crate.

2. **Remove the guard** in the same file. Delete the comment "Do NOT add `session_id` or `auto_route`" at lines 29–30 and the negative test `test_recall_dto_does_not_accept_session_id` at lines 96–105. Replace with positive tests (see §5).

3. **Re-route the handler** at [`crates/http-server/src/routers/recall.rs:117`](../../../crates/http-server/src/routers/recall.rs#L117) from `SearchOrchestrator::search` to `cognee_lib::api::recall::recall(...)` (matching Python `get_recall_router.py:115`). The library function (post-LIB-07) accepts `query`, `session_id`, `scope`, `auto_route`, plus the existing dependency injection params. Remove the misleading parity comment at L94 — under v2 with LIB-07 landed, calling the lib function IS the strict-parity path. Adapt the `cognee_lib::api::recall::RecallResult` to the existing `SearchResult` HTTP response shape (or extend the response DTO if needed; cite Python's response shape from `recall.py:373-475`).

4. **Plumb `session_id` and `scope` through** to the library call:
   ```rust
   let result = cognee_lib::api::recall::recall(
       &payload.query,
       /* session_id */ payload.session_id.as_deref(),
       /* scope */ payload.scope.as_deref(),
       /* auto_route */ payload.auto_route.unwrap_or(false),
       // ... existing component handles
   ).await?;
   ```
   No `SearchRequest` widening required since the handler no longer drives `SearchOrchestrator::search` directly for the v2 path.

5. **Validation** for unknown scope values, per Decision 7:
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

6. **No new wire divergence**. With LIB-07 landed, all four scope sources work end-to-end. The `scope` happy paths AND the `400` validation envelope match Python within the documented envelope-shape gap.

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

- [ ] `RecallPayloadDTO` accepts `sessionId` (camelCase) with `session_id` snake-case alias, per Decision 10.
- [ ] `RecallPayloadDTO` accepts `scope` as `null | string | list<string>` with normalization via `deserialize_with` delegating to LIB-07's `normalize_scope`.
- [ ] Handler re-routed from `SearchOrchestrator::search` to `cognee_lib::api::recall::recall(...)` (Python parity at `get_recall_router.py:115`).
- [ ] `session_id` and `scope` plumbed through to the library call.
- [ ] `scope: null` normalizes to `[Auto]`.
- [ ] `scope: "all"` expands to all four sources via LIB-07's helper.
- [ ] Unknown scope returns **400** (not 422) with the Rust validation envelope (`loc=["body"]`, `type="value_error.json_parse"`, `msg` contains `"Unknown recall scope(s)"`).
- [ ] Integration tests assert byte-shape on the `400` envelope under the current Rust shape.
- [ ] Cross-SDK parity test passes for `session_id` and `scope=all` happy paths.
- [ ] **The negative-test guardrail is gone** (no comment in the codebase still claims `session_id` should not be on the DTO).
- [ ] **No new wire divergence** introduced (Decision 17 — no D-2 needed).

## 7. References

- [Python recall handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L78)
- [Python `normalize_scope`](https://github.com/topoteretes/cognee/blob/main/cognee/memory/entries.py#L81)
- [Rust query router](../../../crates/search/src/query_router.rs)
- [Rust DTO](../../../crates/http-server/src/dto/recall.rs)
