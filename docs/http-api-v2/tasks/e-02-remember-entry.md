# E-02 — `POST /api/v1/remember/entry`

| | |
|---|---|
| Wire path | `POST /api/v1/remember/entry` |
| Status | **Missing** |
| Depends on | LIB-01 (`remember_entry()` facade), LIB-02 (`add_agent_trace_step`). |
| Effort | ~0.5 day (after libraries land). |
| Owner crate | `cognee-http-server` |

## 1. Goal

Land the JSON-bodied counterpart to `POST /remember`. The body is a discriminated-union `MemoryEntry` (QA / Trace / Feedback) routed to the appropriate `SessionManager` method. Used by the cognee-mcp tracing hooks and the cognee Cloud client's `remember_entry()`.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `RememberEntryRequest` model | `cognee/api/v1/remember/routers/get_remember_router.py` | 105–115 |
| `POST /entry` handler | same | 116–164 |

Request body:

```json
{
  "entry": {
    "type": "qa" | "trace" | "feedback",
    "...": "...entry-specific fields..."
  },
  "dataset_name": "main_dataset",
  "session_id": "<required>"
}
```

Response: `RememberResult.to_dict()` with `entry_type` + `entry_id` populated.

Status codes:
- `200` — success.
- `400` — `ValueError` (missing `session_id`, user not found).
- `503` — session cache unavailable.
- `409` — `{error: "An error occurred during remember."}` catch-all (matches Python's bare `except Exception`).

## 3. Current Rust state

No route registered. `POST /api/v1/remember/entry` returns `404` from the cognee-rust HTTP server today.

## 4. Implementation steps

> **Decision (2026-04-29) — Decision 5**: this task owns the structural extension of `RememberResultDTO` with `entry_type: Option<String>` and `entry_id: Option<String>`. Both fields use `#[serde(skip_serializing_if = "Option::is_none")]` so the existing `POST /remember` file-payload responses (E-01) stay byte-identical. The library-side `cognee_lib::RememberResult` extension is owned by LIB-01; this task wires those fields through to the HTTP DTO. Investigation agent: do not re-litigate.

1. **Extend `RememberResultDTO`** in `crates/http-server/src/dto/remember.rs:42` (the parent struct already uses camelCase per CLEAN-01):
   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
   #[serde(rename_all = "camelCase")]
   pub struct RememberResultDTO {
       ...existing fields (already camelCase from CLEAN-01)...
       #[serde(skip_serializing_if = "Option::is_none")]
       pub entry_type: Option<String>,    // wire: "entryType"; values "qa" | "trace" | "feedback"
       #[serde(skip_serializing_if = "Option::is_none")]
       pub entry_id:   Option<String>,    // wire: "entryId"
   }
   ```
   Add a deserialization round-trip test confirming the file-payload response (no `entryType` key) and the entry response (with `entryType` populated) both encode correctly. Negative-test: assert the response body has NO `entry_type` snake_case key — only `entryType`.

2. **Request DTO** in a new `crates/http-server/src/dto/remember_entry.rs`:
   ```rust
   /// Wire is camelCase per Decision 10. snake_case input forms are
   /// also accepted via per-field aliases for compatibility.
   #[derive(Debug, Deserialize, ToSchema)]
   #[serde(rename_all = "camelCase")]
   pub struct RememberEntryRequestDTO {
       pub entry: MemoryEntryDTO,                  // re-export from cognee_models::memory

       #[serde(default = "default_dataset", alias = "dataset_name")]
       pub dataset_name: String,                   // wire: "datasetName"; default "main_dataset"

       #[serde(alias = "session_id")]
       pub session_id: String,                     // wire: "sessionId"; required
   }
   ```
   Use `ValidatedJson` so empty `session_id` is rejected with 400 (Decision 7) before the handler runs.

3. **Handler** in `crates/http-server/src/routers/remember.rs`:
   ```rust
   pub async fn post_remember_entry(
       user: AuthenticatedUser,
       State(state): State<AppState>,
       ValidatedJson(payload): ValidatedJson<RememberEntryRequestDTO>,
   ) -> Result<Json<RememberResultDTO>, ApiError> { ... }
   ```
   - Calls `cognee_lib::api::remember::remember_entry(payload.entry.into(), &payload.dataset_name, &payload.session_id, &user, ...)`.
   - Maps errors:
     - `RememberError::MissingSessionId` / `UserNotFound` → `400 {error}`.
     - `RememberError::SessionCacheUnavailable` → `503 {error}`.
     - any other → `409 {error: "An error occurred during remember."}` (logged as `tracing::error!`).
   - Sends Python-equivalent telemetry attributes on the `cognee.api.remember_entry` tracing span (`endpoint = "POST /v1/remember/entry"`, `entry_type = ...`).
   - Populates `RememberResultDTO::entry_type` (from the discriminator) and `RememberResultDTO::entry_id` (from the library result) on the returned JSON.

4. **Wire into the router** at `crates/http-server/src/routers/remember.rs:285`:
   ```rust
   Router::new()
       .route("/", post(post_remember))
       .route("/entry", post(post_remember_entry))
   ```

5. **OpenAPI** — add the new operation to `crates/http-server/src/openapi.rs` with the `RememberEntryRequestDTO` and response schema; add `RememberEntryRequestDTO` to `ToSchema` derives. The `entry_type` / `entry_id` fields on `RememberResultDTO` need to be advertised as `nullable: true` to match Python's optional shape.

## 5. Tests

- `crates/http-server/tests/test_remember_entry.rs` (new):
  - `qa_entry_returns_qa_id` — assert response body has `entry_type: "qa"` and `entry_id` matches the cache-returned id.
  - `trace_entry_returns_trace_id` — `entry_type: "trace"`.
  - `feedback_entry_for_existing_qa_returns_qa_id` — `entry_type: "feedback"`, `entry_id` equals the input `qa_id`.
  - `feedback_entry_for_missing_qa_returns_errored_status_with_error`.
  - `missing_session_id_returns_400_with_python_validation_envelope` — **integration test** for Decision 7: POST `{"entry":{"type":"qa","question":"x","answer":"y"}}` (no `session_id`); assert status `400`, body `detail[0].loc == ["body","session_id"]`, `type` ends in `value_error` (match v1's existing convention).
  - `unknown_entry_type_returns_400_with_python_validation_envelope` — serde discriminator failure for `entry.type=="bogus"`; assert status `400` (NOT 422), body `detail[0].loc` includes `"body"` and `"entry"`, `type` ends in `value_error`.
  - `session_cache_unavailable_returns_503` (mock returns `RuntimeError`).
- `crates/http-server/src/dto/remember.rs` test additions:
  - `remember_result_dto_skips_entry_fields_when_none` — round-trip a file-payload result; assert the JSON has no `entry_type` / `entry_id` keys (E-01 byte-shape parity).
  - `remember_result_dto_serializes_entry_fields_when_set`.
- `e2e-cross-sdk/harness/test_http_v2_remember_entry.py` — wire-shape parity against Python.

## 6. Acceptance criteria

- [ ] `POST /api/v1/remember/entry` returns 200 with `entry_type` + `entry_id` populated for all three entry kinds.
- [ ] Wire body matches Python's `jsonable_encoder(result.to_dict())` byte-for-byte (verified by structural-diff parity test).
- [ ] OpenAPI document advertises the route with the discriminated-union request body.
- [ ] All 7 unit tests + 1 cross-SDK test pass.
- [ ] No `unwrap()` in the handler; secrets/PII never logged.

## 7. References

- [Python `/entry` handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L115)
- [LIB-01 — `remember_entry()`](lib-01-remember-entry-facade.md)
- [LIB-02 — `add_agent_trace_step`](lib-02-session-manager-trace-step.md)
