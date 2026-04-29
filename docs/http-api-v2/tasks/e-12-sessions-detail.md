# E-12 — `GET /api/v1/sessions/{session_id}`

| | |
|---|---|
| Wire path | `GET /api/v1/sessions/{session_id}` |
| Status | **Missing** |
| Depends on | LIB-02 (`get_agent_trace_session`), LIB-05 (`get_session_row`), which transitively needs LIB-03 (entities). |
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

No route. No `SessionManager::get_agent_trace_session` (LIB-02 adds). No `SessionLifecycleDb::get_session_row` (LIB-05 adds; LIB-03 lands the underlying entities).

## 4. Implementation steps

1. **Handler `get_session_detail`** in `crates/http-server/src/routers/sessions.rs`:
   ```rust
   pub async fn get_session_detail(
       user: AuthenticatedUser,
       State(state): State<AppState>,
       Path(session_id): Path<String>,
   ) -> Result<Json<SessionDetailDTO>, ApiError> { ... }
   ```
   - Resolve permitted datasets.
   - Call `db.get_session_row(session_id, user.id, permitted, prefer_other_owner=false)`.
   - If `None`, return `404 {detail: "session not found"}`.
   - Read `owner_user_id = row.user_id` (string).
   - Best-effort cache reads:
     ```rust
     let (qas, traces) = if state.session_manager().is_available() && !owner_user_id.is_empty() {
         let qas = state.session_manager()
             .read_history(&owner_user_id, &session_id, /*formatted=*/false)
             .await
             .unwrap_or_default();
         let traces = state.session_manager()
             .get_agent_trace_session(&owner_user_id, &session_id)
             .await
             .unwrap_or_default();
         (qas, traces)
     } else {
         (vec![], vec![])
     };
     ```
     **Note**: `unwrap_or_default()` on `Result<Vec<_>, SessionError>` is the Rust idiom for Python's `try/except: pass`. No `.unwrap()` is used.
   - Compute `label` via the fallback chain.
   - Truncate `qas` and `traces` to last 20 (use `qas.iter().rev().take(20).rev().collect()` — order matches Python's `[-20:]`).

2. **DTOs**:
   ```rust
   /// Response DTO — wire is camelCase per Decision 10.
   #[derive(Debug, Serialize, ToSchema)]
   #[serde(rename_all = "camelCase")]
   pub struct SessionDetailDTO {
       #[serde(flatten)]
       pub record: SessionRowDTO,                 // shared with E-09; already camelCase
       pub label: Option<String>,
       pub msg_count: usize,                      // → "msgCount"
       pub tool_calls: usize,                     // → "toolCalls"
       pub qas: Vec<serde_json::Value>,
       pub traces: Vec<serde_json::Value>,
   }
   ```
   `qas` / `traces` typed as `serde_json::Value` to match Python's untyped dicts and to avoid pulling all the cache field shapes into the HTTP DTO.

3. **Wire** at `/api/v1/sessions/{session_id}` (router built in E-09).

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

- [ ] 404 envelope is `{"detail": "session not found"}` (Python's FastAPI `HTTPException` shape).
- [ ] Owner-aware cache reads work (regression-test the dataset-grant scenario).
- [ ] Cache failures degrade silently to empty `qas` / `traces`.
- [ ] Label fallback chain matches Python.
- [ ] Cross-SDK structural diff passes.

## 7. References

- [Python detail handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L254)
- [LIB-02 — `add_agent_trace_step` / `get_agent_trace_session`](lib-02-session-manager-trace-step.md)
- [LIB-03 — `session_records` schema + entities](lib-03-session-records-schema.md)
- [LIB-05 — `get_session_row` repository method](lib-05-session-records-repo.md)
