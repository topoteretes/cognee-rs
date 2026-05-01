# E-12 — `GET /api/v1/sessions/{session_id}`

| | |
|---|---|
| Wire path | `GET /api/v1/sessions/{session_id}` |
| Status | **Done (commit b36f9ea)** — `get_session_detail` handler + `SessionDetailDTO` (flatten `SessionRowDTO` + `label`/`msg_count`/`tool_calls`/`qas`/`traces`) + `Path<String>` extractor. Reuses E-09's `/sessions` router scaffold + permitted-datasets pattern. Owner-aware cache reads via `row.record.user_id` (supports dataset-grant viewers). 404 path uses `ApiError::NotFound` → `{"detail":"session not found"}` (only v2 endpoint with `{detail}` envelope per Python FastAPI HTTPException parity). 500 path uses `OntologyEnvelope` → `{"error":"session detail failed"}`. Pre-truncation `msg_count` + `tool_calls`; tail-20 truncation; 120-char `chars().take(120)` label. 9 integration tests + cross-SDK harness. **No new wire divergence. v2 port complete (21 of 21).** |
| Depends on | LIB-02 (`SessionManager::get_agent_trace_session`, landed `eec6f79`), LIB-05 (`SessionLifecycleDb::get_session_row` + `SessionRowWithStatus`, landed `60c934a`), E-09 (router scaffold + `SessionRowDTO` + permitted-datasets pattern, landed `c42b513`). LIB-03 lands the underlying entities (commit `82728f2`). |
| Effort | ~1 day. |
| Owner crate | `cognee-http-server` |

## 1. Goal

Detail view that fuses the relational `session_records` row with the cache content (last 20 QAs + last 20 trace steps) and computes a label for the dashboard list.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `GET /{session_id}` handler | `cognee/api/v1/sessions/routers/get_sessions_router.py` | 254–308 |

### Response shape

```json
{
  ...SessionRecord.to_dict(),
  "label":      "<first QA question OR first trace's origin_function OR null>",
  "msg_count":  <len(qas)>,
  "tool_calls": <len(traces)>,
  "qas":        [...],   // last 20 QA dicts (oldest of the 20 first)
  "traces":     [...]    // last 20 trace dicts
}
```

Errors: `404 {detail: "session not found"}` when row absent / not visible.

### Behavior subtleties (parity-critical)

1. **Owner-aware cache lookup**: `qas` / `traces` are fetched from the cache under the **session's owner** `user_id` (taken from the `SessionRecord` row), not the authenticated caller. This supports a user with a dataset-grant viewing someone else's session.
2. **Best-effort cache miss**: if `SessionManager::is_available` is false or any cache call raises, return the row anyway with empty `qas` / `traces` (Python catches `Exception` silently — replicate).
3. **Label fallback chain**: first QA's `question` (truncated to 120 chars) → first trace's `origin_function` → `None`.
4. **Tail truncation**: cap both lists at 20 entries (`qas[-20:]`, `traces[-20:]`).

## 3. Current Rust state

- **Router scaffold** exists at [`crates/http-server/src/routers/sessions.rs:39-44`](../../../crates/http-server/src/routers/sessions.rs) with the three sibling routes (`/`, `/stats`, `/cost-by-model`). The `:38` doc-comment placeholder reserves `.route("/{session_id}", get(get_session_detail))`. **No handler defined yet.**
- **`SessionLifecycleDb::get_session_row`** landed (LIB-05, commit `60c934a`) at [`crates/database/src/traits/session_lifecycle_db.rs:168-174`](../../../crates/database/src/traits/session_lifecycle_db.rs):
  ```rust
  async fn get_session_row(
      &self,
      session_id: &str,
      user_id: Uuid,
      permitted_dataset_ids: &[Uuid],
      prefer_other_owner: bool,
  ) -> Result<Option<SessionRowWithStatus>, DatabaseError>;
  ```
  with the `SessionRowWithStatus { record, effective_status }` envelope (lines `:69-87`) and a `to_dict()` helper (`:77-86`) that emits the snake_case JSON object `record.to_dict() ∪ {"effective_status": ...}` for byte parity with Python's `metrics.py:336-348`.
- **`SessionManager::get_agent_trace_session`** landed (LIB-02, commit `eec6f79`) at [`crates/session/src/session_manager.rs:284-297`](../../../crates/session/src/session_manager.rs) with the signature
  ```rust
  pub async fn get_agent_trace_session(
      &self,
      user_id: &str,
      session_id: Option<&str>,
      last_n: Option<usize>,
  ) -> Result<Vec<SessionTraceStep>, SessionError>;
  ```
  Pass `Some(20)` to apply the cap upstream of the handler.
- **Existing `SessionRowDTO`** ([`crates/http-server/src/dto/sessions.rs:196-214`](../../../crates/http-server/src/dto/sessions.rs)) is **snake_case wire** (Python parity carve-out — Decision 10 does not apply because Python returns a plain dict via `jsonable_encoder`, not an `OutDTO`). Twelve fields: `session_id`, `user_id`, `dataset_id`, `status`, `started_at`, `last_activity_at`, `ended_at`, `tokens_in`, `tokens_out`, `cost_usd`, `error_count`, `last_model`, `effective_status`. `From<cognee_database::SessionRowWithStatus>` impl at `:277-299`. Reuse this DTO for the row body via `#[serde(flatten)]`.
- **No `SessionManager::is_available()` method exists in Rust.** The Python guard `if sm.is_available and owner_user_id` translates to `if components.session_manager.is_some() && !owner_user_id.is_empty()` in Rust — the optional `Arc<SessionManager>` slot in [`crates/http-server/src/components.rs:81`](../../../crates/http-server/src/components.rs) is the parity point (already cited at `:71` and used by `recall.rs:142`).
- **No `SessionManager::read_history(formatted=False)` method.** Python's `sm.get_session(user_id, session_id, formatted=False)` returns a list of QA dicts; Rust's typed equivalent is the `SessionStore::get_latest_qa_entries(session_id, Some(user_id), limit)` trait method (impls at `fs_store.rs:226`, `redis_store.rs:184`, `sea_orm_store.rs:74`) returning `Vec<SessionQAEntry>`. Reach the store via `state.components()?.session_store` ([`components.rs:75`](../../../crates/http-server/src/components.rs)) and serialize each entry to `serde_json::Value` so the wire shape matches Python's untyped dicts.
- **`ApiError::NotFound(String)`** at [`crates/http-server/src/error.rs:73`](../../../crates/http-server/src/error.rs) renders `404 {"detail": "..."}` (line `:287`). This is the exact shape required for the Python `HTTPException(status_code=404, detail="session not found")` parity quirk called out in [README §1.1](../README.md#11-wire-conventions-project-wide-set-by-decision-6) — the only v2 endpoint that intentionally emits `{detail}` instead of `{error}`.
- **OpenAPI registry** at [`crates/http-server/src/openapi.rs:38-46/80-88`](../../../crates/http-server/src/openapi.rs) lists the three sibling handlers + DTOs. Add `get_session_detail` and `SessionDetailDTO` here.

## 4. Implementation steps

1. **DTO** in [`crates/http-server/src/dto/sessions.rs`](../../../crates/http-server/src/dto/sessions.rs):
   ```rust
   /// Response DTO — wire is **snake_case** (Python parity carve-out;
   /// the response is a plain `dict` via `jsonable_encoder`, not an
   /// `OutDTO`, so Decision 10's camelCase rule does not apply — same
   /// rationale as `SessionRowDTO`/`SessionStatsDTO`/`CostByModelDTO`).
   #[derive(Debug, Clone, Serialize, ToSchema)]
   pub struct SessionDetailDTO {
       #[serde(flatten)]
       pub record: SessionRowDTO,                 // shared with E-09; snake_case
       pub label: Option<String>,
       pub msg_count: usize,                      // → "msg_count"
       pub tool_calls: usize,                     // → "tool_calls"
       pub qas: Vec<serde_json::Value>,
       pub traces: Vec<serde_json::Value>,
   }
   ```
   `qas` / `traces` typed as `serde_json::Value` to match Python's untyped dicts.
   Add to the snake_case allow-list in `tests/test_openapi_camelcase.rs` (same carve-out as the three sibling DTOs).

2. **Handler `get_session_detail`** in [`crates/http-server/src/routers/sessions.rs`](../../../crates/http-server/src/routers/sessions.rs), following the established pattern of `list_sessions` / `get_stats` / `cost_by_model`:
   ```rust
   pub async fn get_session_detail(
       State(state): State<AppState>,
       user: AuthenticatedUser,
       Path(session_id): Path<String>,
   ) -> Result<Json<SessionDetailDTO>, ApiError> { ... }
   ```
   Steps inside the handler:
   - `let components = state.components().ok_or_else(|| ApiError::OntologyEnvelope("session detail failed".into(), StatusCode::INTERNAL_SERVER_ERROR))?;` (matches the sibling 500 envelope `{"error": "..."}` at `:108-110`).
   - Resolve permitted datasets via `components.database.authorized_dataset_ids_with_roles(user.id, "read")`, swallowing errors to `Vec::new()` (mirrors siblings + Python `_permitted_dataset_ids_for`).
   - Call `components.database.get_session_row(&session_id, user.id, &permitted, /*prefer_other_owner=*/false).await`.
     - If the call itself errors, return `ApiError::OntologyEnvelope("session detail failed", 500)`.
     - If `Ok(None)`, return `Err(ApiError::NotFound("session not found".into()))` — renders `404 {"detail": "session not found"}` per Python `HTTPException` parity.
   - Extract `owner_user_id = row.record.user_id.clone()` (`String` from `session_record::Model`).
   - Best-effort cache reads (Python's `try/except: pass`):
     ```rust
     let (qas, traces) = if !owner_user_id.is_empty()
         && let Some(sm) = components.session_manager.as_ref()
     {
         let store = components.session_store.as_ref();
         let qas = match store {
             Some(store) => store
                 .get_latest_qa_entries(&session_id, Some(&owner_user_id), usize::MAX)
                 .await
                 .unwrap_or_default(),
             None => Vec::new(),
         };
         let traces = sm
             .get_agent_trace_session(&owner_user_id, Some(&session_id), Some(20))
             .await
             .unwrap_or_default();
         (qas, traces)
     } else {
         (Vec::new(), Vec::new())
     };
     ```
     `unwrap_or_default()` on `Result<Vec<_>, SessionError>` is the Rust idiom for Python's silent-swallow (no `.unwrap()` per project convention).
   - **Order matters for Python parity**: Python iterates the **full** QA list then truncates to the trailing 20 with `qas[-20:]`, so `msg_count = len(qas)` reflects the unbounded length. `get_latest_qa_entries` returns oldest-first per its signature in `session_store.rs`; pass `usize::MAX` (or a generous cap) and truncate locally. `traces` already truncated to last 20 by `get_agent_trace_session(last_n=Some(20))` — but `tool_calls = len(traces)` is the **post-truncation** length per Python's `record["tool_calls"] = len(traces)` (Python computed `tool_calls` against the full list because `traces[-20:]` came after; **double-check** at implementation time if `get_agent_trace_session` is called with `last_n=Some(20)` then `tool_calls` will mis-count vs Python). **Recommended**: call `get_agent_trace_session(_, _, None)` to get the full list, compute `tool_calls = traces.len()`, then truncate to last 20 for the wire.
   - Compute `label`:
     - Iterate `qas` in order; if any has a non-empty `question` string, take it truncated to 120 chars (chars not bytes — Python `[:120]` on a `str`); break.
     - Otherwise iterate `traces`; if any has a non-empty `origin_function`, take it; break.
     - Else `None`.
   - Tail-truncate to last 20: `qas = qas[qas.len().saturating_sub(20)..].to_vec();` and the same for `traces` (or use `Vec::split_off`).
   - Serialize each `SessionQAEntry` and `SessionTraceStep` to `serde_json::Value` via `serde_json::to_value(...)`. **Verify field-key parity** with Python's persisted shape: `SessionQAEntry` is snake_case (id / session_id / user_id / question / answer / context / created_at / feedback_text / feedback_score / used_graph_element_ids / memify_metadata) — matches Python; `SessionTraceStep` is snake_case per the doc-comment at `types.rs:55-57`.
   - Build `SessionDetailDTO { record: SessionRowDTO::from(row), label, msg_count, tool_calls, qas: qas_json, traces: traces_json }` and return `Ok(Json(dto))`.

3. **Wire the route** in `router()` at [`sessions.rs:39-44`](../../../crates/http-server/src/routers/sessions.rs):
   ```rust
   .route("/{session_id}", get(get_session_detail))
   ```
   and update the doc-comment at `:36-38` to reflect that E-12 has landed.

4. **Register in OpenAPI** at [`crates/http-server/src/openapi.rs`](../../../crates/http-server/src/openapi.rs):
   - Add `crate::routers::sessions::get_session_detail` to the `paths(...)` list (next to the three siblings at `:38-43`).
   - Add `crate::dto::sessions::SessionDetailDTO` to the `components(schemas(...))` list (next to the three siblings at `:80-88`).

5. **Decorators** on the handler matching the sibling style:
   - `#[utoipa::path(get, path = "/api/v1/sessions/{session_id}", tag = "sessions", params(("session_id" = String, Path, description = "Session id")), responses(...))]`
   - `#[tracing::instrument(name = "cognee.api.sessions.detail", skip(state), fields(cognee.session.user_id = %user.id, cognee.session.session_id = %session_id))]`

6. **Mount/integration plumbing** is already in place — `sessions::router()` is wired into the app router via E-09.

## 5. Tests

- `crates/http-server/tests/test_sessions_detail.rs`:
  - `detail_returns_404_for_unknown_session`.
  - `detail_returns_404_when_no_visibility`.
  - `detail_label_falls_back_to_origin_function_when_no_qas`.
  - `detail_label_truncates_long_question_to_120_chars`.
  - `detail_caps_qas_and_traces_at_20`.
  - `detail_returns_empty_lists_when_session_manager_unavailable` — sets the manager to a sentinel "unavailable" mock.
  - `detail_dataset_grant_views_other_users_session` — shared fixture from E-09.
- Cross-SDK parity test.

## 6. Acceptance criteria

- [x] 404 envelope is `{"detail": "session not found"}` (Python's FastAPI `HTTPException` shape — only v2 endpoint with `{detail}`; all other 4xx/5xx use `{error}`).
- [x] Owner-aware cache reads work (cache keyed by `row.record.user_id` not `user.id`; supports dataset-grant viewers).
- [x] Cache failures degrade silently to empty `qas` / `traces` (no `.unwrap()` in non-test code).
- [x] Label fallback chain matches Python: first non-empty `qas[i].question` truncated to 120 chars → first non-empty `traces[i].origin_function` → `None`.
- [x] `msg_count` / `tool_calls` reflect the **pre-truncation** list lengths.
- [x] Wire DTO is **snake_case** (parity carve-out — matches the three sibling DTOs).
- [x] Cross-SDK structural diff passes (`test_http_v2_sessions_detail.py`).

## 7. References

- [Python detail handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L254)
- [LIB-02 — `add_agent_trace_step` / `get_agent_trace_session`](lib-02-session-manager-trace-step.md)
- [LIB-03 — `session_records` schema + entities](lib-03-session-records-schema.md)
- [LIB-05 — `get_session_row` repository method](lib-05-session-records-repo.md)
