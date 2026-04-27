# Router: recall

The `/api/v1/recall` router is the memory-oriented companion to `/api/v1/search`. It accepts the same wire DTO and the same `SearchType` enum, but layers two behaviors on top: (1) **session-first retrieval** — when the caller passes a `session_id` without explicit `datasets`, Q&A entries cached on the session are searched by keyword overlap before falling through to the graph; and (2) **automatic query-type routing** — when `query_type` is omitted (or supplied with `auto_route=true`), the rule-based `route_query()` classifier picks one of the 15 `SearchType` values from the natural-language query without an LLM call. Both behaviors are already implemented at the library layer; this doc specs the HTTP wrapper.

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../observability.md](../observability.md), [../../api-v2/recall.md](../../api-v2/recall.md), [../../api-v2/impl/recall-plan.md](../../api-v2/impl/recall-plan.md), [routers/search.md](search.md).

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

### 2.2 `POST /api/v1/recall` — semantic search (wire-level alias for `/search`)

**Behavior parity note**: Python's HTTP recall handler imports `from cognee.api.v1.search import search as cognee_search` ([`get_recall_router.py:100`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L100)) and delegates directly to it. The library-level `recall()` function (with auto-routing + session-first dispatch) is **not** reachable through this HTTP endpoint — it exists only in the Python SDK. The Rust port matches Python exactly: HTTP recall invokes `cognee_lib::search` (the search delegate), not `cognee_lib::api::recall::recall`. The two endpoints `/search` and `/recall` are wire-equivalent, differing only in their error envelopes (see below) and tags.

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body** (`application/json`): `RecallPayloadDTO`. Field-for-field copy of `SearchPayloadDTO` (see [search.md §4](search.md#4-dto-definitions)) and identical to Python's [`RecallPayloadDTO`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L23-L34). Defaults:
  - `search_type` defaults to `"GRAPH_COMPLETION"`.
  - `query` defaults to `"What is in the document?"`.
  - `system_prompt` defaults to `"Answer the question using the provided context. Be as brief as possible."`.
  - `top_k` defaults to `10`.
  - `only_context`, `verbose` default to `false`.
  - `datasets`, `dataset_ids`, `node_name` default to `null`.

  **No `session_id` / `auto_route` fields**. Python's HTTP DTO doesn't expose them and Rust matches exactly. The library-level recall capability remains available to embedders via `cognee_lib::api::recall::recall(...)` but is not surfaced through this endpoint.
- **Response body** (`200 OK`, `application/json`): `Vec<RecallResultDTO>`. Same shape as `SearchResultDTO`. Python returns the search results unmodified ([`get_recall_router.py:114`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L114) calls `jsonable_encoder(results)` directly); Rust does the same.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | 401 | `{"detail": "Unauthorized"}` | No credential. |
  | 200 with `[]` | `[]` | `PermissionDeniedError`. **Different from `/api/v1/search`** — Python recall silently returns an empty list ([`get_recall_router.py:127-128`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L127-L128)) instead of 403. Recall is meant to "always succeed" from the caller's perspective. We match. |
  | 422 | `{"error": "Recall prerequisites not met", "hint": "Run `await cognee.remember(...)` or `await cognee.add(...)` then `await cognee.cognify()` before recalling."}` | `DatabaseNotCreatedError`, `UserNotFoundError`, `CogneeValidationError`. Python: [`get_recall_router.py:116-126`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L116-L126). Note the **two-field envelope** (`error`, `hint`) is **distinct from search**'s `{error, detail}` shape — recall uses `hint` instead of `detail`. Match exactly. |
  | 409 | `{"error": "An error occurred during recall."}` | Catch-all for any other exception. Single-field `{error}` envelope. The underlying error is logged at `error!` level. Python: [`get_recall_router.py:129-135`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L129-L135). |

  **Three distinct error envelopes** in this endpoint alone (`{error, hint}`, `{error}`, plus the silent-empty for permission denied). Document loudly in `crates/http-server/src/error.rs`; gate via `ApiError::RecallError(status, ErrorBody)`.
- **Side effects**:
  1. **Search-history write** (every POST). Same two rows as `/api/v1/search`: one `Query` row, one `Result` row. Persisted via `SearchHistoryDb::log_query` + `log_result` from inside `SearchOrchestrator::search`. The history is **shared** between the two endpoints — `GET /api/v1/recall` and `GET /api/v1/search` return the same set.
  2. **Vector / graph reads** as in search.
- **Delegation target**: `state.lib.search()` — the same delegate `POST /api/v1/search` calls. Verbatim with Python's [`get_recall_router.py:100-114`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L100-L114).
- **Validation rules**: same as search.
- **Rate / size limits**: default body limit (100 MiB).
- **Permission gate**: `read` permission on each requested dataset (same as search). When permission resolution drops the entire scope, the orchestrator returns a `PermissionDenied` error which the recall handler maps to **`200 []`** (not 403, unlike search).
- **OpenAPI**: tag `["v1", "recall"]`. Request body schema from `RecallPayloadDTO`; response schema `Vec<RecallResultDTO>`. The error envelopes are declared separately in the `responses` block.
- **Telemetry**: span name `cognee.api.recall`. Attributes (per [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions)):
  - `cognee.search.query` — first 500 chars of the user query.
  - `cognee.search.type` — the supplied `search_type`.
  - `cognee.search.top_k` — resolved `top_k`.
  - `cognee.search.result_count` — final count returned to the caller.
- **Python parity notes**:
  - HTTP recall is a wire-level alias for HTTP search. The error envelopes differ (`{error, hint}` for 422; `{error}` for 409 catch-all; silent `[]` for permission denied) but the underlying request/response shapes and the search invocation itself are identical. Auto-routing and session-first dispatch live in the Python SDK's library-level `recall()` and are not part of the HTTP contract — Rust matches this exactly.

## 3. Cross-cutting behavior

### 3.1 Library-only capabilities (NOT reachable from HTTP)

The Python SDK's library-level `recall()` function exposes scope detection, auto-routing, override tracking, and result tagging — none of which are surfaced through the Python HTTP `/api/v1/recall` endpoint. The Rust port follows the same boundary:

| Capability | Library API (`cognee_lib::api::recall::recall`) | HTTP `/api/v1/recall` |
|---|---|---|
| Scope detection (`session` / `auto` / `graph`) | Yes | No (call site is `cognee_lib::search`, identical to `/search`) |
| Auto-routing via `query_router::route_query()` | Yes | No |
| Override tracking via `query_router_stats` | Yes | No |
| Session-first dispatch | Yes | No |
| Result `_source` tagging | Yes | No |

Library docs for these capabilities live in [../../api-v2/recall.md](../../api-v2/recall.md). They are intentionally not in scope for this HTTP router doc — adding them to the wire DTO would diverge from Python.

### 3.2 Error envelope inconsistency

This router has three distinct error envelope shapes:
- `{detail}` — for 401 (canonical).
- `{error}` — for 409 catch-all and GET-history 500.
- `{error, hint}` — for 422 prerequisite errors.
- `200 []` — for permission denied (silent).

Match all four exactly. Encode via dedicated `ApiError::RecallError { status, body: RecallErrorBody }` variants where `RecallErrorBody` is itself an enum.

## 4. DTO definitions

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use cognee_search::types::SearchType;

/// Mirrors Python `RecallPayloadDTO` (`get_recall_router.py:23-34`).
/// Field-for-field identical to `SearchPayloadDTO`; no Rust additions —
/// matches Python's HTTP contract exactly.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RecallPayloadDTO {
    /// Python: `search_type: SearchType = SearchType.GRAPH_COMPLETION`
    #[serde(default = "default_search_type")]
    pub search_type: SearchType,

    /// Python: `datasets: Optional[list[str]] = None`
    #[serde(default)]
    pub datasets: Option<Vec<String>>,

    /// Python: `dataset_ids: Optional[list[UUID]] = None`
    #[serde(default)]
    pub dataset_ids: Option<Vec<Uuid>>,

    /// Python: `query: str = "What is in the document?"`
    #[serde(default = "default_query")]
    pub query: String,

    /// Python: `system_prompt: Optional[str] = "Answer the question..."`.
    #[serde(default = "default_system_prompt")]
    pub system_prompt: Option<String>,

    /// Python: `node_name: Optional[list[str]] = None`
    #[serde(default)]
    pub node_name: Option<Vec<String>>,

    /// Python: `top_k: Optional[int] = 10`
    #[serde(default = "default_top_k")]
    pub top_k: Option<i32>,

    /// Python: `only_context: bool = False`
    #[serde(default)]
    pub only_context: bool,

    /// Python: `verbose: bool = False`
    #[serde(default)]
    pub verbose: bool,
}

fn default_search_type() -> SearchType { SearchType::GraphCompletion }
fn default_query() -> String { "What is in the document?".into() }
fn default_system_prompt() -> Option<String> {
    Some("Answer the question using the provided context. Be as brief as possible.".into())
}
fn default_top_k() -> Option<i32> { Some(10) }

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
5. The handler delegates to `cognee_lib::search` (the same delegate `/api/v1/search` calls) — matching Python's [`get_recall_router.py:100`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L100) verbatim. Do **not** call `cognee_lib::api::recall::recall` from this handler; that would diverge from Python.
6. OpenAPI: tag `["v1", "recall"]`; declare the three response shapes for `200`, `409`, `422`.
7. Unit tests: DTO defaults; `RecallErrorBody` serialization for both arms.
8. Integration tests in `crates/http-server/tests/test_recall.rs`:
   - POST with `search_type="GRAPH_COMPLETION"` against a populated dataset → 200 with results.
   - POST with `search_type="CYPHER"` and a Cypher query → 200 with results.
   - POST against a dataset the user can't read → returns `200 []` (NOT 403).
   - POST against an empty database → 422 with `{error, hint}`.
   - POST that triggers an arbitrary unhandled error → 409 with `{error}`.
9. Cross-SDK parity test in `e2e-cross-sdk/harness/test_http_recall.py`. Recall and search should produce identical results for identical request bodies (modulo error-envelope differences when failing).

## 6. Open questions

1. **`SearchType::Feedback` parity** — present in the Rust enum but absent from Python's `SearchType`. Drop from the HTTP DTO; keep an internal `SearchTypeInternal` superset for library callers. The HTTP wire enum mirrors Python's set verbatim. Defer the per-router resolution to the search-router doc.
2. **Telemetry parity (PostHog)** — Python's `send_telemetry(...)` is skipped in Rust per [../observability.md §1](../observability.md#1-goals--non-goals). Confirm this gap is documented for the user-facing CHANGELOG.
3. **Empty `[]` permission-denied response** — Python returns `200 []` rather than `403`, which is a deliberate UX choice (recall is "always succeed"). Confirm the e2e parity test asserts on `200` not `403`.
4. **Search-history history-write idempotency** — does Python double-write when the SDK retries? If so, Rust matches; if not, Rust matches; either way confirm via a parity test.
5. **Library-level recall reachability** — embedders who call `cognee_lib::api::recall::recall` directly should still get auto-routing and session-first dispatch. The HTTP layer simply doesn't expose them. Confirm the embedder docs ([../../api-v2/recall.md](../../api-v2/recall.md)) make this distinction clear.
3. **`?include_source=true` query parameter**: should the HTTP layer expose the library's `_source: "session" | "graph"` tag? Useful for frontends building "Recent activity" UIs that distinguish session-cached answers. Recommend yes, behind an opt-in query param to keep default wire format Python-compatible.
4. **Override counter exposure**: where does `record_override`'s state surface to the operator? Options: (a) a new `GET /api/v1/activity/recall-overrides` endpoint, (b) a span attribute on every recall request, (c) only via the in-memory span buffer (current state). Recommend (c) for phase 4; revisit if misrouting becomes a real issue.
5. **Session search algorithm**: the library uses `HashSet::intersection` (token overlap, min length 2). For a session with thousands of Q&A entries, this is O(n) per call. Should the session store cache an inverted index? Out of scope for the HTTP doc — flag in [`crates/session/`](../../../crates/session/).
6. **Permission-denied silent vs explicit**: recall returns `200 []` for permission denied; search returns `403`. Inconsistency is intentional in Python (recall is "memory" so it should "always remember nothing"). Document for cross-SDK test authors.

## 7. References

- Python router: [`cognee/api/v1/recall/routers/get_recall_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py).
- Python recall function: [`cognee/api/v1/recall/recall.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/recall.py).
- Python query router: [`cognee/api/v1/recall/query_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/query_router.py).
- Rust recall library function: [`crates/lib/src/api/recall.rs`](../../../crates/lib/src/api/recall.rs).
- Rust query router: [`crates/search/src/query_router.rs`](../../../crates/search/src/query_router.rs).
- Rust override-counter module: [`crates/search/src/query_router_stats.rs`](../../../crates/search/src/query_router_stats.rs).
- Companion: [routers/search.md](search.md).
- API v2 design doc: [../../api-v2/recall.md](../../api-v2/recall.md).
- API v2 implementation plan: [../../api-v2/impl/recall-plan.md](../../api-v2/impl/recall-plan.md).
- [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution) for authentication resolution.
- [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions) for the tracing-attribute keys cited above.
