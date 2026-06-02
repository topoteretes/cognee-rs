# Router: sessions

Session-management dashboard endpoints. Powers the frontend's "Sessions" panel: a paginated session list, aggregate stat cards, a per-model spend breakdown, and a single-session detail view (record + the trailing QA / trace history). All four endpoints are read-only and share the same visibility model — the caller's own sessions plus sessions attached to a dataset the caller has `read` permission on.

This router belongs to the v2 memory-API effort; its handlers cite "Decision N / divergence D-N" rationale notes that live alongside the broader v2 wire-shape conventions.

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../observability.md](../observability.md), [../tenants.md](../tenants.md).

## 1. Mount & file
- Mount prefix: `/api/v1/sessions`
- Router file: `crates/http-server/src/routers/sessions.rs`
- DTOs: `crates/http-server/src/dto/sessions.rs`
- Python source: [`cognee/api/v1/sessions/routers/get_sessions_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py)

## 2. Endpoints

Four endpoints, all `GET`. Listed path-first (`/`, then the fixed sub-paths, then the parameterized detail route).

### 2.1 `GET /api/v1/sessions` — paginated session list

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params** (`ListSessionsQuery`; literal snake_case wire names — Python `Query()` does not apply the camelCase alias generator to query params):
  - `range` — `24h | 7d | 30d | all`. Default `30d`. Unknown values → `400`.
  - `status` — optional effective-status filter (`completed | failed | abandoned | running`). String passthrough; the `effective_status` SQL expression makes `abandoned` match running rows past the idle threshold.
  - `limit` — page size, validated `1..=500` in the handler. Default `50`.
  - `offset` — page offset (`u32`). Default `0`.
  - `order_by` — `last_activity_at | started_at | ended_at | cost_usd | tokens_in | tokens_out`. Default `last_activity_at`. Unknown values → `400` (divergence D-1: Python silently falls back to `last_activity_at`; Rust rejects to surface client typos).
  - `descending` — `true` → DESC. Default `true`.
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, `SessionListResponseDTO` — **snake_case** wire (see §3):
  - `sessions: Vec<SessionRowDTO>`
  - `total: i64`
  - `limit: u32`, `offset: u32`, `has_more: bool`

  Each `SessionRowDTO`: `session_id`, `user_id`, `dataset_id` (nullable), `status`, `started_at`, `last_activity_at`, `ended_at` (nullable), `tokens_in`, `tokens_out`, `cost_usd`, `error_count`, `last_model` (nullable), `effective_status`. Timestamps use the Decision-6 `+00:00` ISO-8601 shape.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": [{"loc": ["query", "limit"], "msg": "ensure this value is in 1..=500 (got N)", "type": "value_error"}]}` | `limit` outside `1..=500`. |
  | `400` | `{"detail": [...]}` | Unknown `range` / `order_by` value (typed enum rejects at deserialize). |
  | `401` | `{"detail": "Unauthorized"}` | Missing/invalid auth. |
  | `500` | `{"error": "list failed"}` | Components un-wired or repo error. Note the `{error}` envelope (not `{detail}`) — Python's catch-all returns `JSONResponse(500, {"error": "list failed"})`; rendered via `ApiError::OntologyEnvelope`. |

- **Side effects**: read-only. Resolves the caller's permitted dataset ids via `AclDb::authorized_dataset_ids_with_roles(user_id, "read")` (errors swallowed → empty set, Python parity), then `SessionLifecycleDb::list_session_rows(filters)`.
- **Delegation target**: `cognee_database::SessionLifecycleDb::list_session_rows` + `AclDb::authorized_dataset_ids_with_roles`.
- **Validation rules**: `limit ∈ 1..=500` enforced in the handler (Python's `Query(ge=1, le=500)` analog).
- **Permission gate**: visibility = own sessions ∪ sessions on `read`-permitted datasets. No explicit per-row 403.
- **OpenAPI**: tag `sessions`. Response `SessionListResponseDTO`.
- **Telemetry**: span `cognee.api.sessions.list`. Attributes: `cognee.session.user_id`, `cognee.session.range`, `cognee.session.limit`, `cognee.session.offset`.
- **Python parity notes**: snake_case response (plain dict via `jsonable_encoder`, not an `OutDTO`). `_permitted_dataset_ids_for` swallows every exception and returns empty.

### 2.2 `GET /api/v1/sessions/stats` — dashboard aggregate counters

- **Auth**: `required`.
- **Path params**: none.
- **Query params** (`StatsQuery`): `range` — `24h | 7d | 30d | all`. Default `30d`.
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, `SessionStatsDTO` — **snake_case** wire. 14 fields: `range` (echoes the input literal, even when defaulted) + 13 counters from `cognee_database::SessionStats`: `sessions`, `total_spend_usd`, `avg_spend_per_session_usd`, `tokens_in`, `tokens_out`, `tokens_total`, `agent_time_s`, `avg_session_s`, `success_rate`, `completed`, `failed`, `abandoned`, `running`.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | Missing/invalid auth. |
  | `500` | `{"error": "stats failed"}` | Components un-wired or repo error (`{error}` envelope, same catch-all pattern as §2.1). |

- **Side effects**: read-only. Same permitted-dataset resolution as §2.1, then `SessionLifecycleDb::aggregate_stats(user_id, &permitted_dataset_ids, since)`.
- **Delegation target**: `cognee_database::SessionLifecycleDb::aggregate_stats`.
- **Permission gate**: same visibility model as §2.1.
- **OpenAPI**: tag `sessions`. Response `SessionStatsDTO`.
- **Telemetry**: span `cognee.api.sessions.stats`. Attributes: `cognee.session.user_id`, `cognee.session.range`.
- **Python parity notes**: snake_case plain dict; `range` echoes the input string.

### 2.3 `GET /api/v1/sessions/cost-by-model` — per-model spend breakdown

- **Auth**: `required`.
- **Path params**: none.
- **Query params** (`CostByModelQuery`): `range` — `24h | 7d | 30d | all`. Default `30d`.
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, a **plain JSON array** (not an envelope) of `CostByModelDTO` rows — **snake_case** wire: `model`, `session_count`, `cost_usd`, `tokens_in`, `tokens_out`. Sorted by `SUM(cost_usd)` descending; null-model rows fold into a single `"unknown"` bucket (the fallback is applied in the repo layer).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | Missing/invalid auth. |
  | `500` | `{"error": "cost-by-model failed"}` | Components un-wired or repo error (`{error}` envelope). |

- **Side effects**: read-only. Same permitted-dataset resolution as §2.1, then `SessionLifecycleDb::cost_by_model(user_id, &permitted_dataset_ids, since)`.
- **Delegation target**: `cognee_database::SessionLifecycleDb::cost_by_model`.
- **Permission gate**: same visibility model as §2.1.
- **OpenAPI**: tag `sessions`. Response `Vec<CostByModelDTO>`.
- **Telemetry**: span `cognee.api.sessions.cost_by_model`. Attributes: `cognee.session.user_id`, `cognee.session.range`.
- **Python parity notes**: snake_case plain list-of-dicts.

### 2.4 `GET /api/v1/sessions/{session_id}` — single-session detail

- **Auth**: `required`.
- **Path params**: `session_id: String`.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, `SessionDetailDTO` — **snake_case** wire. The `SessionRowDTO` fields are `#[serde(flatten)]`-ed to the top level (no `record` wrapper), plus five extras:
  - `label: Option<String>` — first non-empty QA `question` truncated to 120 **chars** (not bytes), else first non-empty trace `origin_function`, else `null`.
  - `msg_count: usize` — **pre-truncation** QA list length.
  - `tool_calls: usize` — **pre-truncation** trace list length.
  - `qas: Vec<Value>` — trailing-20 QA entries (untyped JSON dicts, matching Python's wire shape).
  - `traces: Vec<Value>` — trailing-20 trace steps (untyped JSON dicts).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | Missing/invalid auth. |
  | `404` | `{"detail": "session not found"}` | Session invisible or absent. **Note the `{detail}` envelope** — this is the only endpoint in this router that uses `{detail}` (FastAPI `HTTPException(404)` parity); the other three use `{error}`. |
  | `500` | `{"error": "session detail failed"}` | Components un-wired or repo error (`{error}` envelope). |

- **Side effects**: read-only. Resolves permitted datasets, then `SessionLifecycleDb::get_session_row(session_id, user_id, &permitted_dataset_ids, false)`. Cache content (`qas`, `traces`) is fetched **owner-aware** — under the session row's own `user_id`, not the authenticated caller — so a dataset-grant viewer sees the actual content of someone else's session. Cache reads are best-effort (`try/except: pass` parity); failures yield empty lists.
- **Delegation target**: `cognee_database::SessionLifecycleDb::get_session_row`, plus `SessionStore::get_latest_qa_entries` and `SessionManager::get_agent_trace_session` for cache content.
- **Validation rules**: none beyond auth; `session_id` is a free-form string (not a UUID-typed path param).
- **Permission gate**: same visibility model as §2.1; invisible rows return `404` (not `403`).
- **OpenAPI**: tag `sessions`. Path param `session_id: String`. Response `SessionDetailDTO`.
- **Telemetry**: span `cognee.api.sessions.detail`. Attributes: `cognee.session.user_id`, `cognee.session.session_id`.
- **Python parity notes**: `msg_count` / `tool_calls` are computed before the trailing-20 slice. Label truncation is on Unicode scalar values (Python `str[:120]`), not bytes. The 404 path intentionally diverges from the sibling endpoints' `{error}` envelope to match FastAPI's `HTTPException`.

## 3. Cross-cutting behavior

- **Authentication mode**: every endpoint is `required`. No public surface.
- **Visibility model**: own sessions ∪ sessions on `read`-permitted datasets, resolved via `AclDb::authorized_dataset_ids_with_roles(user_id, "read")`. Errors resolving permissions are swallowed (empty set), matching Python's `_permitted_dataset_ids_for`.
- **Error envelopes**: the three list/aggregate endpoints (`/`, `/stats`, `/cost-by-model`) emit `{"error": "<msg>"}` at `500` (rendered via `ApiError::OntologyEnvelope`, whose render is `{"error": <msg>}` at the supplied status — `ApiError::Internal` would render `{detail}` and break parity). The detail endpoint's `404` uniquely emits `{"detail": "session not found"}`.
- **Wire-shape carve-out (snake_case)**: every response DTO in this router is snake_case, *not* camelCase, because Python returns plain dicts / lists via `jsonable_encoder` rather than `OutDTO` subclasses, so the camelCase alias generator does not apply. These DTOs are therefore on the snake_case allow-list in `tests/test_openapi_camelcase.rs`.
- **Range windows**: `24h | 7d | 30d | all` only. An earlier draft's `90d` is **not** a Python value and is rejected.

## 4. DTO definitions

See `crates/http-server/src/dto/sessions.rs` for the authoritative definitions. Summary:

```rust
// Query structs (snake_case wire; literal field names).
pub struct ListSessionsQuery {
    pub range: RangeWindow,          // 24h | 7d | 30d | all  (default 30d)
    pub status: Option<String>,      // completed | failed | abandoned | running
    pub limit: u32,                  // default 50; validated 1..=500 in handler
    pub offset: u32,                 // default 0
    pub order_by: OrderBy,           // default last_activity_at; rejects unknown
    pub descending: bool,            // default true
}
pub struct StatsQuery       { pub range: RangeWindow }
pub struct CostByModelQuery { pub range: RangeWindow }

// Response DTOs (snake_case wire).
pub struct SessionListResponseDTO {
    pub sessions: Vec<SessionRowDTO>,
    pub total: i64,
    pub limit: u32,
    pub offset: u32,
    pub has_more: bool,
}

pub struct SessionRowDTO {
    pub session_id: String,
    pub user_id: String,
    pub dataset_id: Option<String>,
    pub status: String,
    pub started_at: DateTime<Utc>,        // iso8601_offset (+00:00)
    pub last_activity_at: DateTime<Utc>,  // iso8601_offset
    pub ended_at: Option<DateTime<Utc>>,  // iso8601_offset_option
    pub tokens_in: i32,
    pub tokens_out: i32,
    pub cost_usd: f64,
    pub error_count: i32,
    pub last_model: Option<String>,
    pub effective_status: String,
}

pub struct SessionStatsDTO {
    pub range: String,                    // echo of the input literal
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

pub struct CostByModelDTO {
    pub model: String,
    pub session_count: i64,
    pub cost_usd: f64,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

pub struct SessionDetailDTO {
    #[serde(flatten)]
    pub record: SessionRowDTO,
    pub label: Option<String>,
    pub msg_count: usize,
    pub tool_calls: usize,
    pub qas: Vec<serde_json::Value>,
    pub traces: Vec<serde_json::Value>,
}
```

## 5. Implementation tasks

This router is implemented. The integration tests live in:
- `crates/http-server/tests/test_sessions_list.rs`
- `crates/http-server/tests/test_sessions_stats.rs`
- `crates/http-server/tests/test_sessions_cost_by_model.rs`
- `crates/http-server/tests/test_sessions_detail.rs`

## 6. Open questions

1. **`order_by` strict rejection (D-1)**: Rust rejects unknown `order_by` / `range` values with `400`; Python silently falls back to `last_activity_at` / `30d`. Kept as a deliberate divergence to surface client typos.
2. **Owner-aware cache reads**: a dataset-grant viewer sees the actual QA / trace content of another user's session. This is intended (Python parity) but worth flagging for any future tenant-isolation hardening.

## 7. References

- Python router: [`cognee/api/v1/sessions/routers/get_sessions_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py)
- Session record model: [`cognee/modules/session_lifecycle/models.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/models.py)
- Session metrics: [`cognee/modules/session_lifecycle/metrics.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/metrics.py)
- DTO definitions: `crates/http-server/src/dto/sessions.rs`
- Cross-router conventions: [README.md §3](README.md#3-cross-router-conventions)
