# E-01 — `POST /api/v1/remember`

| | |
|---|---|
| Wire path | `POST /api/v1/remember` |
| Status | **In Progress** — verify-only investigation (2026-04-29) found wire-shape divergences; see §3.1 |
| Depends on | **LIB-06** (TASK 0-2) — provides the new `RememberStatus` CamelCase library enum (this task translates to lowercase at the wire boundary), `RememberResult.elapsed_seconds: Option<f64>`, and the `PipelineRunInfo.completed_at` / `elapsed_seconds()` accessor that downstream P5 wiring will use. |
| Effort | originally ~0.5 day (verification only); now ~1 day (response DTO needs fields added + `WireRememberStatus` translation enum). |
| Owner crate | `cognee-http-server` |

## 1. Goal

Originally scoped as a verify-only confirmation of the multipart "blob of text/files" path. The 2026-04-29 investigation found multiple wire-shape gaps in the response DTO (see [§3.1](#31-divergences-from-python-wire-output-investigation-2026-04-29)) and one form-input gap (`session_id`). E-01 is no longer pure verify-only — it brings the Rust HTTP `/remember` response into byte-for-byte parity with Python's `RememberResult.to_dict()` output, consuming the library types LIB-06 provides.

What this task does **not** do:
- Wire real `remember()` execution at the HTTP handler. The handler stays at its current `TODO(P5)` stub — only the wire-shape DTO gains the missing fields. P5 (in the existing http-server plan, not v2) is responsible for wiring real execution; once that lands, the new DTO fields populate naturally from the library `RememberResult`.
- Add `entry_type` / `entry_id` to `RememberResultDTO` — those land in **E-02** alongside the `/remember/entry` route (Decision 5).

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `POST ""` handler | `cognee/api/v1/remember/routers/get_remember_router.py` | 28–113 |
| `cognee.remember(data, ...)` | `cognee/api/v1/remember/remember.py` | 1–230 |

Form fields (multipart):

```
data: List[UploadFile]
datasetName: Optional[str]
datasetId: UUID | "" | None
session_id: Optional[str]
node_set: Optional[List[str]]      (Python sends [""] → treat as None)
run_in_background: Optional[bool]  (default false)
custom_prompt: Optional[str]       (default "")
chunks_per_batch: Optional[int]    (default 10)
```

Response: `RememberResult.to_dict()`. `200` on success; `409` `{error}` envelope on exception; ValueError → `400 {error}`.

## 3. Current Rust state

- Router: `crates/http-server/src/routers/remember.rs:284-286` — `Router::new().route("/", post(post_remember))`.
- Multipart parsing: same file, `parse_remember_multipart` at line 31.
- DTO: `crates/http-server/src/dto/remember.rs` — `RememberFormDTO`, `RememberResultDTO`, `UploadedFilePart`.
- Form fields covered (incl. `[""]` → `None` translation): `datasetName`, `datasetId`, `node_set`, `run_in_background`, `custom_prompt`, `chunks_per_batch`. **Note**: `session_id` Form field is in Python ([`get_remember_router.py:34`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L34)) but not in `parse_remember_multipart` (`remember.rs:42-103`) and not stored on `RememberFormDTO` (`dto/remember.rs:16-27`). Python forwards it to `cognee_remember(session_id=...)`. Rust currently silently drops it.
- Existing tests:
  - Inline tests at `crates/http-server/src/routers/remember.rs:290-385` — router-mounting smoke, non-multipart 400, 409 catch-all body shape, 400 validation body shape.
  - Integration tests at `crates/http-server/tests/test_remember.rs:9-81` — 401 unauthenticated; 409/400 body cross-references; end-to-end skipped without `OPENAI_URL`.
  - Cross-SDK harness already exists: `e2e-cross-sdk/harness/test_http_remember.py:1-98` — three Phase-2 LLM-gated tests (`test_remember_blocking`, `test_remember_with_session_id`, `test_remember_without_session_id`). **Caveat**: it sends `application/json` payloads (`{"query": ..., "session_id": ..., "run_in_background": ...}`), but Python's router ([`get_remember_router.py:30-39`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L30-L39)) is `Form(...)`-only — JSON is silently ignored and all fields default. The harness only asserts status-code equality, so it passes mechanically without exercising parity.

### 3.1 Divergences from Python wire output (investigation 2026-04-29)

Comparing `RememberResultDTO` ([`crates/http-server/src/dto/remember.rs:42-51`](../../../crates/http-server/src/dto/remember.rs)) against Python's `RememberResult.to_dict()` ([`cognee/api/v1/remember/remember.py:415-437`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L415-L437)):

| # | Python wire field | Rust DTO field | Status |
|---|---|---|---|
| 1 | `status` always emitted; values `"running"` / `"completed"` / `"errored"` / `"session_stored"` (`remember.py:323-324, 480, 521, 720, 751`) | `status: String` set to `"PipelineRunStarted"` / `"PipelineRunCompleted"` (`remember.rs:268-272`) | ❌ **wire divergence to fix** — wire output must match Python's lowercase. Decision 15 / LIB-06 sets the **library** enum to CamelCase for internal consistency; **E-01 owns the lowercase translation at the HTTP DTO boundary** so the wire is byte-correct vs Python. |
| 2 | `dataset_name` always | `dataset_name` | ✅ match (snake_case correct — see below) |
| 3 | `dataset_id` always (Optional[str]) | `dataset_id: Uuid` | ⚠️ Rust always emits a UUID; Python may emit `null`. Likely ok in practice. |
| 4 | `pipeline_run_id` always (Optional[str]) | `pipeline_run_id: Uuid` | ⚠️ same as #3 — Python may emit `null` (e.g. session_stored mode) |
| 5 | `items_processed` always emitted (default `0`) | absent | ❌ **missing field** |
| 6 | `elapsed_seconds` always emitted (default `null`) | absent | ❌ **missing field** |
| 7 | `session_ids` conditional (when `self.session_ids` is set, list[str]) | absent | ❌ **missing field** — important for session-bridge story |
| 8 | `content_hash` conditional | absent | ❌ **missing field** |
| 9 | `items` conditional (list of per-item dicts) | absent | ❌ **missing field** |
| 10 | `entry_type` conditional (only on `/remember/entry` route) | absent | ✅ correct here — **Decision 5** carves this out for E-02 |
| 11 | `entry_id` conditional (only on `/remember/entry`) | absent | ✅ correct here — **Decision 5** carves this out for E-02 |
| 12 | `error` conditional | `error: Option<String>` (`skip_serializing_if`) | ✅ match |

Form-input divergence: Python `session_id: Optional[str] = Form(default=None, examples=[""])` ([`get_remember_router.py:34`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L34)) is forwarded to `cognee_remember(session_id=...)` ([`get_remember_router.py:84`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L84)). Rust's `parse_remember_multipart` does not parse this form field and the handler does not pass it through to the (currently stubbed) pipeline. Adding the field to `RememberFormDTO` is required for parity.

Behavioral divergence (acknowledged-by-design, not a wire shape issue): Rust's add path is wired (`add_with_params` in `remember.rs:218-220`), but the cognify+memify path is a stub (`remember.rs:229`, comment `TODO(P5): wire real remember() call once ComponentHandles gains all handles`). This means Rust does not yet populate `pipeline_run_id` from a real run, nor `items_processed` / `content_hash` / `items` / `session_ids`. **Wiring real `remember()` execution is out of scope for E-01 by Decision 5's intent; only the wire-shape DTO fields are E-01's concern.**

Casing rule (Decision 10) carve-out: Python's `RememberResult` is **not a pydantic `BaseModel`** (`remember.py:316` — plain `class`); its `to_dict()` produces snake_case keys directly and `jsonable_encoder()` does not apply alias conversion to plain dicts. Therefore `RememberResultDTO`'s `#[serde(rename_all = "snake_case")]` is **correct** and explicitly whitelisted by [CLEAN-01](clean-01-v1-dto-camelcase.md) (commit `e146835`, see CLEAN-01 §3.1 row for `dto/remember.rs`). Do not flip to camelCase.

## 4. Implementation steps (revised by 2026-04-29 investigation)

E-01 is no longer pure verify-only. The implementation agent runs these steps:

1. Extend `RememberFormDTO` (`crates/http-server/src/dto/remember.rs:15-27`) with `pub session_id: Option<String>`. Update `parse_remember_multipart` in `crates/http-server/src/routers/remember.rs` to parse the `session_id` form field (mirrors the existing `custom_prompt` block at lines 88-95). Forward it through to wherever the real `remember()` call will eventually live — for now, propagate to the AddPipeline / dispatch sites if applicable, otherwise document as TODO until P5 wiring lands.

2. Extend `RememberResultDTO` to match Python's `to_dict()` output:
   - Add `items_processed: u32` (always serialized, default 0) — required by Python wire.
   - Add `elapsed_seconds: Option<f64>` (always serialized, may be `null`) — Python emits `null` until pipeline finishes.
   - Add `session_ids: Option<Vec<String>>` with `skip_serializing_if = "Option::is_none"`.
   - Add `content_hash: Option<String>` with `skip_serializing_if = "Option::is_none"`.
   - Add `items: Option<Vec<serde_json::Value>>` (or a typed `RememberItemDTO` mirroring Python's `{name, content_hash, token_count}` shape) with `skip_serializing_if = "Option::is_none"`.
   - **Do NOT add `entry_type` / `entry_id` here** — Decision 5 reserves that for E-02.
   - Keep `rename_all = "snake_case"` per the CLEAN-01 carve-out (Python's `RememberResult` is a plain class, not pydantic).

3. **Introduce `WireRememberStatus`** (Q-E option q, Decision 15) — a typed wire enum in `crates/http-server/src/dto/remember.rs` that serializes to Python's lowercase strings, with a `From<cognee_lib::api::remember::RememberStatus>` impl that does the translation:
   ```rust
   // crates/http-server/src/dto/remember.rs
   use cognee_lib::api::remember::RememberStatus;

   /// Wire-format status for the `/remember` and `/remember/entry` HTTP
   /// responses. Python's `RememberResult.to_dict()` emits these lowercase
   /// strings; we translate from the library's CamelCase enum at the DTO
   /// boundary (Decision 15 — two-layer status convention).
   #[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
   pub enum WireRememberStatus {
       #[serde(rename = "running")]
       Running,
       #[serde(rename = "completed")]
       Completed,
       #[serde(rename = "errored")]
       Errored,
       #[serde(rename = "session_stored")]
       SessionStored,
   }

   impl From<RememberStatus> for WireRememberStatus {
       fn from(s: RememberStatus) -> Self {
           match s {
               RememberStatus::Started       => Self::Running,        // background-mode init
               RememberStatus::Completed     => Self::Completed,
               RememberStatus::Errored       => Self::Errored,
               RememberStatus::SessionStored => Self::SessionStored,
           }
       }
   }
   ```
   Then change `RememberResultDTO.status: String` → `RememberResultDTO.status: WireRememberStatus`. Replace the inline `String` literals at `remember.rs:268-272` with construction via `From<RememberStatus>`. **The wire JSON byte shape is unchanged from Python** — the typed enum is a Rust-internal refactor that just makes the translation type-safe.

   **Decision 15** (two-layer status convention): library enum is CamelCase for internal Rust consistency with the other four pipeline routers; HTTP DTO translates to lowercase for strict Python wire parity. **No new wire divergence** — wire is byte-correct vs Python's `RememberResult.to_dict()`.

4. Populate the new fields where the data is already available (e.g. `items_processed = files.len() as u32` after add succeeds; the rest may stay `None`/`null` until P5 wiring lands real cognify execution). Do NOT block the task on real execution — the wire-shape parity is the goal.

5. Update existing inline + integration tests (`crates/http-server/src/routers/remember.rs:290-385`, `crates/http-server/tests/test_remember.rs`) for the new `status` literals and assert the new fields appear in the JSON body with their Python defaults.

6. Cross-SDK harness review: `e2e-cross-sdk/harness/test_http_remember.py` already exists. Replace its JSON-body invocations with multipart uploads (Python's router is Form-only). Add a structural body-diff (not just status-code equality) so the new wire fields are exercised. Use `assert_responses_match` from `http_helpers` with the existing `_IGNORE` set.

7. Re-confirm `openapi_property_names_are_all_camelcase` (`crates/http-server/tests/test_openapi_camelcase.rs`) still passes — `RememberResultDTO` either stays out of the schema set entirely or gets added to the SNAKE_CASE_WHITELIST with the same justification CLEAN-01 used for it (line 50 / 78 of that doc).

## 5. Polish (if tests reveal gaps)

- **`entry_type` / `entry_id` are NOT added in this task.** Per Decision 5 (2026-04-29), those fields are populated only by `/remember/entry`; the structural change to `RememberResultDTO` lands in **E-02** alongside the new route. On the `POST /remember` file-payload path Python omits both fields from the response, so the Rust DTO without them is already byte-correct. Do not pre-emptively add them here.
- Confirm 409 `{error}` envelope wording — Python returns `"An error occurred during remember."`; Rust must match verbatim.
- The `RememberResult.to_dict()` Python implementation always emits `items_processed` and `elapsed_seconds`, even with default values (`0` and `null`). Use serde defaults (e.g. `#[serde(default)]` on the request side and unconditional emission on the response side, no `skip_serializing_if`) to mirror this.

## 6. Acceptance criteria

- [ ] `RememberFormDTO` has `session_id: Option<String>`; `parse_remember_multipart` parses it; forwarded through to dispatch (or marked with the same `TODO(P5)` as the cognify wiring).
- [ ] `RememberResultDTO` has `items_processed`, `elapsed_seconds`, `session_ids`, `content_hash`, `items` fields, matching Python's snake_case wire output exactly.
- [ ] `WireRememberStatus` enum exists in `crates/http-server/src/dto/remember.rs` with per-variant lowercase serde and `From<cognee_lib::api::remember::RememberStatus>` impl (4 variants, exhaustive). `RememberResultDTO.status` is typed as `WireRememberStatus`, not `String`. Wire emits Python-parity lowercase `"running"` / `"completed"` / `"errored"` / `"session_stored"` (Decision 15).
- [ ] Inline and integration tests updated; all pass.
- [ ] `e2e-cross-sdk/harness/test_http_remember.py` uses multipart bodies and structural body-diff.
- [ ] `cargo test -p cognee-http-server --test test_remember`, `--test test_openapi_camelcase`, `--test test_dto_wire_shape` pass.
- [ ] `scripts/check_all.sh` clean.

## 7. References

- [Python remember router](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py)
- [Rust handler](../../../crates/http-server/src/routers/remember.rs)
- [E-02 — `POST /remember/entry`](e-02-remember-entry.md) — sibling route, shared `RememberResult` type
