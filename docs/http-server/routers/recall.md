# Router: recall

The `/api/v1/recall` router is the memory-oriented companion to `/api/v1/search`. It accepts the same wire DTO and the same `SearchType` enum, but layers two behaviors on top: (1) **session-first retrieval** — when the caller passes a `session_id` without explicit `datasets`, Q&A entries cached on the session are searched by keyword overlap before falling through to the graph; and (2) **automatic query-type routing** — when `query_type` is omitted (or supplied with `auto_route=true`), the rule-based `route_query()` classifier picks one of the 15 `SearchType` values from the natural-language query without an LLM call. Both behaviors are already implemented at the library layer; this doc specs the HTTP wrapper.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../observability.md](../observability.md), [routers/search.md](search.md).

## 1. Mount & file
- Mount prefix: `/api/v1/recall`
- Router file: `crates/http-server/src/routers/recall.rs`
- DTO file: `crates/http-server/src/dto/recall.rs` (mostly re-exports `crates/http-server/src/dto/search.rs` types)
- Python source: [`cognee/api/v1/recall/routers/get_recall_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py)
- Library implementation (already done): [`crates/lib/src/api/recall.rs`](../../../crates/lib/src/api/recall.rs), [`crates/search/src/query_router.rs`](../../../crates/search/src/query_router.rs), [`crates/search/src/query_router_stats.rs`](../../../crates/search/src/query_router_stats.rs)

## 2. Endpoints

### 2.1 `GET /api/v1/recall` — list the caller's recall/search history

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none. Like `/api/v1/search`, Python passes `limit=0` (unlimited) and ignores any query argument ([`get_recall_router.py:56`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L56)).
- **Request body**: none.
- **Response body** (`200 OK`, `application/json`): `Vec<RecallHistoryItemDTO>` — same shape as `SearchHistoryItemDTO` (id / text / user / created_at). Reuses the same underlying `SearchHistoryDb::get_history` and the same relational tables as `/api/v1/search` GET — the two endpoints **share** history rows. Python's [`get_recall_router.py:46-48`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L46-L48) annotates the response with a separate `RecallHistoryItem` Pydantic class but it is structurally identical to `SearchHistoryItem`; Rust uses a single shared DTO behind a type alias.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | 401 | `{"detail": "Unauthorized"}` | No credential. |
  | 500 | `{"error": "An error occurred while fetching recall history."}` | Any DB error. **Note**: this uses `{error}` (single field), NOT the `{error, detail}` envelope that `POST /api/v1/recall` uses. Python: [`get_recall_router.py:60-64`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L60-L64). The `detail` is dropped to avoid leaking DB internals to unauthenticated retry loops; the underlying error is logged at `error!` level (Python `logger.error(..., exc_info=True)`). |

- **Side effects**: none.
- **Delegation target**: `state.lib.search().history(user.id, None).await` — same path as `GET /api/v1/search`. The two endpoints are semantically a single capability; they exist as separate URLs so the frontend can decide which "tab" of history to render.
- **Validation rules**: none.
- **Permission gate**: none beyond auth (history rows are scoped by `user_id`).
- **OpenAPI**: tag `["v1", "recall"]`. `200` response model `Vec<RecallHistoryItemDTO>`.
- **Telemetry**: span name `cognee.api.recall.history`. Same attributes as `cognee.api.search.history`.
- **Python parity notes**: the response model in Python is declared as `list[RecallHistoryItem]` whereas search uses `Union[List[SearchResult], List]`. Functionally identical; the divergence is a Python class-naming quirk we replicate at the DTO level for OpenAPI clarity.

### 2.2 `POST /api/v1/recall` — multi-source semantic recall

**Behavior note**: the Rust handler (`post_recall` in `crates/http-server/src/routers/recall.rs`) does **not** delegate to the search orchestrator as a thin alias. It performs **scope resolution** and **session-first dispatch** in the handler itself by calling the `cognee_search::recall_scope::*` helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`). This mirrors the library-level `cognee::api::recall::recall()` (which the handler cannot import directly due to the http-server → lib cycle constraint), so the fan-out logic lives in the four `pub` `recall_scope` helpers instead. The orchestrator is still required (it backs the `Graph` source and the GET-history path), but the request flow is driven by the resolved scope, not a single search call.

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body** (`application/json`): `RecallPayloadDTO`. Wire is camelCase per Decision 10 with snake_case accepted as an inbound alias. In addition to the `SearchPayloadDTO` fields it carries `session_id` and `scope`. Defaults:
  - `search_type` defaults to `"GRAPH_COMPLETION"`.
  - `query` defaults to `"What is in the document?"`.
  - `system_prompt` defaults to `"Answer the question using the provided context. Be as brief as possible."`.
  - `top_k` defaults to `10`.
  - `only_context`, `verbose` default to `false`.
  - `datasets`, `dataset_ids`, `node_name` default to `null`.
  - `session_id` defaults to `null`.
  - `scope` defaults to `null` (normalized to `["auto"]`).

  **Scope / session dispatch**: `scope` accepts `null | string | list<string>` and is normalized via `cognee_search::recall_scope::normalize_scope` into a list of `RecallScope` values (`auto` / `graph` / `session` / `trace` / `graph_context`; `"all"` expands to all four concrete sources). When `scope` resolves to `[Auto]`, the handler picks sources from the request shape: a `session_id` with no `datasets`/`query_type` yields `[Session, Graph]` with a session-first short-circuit (a session hit skips the graph runner); a `session_id` with datasets/query_type yields `[Session, Graph]` without the short-circuit; no `session_id` yields `[Graph]` only. The `session_id` field also feeds `search_session` / `search_trace` directly.
- **Response body** (`200 OK`, `application/json`): `Vec<serde_json::Value>` — a flat list of dicts, each tagged with a `_source` field (`session` / `trace` / `graph_context` / `graph`) identifying which scope produced it. Items from the different sources are merged in resolved-scope order. Python returns the same flat, `_source`-tagged shape ([`recall.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/recall.py)).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | 401 | `{"detail": "Unauthorized"}` | No credential. |
  | 403 | `{"detail": "Request owner does not have necessary permission: [read] for all datasets requested. [PermissionDeniedError]"}` | A `dataset_ids` entry the caller may not read — someone else's dataset or a UUID that names nothing (one error for the batch, so ids cannot be probed). Python re-raises `PermissionDeniedError` into the global `CogneeApiError` handler ([`get_recall_router.py:322-325`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L322-L325), [`client.py:220-236`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L220-L236)), the same as search. Earlier revisions of this doc recorded a silent `200 []`; upstream no longer does that, and Rust matches current upstream. |
  | 422 | `{"error": "Recall prerequisites not met", "hint": "Run `await cognee.remember(...)` or `await cognee.add(...)` then `await cognee.cognify()` before recalling."}` | `DatabaseNotCreatedError`, `UserNotFoundError`, `CogneeValidationError`. Python: [`get_recall_router.py:116-126`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L116-L126). Note the **two-field envelope** (`error`, `hint`) is **distinct from search**'s `{error, detail}` shape — recall uses `hint` instead of `detail`. Match exactly. |
  | 409 | `{"error": "An error occurred during recall."}` | Catch-all for any other exception. Single-field `{error}` envelope. The underlying error is logged at `error!` level. Python: [`get_recall_router.py:129-135`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L129-L135). |

  **Three distinct error envelopes** in this endpoint alone (`{error, hint}`, `{error}`, plus the canonical `{detail}` shared by 401 and 403). Document loudly in `crates/http-server/src/error.rs`; gate via `ApiError::RecallError(status, ErrorBody)`.
- **Side effects**:
  1. **Search-history write** (every POST). Same two rows as `/api/v1/search`: one `Query` row, one `Result` row. Persisted via `SearchHistoryDb::log_query` + `log_result` from inside `SearchOrchestrator::search`. The history is **shared** between the two endpoints — `GET /api/v1/recall` and `GET /api/v1/search` return the same set.
  2. **Vector / graph reads** as in search.
- **Delegation target**: the `cognee_search::recall_scope::*` helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`), iterated per the resolved scope list. The `Graph` source calls `run_graph` against the `SearchOrchestrator`; the session-backed sources use the optional `session_store` / `session_manager` component handles (which gracefully return `Ok(vec![])` when unwired). The handler does **not** call `cognee::api::recall::recall` (cycle constraint); the `recall_scope` helpers were lifted into `cognee-search` so the fan-out is reachable without a cycle.
- **Validation rules**: same as search.
- **Rate / size limits**: default body limit (100 MiB).
- **Permission gate**: `read` permission on every dataset in scope — same rules as search, see [search.md §Permission gate](search.md). Ids are checked against the caller's readable set in `SearchOrchestrator`, whether they arrived explicitly or came from resolving a `datasets` name: ACL grants (direct, tenant and role) when an `AclDb` is wired, ownership when one is not (OSS, matching Python's `ENABLE_BACKEND_ACCESS_CONTROL=false` default). Any denied id fails the whole batch with `SearchError::PermissionDenied`, which the handler maps to **403** exactly as search does. Names resolve owner- and tenant-scoped. `dataset_ids` takes precedence over `datasets` when both are sent; an empty list is no filter, but a malformed UUID is a validation error rather than a silently-widened query. The typed binding wrappers expose `datasetIds` and `tenant` on recall as well as search (`CogneeRecallOptions`, Java `RecallOptions`).
- **OpenAPI**: tag `["v1", "recall"]`. Request body schema from `RecallPayloadDTO`; response schema `Vec<RecallResultDTO>`. The error envelopes are declared separately in the `responses` block.
- **Telemetry**: span name `cognee.api.recall`. Attributes (per [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions)):
  - `cognee.search.query` — first 500 chars of the user query.
  - `cognee.search.type` — the supplied `search_type`.
  - `cognee.search.top_k` — resolved `top_k`.
  - `cognee.search.result_count` — final count returned to the caller.
- **Python parity notes**:
  - HTTP recall is a wire-level alias for HTTP search. The error envelopes differ (`{error, hint}` for 422; `{error}` for 409 catch-all; silent `[]` for permission denied) but the underlying request/response shapes and the search invocation itself are identical. Auto-routing and session-first dispatch live in the Python SDK's library-level `recall()` and are not part of the HTTP contract — Rust matches this exactly.

## 3. Cross-cutting behavior

### 3.1 Capabilities reachable from HTTP

The HTTP recall handler reproduces the library-level recall fan-out by calling the `cognee_search::recall_scope::*` helpers directly (it cannot import `cognee::api::recall::recall` due to the http-server → lib cycle constraint). The behaviors below are surfaced through the wire DTO's `scope` / `session_id` fields:

| Capability | Reachable from HTTP `/api/v1/recall`? | Notes |
|---|---|---|
| Scope resolution (`auto` / `graph` / `session` / `trace` / `graph_context`) | Yes | Via the `scope` field, normalized by `recall_scope::normalize_scope`. |
| Session-first dispatch | Yes | Auto mode with a `session_id` and no datasets/query_type short-circuits the graph runner on a session hit. |
| Result `_source` tagging | Yes | Each returned dict carries its `_source`. |
| Auto-routing via `query_router::route_query()` | No | `run_graph` is called with `auto_route = false`; the caller's `search_type` is honored verbatim. |
| Override tracking via `query_router_stats` | No | Not invoked from the HTTP handler. |

Auto-routing and override tracking remain library-only. Scope resolution, session-first dispatch, and `_source` tagging are reproduced at the HTTP layer.

### 3.2 Error envelope inconsistency

This router has three distinct error envelope shapes:
- `{detail}` — for 401 (canonical).
- `{error}` — for 409 catch-all and GET-history 500.
- `{error, hint}` — for 422 prerequisite errors.
- `{detail}` — also for 403 permission denied (a `dataset_ids` entry the caller may not read).

Match all four exactly. Encode via dedicated `ApiError::RecallError { status, body: RecallErrorBody }` variants where `RecallErrorBody` is itself an enum.

## 4. DTO definitions

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use cognee_search::recall_scope::RecallScope;
use crate::dto::search::WireSearchType;

/// Mirrors Python `RecallPayloadDTO` (`get_recall_router.py:23-48`).
/// Carries the `SearchPayloadDTO` fields plus `session_id` and `scope`.
/// Wire is camelCase per Decision 10; snake_case accepted as an inbound alias.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecallPayloadDTO {
    #[serde(default = "default_search_type", alias = "search_type")]
    pub search_type: WireSearchType,

    #[serde(default)]
    pub datasets: Option<Vec<String>>,

    #[serde(default, alias = "dataset_ids")]
    pub dataset_ids: Option<Vec<Uuid>>,

    #[serde(default = "default_query")]
    pub query: String,

    #[serde(default = "default_system_prompt", alias = "system_prompt")]
    pub system_prompt: Option<String>,

    #[serde(default, alias = "node_name")]
    pub node_name: Option<Vec<String>>,

    #[serde(default = "default_top_k", alias = "top_k")]
    pub top_k: Option<i32>,

    #[serde(default, alias = "only_context")]
    pub only_context: bool,

    #[serde(default)]
    pub verbose: bool,

    /// Optional session id (`sessionId`; snake_case accepted as alias). Drives
    /// `search_session` / `search_trace` and the auto-mode session-first
    /// short-circuit.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,

    /// Optional source scope — `null | string | list<string>`. Normalized via
    /// `cognee_search::recall_scope::normalize_scope`; unknown values surface a
    /// `serde::de::Error::custom`. Defaults / null → `Some(vec![RecallScope::Auto])`.
    #[serde(default, deserialize_with = "deserialize_scope")]
    pub scope: Option<Vec<RecallScope>>,
}

// `default_search_type`, `default_query`, `default_system_prompt`, `default_top_k`
// are re-used from `crate::dto::search`. `deserialize_scope` builds a
// `ScopeInput` and dispatches into `normalize_scope`.

/// Same shape as `SearchHistoryItemDTO`. Aliased for OpenAPI clarity.
pub type RecallHistoryItemDTO = crate::dto::search::SearchHistoryItemDTO;

/// Same shape as `SearchResultDTO`.
pub type RecallResultDTO = crate::dto::search::SearchResultDTO;

/// Recall-specific error envelopes. All three Python shapes encoded.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(untagged)]
pub enum RecallErrorBody {
    /// `{"error": "...", "hint": "..."}` — used for 422 prerequisite errors.
    WithHint { error: String, hint: String },
    /// `{"error": "..."}` — used for 409 and GET-history 500.
    JustError { error: String },
}
```

### `SearchType` wire shapes

Same as in [search.md §4](search.md#searchtype-wire-shapes) — all 15 `SearchType` values are reachable via the recall endpoint by setting `search_type` explicitly. There is no auto-routing on the HTTP layer; the caller's choice of `search_type` is honored verbatim, identical to `/api/v1/search`.

## 5. Implementation tasks

1. Add `crates/http-server/src/dto/recall.rs` with `RecallPayloadDTO`, type aliases for history/result, `RecallErrorBody`. `#[derive(ToSchema)]`.
2. Add `crates/http-server/src/routers/recall.rs` with `get_recall_history` + `post_recall` handlers and `pub fn router()`.
3. Wire `nest("/recall", recall::router())` in `build_router()`.
4. Extend `crates/http-server/src/error.rs` with `ApiError::RecallError(StatusCode, RecallErrorBody)` so the three envelope shapes serialize correctly.
5. The handler resolves `scope` (via `recall_scope::normalize_scope`) and iterates the resolved sources, calling the `cognee_search::recall_scope::*` helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`). Do **not** call `cognee::api::recall::recall` from this handler — the http-server → lib cycle constraint forbids it; the `recall_scope` helpers are the cycle-free reachable surface.
6. OpenAPI: tag `["v1", "recall"]`; declare the three response shapes for `200`, `409`, `422`.
7. Unit tests: DTO defaults; `RecallErrorBody` serialization for both arms.
8. Integration tests in `crates/http-server/tests/test_recall.rs`:
   - POST with `search_type="GRAPH_COMPLETION"` against a populated dataset → 200 with results.
   - POST with `search_type="CYPHER"` and a Cypher query → 200 with results.
   - POST with `dataset_ids` naming a dataset the user does not own → `403 {detail}`, and the retriever never runs.
   - POST against an empty database → 422 with `{error, hint}`.
   - POST that triggers an arbitrary unhandled error → 409 with `{error}`.
9. Cross-SDK parity test in `e2e-cross-sdk/harness/test_http_recall.py`. Recall and search should produce identical results for identical request bodies (modulo error-envelope differences when failing).

## 6. Open questions

1. **`SearchType::Feedback` parity** — present in the Rust enum but absent from Python's `SearchType`. Drop from the HTTP DTO; keep an internal `SearchTypeInternal` superset for library callers. The HTTP wire enum mirrors Python's set verbatim. Defer the per-router resolution to the search-router doc.
2. **Telemetry parity (PostHog)** — Python's `send_telemetry(...)` is skipped in Rust per [../observability.md §1](../observability.md#1-goals--non-goals). Confirm this gap is documented for the user-facing CHANGELOG.
3. **Permission-denied response** — *resolved*: current Python re-raises `PermissionDeniedError` and answers `403`; the silent `200 []` this question was written against is gone upstream. Any e2e parity test must assert `403`.
4. **Search-history history-write idempotency** — does Python double-write when the SDK retries? If so, Rust matches; if not, Rust matches; either way confirm via a parity test.
5. **Library-level recall reachability** — embedders who call `cognee::api::recall::recall` directly should still get auto-routing and session-first dispatch. The HTTP layer simply doesn't expose them. Confirm the embedder-facing docs make this distinction clear.
3. **`?include_source=true` query parameter**: should the HTTP layer expose the library's `_source: "session" | "graph"` tag? Useful for frontends building "Recent activity" UIs that distinguish session-cached answers. Recommend yes, behind an opt-in query param to keep default wire format Python-compatible.
4. **Override counter exposure**: where does `record_override`'s state surface to the operator? Options: (a) a new `GET /api/v1/activity/recall-overrides` endpoint, (b) a span attribute on every recall request, (c) only via the in-memory span buffer (current state). Recommend (c) for phase 4; revisit if misrouting becomes a real issue.
5. **Session search algorithm**: the library uses `HashSet::intersection` (token overlap, min length 2). For a session with thousands of Q&A entries, this is O(n) per call. Should the session store cache an inverted index? Out of scope for the HTTP doc — flag in [`crates/session/`](../../../crates/session/).
6. **Permission-denied silent vs explicit** — *resolved*: both recall and search return `403 {detail}`; there is no longer an inconsistency to document.
7. **`PermissionDenied` arm** — *resolved*: `cognee_search::SearchError::PermissionDenied` exists. The orchestrator raises it when a dataset in scope is not readable by the caller (or does not exist), before any retriever runs, and both handlers map it to `403`. ACL-aware authorization is **also resolved**: when an `AclDb` is wired (the closed/cloud build) readability is `authorized_dataset_ids_with_roles(user, "read")`, so direct, tenant- and role-inherited grants are all honoured and a shared dataset passes by id. The ownership-only path is the OSS fallback used when no `AclDb` is wired, matching Python's `ENABLE_BACKEND_ACCESS_CONTROL=false` default. Note that name resolution stays owner- and tenant-scoped in both SDKs — Python's `get_dataset_ids` requires a UUID for a dataset you do not own — so a shared dataset is reachable via `datasetIds` and not via `datasets`. The language bindings wire no `AclDb`, so on those surfaces "readable" is always "owned by the caller".

## 7. References

- Python router: [`cognee/api/v1/recall/routers/get_recall_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py).
- Python recall function: [`cognee/api/v1/recall/recall.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/recall.py).
- Python query router: [`cognee/api/v1/recall/query_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/query_router.py).
- Rust recall library function: [`crates/lib/src/api/recall.rs`](../../../crates/lib/src/api/recall.rs).
- Rust query router: [`crates/search/src/query_router.rs`](../../../crates/search/src/query_router.rs).
- Rust override-counter module: [`crates/search/src/query_router_stats.rs`](../../../crates/search/src/query_router_stats.rs).
- Companion: [routers/search.md](search.md).
- [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution) for authentication resolution.
- [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions) for the tracing-attribute keys cited above.
