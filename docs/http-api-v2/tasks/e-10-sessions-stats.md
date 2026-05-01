# E-10 — `GET /api/v1/sessions/stats`

| | |
|---|---|
| Wire path | `GET /api/v1/sessions/stats?range=` |
| Status | **Done (commit 0043fcf)** — `get_stats` handler + `SessionStatsDTO` (14 fields, snake_case wire — CLEAN-01 carve-out) + `StatsQuery` (`IntoParams`, default range `30d`) + `RangeWindow::as_wire_str` helper. Reuses E-09's `/sessions` router scaffold + `ValidatedQuery<T>` extractor + `range_since` helper + permitted-datasets pattern verbatim. 500 wire shape uses `ApiError::OntologyEnvelope` for `{"error":"stats failed"}` Python parity. 8 integration tests (incl. 401 + 500 envelope + range echo) + cross-SDK harness extension (`?range=30d`, `?range=all`). **No new wire divergence.** |
| Depends on | LIB-05 (`SessionLifecycleDb::aggregate_stats` + `SessionStats` — landed) and E-09 (router scaffold + `ValidatedQuery<T>` extractor + `RangeWindow` enum + `range_since` helper — landed). |
| Effort | ~0.5 day (handler + DTO + tests; all primitives already in place). |
| Owner crate | `cognee-http-server` |

## 1. Goal

Aggregate counters for the dashboard stat cards + status bar. Reads `session_records` (and computed `started_at`/`ended_at` durations) — does NOT touch `session_model_usage` (that's E-11).

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `GET /stats` handler | `cognee/api/v1/sessions/routers/get_sessions_router.py` | 112–197 |

### Response shape

```json
{
  "range":                    "30d",
  "sessions":                 <int>,
  "total_spend_usd":          <float>,
  "avg_spend_per_session_usd":<float>,
  "tokens_in":                <int>,
  "tokens_out":               <int>,
  "tokens_total":             <int>,
  "agent_time_s":             <float>,    // sum(end - started) over all rows
  "avg_session_s":            <float>,
  "success_rate":             <float>,    // completed / (completed+failed+abandoned)
  "completed":                <int>,
  "failed":                   <int>,
  "abandoned":                <int>,
  "running":                  <int>
}
```

Notes:
- `end = ended_at OR last_activity_at`. Skip rows where `started_at` or `end` is null.
- Visibility: caller's own rows OR rows whose `dataset_id` is in `permitted_dataset_ids_for(user)`.
- Uses `get_effective_status_sql()` so `running`-past-threshold rows count toward `abandoned`.
- `success_rate = 1.0` when `decided == 0` (Python explicit fallback).

## 3. Current Rust state (verified 2026-04-30)

- **No `/stats` route registered** — `crates/http-server/src/routers/sessions.rs:37-39` registers only `GET /` (`list_sessions`); the doc-comment on `router()` enumerates the planned `/stats`, `/cost-by-model`, `/{session_id}` routes for E-10/E-11/E-12 to add.
- **No `SessionStatsDTO`** in `crates/http-server/src/dto/sessions.rs` (the file currently exports `RangeWindow`, `OrderBy`, `ListSessionsQuery`, `SessionListResponseDTO`, `SessionRowDTO`).
- **LIB-05 deliverables consumed by this task** (verified):
  - `cognee_database::SessionStats` at `crates/database/src/traits/session_lifecycle_db.rs:113-127` — 13 fields covering everything except `range`. Fields: `sessions, total_spend_usd, avg_spend_per_session_usd, tokens_in, tokens_out, tokens_total, agent_time_s, avg_session_s, success_rate, completed, failed, abandoned, running`. **Note:** Python's response includes `"range": <input string>` as the first field (`get_sessions_router.py:181`); `SessionStats` does NOT carry it (range is an input parameter, not a derived counter), so the DTO must add `range` itself.
  - `SessionLifecycleDb::aggregate_stats(user_id, permitted_dataset_ids, since)` at `traits/session_lifecycle_db.rs:185-190`. Concrete impl at `crates/database/src/ops/session_lifecycle.rs:579-741` — three queries (totals / durations Rust-side fold for SQLite parity / status buckets via `effective_status_sql_fragment`), plus the `success_rate == 1.0` when `decided == 0` fallback at lines 715-720.
  - `SessionStats` is re-exported from `cognee_database::lib.rs:34` (already imported by the existing `cognee_database::{...}` import in `routers/sessions.rs:22` — extend that import in step 4).
- **E-09 deliverables consumed by this task** (verified):
  - `RangeWindow` enum at `dto/sessions.rs:35-46` (variants `H24`, `D7`, `D30`, `All`; `Default = D30`).
  - `range_since(range: RangeWindow) -> Option<DateTime<Utc>>` helper at `routers/sessions.rs:166-174` (private to the router module; reuse in-place).
  - `ValidatedQuery<T>` extractor at `crates/http-server/src/middleware/validation.rs:124` (re-exported as `ValidatedQuery` at line 170).
  - The `/sessions` router-mount at `crates/http-server/src/lib.rs::build_router` already exists (E-09).
  - `AclDb::authorized_dataset_ids_with_roles` access via `state.components().database` (E-09 pattern at `routers/sessions.rs:110-120`) — reuse verbatim, including the swallow-error semantics.
  - 500 wire-shape: `ApiError::OntologyEnvelope(msg, StatusCode)` renders `{"error": msg}` at the given status (verified at `error.rs:365`); reuse for `"stats failed"` per Python `:108-110` parity (Python `get_sessions_router.py` reuses the same catch-all idiom across all four read endpoints — `get_stats` does not have its own `try/except` in the source, but the fact that `_permitted_dataset_ids_for` is called means a 500 from underlying SeaORM bubbles up; mirror E-09's pattern for safety).
- **`iso8601_offset` helper** is already landed at `crates/http-server/src/dto/util.rs` (E-03) — not needed for `SessionStatsDTO` because the response carries no `DateTime<Utc>` fields.
- **OpenAPI registration** point at `crates/http-server/src/openapi.rs:38-39` (paths) and `:77-80` (schemas) — add `crate::routers::sessions::get_stats` and `crate::dto::sessions::SessionStatsDTO`.

## 4. Implementation steps

1. **DTO `SessionStatsDTO` + `StatsQuery`** in `crates/http-server/src/dto/sessions.rs`:
   ```rust
   /// Query parameters for `GET /api/v1/sessions/stats`.
   ///
   /// Wire names match the literal Rust field names (snake_case) — Python's
   /// `Query()` does not apply `alias_generator` to query params. Out of
   /// scope for Decision 10.
   #[derive(Debug, Clone, Deserialize, IntoParams)]
   #[into_params(parameter_in = Query)]
   pub struct StatsQuery {
       /// Time window. Default `30d`.
       #[serde(default)]
       pub range: RangeWindow,
   }

   /// Response envelope for `GET /api/v1/sessions/stats`.
   ///
   /// snake_case wire — Python returns a plain dict via `jsonable_encoder`
   /// (`get_sessions_router.py:179-196`), not an `OutDTO`, so `to_camel`
   /// does not apply (same parity carve-out as the list endpoint).
   #[derive(Debug, Clone, Serialize, ToSchema)]
   pub struct SessionStatsDTO {
       /// Echo of the input `range` query parameter (Python emits the literal
       /// string at `:181`, even when the input was the default).
       pub range: String,
       pub sessions: i64,
       pub total_spend_usd: f64,
       pub avg_spend_per_session_usd: f64,
       pub tokens_in: i64,
       pub tokens_out: i64,
       pub tokens_total: i64,
       pub agent_time_s: f64,
       pub avg_session_s: f64,
       pub success_rate: f64,
       pub completed: i64,
       pub failed: i64,
       pub abandoned: i64,
       pub running: i64,
   }
   ```
   - **Wire-shape parity note:** Python's response is a plain dict at `get_sessions_router.py:179-196` — same `to_camel` carve-out as E-09's `SessionListResponseDTO` (see [`README.md §1.1`](../README.md#11-wire-conventions-project-wide-set-by-decision-6)). Use **snake_case** keys (no `rename_all = "camelCase"`).
   - Add a small helper on `RangeWindow` to render the wire string (`"24h" / "7d" / "30d" / "all"`); call it `as_wire_str(self) -> &'static str` so the handler can stamp `range` onto the DTO without re-deriving it from the input. Keep it sibling to `OrderBy::as_column` at `dto/sessions.rs:66-80`.
   - Add the `SessionStatsDTO` and `SessionStatsDTO` struct to the snake_case allow-list in `crates/http-server/tests/test_openapi_camelcase.rs` (the same allow-list E-09's `SessionListResponseDTO` uses).
   - DTO unit tests in the same file: `stats_query_defaults_to_30d`, `session_stats_dto_emits_snake_case_keys` (assert `total_spend_usd` and `success_rate` keys appear verbatim).

2. **Handler `get_stats`** in `crates/http-server/src/routers/sessions.rs`. Sibling to `list_sessions` (lines 78-161); reuses `range_since` (lines 166-174) and the permitted-datasets resolution pattern (lines 100-120):
   ```rust
   #[utoipa::path(
       get,
       path = "/api/v1/sessions/stats",
       tag = "sessions",
       params(StatsQuery),
       responses(
           (status = 200, description = "dashboard counters", body = SessionStatsDTO),
           (status = 401, description = "unauthorized"),
           (status = 500, description = "stats failed"),
       )
   )]
   #[tracing::instrument(name = "cognee.api.sessions.stats", skip(state),
       fields(cognee.session.user_id = %user.id, cognee.session.range = ?query.range))]
   pub async fn get_stats(
       State(state): State<AppState>,
       user: AuthenticatedUser,
       ValidatedQuery(query): ValidatedQuery<StatsQuery>,
   ) -> Result<Json<SessionStatsDTO>, ApiError> {
       let components = state.components().ok_or_else(|| {
           tracing::error!("get_stats: components not configured");
           ApiError::OntologyEnvelope("stats failed".to_string(),
               StatusCode::INTERNAL_SERVER_ERROR)
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
           .aggregate_stats(user.id, &permitted_dataset_ids, since).await {
           Ok(stats) => Ok(Json(SessionStatsDTO {
               range: query.range.as_wire_str().to_string(),
               sessions: stats.sessions,
               total_spend_usd: stats.total_spend_usd,
               avg_spend_per_session_usd: stats.avg_spend_per_session_usd,
               tokens_in: stats.tokens_in,
               tokens_out: stats.tokens_out,
               tokens_total: stats.tokens_total,
               agent_time_s: stats.agent_time_s,
               avg_session_s: stats.avg_session_s,
               success_rate: stats.success_rate,
               completed: stats.completed,
               failed: stats.failed,
               abandoned: stats.abandoned,
               running: stats.running,
           })),
           Err(err) => {
               tracing::error!(error = %err, "get_stats failed");
               Err(ApiError::OntologyEnvelope(
                   "stats failed".to_string(),
                   StatusCode::INTERNAL_SERVER_ERROR,
               ))
           }
       }
   }
   ```
   - Extend the `cognee_database::{...}` import at `routers/sessions.rs:22` to include `SessionStats`.
   - Extend the `crate::dto::sessions::{...}` import at `routers/sessions.rs:25` to include `SessionStatsDTO, StatsQuery`.

3. **Wire `/stats` into the sub-router** at `crates/http-server/src/routers/sessions.rs:37-39`:
   ```rust
   pub fn router() -> Router<AppState> {
       Router::new()
           .route("/", get(list_sessions))
           .route("/stats", get(get_stats))
           // Remaining for E-11 / E-12:
           // .route("/cost-by-model", get(cost_by_model))
           // .route("/{session_id}", get(get_session_detail))
   }
   ```
   Update the doc-comment on `router()` to drop the `/stats` line (now landed) and keep the remaining stubs.

4. **OpenAPI** — add to `crates/http-server/src/openapi.rs`:
   - `crate::routers::sessions::get_stats` under `paths(...)` (alongside the existing `list_sessions` registration at line 39).
   - `crate::dto::sessions::SessionStatsDTO` and `crate::dto::sessions::StatsQuery` under `components(schemas(...))` (alongside the existing E-09 entries at lines 77-80). `StatsQuery` derives `IntoParams`, so it's listed in `paths()` via `params(StatsQuery)` on the handler — only `SessionStatsDTO` needs the schema entry; double-check whether `StatsQuery` itself needs to be in `schemas` (E-09's `ListSessionsQuery` is **not** registered there per `openapi.rs:76-80`, only `SessionListResponseDTO`/`SessionRowDTO`/`OrderBy`/`RangeWindow` — follow the same convention).

5. **Repository** — already landed in LIB-05 (commit `60c934a`). No work in `crates/database/`.

6. **Wire** — `/api/v1/sessions/stats` is reachable as soon as step 3 lands (the `/api/v1/sessions` mount in `build_router` is already active).

## 5. Tests

- **Unit tests** in `crates/http-server/src/dto/sessions.rs` (alongside the existing E-09 tests):
  - `stats_query_defaults_to_30d`.
  - `session_stats_dto_emits_snake_case_keys` — assert `"total_spend_usd"`, `"avg_spend_per_session_usd"`, `"success_rate"`, `"agent_time_s"` appear verbatim and no camelCase variants.
  - `range_window_as_wire_str_round_trips` — `H24 → "24h"`, `D7 → "7d"`, `D30 → "30d"`, `All → "all"`.
- **Integration tests** in `crates/http-server/tests/test_sessions_stats.rs` (new file; pattern lifted from E-09's `test_sessions_list.rs`):
  - `stats_empty_returns_success_rate_one` — fresh user, no sessions; assert `success_rate == 1.0` (explicit Python fallback at `:175`).
  - `stats_buckets_reflect_effective_status` — running row past threshold counts as abandoned (LIB-05's effective-status SQL takes care of this; integration test exercises the round-trip).
  - `stats_durations_skip_null_started_at` — verify `agent_time_s` excludes rows with NULL `started_at`.
  - `stats_visibility_includes_permitted_datasets` — caller's own + `authorized_dataset_ids_with_roles`-returned IDs are OR'd.
  - `stats_range_24h_filters_correctly` — seed older + newer rows; `?range=24h` includes only newer.
  - `stats_range_field_echoes_input` — request `?range=7d`; assert response `range == "7d"`.
  - `stats_unauthenticated_returns_401`.
  - `stats_components_not_configured_returns_500_with_python_error_envelope` — assert `{"error":"stats failed"}` (Python parity per `:108-110`), not `{"detail":...}`.
- Cross-SDK in `e2e-cross-sdk/harness/test_http_v2_sessions.py` (extend the file E-09 created): parity structural-diff for `?range=30d` and `?range=all` (the two stable input shapes; `24h`/`7d` are time-sensitive).

## 6. Acceptance criteria

- [x] `GET /sessions/stats` matches the Python response shape field-for-field (14 keys including `range`).
- [x] `success_rate == 1.0` when `decided == 0` (explicit fallback parity via LIB-05's `aggregate_stats`).
- [x] Effective-status logic flips abandoned-by-time rows correctly (LIB-05).
- [x] Cross-SDK structural diff passes for `?range=30d` and `?range=all`.
- [x] 500 wire shape returns `{"error":"stats failed"}` (E-09's reviewer-amended `:108-110` parity pattern reused).
- [x] OpenAPI advertises `/api/v1/sessions/stats` with `SessionStatsDTO` schema.

## 7. References

- [Python stats handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L112)
- [LIB-03 — schema + entities](lib-03-session-records-schema.md)
- [LIB-05 — repository (`aggregate_stats`)](lib-05-session-records-repo.md)
