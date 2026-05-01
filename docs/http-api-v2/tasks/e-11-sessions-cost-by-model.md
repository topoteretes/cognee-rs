# E-11 — `GET /api/v1/sessions/cost-by-model`

| | |
|---|---|
| Wire path | `GET /api/v1/sessions/cost-by-model?range=` |
| Status | **Done (commit f27aa06)** — `cost_by_model` handler + `CostByModelDTO` (5 fields, snake_case wire — CLEAN-01 carve-out) + `CostByModelQuery` (`IntoParams`, default range `30d`) + `From<CostByModelRow>` impl. Reuses E-09's `/sessions` router scaffold + `ValidatedQuery<T>` extractor + `range_since` helper + permitted-datasets pattern verbatim. 500 wire shape uses `ApiError::OntologyEnvelope` for `{"error":"cost-by-model failed"}` Python parity. 9 integration tests + cross-SDK harness extension. **No new wire divergence.** |
| Depends on | LIB-05 (`SessionLifecycleDb::cost_by_model` + `CostByModelRow` — landed `60c934a`); E-09 (router scaffold + `ValidatedQuery<T>` extractor + `RangeWindow` enum + `range_since` helper — landed `c42b513`); E-10 (`StatsQuery` pattern + `RangeWindow::as_wire_str` for input echo — landed `0043fcf`). LIB-03 (`session_model_usage` table — landed `82728f2`) is consumed transitively via LIB-05's repo. |
| Effort | ~0.5 day (handler + DTO + tests; all primitives already in place). |
| Owner crate | `cognee-http-server` |

## 1. Goal

Per-model cost + token breakdown for the dashboard's "Spend by model" widget. Aggregates `session_model_usage` rows joined back to `session_records` to scope by `last_activity_at >= since`.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `GET /cost-by-model` handler | `cognee/api/v1/sessions/routers/get_sessions_router.py` | 198–252 |

### Response shape

```json
[
  {
    "model": "gpt-4o-mini",
    "session_count": 42,        // distinct session_ids using this model
    "cost_usd": 1.23,
    "tokens_in": 12345,
    "tokens_out": 6789
  },
  ...
]
```

Sorted by `SUM(cost_usd)` descending. Empty list when no usage rows.

Notes:
- `model` falls back to `"unknown"` when null in the source row (`row.model or "unknown"`, Python `:244`). LIB-05's repo already applies the fallback in Rust at `crates/database/src/ops/session_lifecycle.rs:822`.
- Visibility joins through `session_records` so dataset-permission filtering works (Python `:225-231`; Rust `:806-808` SQL).
- `session_count` uses `COUNT(DISTINCT session_id)` — not raw row count (Python `:220`; Rust `:802`).
- Wire shape is a **plain JSON array** (`jsonable_encoder([...])`) — top-level type is `Vec<CostByModelDTO>`, no envelope.

## 3. Current Rust state (verified 2026-04-30)

- **No `/cost-by-model` route registered** — `crates/http-server/src/routers/sessions.rs:40-44` mounts `GET /` (`list_sessions`) and `GET /stats` (`get_stats`); the doc-comment on `router()` at lines 33-39 still enumerates `/cost-by-model` (E-11) and `/{session_id}` (E-12) as planned routes.
- **No `CostByModelDTO`** in `crates/http-server/src/dto/sessions.rs` (current exports: `RangeWindow`, `OrderBy`, `ListSessionsQuery`, `StatsQuery`, `SessionListResponseDTO`, `SessionRowDTO`, `SessionStatsDTO`).
- **LIB-05 deliverables consumed by this task** (verified):
  - `cognee_database::CostByModelRow` at `crates/database/src/traits/session_lifecycle_db.rs:129-139` — 5 fields (`model: String`, `session_count: i64`, `cost_usd: f64`, `tokens_in: i64`, `tokens_out: i64`). Field shape and types are identical to the Python wire response, so the DTO is a one-to-one map.
  - `SessionLifecycleDb::cost_by_model(user_id, permitted_dataset_ids, since)` at `traits/session_lifecycle_db.rs:194-199`. Concrete impl at `crates/database/src/ops/session_lifecycle.rs:759-829` — single SQL with `JOIN session_records sr ON ... GROUP BY smu.model ORDER BY SUM(smu.cost_usd) DESC`. Already applies the `"unknown"` fallback for null models (`:822`).
  - `CostByModelRow` is re-exported from `cognee_database::lib.rs:32` and `traits/mod.rs:18` (extend the existing `cognee_database::{...}` import in `routers/sessions.rs:22`).
- **E-09 / E-10 deliverables consumed by this task** (verified):
  - `RangeWindow` enum at `dto/sessions.rs:35-46` (variants `H24`, `D7`, `D30`, `All`; `Default = D30`).
  - `RangeWindow::as_wire_str(self) -> &'static str` at `dto/sessions.rs:82-96` (returns `"24h" / "7d" / "30d" / "all"`). Not strictly needed for E-11 because the response is a plain array with no `range` echo, but listed here so the next task does not re-introduce it.
  - `range_since(range: RangeWindow) -> Option<DateTime<Utc>>` helper at `routers/sessions.rs:293-301` (private to the router module; reuse in-place — same as E-10 did).
  - `ValidatedQuery<T>` extractor at `crates/http-server/src/middleware/validation.rs:124` (re-exported as `ValidatedQuery` at line 170).
  - The `/sessions` router-mount at `crates/http-server/src/lib.rs::build_router` already exists (E-09). Adding a new route is one line in `router()` at `routers/sessions.rs:40-44`.
  - `AclDb::authorized_dataset_ids_with_roles` access via `state.components().database` (E-09 / E-10 pattern at `routers/sessions.rs:115-125` and `:222-232`) — reuse verbatim, including the swallow-error semantics.
  - 500 wire-shape: `ApiError::OntologyEnvelope(msg, StatusCode)` renders `{"error": msg}` at the given status — reuse for `"cost-by-model failed"` per Python `:108-110` parity (Python's `get_sessions_router.py` reuses the same catch-all idiom across all four read endpoints; the `cost_by_model` handler does not have its own `try/except` in the source, but a 500 from the underlying SeaORM call bubbles up through `_permitted_dataset_ids_for` — mirror E-10's `routers/sessions.rs:208-217, 274-286` pattern).
- **`iso8601_offset` helper** is already landed at `crates/http-server/src/dto/util.rs` (E-03) — not needed for `CostByModelDTO` because the response carries no `DateTime<Utc>` fields.
- **OpenAPI registration** point at `crates/http-server/src/openapi.rs:38-41` (paths) and `:78-84` (schemas) — add `crate::routers::sessions::cost_by_model` and `crate::dto::sessions::CostByModelDTO`.

## 4. Implementation steps

1. **DTO `CostByModelDTO` + `CostByModelQuery`** in `crates/http-server/src/dto/sessions.rs`:
   ```rust
   /// Query parameters for `GET /api/v1/sessions/cost-by-model`.
   ///
   /// Wire names match the literal Rust field names (snake_case) — Python's
   /// `Query()` does not apply `alias_generator` to query params. Out of
   /// scope for Decision 10.
   ///
   /// Mirrors Python's `Query(...)` default at
   /// [`get_sessions_router.py:200`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L200).
   #[derive(Debug, Clone, Deserialize, IntoParams)]
   #[into_params(parameter_in = Query)]
   pub struct CostByModelQuery {
       /// Time window. Default `30d`.
       #[serde(default)]
       pub range: RangeWindow,
   }

   /// Per-model row for `GET /api/v1/sessions/cost-by-model`.
   ///
   /// snake_case wire — Python returns a plain list-of-dicts via
   /// `jsonable_encoder` ([`get_sessions_router.py:241-251`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L241-L251)),
   /// not an `OutDTO`, so `to_camel` does not apply (same parity carve-out
   /// as the list and stats endpoints). Field-for-field parity with
   /// [`cognee_database::CostByModelRow`](../../../cognee_database/struct.CostByModelRow.html).
   #[derive(Debug, Clone, Serialize, ToSchema)]
   pub struct CostByModelDTO {
       pub model: String,
       pub session_count: i64,
       pub cost_usd: f64,
       pub tokens_in: i64,
       pub tokens_out: i64,
   }

   impl From<cognee_database::CostByModelRow> for CostByModelDTO {
       fn from(row: cognee_database::CostByModelRow) -> Self {
           Self {
               model: row.model,
               session_count: row.session_count,
               cost_usd: row.cost_usd,
               tokens_in: row.tokens_in,
               tokens_out: row.tokens_out,
           }
       }
   }
   ```
   - **Wire-shape parity note:** Python's response is a plain list of dicts at `get_sessions_router.py:241-251` — same `to_camel` carve-out as E-09's `SessionListResponseDTO` and E-10's `SessionStatsDTO` (see [`README.md §1.1`](../README.md#11-wire-conventions-project-wide-set-by-decision-6)). Use **snake_case** keys (no `rename_all = "camelCase"`).
   - Add `CostByModelDTO` to the snake_case allow-list in `crates/http-server/tests/test_openapi_camelcase.rs` (same allow-list E-09's `SessionListResponseDTO` and E-10's `SessionStatsDTO` use).
   - DTO unit tests in the same file: `cost_by_model_query_defaults_to_30d`, `cost_by_model_dto_emits_snake_case_keys` (assert `session_count`, `cost_usd`, `tokens_in`, `tokens_out` keys appear verbatim).

2. **Handler `cost_by_model`** in `crates/http-server/src/routers/sessions.rs`. Sibling to `get_stats` (lines 203-288); reuses `range_since` (lines 293-301) and the permitted-datasets resolution pattern (lines 222-232):
   ```rust
   #[utoipa::path(
       get,
       path = "/api/v1/sessions/cost-by-model",
       tag = "sessions",
       params(CostByModelQuery),
       responses(
           (status = 200, description = "per-model cost + token breakdown", body = Vec<CostByModelDTO>),
           (status = 401, description = "unauthorized"),
           (status = 500, description = "cost-by-model failed"),
       )
   )]
   #[tracing::instrument(
       name = "cognee.api.sessions.cost_by_model",
       skip(state),
       fields(
           cognee.session.user_id = %user.id,
           cognee.session.range = ?query.range,
       )
   )]
   pub async fn cost_by_model(
       State(state): State<AppState>,
       user: AuthenticatedUser,
       ValidatedQuery(query): ValidatedQuery<CostByModelQuery>,
   ) -> Result<Json<Vec<CostByModelDTO>>, ApiError> {
       let components = state.components().ok_or_else(|| {
           tracing::error!("cost_by_model: components not configured");
           ApiError::OntologyEnvelope(
               "cost-by-model failed".to_string(),
               StatusCode::INTERNAL_SERVER_ERROR,
           )
       })?;

       let permitted_dataset_ids = match components
           .database
           .authorized_dataset_ids_with_roles(user.id, "read")
           .await {
           Ok(ids) => ids,
           Err(err) => {
               tracing::warn!(error = %err, "authorized_dataset_ids_with_roles failed; proceeding with empty set");
               Vec::new()
           }
       };

       let since = range_since(query.range);

       match components.database
           .cost_by_model(user.id, &permitted_dataset_ids, since).await {
           Ok(rows) => Ok(Json(rows.into_iter().map(CostByModelDTO::from).collect())),
           Err(err) => {
               tracing::error!(error = %err, "cost_by_model failed");
               Err(ApiError::OntologyEnvelope(
                   "cost-by-model failed".to_string(),
                   StatusCode::INTERNAL_SERVER_ERROR,
               ))
           }
       }
   }
   ```
   - The `cognee_database::{...}` import at `routers/sessions.rs:22` already covers `AclDb`, `SessionLifecycleDb`, `SessionListFilters`, `SessionStats`. Extend it with `CostByModelRow` so the `From<…>` impl in step 1 compiles. (Rust's repo already exports this name; see `crates/database/src/lib.rs:32`.)

3. **Wire** at `/api/v1/sessions/cost-by-model`. In `routers/sessions.rs::router()` at lines 40-44:
   ```rust
   pub fn router() -> Router<AppState> {
       Router::new()
           .route("/", get(list_sessions))
           .route("/stats", get(get_stats))
           .route("/cost-by-model", get(cost_by_model))
   }
   ```
   Update the doc-comment above (lines 33-39) — strike `/cost-by-model` from the "remaining handlers" list and leave only `/{session_id}` (E-12).
   No change to `crates/http-server/src/lib.rs::build_router` — the `/sessions` mount already exists (E-09).

4. **OpenAPI registration**. In `crates/http-server/src/openapi.rs`:
   - Paths block at `:38-41`: add `crate::routers::sessions::cost_by_model,` after the existing `get_stats` line.
   - Schemas block at `:78-84`: add `crate::dto::sessions::CostByModelDTO,` after the existing `SessionStatsDTO` line.

## 5. Tests

- `crates/http-server/tests/test_sessions_cost_by_model.rs` (new file, mirrors `test_sessions_stats.rs` shape):
  - `single_model_session_yields_one_row` — one session, one model, one row.
  - `mixed_model_session_splits_correctly` — one session with two models → two rows; `session_count == 1` for each row.
  - `null_model_falls_back_to_unknown` — insert a `session_model_usage` row with `model = NULL`; expect `"model": "unknown"` (LIB-05 already applies the fallback at `ops/session_lifecycle.rs:822`).
  - `range_24h_filters_through_join` — two sessions, only one within the 24h window via the `sr.last_activity_at >= since` join filter.
  - `visibility_through_dataset_permissions` — a session owned by another user but on a permitted dataset is included; one on a non-permitted dataset is excluded.
  - `ordered_by_total_cost_desc` — three rows with different cost totals; expect descending order.
  - `empty_response_is_array_not_null` — fresh DB returns `[]`, not `null`.
  - `unauthenticated_returns_401` — no auth header → 401.
  - `internal_error_returns_envelope` — components un-wired → 500 with `{"error": "cost-by-model failed"}`.
- `e2e-cross-sdk/harness/test_http_v2_sessions.py` — extend with a `cost-by-model` parity test using the same dual-CLI fixture E-09/E-10 use (single session emitting one usage row; structural equality of the JSON array for both backends).

## 6. Acceptance criteria

- [x] Empty response is `[]` (not `null`) — `empty_response_is_array_not_null` test.
- [x] `session_count` uses `COUNT(DISTINCT)` (LIB-05 `cost_by_model` impl).
- [x] Sorted by total `cost_usd` descending — `ordered_by_total_cost_desc` test.
- [x] Null/empty model name → `"unknown"` — `null_model_falls_back_to_unknown` test (recreates table without NOT NULL on `model` to seed; documents the LIB-03 schema constraint and `unwrap_or_else` defensive fallback).
- [x] Wire keys are snake_case (`session_count`, `cost_usd`, `tokens_in`, `tokens_out`) — `cost_by_model_dto_emits_snake_case_keys` test enforces.
- [x] OpenAPI camelCase regression test passes (CLEAN-01 carve-out — `CostByModelDTO` on the snake_case allow-list).
- [x] Cross-SDK structural diff passes — `test_sessions_cost_by_model_range_all_structural_parity`.

## 7. References

- [Python cost_by_model handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L198)
- [LIB-03 — `session_model_usage` table](lib-03-session-records-schema.md)
- [LIB-05 — `cost_by_model` repository method](lib-05-session-records-repo.md) (`SessionLifecycleDb::cost_by_model` at `crates/database/src/traits/session_lifecycle_db.rs:194-199`; impl at `crates/database/src/ops/session_lifecycle.rs:759-829`)
- [E-09 — sessions list (router scaffold + `ValidatedQuery<T>` + `range_since`)](e-09-sessions-list.md)
- [E-10 — sessions stats (sister endpoint, identical handler shape)](e-10-sessions-stats.md)
