# E-09 — `GET /api/v1/sessions`

| | |
|---|---|
| Wire path | `GET /api/v1/sessions?range=&status=&limit=&offset=&order_by=&descending=` |
| Status | **Missing** |
| Depends on | LIB-05 (`SessionLifecycleDb::list_session_rows`), which transitively depends on LIB-03 (entities + migration). |
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
| `range` | `"24h" \| "7d" \| "30d" \| "90d" \| "all"` | `"30d"` | enum |
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

## 3. Current Rust state

No sessions router exists. `grep -rn "/sessions" crates/http-server/src/` returns nothing.

## 4. Implementation steps

1. **DTOs** in a new `crates/http-server/src/dto/sessions.rs`:
   ```rust
   /// Query parameter struct — wire names match the literal Rust field names
   /// (snake_case), per [`../README.md §1.1`](../README.md#11-wire-conventions-project-wide-set-by-decision-6).
   /// Query params are NOT camelCase'd because Python's FastAPI doesn't apply
   /// alias_generator to function-signature `Query()` parameters.
   #[derive(Debug, Deserialize, IntoParams)]
   pub struct ListSessionsQuery {
       #[serde(default = "default_range")]
       pub range: RangeWindow,                    // enum 24h/7d/30d/90d/all
       #[serde(default)]
       pub status: Option<String>,
       #[serde(default = "default_limit")]
       pub limit: u32,                            // validated 1..=500
       #[serde(default)]
       pub offset: u32,
       #[serde(default = "default_order_by")]
       pub order_by: OrderBy,                     // enum (Decision 9 / divergence D-1)
       #[serde(default = "default_true")]
       pub descending: bool,
   }

   /// Response DTO — wire is camelCase per Decision 10 (Python `OutDTO` parity).
   #[derive(Debug, Serialize, ToSchema)]
   #[serde(rename_all = "camelCase")]
   pub struct SessionListResponseDTO {
       pub sessions: Vec<SessionRowDTO>,
       pub total: i64,
       pub limit: u32,
       pub offset: u32,
       pub has_more: bool,                        // serializes as "hasMore"
   }
   ```
   `SessionRowDTO` flattens `SessionRecord.to_dict()` plus `effective_status`, also `#[serde(rename_all = "camelCase")]` — the wire keys are `sessionId`, `userId`, `datasetId`, `startedAt`, `lastActivityAt`, `endedAt`, `tokensIn`, `tokensOut`, `costUsd`, `errorCount`, `lastModel`, `effectiveStatus`. Every `DateTime<Utc>` field on `SessionRowDTO` (`started_at`, `last_activity_at`, `ended_at`) MUST use `#[serde(with = "crate::dto::util::iso8601_offset")]` per [`../README.md §1.1 Wire conventions`](../README.md#11-wire-conventions-project-wide-set-by-decision-6) (Decision 6). The helper module was landed by E-03 (A-2 in the phase order); this task is a consumer.

2. **Router** in a new `crates/http-server/src/routers/sessions.rs`:
   ```rust
   pub fn router() -> Router<AppState> {
       Router::new()
           .route("/", get(list_sessions))
           // E-10/E-11/E-12 also live here:
           .route("/stats", get(get_stats))
           .route("/cost-by-model", get(cost_by_model))
           .route("/{session_id}", get(get_session_detail))
   }
   ```

3. **Handler `list_sessions`**:
   - Resolve `permitted_dataset_ids` for `user.id` via `AclDb::permitted_datasets(user.id, "read")` — same call the activity router uses.
   - Translate `RangeWindow` to `Option<DateTime<Utc>>` (the `_range_since` Python helper).
   - Validate `limit` ∈ `1..=500` (return **400** with the Python validation envelope per [`../README.md §1.1`](../README.md#11-wire-conventions-project-wide-set-by-decision-6)).
   - **Decision 9 (2026-04-29) — Acknowledged divergence D-1**: typed `OrderBy` enum rejects unknown variants at deserialization time. Python silently falls back to `last_activity_at`; Rust deliberately diverges to surface client typos. See [`../README.md §1.2 D-1`](../README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output).
   - Call `state.components.session_lifecycle_db.list_session_rows(...)`.
   - Map result to `SessionListResponseDTO`. `has_more = offset + sessions.len() < total`.

4. **`ValidatedQuery<T>` extractor** — new infrastructure, owned by this task:
   - Add `crate::middleware::validation::Query<T>` (re-exported as `ValidatedQuery`) alongside the existing `ValidatedJson<T>` at [`crates/http-server/src/middleware/validation.rs`](../../../crates/http-server/src/middleware/validation.rs).
   - Implement `FromRequestParts` (not `FromRequest` — query params parse from URL, not body): parse the query string with `serde_urlencoded::from_str::<T>`; on error, wrap into `ApiError::Validation(ValidationDetails { detail, body: None })` with `loc: ["query", "<field>"]` derived from the serde error path.
   - Use this for the `ListSessionsQuery` extraction in this handler (and every later v2 handler with query params).
   - Unit tests:
     - `valid_query_succeeds`.
     - `unknown_order_by_returns_400_with_python_envelope` — request `?order_by=banana`; assert status 400, `detail[0].loc == ["query","order_by"]`, `type` ends with `value_error`.
     - `out_of_range_limit_returns_400_with_python_envelope` — request `?limit=999`; assert as above.

5. **Wire into `AppState`**:
   - Add `session_lifecycle_db: Arc<dyn SessionLifecycleDb>` to `ComponentHandles` (or expose `database.session_lifecycle()`). Mirror the pattern used for `IngestDb`.
   - Mount the router at `/api/v1/sessions` in `crates/http-server/src/lib.rs::build_router`.

6. **OpenAPI** — add the path with `IntoParams` derive on `ListSessionsQuery`. Note: the `OrderBy` enum's variants must be advertised as the `enum: [...]` schema so OpenAPI clients see the allowlist; Python's permissive `str` typing is a divergence — see [`../README.md §1.2 D-1`](../README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output).

## 5. Tests

- `crates/http-server/tests/test_sessions_list.rs` (new):
  - `list_returns_only_caller_owned_and_permitted_dataset_sessions`.
  - `list_pagination_envelope_correct` — seed 75 rows, request limit=20, offset=40, expect `total=75`, `has_more=true`.
  - `list_status_filter_includes_abandoned_via_effective_status` — running row past abandon threshold matches `?status=abandoned`.
  - `list_order_by_invalid_returns_400_with_python_validation_envelope` — **integration test** for Decision 9 / divergence D-1: request `?order_by=banana`, assert status `400`, body `detail[0].loc == ["query","order_by"]`, `type` ends with `value_error`. **Rust-only test** (Python returns 200 with default sort — see [`../README.md §1.2 D-1`](../README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output); the cross-SDK harness must NOT include this input in shared tests).
  - `list_limit_out_of_range_returns_400_with_python_validation_envelope` — **integration test** for Decision 7: request `?limit=999`, assert status `400`, body `detail[0].loc == ["query","limit"]`, `msg` mentions the `1..=500` range, `type` ends with `value_error`.
  - `list_unauthenticated_returns_401`.
- Cross-SDK in `e2e-cross-sdk/harness/test_http_v2_sessions.py`: shared seed → `GET /sessions?range=30d&limit=10` → structural-diff response.

## 6. Acceptance criteria

- [ ] `GET /sessions` returns the documented envelope with all six pagination/filter parameters honored.
- [ ] Effective-status `"abandoned"` inferred at read time matches Python.
- [ ] Cross-SDK structural diff passes for the happy path and `?limit=999` (both backends 400). The `?order_by=banana` input is **not** part of the shared cross-SDK test (divergence D-1) but IS part of the Rust-side integration tests.
- [ ] `ValidatedQuery<T>` extractor lands at `crates/http-server/src/middleware/validation.rs` with all 3 unit tests passing; future tasks with query params reuse it.
- [ ] OpenAPI advertises the route, including the `OrderBy` enum allowlist.

## 7. References

- [Python list_sessions handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L64)
- [LIB-03 — schema + entities + migration](lib-03-session-records-schema.md)
- [LIB-05 — repository trait + impl](lib-05-session-records-repo.md)
