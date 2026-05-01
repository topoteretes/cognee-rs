# E-09 — `GET /api/v1/sessions`

| | |
|---|---|
| Wire path | `GET /api/v1/sessions?range=&status=&limit=&offset=&order_by=&descending=` |
| Status | **Done (commit c42b513)** — `/sessions` router scaffold mounted at `/api/v1/sessions`; new `ValidatedQuery<T>` extractor lands at `crates/http-server/src/middleware/validation.rs` (Decision 9 / divergence D-1); new `iso8601_offset_option` helper at `dto/util.rs` for `Option<DateTime<Utc>>` fields. Sessions DTOs (`RangeWindow`/`OrderBy`/`ListSessionsQuery`/`SessionListResponseDTO`/`SessionRowDTO`) snake_case wire (CLEAN-01 carve-out — Python's plain dict). `list_sessions` handler enforces `1..=500` limit, calls `AclDb::authorized_dataset_ids_with_roles` (errors swallowed per Python parity), maps to `SessionListPage::has_more()`. 7 integration tests + cross-SDK harness. Reviewer-amended: 500 wire shape fixed `{detail}` → `{error}` per Python `:108-110`. |
| Depends on | LIB-05 (`SessionLifecycleDb::list_session_rows` + `SessionListFilters` + `SessionListPage` + `SessionRowWithStatus`), which transitively depends on LIB-03 (entities + migration). **Both landed (LIB-03 commit `82728f2`, LIB-05 commit `60c934a`).** |
| Effort | ~1 day. |
| Owner crate | `cognee-http-server` |

## 1. Goal

Land the paginated session list endpoint. This is the entry point for the cognee-frontend Sessions dashboard; without it, the page renders empty against the Rust backend.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `GET ""` handler | `cognee/api/v1/sessions/routers/get_sessions_router.py` | 64–110 |
| `_RangeLiteral` | same | 30–40 |
| `_range_since` | same | 39–49 |
| `_permitted_dataset_ids_for` | same | 50–60 |
| `list_session_rows` | `cognee/modules/session_lifecycle/metrics.py` | 365–435 |

### Query parameters

| Name | Type | Default | Validation |
|---|---|---|---|
| `range` | `"24h" \| "7d" \| "30d" \| "all"` (Python `_RangeLiteral` at [`get_sessions_router.py:36`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L36); **`"90d"` is NOT a Python value** — strict-parity drops it) | `"30d"` | enum |
| `status` | `Optional[str]` | None | string passthrough |
| `limit` | `int` | 50 | `1..=500` |
| `offset` | `int` | 0 | `>= 0` |
| `order_by` | `str` | `"last_activity_at"` | one of: `last_activity_at`, `started_at`, `ended_at`, `cost_usd`, `tokens_in`, `tokens_out` |
| `descending` | `bool` | `true` | — |

### Response envelope

```json
{
  "sessions": [{ ...SessionRecord.to_dict(), "effective_status": "..." }],
  "total":   <int>,
  "limit":   <int>,
  "offset":  <int>,
  "has_more": <bool>
}
```

Errors: `500 {error: "list failed"}` for any exception (Python's bare `except`).

## 3. Current Rust state (verified 2026-04-30)

- **No sessions router** at `crates/http-server/src/routers/`. No `sessions.rs` and no `mod sessions` in `routers/mod.rs`.
- **No DTO file** at `crates/http-server/src/dto/sessions.rs`. No `mod sessions` in `dto/mod.rs`.
- **No `ValidatedQuery<T>` extractor** at `crates/http-server/src/middleware/validation.rs` — only `LoginForm<T>` and `Json<T>` (the latter aliased as `ValidatedJson`). Implementation patterns to mirror: `Json::from_request` lines 52-104 (Python-shaped validation envelope construction).
- **`ComponentHandles` does NOT have a `session_lifecycle_db` slot** at `crates/http-server/src/components.rs:26-82`. It does have a `database: Arc<DatabaseConnection>` field (line 28) — and `DatabaseConnection` already implements `SessionLifecycleDb` (verified at `crates/database/src/ops/session_lifecycle.rs:836`). **Decision (handler-side):** call `state.components().database.list_session_rows(...)` directly via the `SessionLifecycleDb` trait import — no new `ComponentHandles` slot required, simpler than the originally drafted `Arc<dyn SessionLifecycleDb>` field.
- **`AclDb` trait** at `crates/database/src/traits/acl_db.rs:14-77` exposes `authorized_dataset_ids_with_roles(user_id, "read")` (line 72) — that is the Rust analogue of Python's `get_specific_user_permission_datasets`. The task originally said `AclDb::permitted_datasets`; that method does **not** exist.
- **`iso8601_offset` serde helper** is already landed at `crates/http-server/src/dto/util.rs:37` (E-03 / commit `0dafdee`). Use it on every `DateTime<Utc>` field.
- **LIB-05 deliverables consumed by this task** (verified at `crates/database/src/traits/session_lifecycle_db.rs`):
  - `SessionListFilters { user_id, permitted_dataset_ids, since, status_filter, limit, offset, order_by, descending }` (lines 38-62).
  - `SessionListPage { sessions, total, limit, offset }` + `has_more()` (lines 92-107).
  - `SessionRowWithStatus { record, effective_status }` + `to_dict()` (lines 69-87) — produces the snake_case JSON shape that Python emits.
  - `SessionLifecycleDb::list_session_rows(filters) -> Result<SessionListPage, DatabaseError>` (lines 178-181).
- **Top-level mount point** is `crates/http-server/src/lib.rs::build_router` (lines 49-111). The session router will mount at `/api/v1/sessions` between `/api/v1/recall` (line 98) and `/api/v1/llm` (line 99) — alphabetical insertion preserves the existing pattern.
- **Wire-shape parity note (corrects original §4):** Python's `to_dict()` returns a `dict` directly to FastAPI ([`models.py:68-86`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/models.py#L68-L86)) — Decision 10's `to_camel` alias generator does NOT apply because this is a plain dict, not an `OutDTO` subclass. The Python wire keys for session rows are therefore **snake_case** (`session_id`, `user_id`, `dataset_id`, `started_at`, `last_activity_at`, `ended_at`, `tokens_in`, `tokens_out`, `cost_usd`, `error_count`, `last_model`, `effective_status`). The envelope itself is also snake_case (`sessions`, `total`, `limit`, `offset`, `has_more`). The Rust DTO must therefore emit **snake_case** here — `#[serde(rename_all = "snake_case")]` — to match Python parity, **not** camelCase. (See README §1.1 — the camelCase rule is scoped to `OutDTO` / `InDTO` subclasses; raw dicts via `jsonable_encoder` keep their literal Python keys.)

## 4. Implementation steps (revised by 2026-04-30 investigation)

1. **`ValidatedQuery<T>` extractor** — new infrastructure, owned by this task. Lands FIRST so the handler can consume it. Add to [`crates/http-server/src/middleware/validation.rs`](../../../crates/http-server/src/middleware/validation.rs) sibling to the existing `Json<T>`:
   ```rust
   pub struct Query<T>(pub T);

   impl<T, S> FromRequestParts<S> for Query<T>
   where
       T: DeserializeOwned,
       S: Send + Sync,
   { /* parse req.uri().query() via serde_urlencoded::from_str::<T>; on error,
        wrap into ApiError::Validation(ValidationDetails {
            detail: json!([{"loc": ["query", "<field>"], "msg": ..., "type": "value_error"}]),
            body: None,
        }) */ }
   ```
   - Implement `FromRequestParts` (NOT `FromRequest` — query params parse from URL, not body).
   - Re-export at the module root as `ValidatedQuery` (mirror the existing `ValidatedJson` re-export).
   - Best-effort `loc` field path: parse the serde error message to extract `unknown variant` / `missing field` / `invalid value` patterns and construct `loc = ["query", "<field>"]`. When the field name cannot be determined fall back to `loc = ["query"]` and set `type = "value_error"`.
   - Unit tests in the same file:
     - `valid_query_succeeds`.
     - `unknown_order_by_returns_400_with_python_envelope` — request `?order_by=banana`; assert status 400, `detail[0].loc == ["query","order_by"]`, `type` ends with `value_error`.
     - `out_of_range_limit_returns_400_with_python_envelope` — request `?limit=999`; assert status 400, `detail[0].loc == ["query","limit"]`, `type` ends with `value_error`.

2. **DTOs** in a new `crates/http-server/src/dto/sessions.rs` (also register the module in `crates/http-server/src/dto/mod.rs`). All wire output is **snake_case** per the §3 parity note (Python emits `to_dict()` keys + envelope keys verbatim — no `to_camel` alias generator applies):
   ```rust
   /// Query parameter struct. Wire names match the literal Rust field names
   /// (snake_case), per Python `Query()` defaults at
   /// `get_sessions_router.py:64-72`. Out of scope for Decision 10's
   /// camelCase rule (which targets `OutDTO`/`InDTO` body fields, not query
   /// strings — see [`../README.md §1.1`](../README.md#11-wire-conventions-project-wide-set-by-decision-6)).
   #[derive(Debug, Deserialize)]
   pub struct ListSessionsQuery {
       #[serde(default = "default_range")]
       pub range: RangeWindow,                    // enum 24h | 7d | 30d | all (NO 90d)
       #[serde(default)]
       pub status: Option<String>,
       #[serde(default = "default_limit")]
       pub limit: u32,                            // validated 1..=500 in handler
       #[serde(default)]
       pub offset: u32,                           // u32 enforces >= 0
       #[serde(default)]
       pub order_by: OrderBy,                     // enum (Decision 9 / divergence D-1)
       #[serde(default = "default_true")]
       pub descending: bool,
   }

   #[derive(Debug, Deserialize)]
   #[serde(rename_all = "snake_case")]
   pub enum RangeWindow { #[default] _30d, _24h, _7d, All }
   // Note: actual variant names need serde rename ("24h", "7d", "30d") since
   // identifiers can't start with digits. Use #[serde(rename = "24h")] etc.

   #[derive(Debug, Deserialize, Default)]
   #[serde(rename_all = "snake_case")]
   pub enum OrderBy {
       #[default]
       LastActivityAt, StartedAt, EndedAt, CostUsd, TokensIn, TokensOut,
   }

   /// Response envelope. snake_case wire — Python returns a plain dict
   /// via `jsonable_encoder` ([`get_sessions_router.py:99-107`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L99-L107)).
   #[derive(Debug, Serialize, ToSchema)]
   #[serde(rename_all = "snake_case")]
   pub struct SessionListResponseDTO {
       pub sessions: Vec<SessionRowDTO>,
       pub total: i64,
       pub limit: u32,
       pub offset: u32,
       pub has_more: bool,
   }

   /// Per-row DTO. snake_case keys mirror Python `SessionRecord.to_dict()`
   /// at [`models.py:68-86`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/models.py#L68-L86) plus
   /// `effective_status`. Every `DateTime<Utc>` uses the Decision 6 helper.
   #[derive(Debug, Serialize, ToSchema)]
   #[serde(rename_all = "snake_case")]
   pub struct SessionRowDTO {
       pub session_id: String,
       pub user_id: String,
       pub dataset_id: Option<String>,
       pub status: String,
       #[serde(with = "crate::dto::util::iso8601_offset")]
       pub started_at: chrono::DateTime<chrono::Utc>,
       #[serde(with = "crate::dto::util::iso8601_offset")]
       pub last_activity_at: chrono::DateTime<chrono::Utc>,
       #[serde(with = "crate::dto::util::iso8601_offset_option", default)]
       pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
       pub tokens_in: i32,
       pub tokens_out: i32,
       pub cost_usd: f64,
       pub error_count: i32,
       pub last_model: Option<String>,
       pub effective_status: String,
   }
   ```
   - **Note on `iso8601_offset_option`**: the existing helper at `crates/http-server/src/dto/util.rs:37` serializes `DateTime<Utc>` (non-optional). For the `ended_at: Option<DateTime<Utc>>` field a sibling `iso8601_offset_option` helper is needed. If not yet present, add it adjacent to `iso8601_offset` in the same file, with a unit test that round-trips `None` → JSON `null` and `Some(...)` → `"<rfc3339>+00:00"`.
   - **Construction**: Map `SessionRowWithStatus { record, effective_status }` → `SessionRowDTO` field-by-field (no `to_dict()` JSON detour — direct typed construction is cheaper and satisfies the same wire shape).

3. **Router** in a new `crates/http-server/src/routers/sessions.rs` (also register the module in `crates/http-server/src/routers/mod.rs`):
   ```rust
   pub fn router() -> Router<AppState> {
       Router::new()
           .route("/", get(list_sessions))
           // E-10/E-11/E-12 will add their handlers here:
           // .route("/stats", get(get_stats))
           // .route("/cost-by-model", get(cost_by_model))
           // .route("/{session_id}", get(get_session_detail))
   }
   ```
   E-09 only registers `list_sessions`; the other three routes are stubs left for E-10/11/12. A doc-comment near the `Router::new()` block must enumerate the planned routes so reviewers don't ask why the file is so empty.

4. **Handler `list_sessions`**:
   - Signature: `async fn list_sessions(State(state): State<AppState>, AuthUser(user): AuthUser, ValidatedQuery(query): ValidatedQuery<ListSessionsQuery>) -> Result<Json<SessionListResponseDTO>, ApiError>`.
   - Resolve `permitted_dataset_ids` via `state.components().database.authorized_dataset_ids_with_roles(user.id, "read")` — Python's `get_specific_user_permission_datasets`. On error, treat as empty `Vec<Uuid>` (matches Python's bare `except` at [`get_sessions_router.py:55-58`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L55-L58)).
   - Translate `RangeWindow` → `Option<DateTime<Utc>>` via a private `range_since(range: RangeWindow) -> Option<DateTime<Utc>>` helper that mirrors Python `_range_since` at [`get_sessions_router.py:39-47`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L39-L47). `All` → `None`.
   - Validate `query.limit ∈ 1..=500`. On violation return `ApiError::Validation` with `detail[0].loc = ["query","limit"]`, `msg` mentioning the `1..=500` range, `type = "value_error"`. (Cannot be enforced via serde alone because both bounds are checks on a parsed `u32`; do it as the first line of the handler before calling LIB-05.)
   - Build `SessionListFilters { user_id: user.id, permitted_dataset_ids, since, status_filter: query.status, limit: query.limit, offset: query.offset, order_by: query.order_by.as_str().to_string(), descending: query.descending }`.
   - Call `state.components().database.list_session_rows(filters).await`. On `Err(_)` return `ApiError::Internal { message: "list failed", status: 500 }` (matches Python's bare `except` at [`get_sessions_router.py:108-110`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L108-L110), which emits `{"error": "list failed"}` with status 500). The `ApiError::Internal` wire shape `{"error": "<msg>"}` already matches per [`../README.md §1.1`](../README.md#11-wire-conventions-project-wide-set-by-decision-6).
   - Map `SessionListPage { sessions, total, limit, offset }` → `SessionListResponseDTO { sessions: page.sessions.into_iter().map(SessionRowDTO::from).collect(), total: page.total, limit: page.limit, offset: page.offset, has_more: page.has_more() }`. Use the `has_more()` accessor on `SessionListPage` (already defined at `session_lifecycle_db.rs:99-107`).
   - **Decision 9 (2026-04-29) — Acknowledged divergence D-1**: the typed `OrderBy` enum rejects unknown variants at `serde_urlencoded` deserialization time inside `ValidatedQuery`. Python silently falls back to `last_activity_at`; Rust deliberately diverges to surface client typos. See [`../README.md §1.2 D-1`](../README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output). No additional handler code — the divergence is implemented purely by the typed enum.

5. **Wire into `build_router`**:
   - **No new `ComponentHandles` slot.** `state.components().database` already implements `SessionLifecycleDb` (verified at `crates/database/src/ops/session_lifecycle.rs:836`); the handler imports `cognee_database::SessionLifecycleDb` and calls the trait method on the `Arc<DatabaseConnection>`.
   - Mount the router at `/api/v1/sessions` in `crates/http-server/src/lib.rs::build_router` (between `/api/v1/recall` line 98 and `/api/v1/llm` line 99 to preserve alphabetical ordering): `.nest("/api/v1/sessions", routers::sessions::router())`.

6. **OpenAPI** — add the path to the `paths(...)` list in `crates/http-server/src/openapi.rs:30` and the response/query schemas to `components(schemas(...))` at line 68. Derive `IntoParams` on `ListSessionsQuery`. The `OrderBy` enum's variants must be advertised as the OpenAPI `enum: [...]` allowlist so clients see the divergence D-1 contract — this is the only place the Rust↔Python schema differs.

## 5. Tests

- **Unit tests** in `crates/http-server/src/middleware/validation.rs` (alongside the existing `Json<T>` tests): three tests covering `ValidatedQuery<T>` per §4 step 1.
- `crates/http-server/tests/test_sessions_list.rs` (new):
  - `list_returns_only_caller_owned_and_permitted_dataset_sessions`.
  - `list_pagination_envelope_correct` — seed 75 rows, request limit=20, offset=40, expect `total=75`, `has_more=true`.
  - `list_status_filter_includes_abandoned_via_effective_status` — running row past abandon threshold matches `?status=abandoned`.
  - `list_order_by_invalid_returns_400_with_python_validation_envelope` — **integration test** for Decision 9 / divergence D-1: request `?order_by=banana`, assert status `400`, body `detail[0].loc == ["query","order_by"]`, `type` ends with `value_error`. **Rust-only test** (Python returns 200 with default sort — see [`../README.md §1.2 D-1`](../README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output); the cross-SDK harness must NOT include this input in shared tests).
  - `list_limit_out_of_range_returns_400_with_python_validation_envelope` — **integration test** for Decision 7: request `?limit=999`, assert status `400`, body `detail[0].loc == ["query","limit"]`, `msg` mentions the `1..=500` range, `type` ends with `value_error`.
  - `list_unauthenticated_returns_401`.
- Cross-SDK in `e2e-cross-sdk/harness/test_http_v2_sessions.py`: shared seed → `GET /sessions?range=30d&limit=10` → structural-diff response.

## 6. Acceptance criteria

- [x] `GET /sessions` returns the documented envelope with all six pagination/filter parameters honored.
- [x] Effective-status `"abandoned"` inferred at read time matches Python (verified by `list_status_filter_includes_abandoned_via_effective_status`).
- [x] Cross-SDK structural diff passes for the happy path and `?limit=999`. The `?order_by=banana` input is **not** part of the shared cross-SDK test (divergence D-1) but IS part of the Rust-side integration tests.
- [x] `ValidatedQuery<T>` extractor lands at `crates/http-server/src/middleware/validation.rs` with 3 unit tests passing; future tasks with query params reuse it.
- [x] OpenAPI advertises the route, including the `OrderBy` enum allowlist.
- [x] 500 envelope returns `{"error":"list failed"}` (Python parity per `:108-110`), not `{"detail":...}` — added regression test `list_components_not_configured_returns_500_with_python_error_envelope` during review amendment.

## 7. References

- [Python list_sessions handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L64)
- [LIB-03 — schema + entities + migration](lib-03-session-records-schema.md)
- [LIB-05 — repository trait + impl](lib-05-session-records-repo.md)
