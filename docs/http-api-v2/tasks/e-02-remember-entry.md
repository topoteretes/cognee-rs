# E-02 — `POST /api/v1/remember/entry`

| | |
|---|---|
| Wire path | `POST /api/v1/remember/entry` |
| Status | **Done (commit 75c0886)** — `POST /remember/entry` route registered; `RememberEntryRequestDTO` consumes `cognee_models::memory::MemoryEntry` directly (no wrapper); `post_remember_entry` handler inline-replicates `cognee_lib::api::remember::remember_entry` (cycle workaround per §3.1, mirrors E-04's pattern). `RememberResultDTO` extended with `entry_type` + `entry_id` (Decision 5). New `ApiError::ServiceUnavailable(String)` variant for missing session cache. 7 integration tests + 3-probe cross-SDK harness. **No new wire divergence.** |
| Depends on | **LIB-06** (`RememberResult.entry_type` / `entry_id` library fields + `RememberStatus` enum) — **landed b39cd05**; **LIB-01** (`remember_entry()` facade) — **landed 0818644**; **LIB-02** (`add_agent_trace_step`) — **landed eec6f79**. |
| Effort | ~0.5 day. |
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

## 3. Current Rust state (re-verified 2026-04-30)

- **Route**: `crates/http-server/src/routers/remember.rs:322-324` registers only `POST /` (multipart `post_remember`); no `/entry` sub-route. `POST /api/v1/remember/entry` returns `404` today.
- **Response DTO** `RememberResultDTO` (`crates/http-server/src/dto/remember.rs:104-132`) is the post-E-01 shape (commit 037cad2): `status` (typed as `WireRememberStatus` — see below), `pipeline_run_id`, `dataset_id`, `dataset_name`, `items_processed`, `elapsed_seconds`, `session_ids`, `content_hash`, `items`, `error`. **`entry_type` and `entry_id` are NOT present** — Decision 5 reserves those for E-02. The unit test `remember_result_dto_minimal_wire_shape` (`dto/remember.rs:197-200`) explicitly asserts `!obj.contains_key("entry_type")` / `!obj.contains_key("entry_id")` on the file-payload path; that assertion stays valid after E-02 because the new fields use `#[serde(skip_serializing_if = "Option::is_none")]` and are `None` for the file/text path.
- **Wire status enum** `WireRememberStatus` (`crates/http-server/src/dto/remember.rs:61-71`) is reusable as-is — emits `"running"` / `"completed"` / `"errored"` / `"session_stored"`. The library-side `cognee_lib::api::remember::RememberStatus` (`crates/lib/src/api/remember.rs:39-58`) emits CamelCase `"PipelineRunStarted"` / `"PipelineRunCompleted"` / `"PipelineRunErrored"` / `"SessionStored"` (Decision 15). E-02's handler only ever surfaces `SessionStored` / `Errored` from the library; map both at the DTO boundary.
- **Library facade** `cognee_lib::api::remember::remember_entry()` (`crates/lib/src/api/remember.rs:603-792`, landed 0818644) takes `(entry: MemoryEntry, dataset_name: &str, session_id: &str, owner_id: Uuid, _tenant_id: Option<Uuid>, db: Option<Arc<DatabaseConnection>>, _session_store: Option<Arc<dyn SessionStore>>, session_manager: Option<Arc<SessionManager>>) -> Result<RememberResult, ApiError>`. Returns `Err(ApiError::InvalidArgument)` on empty `session_id`. Uses `SessionLifecycleDb::ensure_and_touch_session` for the best-effort pre-upsert, dispatches by enum variant, populates `entry_type` / `entry_id`. **However**, this function is unreachable from `crates/http-server` (cycle constraint — see §3.1).
- **Memory types** `cognee_models::memory::{MemoryEntry, QAEntry, TraceEntry, FeedbackEntry}` (`crates/models/src/memory.rs`, exported at `crates/models/src/lib.rs:13,33`) are reachable from `cognee-http-server` (already a direct dep — `Cargo.toml:39`). The `MemoryEntry::type_str()` helper (`memory.rs:43-49`) returns the `"qa"`/`"trace"`/`"feedback"` discriminator string for `entry_type` population. **The DTO can be `cognee_models::memory::MemoryEntry` directly** — no separate `MemoryEntryDTO` type is needed (Python's `RememberEntryRequest.entry` is the same `Union[QAEntry, TraceEntry, FeedbackEntry]`). The wire fields are already camelCase + `serde(alias = "<snake_form>")` per Decision 10 (verified by the round-trip tests at `memory.rs:142-365`).
- **`ComponentHandles` slots** (`crates/http-server/src/components.rs:75,81`) already expose `session_store: Option<Arc<dyn SessionStore>>` (added by E-04, commit 9981e79) and `session_manager: Option<Arc<SessionManager>>`. The `database` field (`Arc<DatabaseConnection>`) is also present. Calling `cognee_database::SessionLifecycleDb::ensure_and_touch_session(database.as_ref(), ...)` is therefore already wired.
- **Python source-of-truth re-verified** — handler unchanged at [`get_remember_router.py:115-164`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L115-L164); request model unchanged at lines 101-113. JSON body (NOT multipart). Status mapping `ValueError → 400` / `RuntimeError → 503` / catch-all → 409.
- **Validation extractor** `crate::middleware::validation::Json` (re-exported as `ValidatedJson`) is in place from the v1 work (Decision 7). E-04's recall handler uses it (`recall.rs:28`) — the same import covers E-02. Empty `session_id` parses successfully but is rejected by the library (`ApiError::InvalidArgument`); per the task §5, E-02 must **also** add a pre-handler validator that rejects empty `session_id` with the byte-shape Python validation envelope (`{"detail": [{"loc":["body","session_id"], "msg":"...", "type":"value_error"}], ...}`). The library 400 surfaces only the bare error string; the Python wire shape requires the full envelope.

### 3.1 Cycle constraint — inline replication strategy (Decision 18 precedent)

`crates/http-server/Cargo.toml:35-37` documents the cycle constraint: `cognee-http-server` cannot depend on `cognee-lib` (that crate's `server` feature pulls `cognee-http-server` back). Three options were on the table:

a. **Inline replication** in the http-server handler using `cognee_session::SessionManager` + `cognee_database::SessionLifecycleDb` + `cognee_models::memory::*` directly. ~80 LoC of straightforward dispatch, mirrors `crates/lib/src/api/remember.rs:603-792` byte-for-byte. **Recommended.**
b. Lift `remember_entry()` from `cognee-lib` to a lower-level crate (mirror Decision 18 / LIB-08).
c. Add a `RememberEntryProvider` trait + DI.

**Choice: option (a).** Rationale:
- The library body is short (`save_qa` / `update_qa` / `add_agent_trace_step` / `add_feedback` are all reachable on `Arc<SessionManager>` from `cognee-session` — already a direct dep at `Cargo.toml:107`; `SessionLifecycleDb::ensure_and_touch_session` is reachable on `Arc<DatabaseConnection>` from `cognee-database` — already a direct dep at `Cargo.toml:41`).
- LIB-08 (lifting LIB-07's primitives to `cognee-search`) was justified because `RecallScope` + the four source helpers form a coherent search-routing module that semantically belongs with `SearchOrchestrator`. `remember_entry()` is a thin dispatch — it does not form a coherent module worth lifting.
- Option (b) would introduce a new `cognee-remember` crate (or pollute `cognee-session`) for one ~80 LoC function.
- Option (c) adds DI complexity for one call site.

**Implementation impact**: the per-`Arc<dyn Trait>` parameter style of `cognee_lib::api::remember::remember_entry` is replicated; the library function itself is **not** invoked. The library facade remains the canonical in-process Rust SDK entry point for non-HTTP callers; the HTTP handler is a parallel implementation. If Python's `_dispatch_session_entry` ever changes shape, **both** sites need to update — the doc-update agent must add a "see also" cross-reference between the handler and the library.

The same precedent was set by E-01 for `WireRememberStatus` (`dto/remember.rs:62-71` — standalone enum because `From<cognee_lib::api::remember::RememberStatus>` would cross the cycle) and by E-04 for the `recall_scope::*` helpers (which in turn motivated LIB-08 to lift them into `cognee-search` to remove the duplication). E-02 follows E-01's pattern (no lift) because the helpers are not coherent enough to lift.

## 4. Implementation steps

> **Decision (2026-04-29) — Decision 5**: this task owns the structural extension of `RememberResultDTO` with `entry_type: Option<String>` and `entry_id: Option<String>`. Both fields use `#[serde(skip_serializing_if = "Option::is_none")]` so the existing `POST /remember` file-payload responses (E-01) stay byte-identical (Python omits both keys on the file path). The library-side fields on `cognee_lib::api::remember::RememberResult` are added by **LIB-06** (Q-F, Decision 15); LIB-01's `remember_entry()` facade populates them for the typed-entry path; this task (E-02) wires them through to the HTTP DTO. Investigation agent: do not re-litigate.

> **Implementation strategy — inline replication (per §3.1)**: the cycle constraint at `crates/http-server/Cargo.toml:35-37` forbids importing `cognee_lib::api::remember::remember_entry`. Steps 3+ replicate the library body inline using `cognee_session::SessionManager` + `cognee_database::SessionLifecycleDb` + `cognee_models::memory::*` directly. The library facade is **not** called from the handler.

1. **Extend `RememberResultDTO`** in [`crates/http-server/src/dto/remember.rs:104-132`](../../../crates/http-server/src/dto/remember.rs). **The parent struct uses `rename_all = "snake_case"`** (per [CLEAN-01 §3.1](clean-01-v1-dto-camelcase.md) carve-out — Python's `RememberResult.to_dict()` returns a plain dict, not a pydantic `BaseModel`, so the wire keys are snake_case). Add the two new fields with snake_case wire names:
   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
   #[serde(rename_all = "snake_case")]   // ← KEEP snake_case (CLEAN-01 §3.1 carve-out)
   pub struct RememberResultDTO {
       ...existing fields (snake_case)...
       #[serde(skip_serializing_if = "Option::is_none")]
       pub entry_type: Option<String>,    // wire: "entry_type"; values "qa" | "trace" | "feedback"
       #[serde(skip_serializing_if = "Option::is_none")]
       pub entry_id:   Option<String>,    // wire: "entry_id"
   }
   ```
   Add a serialization round-trip test confirming the file-payload response (no `entry_type` key) and the entry response (with `entry_type` populated) both encode correctly. The OpenAPI camelCase regression test from CLEAN-01 already whitelists `RememberResultDTO`; no whitelist change needed. The existing `remember_result_dto_minimal_wire_shape` test (`dto/remember.rs:163-200`) explicitly asserts `!obj.contains_key("entry_type")` / `!obj.contains_key("entry_id")` for the file-payload path — that assertion must keep passing after the new fields land.

2. **Request DTO** in a new `crates/http-server/src/dto/remember_entry.rs`. The `entry` field uses `cognee_models::memory::MemoryEntry` directly — no wrapper DTO is needed because that type already has the right `serde(tag = "type")` shape and camelCase wire fields with snake_case aliases (verified at `crates/models/src/memory.rs`):
   ```rust
   use cognee_models::memory::MemoryEntry;
   use serde::Deserialize;
   use utoipa::ToSchema;

   /// Wire is camelCase per Decision 10. snake_case input forms are
   /// also accepted via per-field aliases for compatibility.
   #[derive(Debug, Deserialize, ToSchema)]
   #[serde(rename_all = "camelCase")]
   pub struct RememberEntryRequestDTO {
       /// Discriminated union: `{"type": "qa"|"trace"|"feedback", ...}`.
       /// Type defined in `cognee_models::memory`.
       pub entry: MemoryEntry,

       #[serde(default = "default_dataset_name", alias = "dataset_name")]
       pub dataset_name: String,                   // wire: "datasetName"; default "main_dataset"

       #[serde(alias = "session_id")]
       pub session_id: String,                     // wire: "sessionId"; required
   }

   fn default_dataset_name() -> String { "main_dataset".to_string() }
   ```
   Use `ValidatedJson` (re-exported as `crate::middleware::validation::Json`) so malformed bodies and discriminator failures (`entry.type=="bogus"`) are rejected with 400 + the Python validation envelope (Decision 7) before the handler runs. **Add a custom validator** that rejects empty `session_id` with the same envelope shape (Python's `ValueError → 400` would surface only the bare error string; the envelope shape is required by Decision 7's parity rule and §5's `missing_session_id_returns_400_with_python_validation_envelope` test).

3. **Handler** in `crates/http-server/src/routers/remember.rs` — **inline replication of `cognee_lib::api::remember::remember_entry`** (per §3.1; the library function is unreachable due to the cycle):
   ```rust
   use cognee_database::SessionLifecycleDb;
   use cognee_models::memory::{FeedbackEntry, MemoryEntry, QAEntry, TraceEntry};
   use cognee_session::{SessionManager, SessionQAUpdate};

   pub async fn post_remember_entry(
       user: AuthenticatedUser,
       State(state): State<AppState>,
       ValidatedJson(payload): ValidatedJson<RememberEntryRequestDTO>,
   ) -> Result<Json<RememberResultDTO>, ApiError> {
       let started = std::time::Instant::now();

       // ── Resolve required handles from ComponentHandles ──────────────
       let components = state.components().ok_or_else(|| {
           ApiError::DeprecatedConflict("An error occurred during remember.".into())
       })?;
       let session_manager: Arc<SessionManager> = components
           .session_manager
           .clone()
           .ok_or_else(|| {
               // 503 — session cache unavailable (Python `RuntimeError → 503`)
               ApiError::ServiceUnavailable(
                   "Session cache is not configured.".into(),
               )
           })?;
       let database = components.database.clone();

       // ── Pre-upsert the session_records row (best-effort, log-and-swallow)
       if let Err(exc) = SessionLifecycleDb::ensure_and_touch_session(
           database.as_ref(),
           &payload.session_id,
           user.id,
           None,                                   // dataset_id resolution deferred
       ).await {
           tracing::debug!(
               session_id = %payload.session_id,
               "post_remember_entry: pre-upsert session_record failed (non-fatal): {exc}"
           );
       }

       // ── Dispatch by variant — mirrors crates/lib/src/api/remember.rs:650-769
       let entry_type_str = payload.entry.type_str().to_string();
       let user_id_str = user.id.to_string();
       let mut wire_status = WireRememberStatus::SessionStored;
       let mut error_msg: Option<String> = None;
       let entry_id: String = match payload.entry {
           MemoryEntry::Qa(q) => { /* save_qa + optional update_qa */ ... }
           MemoryEntry::Trace(t) => { /* add_agent_trace_step */ ... }
           MemoryEntry::Feedback(f) => {
               let ok = session_manager.add_feedback(
                   Some(&payload.session_id),
                   Some(&user_id_str),
                   &f.qa_id,
                   f.feedback_text.as_deref(),
                   f.feedback_score,
               ).await.map_err(map_session_err)?;
               if !ok {
                   wire_status = WireRememberStatus::Errored;
                   error_msg = Some(format!(
                       "add_feedback: QA {} not found in session {}",
                       f.qa_id, payload.session_id,
                   ));
               }
               f.qa_id  // entry_id = qa_id even when not found (Python parity remember.py:307)
           }
       };

       Ok(Json(RememberResultDTO {
           status: wire_status,
           pipeline_run_id: None,
           dataset_id: None,
           dataset_name: payload.dataset_name,
           items_processed: 0,
           elapsed_seconds: Some(started.elapsed().as_secs_f64()),
           session_ids: Some(vec![payload.session_id.clone()]),
           content_hash: None,
           items: None,
           error: error_msg,
           entry_type: Some(entry_type_str),
           entry_id: Some(entry_id),
       }))
   }
   ```
   - **Variant body details** mirror `crates/lib/src/api/remember.rs:650-769` byte-for-byte:
     - **Qa**: `session_manager.save_qa(Some(session_id), Some(user_id_str), &q.question, &q.answer, Some(q.context.as_str()))` returns `qa_id`; if any of `feedback_text` / `feedback_score` / `used_graph_element_ids` is set, follow up with `session_manager.update_qa(Some(session_id), Some(user_id_str), &qa_id, SessionQAUpdate { feedback_text: feedback_text.map(Some), feedback_score: feedback_score.map(Some), used_graph_element_ids: <typed>, ..Default::default() })`. The `used_graph_element_ids` value (a `serde_json::Value`) is `serde_json::from_value(value)` into the `SessionQAUpdate`'s typed shape — surface a 400 `ApiError::BadRequest` on parse failure (with the message Python returns).
     - **Trace**: `session_manager.add_agent_trace_step(&user_id_str, Some(&payload.session_id), &t.origin_function, &t.status, &t.memory_query, &t.memory_context, t.method_params.unwrap_or(serde_json::Value::Null), t.method_return_value, &t.error_message, "")` — the trailing `""` is `session_feedback`; `t.generate_feedback_with_llm` is logged-and-ignored (LIB-01-followup TODO comment in handler).
     - **Feedback**: as above.
   - **Error mapping helper** `fn map_session_err(e: cognee_session::SessionError) -> ApiError` — `SessionError::StoreError("...cache unavailable...")` → `ApiError::ServiceUnavailable(503)`; everything else → `ApiError::DeprecatedConflict(409, "An error occurred during remember.")`. (Add the 503 variant to `ApiError` if not already there — see step 3.5.)
   - **Telemetry**: tracing span `cognee.api.remember_entry` with attributes `endpoint = "POST /v1/remember/entry"`, `entry_type = ...`, `cognee.user_id`. No PII in span fields; no question/answer text.

3.5. **`ApiError::ServiceUnavailable(String)` variant** — verify it exists in `crates/http-server/src/error.rs` (grep already shows `BadRequest` / `DeprecatedConflict` etc.). If absent, add it with `IntoResponse` mapping `(StatusCode::SERVICE_UNAVAILABLE, json!({"error": msg}))` to match Python's `JSONResponse(status_code=503, content={"error": str(error)})`. Wire it into the existing match on `ApiError`.

4. **Wire into the router** at `crates/http-server/src/routers/remember.rs:322-324`:
   ```rust
   pub fn router() -> Router<AppState> {
       Router::new()
           .route("/", post(post_remember))
           .route("/entry", post(post_remember_entry))
   }
   ```

5. **OpenAPI** — add `crate::routers::remember::post_remember_entry` to the `paths(...)` list in `crates/http-server/src/openapi.rs:30-65` (note: `post_remember` itself is not yet listed; add it too if part of the broader OpenAPI task, otherwise out of scope for E-02). Add `RememberEntryRequestDTO` and `RememberItemDTO` (the latter wasn't yet listed) to `components(schemas(...))` if missing. The `entry_type` / `entry_id` fields on `RememberResultDTO` need to be advertised as `nullable: true` to match Python's optional shape — add `#[schema(nullable)]` attributes if utoipa requires it explicitly. Keep `RememberResultDTO` in the schema list.

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

- [x] `POST /api/v1/remember/entry` returns 200 with `entry_type` + `entry_id` populated for all three entry kinds.
- [x] Wire body matches Python's `jsonable_encoder(result.to_dict())` shape (cross-SDK harness verifies).
- [x] OpenAPI document advertises the route with the request body.
- [x] All 7 integration tests + cross-SDK harness pass.
- [x] No `unwrap()` in the handler; secrets/PII never logged.

## 7. References

- [Python `/entry` handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L115)
- [LIB-01 — `remember_entry()`](lib-01-remember-entry-facade.md)
- [LIB-02 — `add_agent_trace_step`](lib-02-session-manager-trace-step.md)
