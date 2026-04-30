# E-01 — `POST /api/v1/remember`

| | |
|---|---|
| Wire path | `POST /api/v1/remember` |
| Status | **Done (commit 037cad2)** — wire-shape divergences from §3.1 are resolved; `WireRememberStatus` introduced as a standalone wire enum (no library coupling per the cycle constraint in §3). |
| Depends on | **LIB-06** (TASK 0-2) — **shipped at commit b39cd05**. Provides: (a) `cognee_lib::api::remember::RememberStatus` CamelCase enum with the four variants `Started` / `Completed` / `Errored` / `SessionStored` ([`crates/lib/src/api/remember.rs:39-58`](../../../crates/lib/src/api/remember.rs#L39-L58)); (b) `From<cognee_core::pipeline::PipelineRunStatus> for RememberStatus` ([same file:60-69](../../../crates/lib/src/api/remember.rs#L60-L69)); (c) `RememberResult.elapsed_seconds: Option<f64>` ([same file:96-98](../../../crates/lib/src/api/remember.rs#L96-L98)); (d) `RememberResult.entry_type` / `entry_id` ([same file:104-110](../../../crates/lib/src/api/remember.rs#L104-L110)); (e) `PipelineRunInfo.completed_at` + `elapsed_seconds()` accessor for downstream P5 wiring. E-01 consumes (a)–(d) at the HTTP DTO boundary; (e) is reserved for the P5 wiring task that will replace the current cognify+memify stub. |
| Effort | originally ~0.5 day (verification only); now ~1 day (response DTO needs fields added + `WireRememberStatus` translation enum). LIB-06 already shipped the library-side types so E-01 only owns the HTTP DTO layer. |
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
- **Crate dependency constraint (discovered 2026-04-29)**: `cognee-http-server` does **not** depend on `cognee-lib` and cannot — `cognee-lib`'s `server` feature pulls in `cognee-http-server`, so a back-edge would create a cycle ([`crates/http-server/Cargo.toml:36-38`](../../../crates/http-server/Cargo.toml#L36-L38), [`crates/lib/Cargo.toml:24,82`](../../../crates/lib/Cargo.toml#L24)). LIB-06's library `RememberStatus` enum lives in `crates/lib/src/api/remember.rs:40-58` and is therefore **not directly importable from http-server**. E-01 must define `WireRememberStatus` as a **standalone** type in `crates/http-server/src/dto/remember.rs` with no `From<cognee_lib::api::remember::RememberStatus>` impl. The `From<RememberStatus>` impl that the original §4 step 3 sketched is **deferred to the P5 wiring task** — that task is the one that will route the handler through `cognee_lib::api::remember::remember()` and at that point will need (and own) the cross-crate translation, either by moving `RememberStatus` to a leaf crate (e.g. `cognee-models`) or by hosting the translation in `cognee-lib` and exposing it via a small adapter. Until then, the http-server handler builds `WireRememberStatus` values directly from local state (`run_in_background → Running` else `Completed`).
- Existing tests:
  - Inline tests at `crates/http-server/src/routers/remember.rs:290-385` — router-mounting smoke, non-multipart 400, 409 catch-all body shape, 400 validation body shape.
  - Integration tests at `crates/http-server/tests/test_remember.rs:9-81` — 401 unauthenticated; 409/400 body cross-references; end-to-end skipped without `OPENAI_URL`.
  - Cross-SDK harness already exists: `e2e-cross-sdk/harness/test_http_remember.py:1-98` — three Phase-2 LLM-gated tests (`test_remember_blocking`, `test_remember_with_session_id`, `test_remember_without_session_id`). **Caveat**: it sends `application/json` payloads (`{"query": ..., "session_id": ..., "run_in_background": ...}`), but Python's router ([`get_remember_router.py:30-39`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L30-L39)) is `Form(...)`-only — JSON is silently ignored and all fields default. The harness only asserts status-code equality, so it passes mechanically without exercising parity.

### 3.1 Divergences from Python wire output (investigation 2026-04-29)

> **Resolved in commit 037cad2.** All wire-shape divergences listed below have been addressed: `RememberResultDTO` now emits Python's snake_case keys with `items_processed` / `elapsed_seconds` / `session_ids` / `content_hash` / `items` populated; `dataset_id` / `pipeline_run_id` are `Option<Uuid>` (always-emit, may be `null`); `status` is typed as `WireRememberStatus` and emits Python's lowercase strings.

Comparing `RememberResultDTO` ([`crates/http-server/src/dto/remember.rs:42-51`](../../../crates/http-server/src/dto/remember.rs)) against Python's `RememberResult.to_dict()` ([`cognee/api/v1/remember/remember.py:415-437`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L415-L437)):

| # | Python wire field | Rust DTO field | Status |
|---|---|---|---|
| 1 | `status` always emitted; values `"running"` / `"completed"` / `"errored"` / `"session_stored"` (`remember.py:323-324, 480, 521, 720, 751`) | `status: String` set to `"PipelineRunStarted"` / `"PipelineRunCompleted"` (`remember.rs:268-272`) | ❌ **wire divergence to fix** — wire output must match Python's lowercase. Decision 15 / LIB-06 (shipped in commit b39cd05) sets the **library** enum to CamelCase for internal consistency; **E-01 owns the lowercase translation at the HTTP DTO boundary** so the wire is byte-correct vs Python. The library types are now in place at [`crates/lib/src/api/remember.rs:39-58`](../../../crates/lib/src/api/remember.rs#L39-L58); E-01 introduces a wire-only `WireRememberStatus` enum that is **not coupled** to the library type because of the http-server↔lib cycle constraint (see §3 last bullet). |
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

## 4. Implementation steps (revised 2026-04-29 post-LIB-06)

E-01 is no longer pure verify-only. LIB-06 has shipped (commit b39cd05) — its library types are available but **cannot be directly imported from http-server** due to the `cognee-lib` ↔ `cognee-http-server` cycle constraint (see §3 last bullet). The implementation agent runs these steps:

1. Extend `RememberFormDTO` ([`crates/http-server/src/dto/remember.rs:15-27`](../../../crates/http-server/src/dto/remember.rs#L15-L27)) with `pub session_id: Option<String>`. Update `parse_remember_multipart` ([`crates/http-server/src/routers/remember.rs:31-120`](../../../crates/http-server/src/routers/remember.rs#L31-L120)) to parse the `session_id` form field (mirrors the existing `custom_prompt` block at [lines 88-95](../../../crates/http-server/src/routers/remember.rs#L88-L95)). Treat empty string as `None` (Python's `examples=[""]` is illustrative — empty is the "absent" sentinel). Forward `form.session_id` through to wherever the real `remember()` call will eventually live; until P5 wiring lands, attach a `TODO(P5)` comment next to the dispatch site so it isn't silently dropped.

2. Extend `RememberResultDTO` ([`crates/http-server/src/dto/remember.rs:42-51`](../../../crates/http-server/src/dto/remember.rs#L42-L51)) to match Python's `to_dict()` output ([`cognee/api/v1/remember/remember.py:415-437`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L415-L437)):
   - Add `items_processed: u32` (always serialized, default 0) — Python always emits this, even when 0.
   - Add `elapsed_seconds: Option<f64>` (always serialized — emit `null` when absent, **not** omit). Use a custom serializer or just plain `Option<f64>` without `skip_serializing_if` (serde defaults to emit `null`).
   - Add `session_ids: Option<Vec<String>>` with `#[serde(skip_serializing_if = "Option::is_none")]` — Python only emits this key when `self.session_ids` is set ([remember.py:425-426](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L425-L426)).
   - Add `content_hash: Option<String>` with `#[serde(skip_serializing_if = "Option::is_none")]` — Python `if self.content_hash:` ([remember.py:427-428](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L427-L428)).
   - Add `items: Option<Vec<RememberItemDTO>>` (or `Vec<serde_json::Value>`) with `#[serde(skip_serializing_if = "Option::is_none")]`. Prefer a typed `RememberItemDTO { name: Option<String>, content_hash: Option<String>, token_count: Option<i64> }` so the OpenAPI schema is meaningful — fields mirror `cognee_lib::api::remember::RememberItemInfo` ([`crates/lib/src/api/remember.rs:72-82`](../../../crates/lib/src/api/remember.rs#L72-L82)) but the http-server DTO must be standalone (no library import — see §3 last bullet).
   - Make `dataset_id` and `pipeline_run_id` `Option<Uuid>` with `#[serde(skip_serializing_if = "Option::is_none")]` — Python `to_dict` always emits the keys but they may be `None`. Re-check parity: Python emits `"dataset_id": None` (key present, value null) **always** ([remember.py:419-421](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L419-L421)) — so do **not** apply `skip_serializing_if` to these two; emit `null`. Today the Rust DTO has `dataset_id: Uuid` and `pipeline_run_id: Uuid` (always present) which is "wrong direction" but practically still byte-equivalent on the success path; flip to `Option<Uuid>` without skip so we emit `null` on the session-stored path.
   - **Do NOT add `entry_type` / `entry_id` here** — Decision 5 reserves them for E-02 (the `/remember/entry` route). On the file-payload path Python omits both keys, so the file-path response stays correct.
   - Keep `#[serde(rename_all = "snake_case")]` per the CLEAN-01 carve-out (Python's `RememberResult` is a plain class, not pydantic — see [CLEAN-01 §3.1 row for `dto/remember.rs`](clean-01-v1-dto-camelcase.md)).

3. **Introduce `WireRememberStatus`** in `crates/http-server/src/dto/remember.rs` — a standalone typed wire enum that serializes to Python's lowercase strings. **Do NOT** add a `From<cognee_lib::api::remember::RememberStatus>` impl: the http-server crate cannot depend on `cognee-lib` (cycle constraint, §3 last bullet). The cross-crate translation is **deferred to the P5 wiring task** — that task introduces the actual library call and at that point will own the `From` translation (either by moving the source enum to a leaf crate, or by adding a thin adapter accessor on `AppState`'s `ComponentHandles`).

   ```rust
   // crates/http-server/src/dto/remember.rs

   /// Wire-format status for the `/remember` and `/remember/entry` HTTP
   /// responses. Python's `RememberResult.to_dict()` emits these exact
   /// lowercase strings — see `cognee/api/v1/remember/remember.py:323-324`.
   ///
   /// Decision 15 (two-layer status convention): the library
   /// `cognee_lib::api::remember::RememberStatus` enum (LIB-06, commit
   /// b39cd05) emits CamelCase for internal Rust consistency with
   /// `cognee_core::PipelineRunStatus`. The HTTP layer translates back to
   /// Python's lowercase here for strict wire parity. **No wire divergence.**
   ///
   /// The cross-crate `From<cognee_lib::api::remember::RememberStatus>`
   /// translation is deferred to the P5 wiring task because
   /// `cognee-http-server` cannot depend on `cognee-lib` (cycle).
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
   ```

   Then change `RememberResultDTO.status: String` → `RememberResultDTO.status: WireRememberStatus`. Replace the inline `String` literals at [`remember.rs:268-272`](../../../crates/http-server/src/routers/remember.rs#L268-L272) with `WireRememberStatus::Running` / `WireRememberStatus::Completed` based on the `run_in_background` flag (today's logic). **The wire JSON byte shape now matches Python** — the typed enum is the type-safe replacement for the previous CamelCase string literals.

4. Populate the new fields where the data is already available:
   - `items_processed = files.len() as u32` after `add_with_params` succeeds (the file part list is the truth source until P5 wires the real library call).
   - `elapsed_seconds`: capture an `Instant::now()` at handler entry, compute `start.elapsed().as_secs_f64()` after dispatch, populate `Some(elapsed)`.
   - `session_ids`, `content_hash`, `items`: leave `None` for now with a `TODO(P5)` comment; populated when the handler routes through `cognee_lib::api::remember::remember()` and reads the populated `RememberResult`.
   - `dataset_id`/`pipeline_run_id`: continue populating from current sources (dispatch-returned run id; resolved/created dataset id).

5. Update existing inline + integration tests for the new `status` literals and the new fields:
   - `crates/http-server/src/routers/remember.rs:290-385` — the inline tests; add a body-shape assertion that builds a `RememberResultDTO` and round-trips through `serde_json::to_value` to verify the lowercase status strings and the presence/absence of conditional keys.
   - `crates/http-server/tests/test_remember.rs:9-81` — add a body-shape integration test (gated as needed) that asserts the lowercase `status`, the always-present `items_processed` / `elapsed_seconds` keys, and the conditional omission of `session_ids` / `content_hash` / `items` when not set.
   - Add a serde-roundtrip unit test for `WireRememberStatus` (each variant → expected lowercase string).

6. Cross-SDK harness rework: `e2e-cross-sdk/harness/test_http_remember.py:1-98` currently sends `application/json` payloads (`{"query": ..., "session_id": ..., "run_in_background": ...}`), which Python's `Form(...)`-only router silently ignores ([`get_remember_router.py:30-39`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L30-L39)). Replace JSON-body invocations with multipart uploads on **both** SDK calls. Add a structural body-diff using `assert_responses_match` from `http_helpers` with an `_IGNORE` set excluding `pipeline_run_id`, `dataset_id`, and `elapsed_seconds` (which differ per-run).

7. Re-confirm `openapi_property_names_are_all_camelcase` (`crates/http-server/tests/test_openapi_camelcase.rs`) still passes — `RememberResultDTO` (and the new `RememberItemDTO`, if introduced) stays in the snake_case whitelist with the CLEAN-01 carve-out justification (Python's `RememberResult` is a plain class, not a pydantic `BaseModel`, so it does not pass through `to_camel`).

## 5. Polish (if tests reveal gaps)

- **`entry_type` / `entry_id` are NOT added in this task.** Per Decision 5 (2026-04-29), those fields are populated only by `/remember/entry`; the structural change to `RememberResultDTO` lands in **E-02** alongside the new route. On the `POST /remember` file-payload path Python omits both fields from the response, so the Rust DTO without them is already byte-correct. Do not pre-emptively add them here.
- Confirm 409 `{error}` envelope wording — Python returns `"An error occurred during remember."`; Rust must match verbatim.
- The `RememberResult.to_dict()` Python implementation always emits `items_processed` and `elapsed_seconds`, even with default values (`0` and `null`). Use serde defaults (e.g. `#[serde(default)]` on the request side and unconditional emission on the response side, no `skip_serializing_if`) to mirror this.

## 6. Acceptance criteria

- [x] `RememberFormDTO` has `session_id: Option<String>`; `parse_remember_multipart` parses it (empty → `None`); forwarded through to the dispatch site or marked with `TODO(P5)`.
- [x] `RememberResultDTO` has `items_processed`, `elapsed_seconds`, `session_ids`, `content_hash`, `items` fields, matching Python's snake_case wire output exactly. `dataset_id` / `pipeline_run_id` flipped to `Option<Uuid>` (always-emit, may be `null`).
- [x] `WireRememberStatus` enum exists in `crates/http-server/src/dto/remember.rs` with per-variant lowercase serde (4 variants: `running`, `completed`, `errored`, `session_stored`). `RememberResultDTO.status` is typed as `WireRememberStatus`, not `String`. Wire emits Python-parity lowercase `"running"` / `"completed"` / `"errored"` / `"session_stored"` (Decision 15).
- [x] **No** `From<cognee_lib::api::remember::RememberStatus>` impl in this task — it's deferred to the P5 wiring task per the `cognee-lib` ↔ `cognee-http-server` cycle constraint (§3 last bullet). E-01 only owns the wire-side type.
- [x] Inline and integration tests updated; all pass.
- [x] `e2e-cross-sdk/harness/test_http_remember.py` uses multipart bodies and structural body-diff.
- [x] `cargo test -p cognee-http-server --test test_remember`, `--test test_openapi_camelcase`, `--test test_dto_wire_shape` pass.
- [x] `scripts/check_all.sh` clean.

## 7. References

- [Python remember router](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py)
- [Rust handler](../../../crates/http-server/src/routers/remember.rs)
- [E-02 — `POST /remember/entry`](e-02-remember-entry.md) — sibling route, shared `RememberResult` type
