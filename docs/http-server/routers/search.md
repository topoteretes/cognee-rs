# Router: search

The `/api/v1/search` router is the primary read-path entry point. `POST /` runs a semantic search across the user's knowledge graph using one of fifteen `SearchType` strategies and persists the question/answer pair to the search history. `GET /` returns the last 50 history rows. Compared to `/api/v1/recall`, this router does **not** auto-route the query type (the caller picks one explicitly via `search_type`) and does **not** perform session-first lookup; recall layers both on top of the same underlying `SearchOrchestrator`.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/search`
- Router file: `crates/http-server/src/routers/search.rs`
- DTO file: `crates/http-server/src/dto/search.rs`
- Python source: [`cognee/api/v1/search/routers/get_search_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py)

## 2. Endpoints

### 2.1 `GET /api/v1/search` — list the caller's search history

- **Auth**: `required` (`AuthenticatedUser` extractor — see [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution)).
- **Path params**: none.
- **Query params**: none. Python ignores any limit query argument and hardcodes `limit=0` ([`get_search_router.py:81`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L81)). We match this — the SearchHistoryDb method interprets `Some(0)` and `None` identically as "no LIMIT clause".
- **Request body**: none.
- **Response body** (`200 OK`, `application/json`): `Vec<SearchHistoryItemDTO>` (see §4). Returns both `Query` and `Result` rows interleaved by `created_at` (asc), one per row, matching Python's `UNION ... ORDER BY created_at` from [`get_history.py:18`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/operations/get_history.py#L18). Returns `[]` (not `null`) when the user has no history.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | 401 | `{"detail": "Unauthorized"}` | No credential, `REQUIRE_AUTHENTICATION=true`. |
  | 500 | `ErrorResponseDTO {error, detail}` | Any error from the relational DB. Python emits the literal `error="Internal server error"` with `detail=str(exc)` ([`get_search_router.py:84-91`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L84-L91)). The Rust handler maps `SearchError::Database` → this shape directly — it does **not** flow through `ApiError::Internal`'s usual `{"detail": "..."}` shape because Python uses `ErrorResponse {error, detail}` here. |

- **Side effects**: none. Read-only against the relational DB (`queries` and `results` tables).
- **Delegation target**: `state.lib.search().history(user.id, /* limit = */ None).await` → `SearchOrchestrator::get_history` → `SearchHistoryDb::get_history` (see [`crates/search/src/orchestration/search_orchestrator.rs:70-80`](../../../crates/search/src/orchestration/search_orchestrator.rs)).
- **Validation rules**: none.
- **Rate / size limits**: default. History tables are small per-user; no LIMIT is enforced here on purpose.
- **Permission gate**: none beyond auth. The query is scoped to `user.id` so RLS-style isolation is automatic.
- **OpenAPI**: tag `["v1", "search"]`. `responses` declared for `200`, `403`, `422`, `500` (the last three are inherited from Python's [`responses=`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L52-L55) decorator argument even though only `500` actually fires for `GET`).
- **Telemetry**: span name `cognee.api.search.history`. Attributes: `cognee.search.user_id`. Python emits a telemetry event `"Search API Endpoint Invoked"` ([`get_search_router.py:74`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L74)); Rust skips the analytics emit (no telemetry backend) and relies on `tracing` + the in-memory span buffer per [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions).
- **Python parity notes**:
  - The response is intentionally a flat list of mixed `Query` (`user="user"`) and `Result` (`user="system"`) rows, sorted by `created_at`. The frontend pairs them up by adjacency, not by `query_id`.
  - Python returns rows of length 4 (`id`, `text`, `created_at`, `user`) — this Rust DTO matches that shape; the underlying `SearchHistoryEntry` has additional fields (`query_id`, `entry_type`, `query_type`) that are **not serialized** for compat. See §4.

### 2.2 `POST /api/v1/search` — run a semantic search

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body** (`application/json`): `SearchPayloadDTO` (see §4 for the full struct). Pydantic-parity defaults are critical because Python clients post empty bodies and rely on the server-side defaults:
  - `search_type`: defaults to `"GRAPH_COMPLETION"` (string, case-sensitive uppercase).
  - `query`: defaults to `"What is in the document?"`.
  - `system_prompt`: defaults to `"Answer the question using the provided context. Be as brief as possible."`.
  - `top_k`: defaults to `10`.
  - `only_context`: defaults to `false`.
  - `verbose`: defaults to `false`.
  - `datasets`, `dataset_ids`, `node_name`: default to `null`.
- **Response body** (`200 OK`, `application/json`): `Vec<SearchResultDTO>`. Each item is `{search_result, dataset_id, dataset_name}` ([Python `SearchResult`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/types/SearchResult.py)). `search_result` is the polymorphic payload produced by the chosen retriever — see §4 for the shape per `SearchType`. May be `[]` (empty list) when no datasets resolve, when permission is denied (silently — see error table), or when the retriever genuinely returns no hits.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | 401 | `{"detail": "Unauthorized"}` | No credential. |
  | 403 | `ErrorResponseDTO {error="Permission denied", detail}` | `PermissionDeniedError` from `get_authorized_existing_datasets`. Note: Python's [`get_search_router.py:168-175`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L168-L175) returns 403 here, while [`recall`](recall.md) silently returns `[]` for the same exception. We match Python on both. |
  | 422 | `ErrorResponseDTO {error="Search prerequisites not met, hint: ...", detail}` | `DatabaseNotCreatedError`, `UserNotFoundError`, `CogneeValidationError`. Python sets `status_code = getattr(e, "status_code", 422)` ([`get_search_router.py:177`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L177)) — i.e. exception classes can override the status. Rust maps `SearchError::DatasetNotFound` → 422 by default, `SearchError::InvalidInput` → 422, others case-by-case. |
  | 500 | `ErrorResponseDTO {error="Internal server error", detail}` | Any other exception. Detail is `error.to_string()` (Python: `str(error)`). |

  All four use the `ErrorResponseDTO` envelope (`{error, detail}`), **not** the canonical `{detail: "..."}` shape used by the rest of the API. This matches Python's `ErrorResponse` model imported at [`get_search_router.py:20`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L20). Document this loudly in `crates/http-server/src/error.rs` so future maintainers don't "fix" it.
- **Side effects**:
  1. **Search-history write (every POST)**. Two relational-DB rows per call:
     - One row in `queries` (`Query` model in Python, `query_text=payload.query`, `query_type=search_type.value`, `user_id=user.id`) via `SearchHistoryDb::log_query` ([`crates/database/src/traits/search_db.rs`](../../../crates/database/src/traits/search_db.rs)).
     - One row in `results` (`Result` model, `value=jsonable_encoder(results)`, `query_id=<above>`, `user_id=user.id`) via `SearchHistoryDb::log_result`.
     - These writes happen inside the `SearchOrchestrator::search` flow — the HTTP handler does not need to invoke them directly. Failure to log is **non-fatal** (Python wraps in best-effort try/except; Rust matches by catching `DatabaseError` from the log call and emitting a `tracing::warn!`). The retriever output is returned to the caller regardless.
     - Note: the `results.value` column is a serialized JSON blob; `jsonable_encoder(results)` is the Python serializer. Rust uses `serde_json::to_string(&search_response)` for the same bytes-on-disk shape. Cross-SDK parity test confirms.
  2. **Vector-DB / graph-DB reads** depending on `search_type`. No mutations.
  3. **Optional**: when `enable_access_tracking=true` on the orchestrator, source `Data` rows have their `last_accessed` timestamp bumped (Rust-only; off by default — see [`SearchOrchestrator::with_access_tracking`](../../../crates/search/src/orchestration/search_orchestrator.rs)).
- **Delegation target**: `state.lib.search().run(SearchRequest { ... }).await` which delegates to `SearchOrchestrator::search` (see [`crates/search/src/orchestration/search_orchestrator.rs:114`](../../../crates/search/src/orchestration/search_orchestrator.rs)). The HTTP handler is responsible only for: (a) decoding the DTO, (b) building a `SearchRequest`, (c) invoking the orchestrator, (d) post-processing the response into `Vec<SearchResultDTO>`.
- **Validation rules**:
  - `top_k`: when supplied, must be a positive integer (`> 0`). Python's underlying `search()` accepts any int but yields an empty list for `top_k <= 0`; Rust returns `400 BadRequest` with `detail="top_k must be positive"`. Cross-SDK note: this is a Rust strictness add — flag in §6.
  - `dataset_ids`: when supplied alongside `datasets`, the Python search function logs a warning and prefers `dataset_ids`. Rust matches: if both are non-empty, `dataset_ids` wins and `datasets` is silently ignored ([orchestrator §141-160](../../../crates/search/src/orchestration/search_orchestrator.rs)).
  - `search_type`: must be one of the 15 enum variants below. Unknown values produce `422 ValidationError` from the custom `Json` extractor (matches Python's `RequestValidationError` handler).
  - `query`: max length 100,000 chars (Rust safety cap — Python has no explicit limit). If exceeded, return `400`. Document in §6.
- **Rate / size limits**: default body limit 100 MiB (architecture default). Per-handler request rate not enforced — see §6.
- **Permission gate**: `state.lib.permissions().visible_datasets(user.id, "read")` is computed inside the orchestrator's `dataset_resolver` when `datasets` / `dataset_ids` are supplied. Datasets the user cannot read are silently dropped (Python: `get_authorized_existing_datasets("read", user)`). When the resulting set is empty, the orchestrator returns `Err(PermissionDenied)` which the handler maps to `403`.
- **OpenAPI**: tag `["v1", "search"]`. Request body schema generated from `SearchPayloadDTO`. Response schema is `Vec<SearchResultDTO>`. Custom `200` example pinned to a `GraphCompletion` payload so the docs site shows a non-empty illustration.
- **Telemetry**: span name `cognee.api.search`. Attributes (per [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions)):
  - `cognee.search.type` — the chosen `SearchType` (`"GRAPH_COMPLETION"`, etc.).
  - `cognee.search.query.len` — character count.
  - `cognee.search.top_k` — the resolved `top_k`.
  - `cognee.search.dataset_count` — how many datasets are in scope post-resolution.
  - `cognee.search.result_count` — set after the retriever returns.
  - `cognee.search.user_id` — the caller's UUID.
  Inner spans (`cognee.search.authorize`, `cognee.search.<retriever>`) are emitted by the orchestrator and propagate to the in-memory span buffer.
- **Python parity notes**:
  - Python returns `jsonable_encoder(results)` ([`get_search_router.py:167`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L167)), which converts `SearchResult` Pydantic models to plain dicts. Rust uses `serde_json` directly and emits the same JSON.
  - The `verbose` flag toggles a "backwards-compatible" wire format in Python ([`search.py:131`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/methods/search.py#L131)) — when `false`, results are flattened to `Vec<Value>` (raw payloads); when `true`, the full `SearchResult` wrapper objects are returned. Rust must replicate `_backwards_compatible_search_results` byte-for-byte.
  - `only_context=true` short-circuits the LLM call for completion-type searches and returns just the prepared context. The orchestrator's `SearchParams.only_context` field carries this through; verify the wire format matches Python in cross-SDK tests.
  - `node_name` filters results to specific `NodeSet` membership — used for "scoped" recall workflows. Empty list and `null` are equivalent.
  - **No `session_id` parameter.** Python's `SearchPayloadDTO` does not expose `session_id`; only the `recall` router does. Search is stateless w.r.t. sessions.

## 3. Cross-cutting behavior

- **Response envelope**: search responses do NOT use the canonical `{"detail": ...}` shape; they use Python's `ErrorResponse {error, detail}` (defined at [`cognee/api/DTO.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py)). The Rust `ApiError` enum gains a dedicated `ApiError::SearchError(status, error, detail)` variant whose `IntoResponse` impl emits this two-field envelope. Re-used by `recall`.
- **Search history isolation**: history rows are scoped by `user_id`; cross-tenant leakage is impossible because the `WHERE user_id = :uid` clause is mandatory in `get_history`.
- **Dataset name resolution**: when `datasets: ["foo"]` is supplied, names are resolved via owner-scoped lookup (`get_dataset_by_name(owner_id=user.id)`). To search a dataset owned by a different user (e.g. shared via ACL), the caller must supply `dataset_ids` — names alone won't reach across owners. This is intentional Python behavior ([`get_search_router.py` docstring](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L23-L24)).
- **No retry, no streaming**: each request is a single round trip. Streaming search results is not implemented in Python and not in scope for the Rust port.

## 4. DTO definitions

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use cognee_search::types::SearchType;

/// Mirrors Python `SearchPayloadDTO` in `get_search_router.py:25-36`.
///
/// Field order matches Python intentionally so utoipa-generated OpenAPI
/// renders identically to the Pydantic schema. Defaults must round-trip
/// through `serde(default = "...")` to preserve "POST {} works" behavior.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchPayloadDTO {
    /// Python: `search_type: SearchType = Field(default=SearchType.GRAPH_COMPLETION)`
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
    /// Note this is `Option<String>` to allow explicit `null`, but defaults
    /// to a non-null string when the field is absent.
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

/// Mirrors Python `SearchHistoryItem` (defined inline in `get_search_router.py:42-46`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchHistoryItemDTO {
    pub id: Uuid,
    pub text: String,
    /// `"user"` for query rows, `"system"` for result rows.
    pub user: String,
    pub created_at: DateTime<Utc>,
}

/// Mirrors Python `SearchResult` (`cognee/modules/search/types/SearchResult.py`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchResultDTO {
    /// Polymorphic — see "Wire shape per SearchType" below.
    pub search_result: Value,
    pub dataset_id: Option<Uuid>,
    pub dataset_name: Option<String>,
}

/// Python's `ErrorResponse {error, detail}` (`cognee/api/DTO.py`).
/// Used by the search and recall routers; NOT the global `{detail}` shape.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorResponseDTO {
    pub error: String,
    pub detail: Option<String>,
}
```

### `SearchType` wire shapes

The `SearchType` enum is defined in [`crates/search/src/types/search_type.rs`](../../../crates/search/src/types/search_type.rs) with `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`. Wire values match Python's [`SearchType`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/types/SearchType.py) byte-for-byte. All 15 variants:

| Wire value (string) | Rust enum variant | Python `SearchType` | Rust status | Notes |
|---|---|---|---|---|
| `"GRAPH_COMPLETION"` | `GraphCompletion` (default) | `GRAPH_COMPLETION` | Implemented (E2E-tested) | Default — natural-language Q&A using full graph context + LLM. `search_result` is a `String`. |
| `"GRAPH_COMPLETION_COT"` | `GraphCompletionCot` | `GRAPH_COMPLETION_COT` | Implemented (E2E-tested) | Chain-of-thought variant — multi-step reasoning, returns `String`. |
| `"GRAPH_COMPLETION_CONTEXT_EXTENSION"` | `GraphCompletionContextExtension` | `GRAPH_COMPLETION_CONTEXT_EXTENSION` | Implemented (E2E-tested) | Walks N-hop neighborhoods; honors `neighborhood_depth` / `neighborhood_seed_top_k` (orchestrator-only). |
| `"GRAPH_SUMMARY_COMPLETION"` | `GraphSummaryCompletion` | `GRAPH_SUMMARY_COMPLETION` | Implemented (E2E-tested) | Returns a `String` summary built from graph triplets. |
| `"TRIPLET_COMPLETION"` | `TripletCompletion` | `TRIPLET_COMPLETION` | Implemented (E2E-tested) | Vector search over the `Triplet/text` collection (built by memify). `search_result` is a `Vec<Value>` of triplet payloads. |
| `"RAG_COMPLETION"` | `RagCompletion` | `RAG_COMPLETION` | Implemented (E2E-tested) | Vector retrieval + LLM answer over `DocumentChunk/text`. Returns `String`. |
| `"CHUNKS"` | `Chunks` | `CHUNKS` | Implemented (E2E-tested) | Raw chunk retrieval, no LLM. Returns `Vec<ChunkPayload>`. |
| `"SUMMARIES"` | `Summaries` | `SUMMARIES` | Implemented (E2E-tested) | Retrieves `TextSummary` payloads. Returns `Vec<SummaryPayload>`. |
| `"TEMPORAL"` | `Temporal` | `TEMPORAL` | Implemented (E2E-tested) | Time-aware retrieval; structured `Vec<Value>` keyed by event timestamps. |
| `"CYPHER"` | `Cypher` | `CYPHER` | Implemented | Raw Cypher pass-through to Ladybug. `search_result` is a `Vec<Vec<Value>>` (rows × columns). 422 if `query` does not parse. |
| `"NATURAL_LANGUAGE"` | `NaturalLanguage` | `NATURAL_LANGUAGE` | Implemented | NL → Cypher → execute. Returns the same `Vec<Vec<Value>>` shape as `CYPHER`. |
| `"FEELING_LUCKY"` | `FeelingLucky` | `FEELING_LUCKY` | Implemented | Combines several retrievers and lets the LLM pick. Returns `String`. |
| `"FEEDBACK"` | `Feedback` | (Python equivalent: see note) | Implemented (Rust-only label) | Rust enum variant `Feedback` exists but the wire string is **not** in the Python `SearchType` enum. The Python type set has `FEELING_LUCKY` instead — confirm whether `Feedback` should be removed from the wire DTO or kept Rust-only behind an experimental flag. **Open question §6.** |
| `"CODING_RULES"` | `CodingRules` | `CODING_RULES` | Implemented | Returns a `Vec<RulePayload>` matching the `Rule {node_set, text}` schema. Used by IDE plugins. |
| `"CHUNKS_LEXICAL"` | `ChunksLexical` | `CHUNKS_LEXICAL` | Implemented | BM25 / lexical chunk retrieval (no embedding). Returns `Vec<ChunkPayload>`. |

Per the project guide, **9 of the 15** are covered by the E2E search-matrix test. The remaining 6 (`Cypher`, `NaturalLanguage`, `FeelingLucky`, `Feedback`, `CodingRules`, `ChunksLexical`) have unit-level coverage but no cross-SDK comparison yet. Cross-SDK parity tests for the missing 6 should land in the same PR as the HTTP server (see §5 task list).

### Wire shape of `search_result` (per retriever)

The orchestrator returns `SearchResponse { search_type, result: SearchOutput, ... }` where `SearchOutput` is the tagged enum from [`crates/search/src/types/search_result.rs`](../../../crates/search/src/types/search_result.rs):

```rust
#[serde(tag = "kind", content = "data")]
pub enum SearchOutput {
    Items(Vec<SearchItem>),       // CHUNKS, SUMMARIES, TRIPLET_COMPLETION, CHUNKS_LEXICAL, TEMPORAL, CODING_RULES
    Text(String),                 // GRAPH_COMPLETION, GRAPH_COMPLETION_COT, GRAPH_SUMMARY_COMPLETION, RAG_COMPLETION, FEELING_LUCKY
    Texts(Vec<String>),           // not currently emitted by any retriever
    GraphQueryRows(Vec<Vec<Value>>), // CYPHER, NATURAL_LANGUAGE
    Rules(Vec<Rule>),             // CODING_RULES (alternate path)
    Ack { message: String },      // not used by search; reserved for memify-like ack flows
    Structured(Value),            // when SearchParams.response_schema is supplied (HTTP DTO does NOT expose it yet)
}
```

For Python parity, the HTTP layer **must flatten** `SearchOutput` into the polymorphic `search_result` field on `SearchResultDTO`. Concretely:

- `SearchOutput::Text(s)` → `search_result: s` (plain JSON string).
- `SearchOutput::Items(items)` → `search_result: items` (plain JSON array).
- `SearchOutput::GraphQueryRows(rows)` → `search_result: rows` (array of arrays).
- `SearchOutput::Rules(rules)` → `search_result: rules`.
- `SearchOutput::Structured(v)` → `search_result: v`.
- `SearchOutput::Ack { message }` → `search_result: {"message": message}` (object). Reserved.

This flattening is the responsibility of `crates/http-server/src/dto/search.rs::flatten_search_response()` and is unit-tested with one fixture per `SearchType`.

## 5. Implementation tasks

1. Add `crates/http-server/src/dto/search.rs` with `SearchPayloadDTO`, `SearchHistoryItemDTO`, `SearchResultDTO`, `ErrorResponseDTO`, plus `flatten_search_response()`. `#[derive(ToSchema)]` everywhere.
2. Add `crates/http-server/src/routers/search.rs` with two handlers (`get_search_history`, `post_search`) and a `pub fn router() -> Router<AppState>`.
3. Wire the router into `build_router()` under `nest("/search", search::router())`.
4. Extend `crates/http-server/src/error.rs` with the `ErrorResponseDTO` envelope variant; ensure `ApiError::SearchError(status, error, detail)` returns `{error, detail}` JSON.
5. Add OpenAPI tag `"v1"` and `"search"`; pin a `200` response example using the `GRAPH_COMPLETION` shape.
6. Add unit tests for DTO defaults (POST with `{}` round-trips), `SearchType` (de)serialization, and `flatten_search_response()`.
7. Add integration tests in `crates/http-server/tests/test_search.rs`:
   - History returns `[]` for a fresh user.
   - POST writes one query + one result row and the next GET returns both.
   - POST with `dataset_ids=[non_existent_uuid]` returns `403`.
   - POST with `search_type="CYPHER"` and a malformed query returns `422`.
8. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_search.py`:
   - Same payload to Python uvicorn and Rust binary; assert response shape (modulo timestamps and UUIDs) byte-equality for each of the 9 E2E-tested `SearchType` values.
9. Snapshot the generated OpenAPI fragment for `/api/v1/search` and diff against Python's `openapi.json`.

## 6. Open questions

1. **`Feedback` variant**: present in the Rust `SearchType` enum but absent from Python's. Drop it from the wire DTO; keep an internal-only `SearchTypeInternal` superset for library callers. The HTTP DTO mirrors Python's set verbatim.
2. **`top_k <= 0` strictness**: Python silently returns `[]`. Rust matches — return `[]` (no `400`) and emit a `tracing::warn!` for diagnostics.
3. **`query` length cap**: Python has none. Rust matches — no application-level cap. The HTTP body-size limit ([../architecture.md §8](../architecture.md#8-middleware-stack)) provides the only effective bound.
4. **`response_schema` parameter**: the orchestrator supports `SearchParams.response_schema` for `SearchOutput::Structured`, but Python's HTTP DTO does not expose it. Strict mirror — the field is not on the HTTP DTO. Library callers can still access it via `cognee_lib::search` directly.
5. **`session_id` on search**: Python's `search()` library function accepts `session_id`, but the HTTP DTO does not expose it. Rust matches: no `session_id` on `SearchPayloadDTO`. Confirmed.
6. **History `limit` query param**: Python does not paginate the history endpoint. Rust matches — no `?limit=N` query parameter. Frontends needing pagination must implement client-side slicing.

## 7. References

- Python router: [`cognee/api/v1/search/routers/get_search_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py).
- Python search function: [`cognee/api/v1/search/search.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/search.py).
- Python search-history operations: [`cognee/modules/search/operations/log_query.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/operations/log_query.py), [`log_result.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/operations/log_result.py), [`get_history.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/operations/get_history.py).
- Python `SearchResult`: [`cognee/modules/search/types/SearchResult.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/types/SearchResult.py).
- Python `SearchType`: [`cognee/modules/search/types/SearchType.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/types/SearchType.py).
- Python `ErrorResponse`: [`cognee/api/DTO.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py).
- Rust `SearchType`: [`crates/search/src/types/search_type.rs`](../../../crates/search/src/types/search_type.rs).
- Rust `SearchResponse` / `SearchOutput`: [`crates/search/src/types/search_result.rs`](../../../crates/search/src/types/search_result.rs).
- Rust `SearchOrchestrator`: [`crates/search/src/orchestration/search_orchestrator.rs`](../../../crates/search/src/orchestration/search_orchestrator.rs).
- Rust `SearchHistoryDb` trait: [`crates/database/src/traits/search_db.rs`](../../../crates/database/src/traits/search_db.rs).
- Companion: [routers/recall.md](recall.md) — the auto-routing variant of this endpoint.
- [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution) for authentication resolution order.
- [../tenants.md §5](../tenants.md#5-permission-resolution) for `read` permission resolution against datasets.
- [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions) for the tracing-attribute keys cited above.
