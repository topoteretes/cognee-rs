# Cognee HTTP API v2 — Memory-Oriented HTTP Surface

This document is the **root index** for the cognee-rust HTTP API v2 implementation package. It mirrors the structure of [`../http-server/`](../http-server/) (which covers the v1 HTTP surface) but is scoped specifically to the **memory-oriented v2 HTTP endpoints** that the Python cognee FastAPI server exposes on top of the v1 primitives.

> ⚠️  **Naming**: Python cognee does not use a literal `/api/v2/` URL prefix — every cognee HTTP route lives under `/api/v1/`. The "v2" label refers to the SDK's _**V2 memory-oriented API**_ (`remember` / `recall` / `improve` / `forget` / `serve` / `disconnect` / `visualize`), as documented in [`../api-v2/README.md`](../api-v2/README.md). This package documents the **HTTP server surface** that exposes those v2 SDK functions, plus the auxiliary `/sessions` dashboard router that only exists to support the v2 workflow.

For the SDK-level (in-process Rust API) gap analysis of the same v2 functions see [`../api-v2/README.md`](../api-v2/README.md) — it is the authoritative source for the **library** parity. This document is the authoritative source for the **HTTP wire** parity.

---

## 1. Goal

Port the Python v2 HTTP surface ([`cognee/api/v1/{remember,recall,improve,forget,sessions,visualize}/routers/`](https://github.com/topoteretes/cognee/tree/main/cognee/api/v1)) to the cognee-rust `crates/http-server/` so that v2-aware HTTP clients (the cognee-frontend "Memory" pages, the cognee-mcp memory tools, the cognee Cloud client, third-party agent integrations) work byte-for-byte against either backend.

The strict-Python-parity rule from [`../http-server/plan.md §1`](../http-server/plan.md#1-goal) applies here too. Where the Rust v1 work introduced acknowledged divergences (pipeline-run registry eviction, graceful-shutdown error rows), they carry over unchanged. No new Rust-side improvements are allowed.

### 1.1 Wire conventions (project-wide, set by Decision 6)

These apply to **every** v2 DTO touched by this package. Tasks reference this section instead of restating the rule each time.

- **Timestamps on the wire**: serialized as RFC 3339 with explicit `+00:00` offset (Python's `datetime.isoformat()` shape), e.g. `"2026-04-29T14:32:01.123456+00:00"`. Deserialization is lenient: both `+00:00` and the equivalent `Z` suffix are accepted, so Rust-style clients (`chrono::DateTime<Utc>` default) and Python-style clients interoperate. Implementation: a `mod iso8601_offset` helper at [`crates/http-server/src/dto/util.rs`](../../crates/http-server/src/dto/util.rs) lands in **LIB-03** (the first task that introduces a wire-visible `DateTime<Utc>` field); every `DateTime<Utc>` field across the v2 DTOs uses `#[serde(with = "crate::dto::util::iso8601_offset")]`.
- **Field name casing (Decision 10, polarity-corrected 2026-04-29)**: Python's `cognee.api.DTO.InDTO` / `OutDTO` set `alias_generator=to_camel` + `populate_by_name=True` ([`cognee/api/DTO.py:7-17`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py#L7-L17)), so **every request/response DTO field is camelCase on the wire**, with snake_case accepted as an inbound alias only. Rust convention: `#[serde(rename_all = "camelCase")]` on every body DTO; per-field `#[serde(alias = "<snake_form>")]` on every multi-word input field for compatibility with snake_case-sending clients. **Out of scope** for this rule (these stay snake_case or whatever literal Python name FastAPI uses): query parameters declared at the FastAPI function signature (`order_by: str = Query(...)`); multipart form fields declared at the function signature (`datasetName: ... = Form(...)` / `session_id: ... = Form(...)`); HTTP headers; URL path parameters. The v1 port currently has drift; CLEAN-01 (Phase 0) audits and fixes it before any v2 task runs.
- **Optional fields**: `#[serde(skip_serializing_if = "Option::is_none")]` so absent fields are omitted from the JSON, not emitted as `"field": null`. Python's `jsonable_encoder` does the same.
- **Top-level error envelopes**: `{"error": "<message>"}` for cognee's catch-all 4xx/5xx (Python parity quirk — bare except → 409); `{"detail": "<message>"}` only for FastAPI's intrinsic `HTTPException` raises (e.g. 404 from `get_session_detail`).
- **Request-validation envelope (Decision 7)**: full Python FastAPI shape, byte-for-byte. The shape applies to both body and query-parameter validation — for query params this means a new `ValidatedQuery<T>` extractor that mirrors `ValidatedJson<T>`'s envelope (axum's default `Query<T>` rejection does not match the Python shape). Status code is **`400`** (Python overrides FastAPI's default `422` globally — see [`cognee/api/client.py:166-178`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L166-L178)). Body shape:
  ```json
  {
    "detail": [{"loc": ["body", "<field>"], "msg": "...", "type": "value_error"}],
    "body":   <raw input echo OR null>
  }
  ```
  Already implemented by [`crates/http-server/src/middleware/validation.rs`](../../crates/http-server/src/middleware/validation.rs) — every v2 handler must use `crate::middleware::validation::Json<T>` (re-exported as `ValidatedJson`) instead of `axum::Json<T>`. Custom validation logic (scope normalization on `/recall`, `order_by` / `limit` on `/sessions`, missing `session_id` on `/remember/entry`, etc.) must produce the same `[{loc, msg, type}]` array — list `loc` from outermost (`"body"` or `"query"`) to the field name, set `type` to a Python-equivalent discriminator (`"value_error"` for malformed values, `"value_error.missing"` for required-field omission, etc.). Integration tests for every v2 validation path MUST assert byte-shape parity, not just `is_array()`.

### 1.2 v2 acknowledged divergences (changes to steady-state wire output)

The strict-parity rule from [`../http-server/plan.md §1`](../http-server/plan.md#1-goal) carves out "operator-side robustness improvements that don't change steady-state wire output". v2 inherits both v1 divergences listed there (pipeline-run registry eviction; graceful-shutdown `DATASET_PROCESSING_ERRORED` rows). v2 additionally introduces the following divergences — each **does** change steady-state wire output and is explicitly accepted for UX or maintainability reasons:

| # | Divergence | Affects | Rationale | Decision |
|---|---|---|---|---|
| D-1 | `GET /api/v1/sessions?order_by=<unknown>` returns **400** with the Python validation envelope. Python silently falls back to `last_activity_at` and returns 200. | E-09 | Silent fallback is a UX footgun: clients passing user-driven sort fields can't tell typos from real bugs. The 400 makes the contract explicit. The cross-SDK harness excludes this specific input from shared tests since the two backends differ here. | Decision 9 (2026-04-29) |

When proposing a new divergence, add a row here **before** landing the code; the four-agent review pipeline rejects undocumented wire-shape changes.

## 2. What "v2" means at the HTTP level

The v2 HTTP surface is the union of:

1. **The four memory endpoints** that the Python `cloud_client.py` advertises as "V2 Operations" (see [`cognee/api/v1/serve/cloud_client.py:47`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/serve/cloud_client.py#L47)):
   - `POST /api/v1/remember` (+ `POST /api/v1/remember/entry`)
   - `GET /api/v1/recall` (history) and `POST /api/v1/recall` (search)
   - `POST /api/v1/improve`
   - `POST /api/v1/forget`
2. **`POST /api/v1/visualize`** and **`GET /api/v1/visualize`** — the d3.js HTML graph visualizer that the v2 SDK's `visualize()` exposes.
3. **The `/api/v1/sessions` dashboard router** — list / stats / cost-by-model / detail. These are not v2 SDK functions but they are the only HTTP read path for session memory written by `remember()` / `recall()` / `improve()`, so v2 frontends require them.

Endpoints under `/api/v1/auth`, `/api/v1/add`, `/api/v1/cognify`, `/api/v1/search`, `/api/v1/datasets`, etc. are **out of scope** here — they are v1 HTTP, covered by [`../http-server/`](../http-server/). The v2 implementation will reuse the existing crate skeleton, `AppState`, `ApiError`, auth stack, and middleware untouched.

## 3. Tasks — implementation status

Each row links to a self-contained implementation plan under [`tasks/`](tasks/). Reflects branch `main` as of 2026-04-28 (post-P8).

### Pre-port cleanup & enablers (Phase 0)

Lands **before** Phase A. Holds (a) v1 wire-shape drift fixes surfaced during the Decision 10 audit, and (b) library/runtime enablers that downstream phases consume.

| # | Task | Scope | Status | Blocks |
|---|---|---|---|---|
| CLEAN-01 | [v1 HTTP DTO casing audit and fix (camelCase wire parity)](tasks/clean-01-v1-dto-camelcase.md) | Audit every v1 request/response DTO; flip `rename_all = "snake_case"` → `"camelCase"` and add per-field `serde(alias)` for input compatibility. Add unit + integration + OpenAPI-schema regression tests so the convention is enforced going forward. | **Done** (commit e146835) | every v2 task that adds or modifies a body/response DTO |
| LIB-06 | [Generic pipeline payload mechanism + library-side CamelCase remember status](tasks/lib-06-pipeline-payload-mechanism.md) | Lands the `PipelineWatcher::on_payload_field` event channel + DB-backed accumulator (new `pipeline_run_payload_fields` table) + `completed_at`/`elapsed_seconds()` on `PipelineRunInfo` + `run_id` on `PipelineContext` + library `RememberStatus` CamelCase serde + `RememberResult.elapsed_seconds: Option<f64>` + `RememberResult.entry_type`/`entry_id`. See task doc for the 15 implementation steps and 4 phases. Decision 15 (two-layer status convention; **no** wire divergence). Listed under Phase 0 because it must execute before E-01 / E-02 / LIB-01; categorically it's a library prerequisite (also indexed in the Library prerequisites table below for cross-reference). | **Done** (commit b39cd05) | E-01, E-02, LIB-01 |

### Library prerequisites

These six changes must land before (or alongside) the HTTP work — they are dependencies of the missing/partial endpoints below.

| # | Task | Scope | Status | Blocks |
|---|---|---|---|---|
| LIB-01 | [`remember_entry()` facade + `MemoryEntry` types](tasks/lib-01-remember-entry-facade.md) | New library function in `cognee-lib`; new `QAEntry` / `TraceEntry` / `FeedbackEntry` discriminated-union types in `cognee-models`. | **Done** (commit 0818644) | E-02 |
| LIB-02 | [`SessionManager::add_agent_trace_step` parity](tasks/lib-02-session-manager-trace-step.md) | New `SessionTraceStep` type, `SessionStore::save_trace_step` / `read_trace_steps` on all three backends (fs / redis / sea_orm), wrapper methods on `SessionManager`. SeaORM migration for `session_trace_steps`. | **Done** (commit eec6f79) | LIB-01, E-02, E-12 |
| LIB-03 | [`session_records` + `session_model_usage` schema and entities](tasks/lib-03-session-records-schema.md) | SeaORM entities + migration only. The repository trait + impl + tests live in **LIB-05** (Decision 13 split). | **Done** (commit 82728f2) | LIB-05 |
| LIB-04 | [Refactor `improve()` to `ImproveParams` struct](tasks/lib-04-improve-params-struct.md) | Mechanical refactor of `cognee_lib::api::improve::improve()`'s 18-positional-parameter signature to a single `ImproveParams<'_>` struct. 5 call sites migrate. Decision 8 — pulled out of E-05 to keep that task scoped to "DTO + handler". | **Done** (commit 9f1879e) | LIB-01, E-05 |
| LIB-05 | [`SessionLifecycleDb` trait + repository impl + tests](tasks/lib-05-session-records-repo.md) | The `SessionLifecycleDb` trait with `ensure_and_touch_session` / `accumulate_usage` / `get_session_row` / `list_session_rows` / `aggregate_stats` / `cost_by_model`, its concrete impl on `DatabaseConnection`, the effective-status SQL helper, and 8 repository tests. Second half of the original LIB-03 scope (Decision 13 split). | **Done** (commit 60c934a) | E-09, E-10, E-11, E-12 |
| LIB-06 | [Generic pipeline payload mechanism + library-side CamelCase remember status](tasks/lib-06-pipeline-payload-mechanism.md) | Four pieces: (1) extend `cognee_core::PipelineRunInfo` with `completed_at` + `elapsed_seconds()` and add `run_id` to `PipelineContext`; (2) new `PipelineWatcher::on_payload_field(...)` event hook + `TaskContext::publish_payload_field(...)` helper — payload lives in the watcher event channel, NOT as state on the snapshot; (3) DB-backed default accumulator — new `pipeline_run_payload_fields` table + `PipelineRunRepository` trait extension + `SeaOrmPipelineRunRepository` impl + `DefaultPipelineRunRegistry::get_payload(run_id)` accessor; (4) `cognee_lib::api::remember` updates: `RememberStatus` serde flip to CamelCase `PipelineRun*` strings (library-internal consistency), `From<PipelineRunStatus>`, `RememberResult.elapsed_seconds: Option<f64>`, plus `RememberResult.entry_type` / `entry_id` fields (Q-F — relieves LIB-01 of that scope). Convenience functions (`cognify`/`memify`/`add`) get explicit TODO markers — they bypass `cognee_core::execute()` today, so are out of scope. The HTTP wire keeps Python's lowercase status format; E-01 owns the lowercase translation at the DTO boundary. **No wire divergence** (Decision 15 — two-layer status convention). | **Done** (commit b39cd05) | E-01, E-02, LIB-01 |
| LIB-07 | [`recall()` scope widening](tasks/lib-07-recall-scope-widening.md) | Widen `cognee_lib::api::recall::recall()` to accept `scope: Option<Vec<RecallScope>>` and implement source fan-out across `graph` / `session` / `trace` / `graph_context`. New `RecallScope` enum + `normalize_scope()` helper + private `_search_session` / `_search_trace` / `_fetch_graph_context` helpers + 22 tests. Per **Decision 17** (2026-04-30), this was split out of E-04 so that E-04 retains strict Python parity (no D-2 wire divergence). | **Done** (commit 7d25c0b) | E-04 |
| LIB-08 | [Lift `RecallScope` + helpers from `cognee-lib` to `cognee-search`](tasks/lib-08-recall-scope-lift.md) | Architectural refactor — move `RecallScope`, `ScopeInput`, `normalize_scope`, `RecallSource` (with `Trace`/`GraphContext`), `RecallItem`, and the four source helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) from `cognee-lib::api::recall` to `crates/search/src/recall_scope.rs`. `cognee-lib::api::recall` re-exports them so its public API stays stable. Per **Decision 18** (2026-04-30), this resolves the http-server↔lib cycle the E-04 implementation-side investigation surfaced. `normalize_scope` returns `SearchError::InvalidInput` at the new location (error message string byte-identical). 14 unit tests at new location; LIB-07's 8 integration + 3 override tests still pass via re-export. No cycle. | **Done** (commit f98cac7) | E-04 |

### Endpoints

The Python source-of-truth column links to the file that defines each handler in upstream cognee.

| # | Endpoint | Python source | Status | Plan |
|---|---|---|---|---|
| E-01 | `POST /api/v1/remember` | [`get_remember_router.py:28`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L28) | **Done** (commit 037cad2) | [tasks/e-01-remember.md](tasks/e-01-remember.md) |
| E-02 | `POST /api/v1/remember/entry` | [`get_remember_router.py:115`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L115) | **Done** (commit 75c0886) | [tasks/e-02-remember-entry.md](tasks/e-02-remember-entry.md) |
| E-03 | `GET /api/v1/recall` | [`get_recall_router.py:58`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L58) | **Done** (commit 0dafdee) | [tasks/e-03-recall-history.md](tasks/e-03-recall-history.md) |
| E-04 | `POST /api/v1/recall` | [`get_recall_router.py:78`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L78) | **Done** (commit 9981e79) | [tasks/e-04-recall-search.md](tasks/e-04-recall-search.md) |
| E-05 | `POST /api/v1/improve` | [`get_improve_router.py:39`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py#L39) | **Done** (commit 43e2a72) — DTO + telemetry plumbing only; real library wire-up is the deferred P5 follow-up (cycle constraint, same as E-04). | [tasks/e-05-improve.md](tasks/e-05-improve.md) |
| E-06 | `POST /api/v1/forget` | [`get_forget_router.py:25`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py#L25) | **Done — verified, no code change** | [tasks/e-06-forget.md](tasks/e-06-forget.md) |
| E-07 | `GET /api/v1/visualize` | [`get_visualize_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py) | **Done (commit 35d6b3c)** | [tasks/e-07-visualize.md](tasks/e-07-visualize.md) |
| E-08 | `POST /api/v1/visualize/multi` | [`get_visualize_router.py:77`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L77) (mounted at `/api/v1/visualize` per [`client.py:241`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L241)) | **Done (commit afa048f, Decision 16 — Option A)** | [tasks/e-08-visualize-multi.md](tasks/e-08-visualize-multi.md) |
| E-09 | `GET /api/v1/sessions` | [`get_sessions_router.py:64`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L64) | **Missing** | [tasks/e-09-sessions-list.md](tasks/e-09-sessions-list.md) |
| E-10 | `GET /api/v1/sessions/stats` | [`get_sessions_router.py:112`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L112) | **Missing** | [tasks/e-10-sessions-stats.md](tasks/e-10-sessions-stats.md) |
| E-11 | `GET /api/v1/sessions/cost-by-model` | [`get_sessions_router.py:198`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L198) | **Missing** | [tasks/e-11-sessions-cost-by-model.md](tasks/e-11-sessions-cost-by-model.md) |
| E-12 | `GET /api/v1/sessions/{session_id}` | [`get_sessions_router.py:254`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L254) | **Missing** | [tasks/e-12-sessions-detail.md](tasks/e-12-sessions-detail.md) |

### Legend
- **Implemented** — route exists, DTO matches Python, handler delegates to the right library function, parity tests would pass. Task plan is verification-only.
- **Partial** — route registered but the DTO or behavior is missing fields/branches that exist in Python.
- **Missing** — no Rust route at the path.

### Status roll-up

| State | Cleanup | Library | Endpoints |
|---|---|---|---|
| Done | 1 (CLEAN-01) | 8 (LIB-01, LIB-02, LIB-03, LIB-04, LIB-05, LIB-06, LIB-07, LIB-08) | 8 (E-01, E-02, E-03, E-04, E-05, E-06, E-07, E-08) |
| Missing | — | — | 4 (E-09, E-10, E-11, E-12) |
| **Total** | **1** | **8** | **12** |

Grand total: **21 tasks** (1 cleanup + 8 library + 12 endpoints; LIB-07 added 2026-04-30 per Decision 17; LIB-08 added 2026-04-30 per Decision 18). **Phases A, B, C complete; Phase D in progress (1 of 5 done).** Resume point moves to **D-2 (E-09)** — `GET /sessions` (depends on LIB-05, landed). Remaining endpoints: E-09, E-10, E-11, E-12 — all `/sessions` dashboard endpoints depending on LIB-05's `SessionLifecycleDb` trait.

## 4. Summary of findings

- **5 of 12 endpoints fully Implemented** — the four v1-era endpoints (`POST /remember` file path, `GET /recall`, `POST /forget`, `GET /visualize`) plus `POST /visualize/multi` (a parity port of [`cognee/api/v1/users/routers/get_visualize_router.py:77`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L77); not a divergence).
- **2 of 12 Partial** — `POST /recall` (no `session_id` / `scope`) and `POST /improve` (no `session_ids` / `extraction_tasks` / `enrichment_tasks` / `data` / `node_name`). These two are the **highest-impact gaps**: they break the v2 session-bridge story end to end. Without them, the Rust HTTP server cannot do session-first recall and cannot trigger the four-stage `improve()` flow even though the underlying `cognee-cognify` crate already supports both.
- **5 of 12 Missing** — `POST /remember/entry` plus the four `/sessions/*` endpoints. Adding `/remember/entry` is small (one DTO + one library facade); the `/sessions` quartet is the largest single piece of work in this package because it requires net-new SeaORM schema for `session_records` + `session_model_usage` and corresponding repository methods.
- **No Rust-only divergences.** Every Rust route maps to a Python counterpart at the same wire path.

### Library prerequisites (must land before HTTP work)

The v2 HTTP gaps are **mostly thin handlers** — the substance lives in the SDK and database crates. Before the HTTP work can start:

1. `cognee-lib` needs a `remember_entry(MemoryEntry, dataset_name, session_id, user)` facade. Today `crates/lib/src/api/remember.rs` only handles the file/text path; no equivalent of the Python `remember(entry, ...)` discriminated dispatch exists.
2. `cognee-session` needs `add_qa()`, `add_agent_trace_step()`, `add_feedback()` parity. `crates/session/src/session_manager.rs` has `save_qa` / `add_feedback` / etc. but not `add_agent_trace_step` (no `TraceEntry` type yet).
3. `cognee-database` needs `session_records` and `session_model_usage` SeaORM entities and the corresponding `SessionRecordsRepository::{list_session_rows,get_session_row,stats,cost_by_model}` repository trait. Python's logic uses raw SQLAlchemy; the Rust version must produce the same wire output via SeaORM.

These are flagged in the per-endpoint gap docs that will follow this README.

### Rough effort ordering (smallest → largest)

| Rank | Endpoint(s) | Approx. effort | Notes |
|---|---|---|---|
| 1 | `POST /api/v1/recall` — add `session_id` + `scope` | 1 day | DTO + handler plumbing; `query_router` already understands session-first. Reverse the "Do NOT add session_id" guardrail. |
| 2 | `POST /api/v1/remember/entry` | 1.5 days | `MemoryEntry` discriminated DTO + `remember_entry()` library facade + `add_agent_trace_step` in `cognee-session`. |
| 3 | `POST /api/v1/improve` — add `session_ids` + extraction/enrichment/data/node_name | 2 days | DTO + handler; library already implements the 4-stage flow. |
| 4 | `GET /api/v1/sessions` + `/{id}` | 2 days | Needs `session_records` SeaORM entity + repo. |
| 5 | `GET /api/v1/sessions/stats` + `/cost-by-model` | 2 days | Needs `session_model_usage` SeaORM entity + aggregate queries. Could be batched with #4 since they share the migration. |

**Total**: ~9 engineer-days for full v2 HTTP parity, **assuming** the library prerequisites above land first (allow another ~3 days for those).

## 5. Sub-document index

The 19 task docs in [`tasks/`](tasks/) (1 cleanup + 6 library + 12 endpoint) are the authoritative implementation plans. The [implementation prompt](IMPLEMENTATION-PROMPT.md) drives execution.

| # | Document | Scope | Status |
|---|---|---|---|
| 1 | [tasks/](tasks/) | Per-task implementation plans. One file for the v1 cleanup (CLEAN-01), one per library prerequisite (LIB-01 to LIB-06), and one per endpoint (E-01 to E-12). | **Done** |
| 2 | [IMPLEMENTATION-PROMPT.md](IMPLEMENTATION-PROMPT.md) | Sequential task list (Phases 0 → A → B → C → D, 19 tasks total) + four-agent pipeline (investigation → implementation → review → doc-update) the driver model copy-pastes per task. Mirrors [`../http-server/implementation/IMPLEMENTATION-PROMPT.md`](../http-server/implementation/IMPLEMENTATION-PROMPT.md), adapted to the flatter v2 doc tree. | **Done** |
| 3 | [e2e-parity.md](e2e-parity.md) | Add v2 endpoints to the existing cross-SDK harness in `e2e-cross-sdk/`. The v1 harness (commit `2faf3ac`) is a template; v2 needs new `test_http_v2_*.py` files for the 7 partial/missing endpoints. Per-task plans already enumerate the test files; this doc would aggregate the wave plan. | **Not Started** |

Each task doc is the owner of its slice of the work; this README is the single source of truth for **what's where** at the v2 layer.

## 6. References

- **Python source-of-truth**: [`cognee/api/v1/{remember,recall,improve,forget,sessions,visualize}/routers/`](https://github.com/topoteretes/cognee/tree/main/cognee/api/v1)
- **V2 SDK gap analysis (in-process Rust API)**: [`../api-v2/README.md`](../api-v2/README.md)
- **V1 HTTP server plan (parent structure this folder mirrors)**: [`../http-server/plan.md`](../http-server/plan.md)
- **V1 HTTP routers status table**: [`../http-server/routers/README.md`](../http-server/routers/README.md)
- **Rust v2 SDK entry points already in place**: [`crates/lib/src/api/{remember,recall,improve,forget,visualize}.rs`](../../crates/lib/src/api/)
- **Cloud client that consumes this surface**: [`cognee/api/v1/serve/cloud_client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/serve/cloud_client.py)
- **Project guide**: [`../../.claude/CLAUDE.md`](../../.claude/CLAUDE.md)
