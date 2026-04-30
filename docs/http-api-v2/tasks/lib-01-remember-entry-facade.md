# LIB-01 — `cognee-lib` `remember_entry()` facade + `MemoryEntry` types

| | |
|---|---|
| Scope | New library function + new shared types. |
| Status | **Done (commit 0818644)** — `MemoryEntry` tagged enum + 3 variant structs in `cognee-models` (Decision 2); `remember_entry()` facade in `cognee-lib` dispatches to `SessionManager::{save_qa, update_qa, add_agent_trace_step, add_feedback}`; populates `RememberResult.entry_type`/`entry_id` for all branches per Decision 5; best-effort pre-upsert via `SessionLifecycleDb::ensure_and_touch_session` (log-and-swallow). 6 integration tests + 4 round-trip/helper tests. `generate_feedback_with_llm` deferred as TODO (LLM-handle plumbing out of scope). |
| Blocks | E-02 (`POST /remember/entry`). |
| Depends on | **LIB-06** (adds `RememberResult.entry_type` / `entry_id` fields + the new `RememberStatus` CamelCase enum — see Decision 15 / Q-F) — **landed (commit b39cd05)**; **LIB-02** (`SessionManager::add_agent_trace_step`) — **landed (commit eec6f79)**; **LIB-04** (`ImproveParams<'_>` — the existing `remember_session` already calls `improve()` with the new struct shape) — **landed (commit 9f1879e)**. |
| Effort | ~0.5 day. |
| Owner crates | `cognee-models`, `cognee-lib`. |

## 1. Goal

Land a Rust counterpart of Python's discriminated-union dispatch in [`cognee/api/v1/remember/remember.py:262`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L262):

```python
if isinstance(entry, QAEntry):    qa_id   = await sm.add_qa(...)
if isinstance(entry, TraceEntry): trace_id = await sm.add_agent_trace_step(...)
if isinstance(entry, FeedbackEntry): ok    = await sm.add_feedback(...)
```

The HTTP endpoint at [`POST /api/v1/remember/entry`](../../http-api-v2/tasks/e-02-remember-entry.md) calls `cognee.api.v1.remember(entry, ...)` to perform this dispatch — Rust needs the same surface.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `QAEntry` | `cognee/memory/entries.py` | 18–31 |
| `TraceEntry` | `cognee/memory/entries.py` | 34–50 |
| `FeedbackEntry` | `cognee/memory/entries.py` | 53–64 |
| `MemoryEntry` (Union alias) | `cognee/memory/entries.py` | 67 |
| Dispatch logic | `cognee/api/v1/remember/remember.py` | 262–311 |
| `RememberResult.entry_type / entry_id` | `cognee/api/v1/remember/remember.py` | ~360 |

## 3. Current Rust state (re-verified 2026-04-30)

- `crates/lib/src/api/remember.rs:198` exposes `remember(data: Vec<DataInput>, ...)` only (file/text path); no `remember_entry`.
- No `MemoryEntry`, `QAEntry`, `TraceEntry`, `FeedbackEntry` types anywhere in the workspace (the only matching grep hits — `SessionQAEntry` in `crates/session/src/types.rs:21`, `SessionTraceStep` in the same file — are pre-existing persisted-shape types from LIB-02, not the wire DTO union LIB-01 must ship).
- `RememberResult` (`crates/lib/src/api/remember.rs:89-115`) **already carries** `entry_type: Option<String>` and `entry_id: Option<String>` (landed in LIB-06 commit b39cd05); `cognee-models/src/lib.rs` does **not** yet contain `pub mod memory`. Both are reflected in step 1 / step 2 below.
- `RememberStatus` (`crates/lib/src/api/remember.rs:39-58`) ships the four CamelCase variants `Started` / `Completed` / `Errored` / `SessionStored` per Decision 15. LIB-01 reuses the existing enum unchanged.
- `SessionManager::add_agent_trace_step` (LIB-02 — `crates/session/src/session_manager.rs:251-278`) is available; signature uses `(user_id: &str, session_id: Option<&str>, origin_function, status, memory_query, memory_context, method_params, method_return_value, error_message, session_feedback)` and returns the generated `trace_id` directly.
- `SessionManager::add_feedback` (`crates/session/src/session_manager.rs:167-198`) is available and validates `feedback_score ∈ 1..=5`; it returns `Result<bool, SessionError>` (true when the QA was found and updated).
- `SessionManager` exposes `save_qa` (`crates/session/src/session_manager.rs:96-108`) but **not** `add_qa`. Python's `add_qa` ([`session_manager.py:140-150`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/session/session_manager.py#L140)) accepts `feedback_text`, `feedback_score`, and `used_graph_element_ids` directly; Rust's `save_qa` does not. The Rust `SessionQAUpdate` struct (`crates/session/src/session_store.rs:17-23`) already carries those three fields, so the LIB-01 dispatch composes `save_qa` followed by `update_qa` when any of the three fields is set (instead of widening `save_qa`'s public signature).
- `SessionLifecycleDb::ensure_and_touch_session` (LIB-05 — `crates/database/src/traits/session_lifecycle_db.rs:147` + impl `crates/database/src/ops/session_lifecycle.rs:837`) is available for the pre-upsert in step 3 (best-effort — log-and-swallow, matching Python `try/except` at [`remember.py:232-253`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L232-L253)).
- The existing `remember_session` already calls `improve()` with `ImproveParams { ... }` (LIB-04 landed) — `crates/lib/src/api/remember.rs:497-516`. LIB-01 does **not** modify that call site; the file/text path is unchanged.

> The task doc's pseudocode signature `remember_entry(entry, dataset_name, session_id, user, components: &ComponentHandles)` references a `ComponentHandles` aggregate that does not yet exist in `cognee-lib`. The implementation agent should adopt the same per-`Arc<dyn Trait>` parameter style used by `remember()` today (LLM, storage, graph, vector, embedding, db, session_store, session_manager) so the public surface is consistent — see step 3 for the concrete signature.

## 4. Implementation steps

> **Decision (2026-04-29)**: `MemoryEntry` types live in `cognee-models`, not in a new dedicated `cognee-memory` crate. Rationale: `cognee-models` is the project's "data structures, no traits" crate per [`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md), which is the right shape for these pure-data discriminated-union types; spinning up a new crate adds workspace noise without separating any meaningful concern. Investigation agent: do not re-litigate.

1. **Add `MemoryEntry` types** in a new module `crates/models/src/memory.rs` (re-exported from `cognee_models::memory`):
   ```rust
   /// Discriminator values stay snake_case ("qa", "trace", "feedback") per Python's
   /// Literal["qa"] / Literal["trace"] / Literal["feedback"]. The inner field
   /// names on each variant are camelCase on the wire per Decision 10.
   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(tag = "type", rename_all = "snake_case")]   // discriminator → snake; variant
                                                       // structs handle their own field casing
   pub enum MemoryEntry {
       Qa(QAEntry),                                    // type discriminator: "qa"
       Trace(TraceEntry),                              // "trace"
       Feedback(FeedbackEntry),                        // "feedback"
   }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "camelCase")]                  // wire field names per Decision 10
   pub struct QAEntry {
       pub question: String,                           // single-word
       pub answer: String,                             // single-word
       #[serde(default)]
       pub context: String,                            // single-word; default ""
       #[serde(default, alias = "feedback_text")]
       pub feedback_text: Option<String>,              // wire: "feedbackText"
       #[serde(default, alias = "feedback_score")]
       pub feedback_score: Option<i32>,                // wire: "feedbackScore"
       #[serde(default, alias = "used_graph_element_ids")]
       pub used_graph_element_ids: Option<serde_json::Value>,  // wire: "usedGraphElementIds"
   }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "camelCase")]
   pub struct TraceEntry {
       #[serde(alias = "origin_function")]
       pub origin_function: String,                    // wire: "originFunction"
       #[serde(default)]
       pub status: TraceStatus,                        // enum Success | Error → "success"/"error"
       #[serde(default, alias = "method_params")]
       pub method_params: Option<serde_json::Value>,   // wire: "methodParams"
       #[serde(default, alias = "method_return_value")]
       pub method_return_value: Option<serde_json::Value>,  // wire: "methodReturnValue"
       #[serde(default, alias = "memory_query")]
       pub memory_query: String,                       // wire: "memoryQuery"; default ""
       #[serde(default, alias = "memory_context")]
       pub memory_context: String,                     // wire: "memoryContext"; default ""
       #[serde(default, alias = "error_message")]
       pub error_message: String,                      // wire: "errorMessage"; default ""
       #[serde(default, alias = "generate_feedback_with_llm")]
       pub generate_feedback_with_llm: bool,           // wire: "generateFeedbackWithLlm"; default false
   }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "camelCase")]
   pub struct FeedbackEntry {
       #[serde(alias = "qa_id")]
       pub qa_id: String,                              // wire: "qaId"; required
       #[serde(default, alias = "feedback_text")]
       pub feedback_text: Option<String>,              // wire: "feedbackText"
       #[serde(default, alias = "feedback_score")]
       pub feedback_score: Option<i32>,                // wire: "feedbackScore"
   }
   ```
   Field-for-field with Python (see `cognee/memory/entries.py`). `QAEntry::context` defaults to `""`, `TraceEntry::status` defaults to `"success"`, `TraceEntry::generate_feedback_with_llm` defaults to `false`. The `serde(alias)` on every multi-word field accepts snake_case inputs (Python `populate_by_name=True` parity).

2. **`RememberResult.entry_type` / `entry_id` fields** — ~~already present after LIB-06 lands (Q-F / Decision 15)~~ ✅ landed in commit b39cd05 (`crates/lib/src/api/remember.rs:107-110`). `RememberStatus` (lines 39-58) also already emits `"PipelineRunStarted"` / `"PipelineRunCompleted"` / `"PipelineRunErrored"` / `"SessionStored"` per Decision 15. LIB-01 just populates `entry_type` / `entry_id` in step 3 below; no struct extension is needed in this task.

3. **Add `remember_entry()` facade** alongside the existing `remember()` in `crates/lib/src/api/remember.rs`. Adopt the per-`Arc<dyn Trait>` parameter style of the existing `remember()` (no `ComponentHandles` aggregate exists today):
   ```rust
   #[allow(clippy::too_many_arguments)]
   pub async fn remember_entry(
       entry: MemoryEntry,
       dataset_name: &str,
       session_id: &str,
       owner_id: Uuid,
       tenant_id: Option<Uuid>,
       db: Option<Arc<DatabaseConnection>>,
       session_store: Option<Arc<dyn SessionStore>>,
       session_manager: Option<Arc<SessionManager>>,
   ) -> Result<RememberResult, ApiError>;
   ```
   - Returns `Err(ApiError::InvalidArgument("session_id is required..."))` when `session_id` is empty (Python raises `ValueError` → 400). The dedicated `RememberError::MissingSessionId` / `SessionCacheUnavailable` variants are **out of scope** here — the existing `remember()` reuses `ApiError::InvalidArgument` for the same case (`crates/lib/src/api/remember.rs:464-468`); E-02 maps this `ApiError` variant to HTTP 400 in its handler. (Step 5 below is therefore amended: no new error variants — see the strikethrough.)
   - Pre-upserts the `session_records` row via `cognee_database::SessionLifecycleDb::ensure_and_touch_session` (LIB-05). Resolve `dataset_id` via dataset lookup if the metadata DB is available; on any failure log at `debug` level and pass `None`, matching Python's `try/except → resolved_dataset = None` at [`remember.py:232-253`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L232-L253).
   - Match-dispatches:
     - `MemoryEntry::Qa(q)` → `SessionManager::save_qa(...)` (current Rust name — Python's `add_qa`) for the (question, answer, context) triple, then if any of `feedback_text` / `feedback_score` / `used_graph_element_ids` is set, `SessionManager::update_qa(...)` with a `SessionQAUpdate` carrying those fields. `entry_id = qa_id`. (Rationale: `SessionQAUpdate` already carries all three fields — `crates/session/src/session_store.rs:17-23` — so the dispatch composes existing methods rather than widening `save_qa`'s signature.)
     - `MemoryEntry::Trace(t)` → `SessionManager::add_agent_trace_step(user_id, session_id, origin_function, status, memory_query, memory_context, method_params, method_return_value, error_message, session_feedback)` (LIB-02; Rust returns `String`, not `Option`). `entry_id = trace_id`. Pass `t.method_params.unwrap_or(serde_json::Value::Null)` since the Rust signature requires a non-optional `serde_json::Value`. The `t.generate_feedback_with_llm` flag is currently parity-stub: log-and-pass `session_feedback = ""` (Python's LLM-feedback-generation lives at the SessionManager level and is **out of scope** for LIB-01 — track via a `// TODO(LIB-01-followup)` comment).
     - `MemoryEntry::Feedback(f)` → `SessionManager::add_feedback(session_id, user_id, qa_id, feedback_text, feedback_score)` (returns `Result<bool, _>`). On `Ok(true)` set `entry_id = f.qa_id`; on `Ok(false)` set `status = Errored` and `error = Some(format!("add_feedback: QA {qa_id} not found in session {session_id}"))`.
   - Sets `result.entry_type` from the discriminator string (`"qa" | "trace" | "feedback"`).
   - Sets `result.entry_id` to the cache-returned id.
   - Sets `result.status = RememberStatus::SessionStored` on success, `RememberStatus::Errored` + populated `error` on failure. (E-01 has already added the lowercase HTTP-wire translation at the DTO boundary; LIB-01 emits the library-side CamelCase form unchanged.)
   - Returns a fully-populated `RememberResult { dataset_name, dataset_id: None, session_ids: Some(vec![session_id.to_string()]), pipeline_run_id: None, elapsed_seconds: Some(start.elapsed().as_secs_f64()), content_hash: None, items_processed: 0, items: vec![], cognify_result: None, memify_result: None, .. }` — mirrors the shape `remember_session` returns today (`crates/lib/src/api/remember.rs:540-555`).

4. **Re-export from `cognee-lib`**:
   - In `crates/models/src/lib.rs`: `pub mod memory;` (with the new `crates/models/src/memory.rs` module).
   - In `crates/lib/src/api/mod.rs`: extend `pub use remember::{...}` to include `remember_entry`.
   - The umbrella `cognee_lib::models::*` re-export already covers `cognee_models::memory::*` once `pub mod memory` is added (see `crates/lib/src/lib.rs:40-42`).

5. ~~**No new error variants** beyond `RememberError::MissingSessionId` and `RememberError::SessionCacheUnavailable`. The HTTP layer maps these to 400 / 503 (E-02).~~ **Amended 2026-04-30**: a dedicated `RememberError` enum is **not** introduced. LIB-01 reuses the existing `ApiError` variants — `ApiError::InvalidArgument` for missing `session_id` (the same path `remember()` uses today at `crates/lib/src/api/remember.rs:464-468`) and the existing `ApiError::Session(SessionError)` `From` impl for cache failures. E-02's handler maps `ApiError::InvalidArgument` → 400 and `ApiError::Session(SessionError::StoreError(_) /* "cache unavailable" */ )` → 503; the catch-all 409 stays at the handler boundary (matches Python's bare `except Exception`). This avoids a new error enum that would duplicate the existing API-error surface.

## 5. Tests

- `crates/lib/tests/test_remember_entry.rs` (new):
  - `test_qa_entry_dispatch_returns_qa_id` — uses an in-test `SessionStore` impl (no `MockSessionStore` exists in `cognee-test-utils` today; an inline `struct InMemorySessionStore` in the test file is the established pattern, see `crates/delete/src/lib.rs:3565-3580` for the prior art), asserts `entry_type=="qa"`, `entry_id` matches.
  - `test_qa_entry_with_optional_fields_persists_via_update_qa` — covers the `feedback_text` / `feedback_score` / `used_graph_element_ids` follow-up `update_qa` call.
  - `test_trace_entry_dispatch` — same pattern via `add_agent_trace_step` (LIB-02 already lands the trait/impls).
  - `test_feedback_entry_dispatch_returns_qa_id_on_success`.
  - `test_feedback_entry_returns_errored_when_qa_missing` — assert `status == Errored` and `error` contains the qa_id.
  - `test_missing_session_id_returns_error` — empty string yields `ApiError::InvalidArgument`.
  - `test_round_trip_memory_entry_qa_json` (in `crates/models/src/memory.rs` `#[cfg(test)] mod tests`): assert `serde_json::from_str(r#"{"type":"qa","question":"q","answer":"a"}"#)` deserializes to `MemoryEntry::Qa` with `context == ""`. Cover camelCase wire (`"feedbackText"`) plus snake_case alias (`"feedback_text"`) input parity.
  - `test_round_trip_memory_entry_trace_json` and `test_round_trip_memory_entry_feedback_json` covering the analogous shapes.

## 6. Acceptance criteria

- [x] `MemoryEntry` discriminated enum lives in `cognee-models` with `serde(tag="type")` and round-trips to the Python wire shape (verified by 3 JSON fixture tests in `crates/models/src/memory.rs`).
- [x] `remember_entry()` exists in `cognee_lib::api::remember` and dispatches to the three `SessionManager` methods.
- [x] `RememberResult.entry_type` and `entry_id` populated for all three branches (qa, trace, feedback) — including the `Ok(false)` feedback path which still sets `entry_id = qa_id` per Python parity.
- [x] No `unwrap()` in non-test code.
- [x] `cargo check --all-targets` clean; 6 integration tests + 4 model round-trip tests pass.

## 7. References

- [Python `MemoryEntry`](https://github.com/topoteretes/cognee/blob/main/cognee/memory/entries.py)
- [Python remember dispatch](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L262)
- [E-02 — `POST /remember/entry`](e-02-remember-entry.md)
- [LIB-02 — `add_agent_trace_step`](lib-02-session-manager-trace-step.md)
