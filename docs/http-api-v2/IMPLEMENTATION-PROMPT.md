# Implementation Driver Prompt — cognee-rust HTTP API v2 port

> **Read this first.** This document is the entry-point prompt for the model executing the cognee-rust HTTP API v2 port (the memory-oriented HTTP surface: `remember` / `recall` / `improve` / `forget` / `visualize` / `sessions`). It defines the task list, the per-task four-agent pipeline, the sub-agent prompts you will copy-paste, and the conventions for commits / verification / status tracking.

## Fresh-session orientation (read in this order)

If you are starting a clean session, read these documents before doing anything:

1. [`README.md`](README.md) — package overview, what "v2" means at the HTTP level, drift inventory.
2. [`README.md §1.1 Wire conventions`](README.md#11-wire-conventions-project-wide-set-by-decision-6) — timestamp / casing / envelope / validation rules. **Every** v2 task obeys these.
3. [`README.md §1.2 v2 acknowledged divergences`](README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output) — the one Rust↔Python wire divergence (`order_by` validation on `/sessions`).
4. **§0** of this file — current resume point + phase table.
5. **§0.5 Decisions log** of this file — index of every project-wide decision (Decisions 1 through 15) with a one-line summary and where it's canonically recorded. Decision 15 (the two-layer status convention for `/remember`) is the most recent and is owned by **LIB-06**.
6. The task doc for the resume point in §0 — start your work there.

## 0. Current state

**14 tasks complete (CLEAN-01, LIB-06, E-01, E-03, E-06, E-07, E-08, LIB-02, LIB-03, LIB-05, LIB-04, LIB-01, LIB-07, LIB-08). Resume point: TASK C-1 (E-04) — Phase B fully complete (8 of 8 library prerequisites done); entering Phase C.**

The v2 doc package landed in commits ending `…/docs/http-api-v2/`. The port has **21 tasks** total (Decision 17 added LIB-07; Decision 18 added LIB-08). 7 of 21 tasks are at status **Not Started** (1 cleanup + 8 library prerequisites + 5 endpoints done; 7 endpoints remain; CLEAN-01 e146835, LIB-06 b39cd05, E-01 037cad2, E-03 0dafdee, E-06 verified-no-code-change, E-07 35d6b3c, E-08 afa048f, LIB-02 eec6f79, LIB-03 82728f2, LIB-05 60c934a, LIB-04 9f1879e, LIB-01 0818644, LIB-07 7d25c0b, LIB-08 f98cac7).

Phases and their tasks (do them in this order — see §2 for the dependency rationale):

| Phase | ID | Task | Notes |
|---|---|---|---|
| **0 — Pre-port cleanup & enablers** | 0-1 | [CLEAN-01](tasks/clean-01-v1-dto-camelcase.md) | **Done** (commit e146835) — Fix v1 HTTP DTO casing drift (snake_case → camelCase wire parity). Adds an OpenAPI-schema regression test that prevents future drift. Decision 10. |
| **0 — Pre-port cleanup & enablers** | 0-2 | [LIB-06](tasks/lib-06-pipeline-payload-mechanism.md) | **Done (commit b39cd05).** Generic pipeline payload event channel via `PipelineWatcher::on_payload_field`, DB-backed accumulator (new `pipeline_run_payload_fields` table + repo trait extension + SeaORM impl + registry accessor), `completed_at`/`elapsed_seconds()` on `PipelineRunInfo`, `run_id` on `PipelineContext`, library `RememberStatus` flip to CamelCase + `From<PipelineRunStatus>` + `Started` variant, `RememberResult.elapsed_seconds: Option<f64>`, `RememberResult.entry_type`/`entry_id`. Convenience-function TODOs note that `cognify`/`memify`/`add` bypass `execute()` today and are deferred. HTTP wire keeps Python's lowercase status (E-01 translates). Decision 15 — **no** wire divergence. Must land before E-01 / E-02 / LIB-01. |
| **A — Verify** | A-1 | [E-01](tasks/e-01-remember.md) | `POST /remember` — **Done (commit 037cad2).** Brought `RememberResultDTO` to byte-for-byte parity with Python's `RememberResult.to_dict()` (added `items_processed`/`elapsed_seconds`/`session_ids`/`content_hash`/`items`; flipped `dataset_id`/`pipeline_run_id` to `Option<Uuid>`); introduced `WireRememberStatus` standalone wire enum that emits Python's lowercase strings (Decision 15). The `From<cognee_lib::api::remember::RememberStatus>` impl is deferred to the P5 wiring task (cycle constraint). |
|   | A-2 | [E-03](tasks/e-03-recall-history.md) | `GET /recall` — **Done (commit 0dafdee).** Decision 6 polish — landed the project-wide `iso8601_offset` serde helper at `crates/http-server/src/dto/util.rs` (5 unit tests) and applied it to `SearchHistoryItemDTO::created_at` (shared between `GET /search` and `GET /recall`). Cross-SDK parity test `e2e-cross-sdk/harness/test_http_v2_recall_history.py` asserts byte equality on `createdAt`. |
|   | A-3 | [E-06](tasks/e-06-forget.md) | `POST /forget` — **Done — verified, no code change.** Investigation 2026-04-29: zero divergences vs Python `cognee/api/v1/forget/routers/get_forget_router.py`; existing cross-SDK harness `e2e-cross-sdk/harness/test_http_forget.py` already covers all three modes + non-existent. Verify-only short-circuit per §0 Lessons #3. |
|   | A-4 | [E-07](tasks/e-07-visualize.md) | `GET /visualize` — **Done (commit 35d6b3c).** Cross-SDK harness rewrite for Decision 11: replaced stale `<!--JSON_ISLAND_START/END-->` greps with the seven-`__*_DATA__` extraction strategy. No code change in `crates/`; harness now structurally diffs the seven JS-variable JSON payloads (nodes/links/schema + four color maps) with stable sort, reverses Python's `</` → `<\/` escape before `json.loads`, and includes a negative test asserting the harness detects real graph differences. |
|   | A-5 | [E-08](tasks/e-08-visualize-multi.md) | `POST /visualize/multi` — **Done (commit afa048f, Decision 16 — Option A convergence).** |
| **B — Library prerequisites** | B-1 | [LIB-02](tasks/lib-02-session-manager-trace-step.md) | `add_agent_trace_step` (independent) — **Done (commit eec6f79).** |
|   | B-2 | [LIB-03](tasks/lib-03-session-records-schema.md) | **Done (commit 82728f2).** `session_records` + `session_model_usage` schema + entities + migration (Decision 13 — first half of the original LIB-03 scope). |
|   | B-3 | [LIB-05](tasks/lib-05-session-records-repo.md) | **Done (commit 60c934a).** `SessionLifecycleDb` trait + `DatabaseConnection` impl + 8 repository tests (Decision 13 — second half; depends on LIB-03). Read-time effective-status helper translates Python's runtime abandon-threshold (default 1800s, Decision 12). |
|   | B-4 | [LIB-04](tasks/lib-04-improve-params-struct.md) | **Done (commit 9f1879e).** Refactored `improve()` to take an `ImproveParams<'_>` struct (18 fields, no `Default` derive); migrated all 5 call sites (`remember.rs::self_improvement` + 2 in `improve_e2e.rs` + 2 in `improve_sync_only.rs`); removed `#[allow(clippy::too_many_arguments)]`. Wire shape unchanged. |
|   | B-5 | [LIB-01](tasks/lib-01-remember-entry-facade.md) | **Done (commit 0818644).** `remember_entry()` facade dispatches typed memory entries (QA / Trace / Feedback) to `SessionManager`. Per Decision 2, `MemoryEntry` types live in `cognee-models`. Best-effort pre-upsert via `SessionLifecycleDb::ensure_and_touch_session` (log-and-swallow). Populates `RememberResult.entry_type`/`entry_id` for all branches per Decision 5. 6 integration tests + 4 model round-trip tests. `generate_feedback_with_llm` deferred as TODO (LLM-handle plumbing out of scope). |
|   | B-6 | [LIB-07](tasks/lib-07-recall-scope-widening.md) | **Done (commit 7d25c0b).** Widened `cognee_lib::api::recall::recall()` with `scope: Option<Vec<RecallScope>>` + `session_manager: Option<&SessionManager>` parameters and four-source fan-out (`graph` / `session` / `trace` / `graph_context`). New `RecallScope` enum + `normalize_scope()` helper (Python-byte-exact error message) + `RecallSource` extended with `Trace` / `GraphContext`. `_fetch_graph_context` reads `SessionManager::get_graph_context` snapshot (NOT a graph-DB walk). `auto` resolution mirrors Python `recall.py:374-386` with `auto_fallthrough` short-circuit. 14 unit tests + 8 integration tests. Per **Decision 17** (2026-04-30), this was split out of E-04 so that E-04 retains strict Python parity (no D-2 wire divergence). |
|   | B-7 | [LIB-08](tasks/lib-08-recall-scope-lift.md) | **Done (commit f98cac7).** Lifted `RecallScope`, `ScopeInput`, `normalize_scope`, `RecallSource` (with `Trace`/`GraphContext`), `RecallItem`, and the four source helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) from `cognee-lib::api::recall` to `crates/search/src/recall_scope.rs`. `cognee-lib::api::recall::*` re-exports preserve the public API. Error type pivoted from `ApiError::InvalidArgument` → `SearchError::InvalidInput` (error message string byte-identical to LIB-07); added `From<SessionError> for SearchError`. `recall()` body now wraps with `map_err(|e| ApiError::Search(e.to_string()))` at helper call sites. 14 unit tests now pass at the new location; LIB-07's 8 integration + 3 override tests still pass via re-export. No cycle (`cargo tree -p cognee-search | grep cognee-lib` empty). Per **Decision 18** (2026-04-30). Pure relocation — no behavior change. |
| **C — Partial endpoints** | C-1 | [E-04](tasks/e-04-recall-search.md) | `POST /recall` — add `session_id` + `scope` (depends on **B-7 LIB-08** for accessibility from http-server; LIB-07 + LIB-08 together provide the four-source fan-out + types reachable from the HTTP layer). |
|   | C-2 | [E-05](tasks/e-05-improve.md) | `POST /improve` — add `session_ids` + extraction/enrichment/data/node_name (consumes B-4's `ImproveParams`) |
| **D — Missing endpoints** | D-1 | [E-02](tasks/e-02-remember-entry.md) | `POST /remember/entry` (depends on B-5) |
|   | D-2 | [E-09](tasks/e-09-sessions-list.md) | `GET /sessions` (depends on B-3) |
|   | D-3 | [E-10](tasks/e-10-sessions-stats.md) | `GET /sessions/stats` (depends on B-3) |
|   | D-4 | [E-11](tasks/e-11-sessions-cost-by-model.md) | `GET /sessions/cost-by-model` (depends on B-3) |
|   | D-5 | [E-12](tasks/e-12-sessions-detail.md) | `GET /sessions/{id}` (depends on B-1 + B-3) |

**Latest commit on branch:** check with `git log --oneline -1` before starting.

### Key architectural facts established before this work begins

- The HTTP server crate `crates/http-server/` is already in place from the v1 port (P0 commit `323e3e1`). The v2 work **does NOT** introduce a new crate — it extends `crates/http-server/src/routers/` and `crates/http-server/src/dto/`.
- `crates/http-server/` does **not** depend on `cognee-lib` (avoids a cycle); handlers reach component crates directly via `ComponentHandles` on `AppState`. New library functions (LIB-01) live in `cognee-lib` and are called from handlers via the existing `state.lib` (or equivalent) accessor.
- `cargo check --all-targets` is the ground truth for compilation. **rust-analyzer shows many false-positive errors** (feature-gated files appear "unlinked"; `bin`-feature-gated `main.rs` shows errors when the feature is off). Always use `cargo check`, not rust-analyzer output.
- `scripts/check_all.sh` pre-existing failures (JS binding test under ts-jest, CLI E2E tests requiring `OPENAI_TOKEN`) are unrelated to v2 work and safe to ignore in the review checklist.

### Lessons learned from the v1 port — apply to every v2 task

1. **Review agent `--amend` pitfall**: if the implementation agent leaves uncommitted changes and the review agent runs `git commit --amend --no-edit`, it amends the **previous** commit (not a new one), folding unrelated changes in. To prevent this: the implementation agent MUST always `git add <files> && git commit -m "..."` as its last step. If changes exist but were not committed, the review agent should create a new commit (with the correct message), not amend.
2. **One commit per task**: the implementation agent commits; the review agent only amends that commit (never creates a new one); the doc-update agent creates one small docs-only commit. Three agents, at most two commits per task.
3. **Verify-only tasks often produce zero-line code commits**. The investigation agent may report READY with a step list of "add cross-SDK parity test" only. That's fine — the implementation commit is the new test file plus any test-data fixtures, nothing in `crates/http-server/src/`. If the diff is genuinely empty after running tests, mark the task done in the README without a commit (note in the README row `(verified, no code change)`). Pure verify-only tasks in this port: **A-1 (E-01), A-3 (E-06), A-4 (E-07), A-5 (E-08)**. **A-2 (E-03) is NOT pure verify-only** — it owns the `iso8601_offset` serde helper module per Decision 6, so it always lands a real code commit.

---

## 0.5 Decisions log

Every project-wide decision is recorded in a "Decision (2026-04-29) — Decision N" note in either [`README.md`](README.md) (cross-cutting conventions) or the relevant task doc (task-specific). Each note includes a "**Investigation agent: do not re-litigate**" line that the four-agent pipeline must honor.

| # | Decision | Outcome | Recorded in |
|---|---|---|---|
| 1 | E-08 visualize/multi: divergence or parity? | Parity port (Python's router lives at `users/routers/` but mounts at `/api/v1/visualize`). | [tasks/e-08-visualize-multi.md](tasks/e-08-visualize-multi.md) header note |
| 2 | LIB-01 `MemoryEntry` types crate placement | `cognee-models` (not a new `cognee-memory` crate). | [tasks/lib-01-remember-entry-facade.md §4](tasks/lib-01-remember-entry-facade.md#4-implementation-steps) |
| 3 | LIB-03 `SessionLifecycleDb` trait crate placement | `cognee-database` (not `cognee-session`). | [tasks/lib-03-session-records-schema.md](tasks/lib-03-session-records-schema.md) header |
| 4 | Branch policy | Direct-to-`main`, no feature branches. | §8.1 below |
| 5 | When to add `entry_type` / `entry_id` to `RememberResultDTO` | E-02 owns the structural change; E-01 stays verify-only. | [tasks/e-02-remember-entry.md §4](tasks/e-02-remember-entry.md#4-implementation-steps) + [tasks/e-01-remember.md §5](tasks/e-01-remember.md) |
| 6 | Timestamp serialization parity | Custom `iso8601_offset` serde helper (emit `+00:00`, accept either `+00:00` or `Z`). Helper module owned by **E-03**; every `DateTime<Utc>` field across v2 DTOs uses `#[serde(with = "crate::dto::util::iso8601_offset")]`. | [README.md §1.1](README.md#11-wire-conventions-project-wide-set-by-decision-6); helper landed in [tasks/e-03-recall-history.md §5](tasks/e-03-recall-history.md) |
| 7 | Validation envelope shape | Full Python FastAPI shape (`{"detail": [{"loc","msg","type"}], "body": <raw>}`); status code **`400`** (Python overrides FastAPI's default 422 globally). Already implemented in v1's `ValidatedJson`. Every v2 validation path needs a byte-shape integration test. | [README.md §1.1](README.md#11-wire-conventions-project-wide-set-by-decision-6) |
| 8 | Refactor `improve()` to `Params` struct | New task **LIB-04** (separate from E-05). | [tasks/lib-04-improve-params-struct.md](tasks/lib-04-improve-params-struct.md) |
| 9 | `order_by=invalid` parity (sessions) | Return `400` with the Python validation envelope (Rust-only divergence vs Python's silent fallback). Recorded as **divergence D-1**. Implementation needs a new `ValidatedQuery<T>` extractor sibling to `ValidatedJson<T>`, owned by **E-09**. | [README.md §1.2](README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output); helper landed in [tasks/e-09-sessions-list.md §4 step 4](tasks/e-09-sessions-list.md) |
| 10 | DTO field casing convention | **camelCase** on the wire (Python's `to_camel` alias generator + `populate_by_name=True`). v1 has drift; **CLEAN-01** (Phase 0) audits and fixes it. Every body/response DTO uses `#[serde(rename_all = "camelCase")]`; multi-word input fields add `#[serde(alias = "<snake_form>")]`. Out-of-scope: query params, multipart form, headers, paths. | [README.md §1.1](README.md#11-wire-conventions-project-wide-set-by-decision-6); enforced by [tasks/clean-01-v1-dto-camelcase.md](tasks/clean-01-v1-dto-camelcase.md) and its OpenAPI-schema regression test |
| 11 | Visualize HTML byte-parity strategy | Template-extracted JSON equality — regex out the seven `__*_DATA__` JS variable substitutions, structural-diff with stable sort. Bundle hash / CDN URL / theme / layout out of scope. | [tasks/e-07-visualize.md §4](tasks/e-07-visualize.md) and [tasks/e-08-visualize-multi.md §4](tasks/e-08-visualize-multi.md) |
| 12 | Session abandon-threshold default | `1800` seconds (30 min). Env var `SESSION_ABANDON_AFTER_SECONDS` (no `COGNEE_` prefix). Verified against [`cognee/modules/session_lifecycle/metrics.py:47-52`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/metrics.py#L47-L52). Consumed by LIB-05's effective-status helper (the threshold is applied at read time, not schema time, so LIB-03 does not reference it). | [tasks/lib-05-session-records-repo.md](tasks/lib-05-session-records-repo.md) |
| 13 | LIB-03 commit count | Split into two tasks: **LIB-03** (schema/entities/migration only) and **LIB-05** (`SessionLifecycleDb` trait + impl + 8 tests). One commit per task. | [tasks/lib-03-session-records-schema.md](tasks/lib-03-session-records-schema.md) header + [tasks/lib-05-session-records-repo.md](tasks/lib-05-session-records-repo.md) header |
| 14 | Commit prefix | `http-api-v2:` for all 19 commits including CLEAN-01 and LIB-06. | §8.2 below |
| 15 | Two-layer status convention: library CamelCase, HTTP wire lowercase | `cognee_lib::api::remember::RememberStatus` emits CamelCase strings (`"PipelineRunStarted"` / `"PipelineRunCompleted"` / `"PipelineRunErrored"` / `"SessionStored"`) for **internal consistency** with the other four Rust pipeline APIs. The HTTP `/remember` and `/remember/entry` routers **temporarily translate** to Python's lowercase wire format (`"running"` / `"completed"` / `"errored"` / `"session_stored"`) to preserve strict Python wire parity. Translation lives at the HTTP DTO boundary (`crates/http-server/src/dto/remember.rs`), not inside the library. **No new wire divergence is introduced** — D-2 is intentionally NOT created. The translation is "temporary" in the sense that it's a transitional adapter; if Python ever switches its remember status enum to CamelCase, the translation simply goes away. Owned by **LIB-06** (lib enum) + **E-01** (HTTP translation). | [tasks/lib-06-pipeline-payload-mechanism.md](tasks/lib-06-pipeline-payload-mechanism.md) §4 step 4; [tasks/e-01-remember.md §4 step 3](tasks/e-01-remember.md#4-implementation-steps-revised-by-2026-04-29-investigation) |
| 16 | E-08 visualize/multi: convergence vs accept-as-divergence | **Option A (convergence)** chosen by user 2026-04-29. Rust's `cognee_visualization::render_multi_user` will be modified to (a) deduplicate nodes by `str(node_id)` first-write-wins (Python `cognee_network_visualization.py:142`), (b) deduplicate edges by `(source, target, relation)` (Python L150-155), and (c) accept a human-readable `user_label: String` (caller resolves `user.email.unwrap_or_else(|| user.id.to_string())`) instead of a stringified UUID. The HTTP handler at `routers/visualize.rs::post_visualize_multi` resolves each `pair.user_id` to a `User` row before calling `render_multi_user`. **No new wire divergence** — converges Rust to Python on the seven `__*_DATA__` payloads. E-08 is no longer pure verify-only; it lands a real library + handler commit. Investigation agent: do not re-litigate. | [tasks/e-08-visualize-multi.md §3.1, §4](tasks/e-08-visualize-multi.md) |
| 17 | E-04 recall/scope: split into LIB-07 + E-04 | **Option B (lib widening)** chosen by user 2026-04-30. Investigation found that `cognee_lib::api::recall::recall()` accepts only `session_id` + `auto_route` and has no `scope` parameter, no `RecallScope` enum, no `_search_trace` / `_fetch_graph_context` helpers. Honoring v2's `scope` ∈ {trace, graph_context, all} requires library widening. Rather than add a Rust-only D-2 wire divergence (Option A — DTO + handler reject those scopes with 409), the work is split: a new **LIB-07** prerequisite widens `recall()` with the `scope` parameter + four-source fan-out + helpers + tests; **E-04** then plumbs the HTTP DTO straight through to the widened library and retains strict Python parity. **No new wire divergence**. Investigation agent: do not re-litigate. | [tasks/lib-07-recall-scope-widening.md](tasks/lib-07-recall-scope-widening.md) header + [tasks/e-04-recall-search.md §3 "Library scope of E-04"](tasks/e-04-recall-search.md) |
| 18 | E-04 cycle resolution: lift LIB-07 primitives to a lower-level crate | **Option α (lift into `cognee-search`)** chosen by user 2026-04-30. The E-04 implementation-side investigation surfaced that `cognee-http-server` cannot import from `cognee-lib` (cycle constraint at `crates/http-server/Cargo.toml:35-37`, same constraint that forced E-01's standalone `WireRememberStatus` enum). LIB-07 placed `RecallScope` / `normalize_scope` / source helpers in `cognee-lib` per the Decision 17 spec, but those locations are unreachable from the HTTP layer. Three options surfaced (α: lift into `cognee-search` so http-server can import directly; β: replicate in `cognee-http-server` — E-01's pattern; γ: add `RecallProvider` trait + DI). User chose **α** because the primitives are search-routing concerns that semantically belong with `SearchOrchestrator` and α preserves LIB-07's investment without code duplication. New task **LIB-08** (B-7) lifts the types + helpers; `cognee-lib::api::recall` re-exports them so its public API stays stable. **No behavior change**, **no new wire divergence**. Investigation agent: do not re-litigate; if the destination crate (`cognee-search`) turns out to need an extra `cognee-session` dep that creates other issues, document and pick the alternative. | [tasks/lib-08-recall-scope-lift.md](tasks/lib-08-recall-scope-lift.md) header + [tasks/e-04-recall-search.md](tasks/e-04-recall-search.md) (cycle constraint discussion) |

### Project-wide infrastructure introduced by this port

| Module / Type | Owned by | Consumed by |
|---|---|---|
| `crates/http-server/src/dto/util.rs::iso8601_offset` (serde helper for `DateTime<Utc>`) | E-03 (Decision 6; landed 0dafdee) | every later task with a `DateTime<Utc>` wire field — E-09's `SessionRowDTO`, E-10's `SessionStatsDTO`, E-12's detail response |
| `crates/http-server/src/middleware/validation.rs::Query` (`ValidatedQuery<T>` query-param extractor with the Python validation envelope) | E-09 (Decision 9) | every later task with query-param validation needs |
| OpenAPI camelCase regression test | CLEAN-01 (Decision 10) | every task that adds a DTO — the test prevents new snake_case fields from landing |
| `cognee_core::PipelineRunInfo.completed_at: Option<DateTime<Utc>>` + `elapsed_seconds() -> Option<f64>` accessor | LIB-06 (Decision 15) | every consumer that wants wall-clock duration without re-tracking |
| `cognee_core::PipelineContext.run_id: Option<Uuid>` (set by `execute()` so tasks can attribute payload events) | LIB-06 (Q-I) | `TaskContext::publish_payload_field` and any future per-run-attribution code |
| `cognee_core::PipelineWatcher::on_payload_field(run_id, key, value)` watcher event hook (default no-op) + `TaskContext::publish_payload_field(...)` helper | LIB-06 (Q-G/Q-J, Decision 15) | tasks running inside `cognee_core::execute()` that need to attach run-scoped metadata; future P5 wiring of real `remember()` HTTP execution |
| `cognee_database::PipelineRunRepository::set_payload_field` / `get_payload` trait methods + `pipeline_run_payload_fields` table + SeaORM entity | LIB-06 (Q-H) | `DefaultPipelineRunRegistry`'s `ScopedRunWatcher` (persists payload), `get_payload(run_id)` accessor, future consumers reading accumulated payload |
| `cognee_lib::api::remember::RememberStatus` CamelCase serde (emits `"PipelineRunStarted"` / `"PipelineRunCompleted"` / `"PipelineRunErrored"` / `"SessionStored"` for library-internal consistency) + `From<cognee_core::pipeline::PipelineRunStatus>` | LIB-06 (Decision 15) | E-01's HTTP translation layer, E-02's `/remember/entry` response, LIB-01's `remember_entry()` facade |
| `cognee_lib::api::remember::RememberResult.entry_type` / `entry_id` fields | LIB-06 (Q-F) | LIB-01 (populates them for typed-entry path), E-02 (HTTP DTO wiring) |
| `crates/http-server/src/dto/remember.rs::WireRememberStatus` (typed lowercase wire enum; standalone — no `From<cognee_lib::api::remember::RememberStatus>` impl, deferred to P5 per the http-server↔lib cycle constraint discovered in [tasks/e-01-remember.md §3](tasks/e-01-remember.md#3-current-rust-state)) | E-01 (Q-E, Decision 15; landed commit 037cad2) | E-02 (`/remember/entry` reuses the same DTO + translation) |
| `cognee_models::memory::{MemoryEntry, QAEntry, TraceEntry, FeedbackEntry}` types (tagged enum with `type` discriminator emitting `"qa"`/`"trace"`/`"feedback"`; camelCase wire fields with snake_case `serde(alias)` per Decision 10) + `cognee_lib::api::remember::remember_entry()` facade dispatching to `SessionManager::{save_qa, update_qa, add_agent_trace_step, add_feedback}` | LIB-01 (Decision 2; landed 0818644) | E-02 (`POST /remember/entry`) |
| `cognee_search::recall_scope::{RecallScope, normalize_scope, ScopeInput, RecallSource, RecallItem}` + four `pub` source helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) + `From<SessionError> for SearchError` impl + widened `cognee_lib::api::recall::recall()` signature with `scope: Option<Vec<RecallScope>>` and `session_manager: Option<&SessionManager>` + four-source fan-out with Python-parity `auto_fallthrough` short-circuit (Python `recall.py:374-386, 508-509`); `cognee-lib` re-exports the types via `pub use cognee_search::recall_scope::*` so its public API stays stable | LIB-07 (Decision 17; landed 7d25c0b) → LIB-08 lift (Decision 18; landed f98cac7) | E-04 (`POST /recall` adds `session_id` + `scope`; can `use cognee_search::recall_scope::*` directly without crossing the cycle) |
| `cognee_lib::api::improve::ImproveParams<'_>` struct (18 fields, no `Default` derive; `cognify_config` and `add_pipeline` borrow lifetimes preserved) | LIB-04 (Decision 8; landed 9f1879e) | LIB-01 (touches one of the call sites), E-05 (adds 3 v2 fields) |
| `cognee_session::types::SessionTraceStep` (persisted shape — no `created_at`, includes `session_feedback`) | LIB-02 (landed eec6f79) | LIB-01 (`remember_entry()` typed-trace path), E-02, E-12 (`/sessions/{id}` returns trace tail) |
| `cognee_session::session_store::SessionStore::{save_trace_step, read_trace_steps}` trait methods (default impls return `SessionError::StoreError`; fs / redis (RPUSH) / sea_orm backends override) + `m20260429_000003_session_trace_steps` migration + `SessionTraceStepEntity` SeaORM entity | LIB-02 (landed eec6f79) | LIB-01, E-02, E-12 |
| `cognee_session::session_manager::SessionManager::{add_agent_trace_step, get_agent_trace_session}` wrappers (server-generated UUID4 `trace_id`; `last_n` tail-truncate) | LIB-02 (landed eec6f79) | LIB-01 (typed-trace persistence), E-12 (detail endpoint reads back via `get_agent_trace_session`) |
| `cognee_database::entities::{session_record, session_model_usage}` SeaORM entities (composite PKs, `to_dict()` helpers with Python field-order parity via `serde_json/preserve_order`) + `m20260501_000003_session_records` migration (creates both tables + four named indexes `ix_session_records_{user_id,dataset_id,last_activity_at,status}`) | LIB-03 (landed 82728f2) | LIB-05 (`SessionLifecycleDb` trait + impl), E-09, E-10, E-11, E-12 |
| `cognee_database::SessionLifecycleDb` trait (6 async methods: `ensure_and_touch_session`, `accumulate_usage`, `get_session_row`, `list_session_rows`, `aggregate_stats`, `cost_by_model`) + `DatabaseConnection` impl + read-time `effective_status_expr` helper (`now() - SESSION_ABANDON_AFTER_SECONDS` bound as SQL parameter, default 1800s per Decision 12) + 5 domain types (`SessionListFilters`, `SessionListPage` with `has_more()`, `SessionRowWithStatus` with `to_dict()`, `SessionStats`, `CostByModelRow`) + 8 repo tests | LIB-05 (Decisions 3, 12, 13; landed 60c934a) | E-09 (`GET /sessions`), E-10 (`/sessions/stats`), E-11 (`/sessions/cost-by-model`), E-12 (`/sessions/{id}`) |

When a later task's investigation agent finds the upstream module missing, it reports **BLOCKED** and names the prerequisite task — never re-implements it locally.

---

You are implementing the cognee-rust HTTP API v2 surface (`crates/http-server/src/routers/{remember,recall,improve,forget,visualize,sessions}.rs`, related DTOs, six library/database changes, one pre-port v1 cleanup, and cross-SDK parity tests) by working sequentially through 19 implementation tasks. The task docs in [`tasks/`](tasks/) own the **what** and **why**; this prompt owns the **how to drive each task to completion**. Never skip a task and never run two in parallel.

## 1. Mission

Bring the Rust cognee HTTP server to byte-for-byte parity with Python's v2 memory-oriented HTTP surface ([`cognee/api/v1/{remember,recall,improve,forget,sessions,visualize}/routers/`](https://github.com/topoteretes/cognee/tree/main/cognee/api/v1)) so v2-aware HTTP clients (cognee-frontend Memory pages, cognee-mcp memory tools, cognee Cloud client, third-party agent integrations) work unchanged against either backend.

The strict-Python-parity rule from [`../http-server/plan.md §1`](../http-server/plan.md#1-goal) applies. Two acknowledged divergences inherited from the v1 work (pipeline-run registry eviction; graceful-shutdown error rows) carry over as-is. **No new Rust-side improvements are allowed**, including any new endpoints, additional DTO fields, or "while we're here" cleanups not authorized by a task doc.

There are no Rust-only divergences in the v2 surface. Every Rust route maps to a Python counterpart at the same wire path.

## 2. Task list

Execute the 19 tasks in the order listed in §0. **Do not skip ahead, do not reorder, do not run two tasks in parallel.**

### Why this order?

Phases 0 → A → B → C → D are arranged so each task's dependencies are satisfied by an earlier task:

- **Phase 0** lands two cross-cutting enablers that downstream phases depend on:
  - **0-1 CLEAN-01** — v1 HTTP DTO casing cleanup. Must run before any v2 task that adds or modifies a body/response DTO, otherwise the camelCase convention can't be applied consistently. Surfaced by the Decision 10 audit: Python's `InDTO`/`OutDTO` emit camelCase via `to_camel` alias generator; v1 has drift. Lands the OpenAPI-schema regression test that locks in the convention forever.
  - **0-2 LIB-06** — generic pipeline payload mechanism + library-side `RememberStatus` CamelCase serde + DB-backed payload accumulator. Must run before E-01 / E-02 / LIB-01 — they all consume the new types. Decision 15 records the two-layer status convention (library CamelCase, HTTP wire lowercase translated by E-01).
- **Phase A** verifies the existing implemented endpoints first. Catches any regressions or undocumented divergences **before** new code lands and makes them harder to spot. A-2 (E-03) additionally owns the project-wide `iso8601_offset` serde helper introduced by Decision 6 — every later task that ships a `DateTime<Utc>` field reuses it.
- **Phase B** lands the five long-form library/database prerequisites (LIB-01..LIB-05). LIB-06 lands earlier in Phase 0 because E-01 / E-02 in Phase A depend on it. Endpoints that depend on them (D-1, D-2..D-5) and Phase C (E-05) are blocked until B is done. Within B:
  - B-1 (LIB-02 — trace step) is independent.
  - B-2 (LIB-03 — session_records schema + entities + migration) is independent.
  - B-3 (LIB-05 — session_records repository) depends on B-2 (Decision 13 split: schema lands first, repo second; LIB-03 and LIB-05 are kept adjacent so the work flows as one chunk).
  - B-4 (LIB-04 — `ImproveParams` refactor) is independent of B-1/B-2/B-3 but must run **before** B-5 (LIB-01 modifies one of LIB-04's call sites) and before C-2 (which adds new `ImproveParams` fields).
  - B-5 (LIB-01 — `remember_entry()` facade) needs `add_agent_trace_step` from B-1 and the new `ImproveParams` shape from B-4.
- **Phase C** modifies two existing handler DTOs. C-1 (E-04) is independent of Phase B. C-2 (E-05) consumes the `ImproveParams` struct from B-4 — adding the three new v2 fields becomes a single struct-field addition + handler wiring instead of a 20-positional-arg signature change.
- **Phase D** lands the five new endpoints. Each depends on Phase B; D-1 (E-02) on B-5 (LIB-01) for the `remember_entry()` facade; D-2..D-5 on B-3 (LIB-05) for the repository methods; D-5 (E-12) additionally on B-1 (LIB-02) for `get_agent_trace_session` and on D-2 (E-09) for the shared `SessionRowDTO`/router-mount.

### Status tracking

The single source of truth for **status** is the two tables in [README.md §3](README.md#3-tasks--implementation-status) (Library prerequisites table + Endpoints table) plus the roll-up below them. After every task the doc-update agent flips the row from `Not Started` → `In Progress` → `Done` and updates the roll-up counts.

## 3. Per-task pipeline overview

For **each** task above, you run four sub-agents in sequence:

```
Step 1 (Investigation) ──► Step 2 (Implementation) ──► Step 3 (Review) ──► Step 4 (Doc Update)
        │                          │                            │                       │
        ▼                          ▼                            ▼                       ▼
   Updates docs               Commits work               Amends commit if          Updates status
   to actualize               with meaningful            review finds              tables in
   the task spec              message                    issues                    README.md
```

The agents are **sequential** — Step 2 reads what Step 1 produced; Step 3 reads what Step 2 committed; Step 4 reads what Step 3 settled. Never run them in parallel.

Below in §4–§7 are the four sub-agent prompts. Wherever you see `${TASK_DOC}` (e.g. `e-02-remember-entry.md`) substitute the actual filename for the current task. Wherever you see `${TASK_ID}` substitute the task identifier (`A-1`, `B-3`, `D-5`, etc.) and `${TASK_REF}` the v2 reference (`E-01`, `LIB-03`, `E-12`, etc., per the README tables).

## 4. Sub-agent: Investigation

**Purpose**: confirm the task is still applicable, that no parts have already been implemented (so we don't redo them), and that the docs accurately describe the current codebase state. Update docs at every level where reality has drifted.

Spawn this agent (use `subagent_type: general-purpose`):

```
You are the investigation agent for ${TASK_ID} (${TASK_REF}) of the cognee-rust HTTP API v2 port.

Working dir: /home/dmytro/dev/cognee/cognee-rust

Your task doc: docs/http-api-v2/tasks/${TASK_DOC}

**Steps**:

1. **Orient yourself** by reading these in order:
   - `docs/http-api-v2/README.md` — package overview.
   - `docs/http-api-v2/README.md §1.1 Wire conventions` — project-wide DTO rules (camelCase, timestamps, validation envelope).
   - `docs/http-api-v2/README.md §1.2 v2 acknowledged divergences` — the one Rust↔Python wire divergence (D-1).
   - `docs/http-api-v2/IMPLEMENTATION-PROMPT.md §0` and **§0.5 Decisions log** — the index of every project-wide decision.
   - The task doc at `docs/http-api-v2/tasks/${TASK_DOC}` end-to-end. Note every file path it claims will be created/modified, every function/struct/trait name it cites, every test file it mandates, every dependency it declares.

2. **Honor the decision notes.** Every "Decision (2026-04-29) — Decision N" block in a task doc carries an explicit "Investigation agent: do not re-litigate" line. If you find the decision is technically incorrect (e.g. a Python file path it cites no longer exists), document the discrepancy in your final report and **stop** — do NOT silently flip the decision. The user resolves these.

3. Read every doc the task doc references:
   - The Python source-of-truth files cited (clone with `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python` if not already present at /tmp/cognee-python).
   - Sibling task docs that this task depends on or blocks (per the §0 dependency notes).
   - The v1 strict-parity rule at `docs/http-server/plan.md §1`.
   Note any anchors that do not resolve.

4. Read the actual current state of the cognee-rust codebase for everything the task doc names:
   - For every file path the task says will be NEW: confirm it does not already exist. If it does, read it and report what's already there.
   - For every cited function/struct/trait: grep to confirm it does (or does not) exist. Note any signature drift since the task doc was written.
   - For every cited line range in existing files: open the file, confirm the citation still resolves to the right code.
   - For every dependency declared in the task: confirm the prerequisite has been landed (e.g. if the task says "Depends on B-3 (LIB-05)", check that `SessionLifecycleDb` and its impl are present in `cognee-database`).
   - For tasks that **consume** project-wide infrastructure (per §0.5's "Project-wide infrastructure" table — `iso8601_offset`, `ValidatedQuery`, `MemoryEntry`, `ImproveParams`), check that the owner task has actually shipped it. If not, report BLOCKED.

5. Identify partial implementations. If a previous PR has already landed steps 1..N of this task, your output must list:
   - Which steps in §4 of the task doc are already done.
   - Which steps remain.
   - Whether any "done" step has rotted (regressed) since it was implemented.

6. **Verify-only task short-circuit**: if the task is **A-1 (E-01)**, **A-3 (E-06)**, **A-4 (E-07)**, or **A-5 (E-08)** (the four pure verify-only tasks — see Lessons §3 in §0), do an extra pass:
   - Run the existing parity tests if any (e.g. `cargo test -p cognee-http-server --test test_recall`).
   - Compare the Rust handler/DTO byte-for-byte against the Python source-of-truth — list every divergence found.
   - If divergences exist, the task is no longer "verify only" — escalate the task doc with the specific deltas before the implementation agent runs. If zero divergences, the implementation step list reduces to "add cross-SDK parity test if missing".
   - **A-2 (E-03) is NOT in this list** — it owns the `iso8601_offset` helper module and always lands code. Do NOT short-circuit it.

7. Update docs to actualize. Where you find drift, **edit the docs**:
   - Stale anchors → fix them.
   - Wrong file paths → correct them.
   - Function names that were renamed in the codebase → update citations.
   - Steps already completed → strike through with a note `(already landed in <commit>)`.
   - Acceptance-criteria checkboxes that already pass → mark them.
   - Status mismatches between the task doc and README §3 → fix the README row.

8. Update the README.md status tables for the current task: `Not Started` → `In Progress`. Decrement the corresponding "Status roll-up" count. **Note for LIB-06**: it is dual-listed in §3 (Phase 0 enablers table for execution order; Library prerequisites table for category) — flip the Status column in **both** rows. The roll-up count is not double-counted; LIB-06 contributes one to the Library count.

9. Final report: one of these four verdicts:
   - **READY** — task is applicable, docs are now actualized, hand off to the implementation agent. Include a list of the §4 steps the implementation agent must execute (excluding any already-done ones).
   - **PARTIAL** — a slice of the task is already done; implementation agent should pick up at step N. Same step list as READY, but with the already-done prefix removed.
   - **OBSOLETE** — the task is no longer applicable (e.g. Python upstream removed the endpoint, the cognee-rust facade changed in a way that invalidates the spec, or a prerequisite task was skipped). Document why and stop. Do NOT proceed to implementation.
   - **BLOCKED** — a declared prerequisite has not been landed (e.g. starting D-1 before B-5). Stop. Tell the orchestrator which prerequisite is missing and which task ID needs to run first.

**Constraints**:
- Strict Python parity rule (see `docs/http-server/plan.md §1`). The two v1 divergences listed there + the one v2 divergence (D-1, see README §1.2) are the only allowed deviations. If a task doc seems to imply a new Rust-only behavior not in those lists, flag it as a finding.
- You may edit any file under `docs/http-api-v2/`. You may also fix anchors in `docs/http-server/` that the v2 docs cross-reference. You may NOT edit code under `crates/`.
- Use the Read / Edit / Write / Bash / Grep tools. Do NOT spawn nested agents.
- Cite every claim with a file:line reference.
- Length: 800–1500 words for the final report.
```

After this agent returns, **read its verdict.**
- READY or PARTIAL → proceed to Step 2 (Implementation) with the agent's step list.
- OBSOLETE → do NOT proceed. Update [README.md](README.md) status to `Skipped (obsolete)` with a one-line note, then move to the next task in §0 above.
- BLOCKED → do NOT proceed. Stop and ask the user — the order in §0 should never produce this verdict; if it did, something has changed underneath and the user needs to inspect.

## 5. Sub-agent: Implementation

**Purpose**: execute the actualized task doc end-to-end, run all verification commands, and commit with a meaningful message.

Spawn this agent (use `subagent_type: general-purpose`):

```
You are the implementation agent for ${TASK_ID} (${TASK_REF}) of the cognee-rust HTTP API v2 port.

Working dir: /home/dmytro/dev/cognee/cognee-rust

Your task doc: docs/http-api-v2/tasks/${TASK_DOC}

The investigation agent already actualized the docs. The §4 step list you must execute is below (verbatim from the investigation agent's READY/PARTIAL report):

${STEP_LIST_FROM_INVESTIGATION}

**Execution rules**:

1. **Orient yourself** by reading the task doc end-to-end + the v2 root README + any sibling task doc declared as a dependency. Read **README §1.1 Wire conventions** and **§1.2 v2 acknowledged divergences** — they are project-wide rules every commit must respect.

2. **Honor the decision notes.** Every "Decision (2026-04-29) — Decision N" block in your task doc is settled. Don't change the choice — just execute. Reference it in the commit body when the choice is non-obvious to a reviewer.

3. Execute the steps in §4 of the task doc in order. After every step:
   - Run `cargo check --all-targets` (NOT `--release`).
   - If the step has a specific `Verify:` command, run that too.
   - Confirm no warnings introduced beyond pre-existing ones.

4. After all steps:
   - Run `cargo fmt`.
   - Run `cargo check --all-targets`.
   - Run `cargo test --workspace` (debug mode, no `--release`).
   - Run `scripts/check_all.sh`.
   - All four must pass before you proceed.

5. Regression check: confirm no previously-passing test now fails. If a previously-passing test fails, you have introduced a regression — fix it before committing. Do not silently accept the regression.

6. Commit. **Single commit per task.** Decision 13 split LIB-03/LIB-05 into two separate tasks (each landing one commit) — there is no "two commits in one task" case in this port.

   Commit message format:
   ```
   http-api-v2: ${TASK_REF} <one-line summary>

   <2–4 sentence body explaining what landed and why, citing the task doc>
   <List of acceptance-criteria checkboxes that now pass.>

   Refs: docs/http-api-v2/tasks/${TASK_DOC}
   ```

   Use a heredoc per the project guide. Add `Co-Authored-By:` line per project convention. The prefix is `http-api-v2:` for **every** task including CLEAN-01 (Decision 14).

7. **Verify-only task short-circuit**: if the task is one of A-1 / A-3 / A-4 / A-5 (the four pure verify-only tasks per §0 Lessons #3) AND the only step is "add cross-SDK parity test" AND the new test passes, your commit is the test file + fixtures only. If the verification revealed **zero** work needed (no test missing, no divergence), do NOT make an empty commit — return DONE with the note "no code change required, README to record (verified, no code change)" and let the doc-update agent record it. **A-2 (E-03) is NOT a verify-only task** — it always lands code.

8. Do NOT push. Just commit locally.

9. Final report: one of these three verdicts:
   - **DONE** — all steps executed, all tests pass, commit landed locally. Include the commit SHA. (Or, for verify-only with zero work, report DONE with the no-code-change note and no SHA.)
   - **PARTIAL** — N steps landed; remaining steps blocked. Document why each remaining step is blocked (cite the error or missing dependency). Include the commit SHA for whatever did land.
   - **FAILED** — could not land any steps. Document the blocker. Do NOT commit a half-done state.

**Constraints**:
- Coding conventions per `.claude/CLAUDE.md` (project root, NOT `docs/.claude/CLAUDE.md`): no `unwrap()` in non-test code; use `expect("reason why this can never panic")` or `?`. Lock-poison `unwrap()` is OK with a `// lock poison is unrecoverable` comment.
- Strict Python parity. The only acknowledged divergences are: (a) the two v1 divergences in `docs/http-server/plan.md §1`, and (b) the one v2 divergence in README §1.2 (D-1: `order_by` validation). Do NOT introduce new ones — if a parity question arises mid-task, stop and ask via the FAILED verdict.
- Run `cargo fmt` before each `cargo check` so formatting is never the blocker.
- The implementor MUST cite the relevant task-doc section for any non-obvious decision (in the commit body, not in code).
- Do NOT edit any doc under `docs/http-api-v2/`. The doc-update agent owns those edits.
- For tasks that **consume** project-wide infrastructure (per §0.5's table — `iso8601_offset`, `ValidatedQuery`, `MemoryEntry`, `ImproveParams`, the camelCase OpenAPI regression test), use the existing implementation; do NOT re-implement it locally.
- Do NOT spawn nested agents.
```

After this agent returns:
- DONE → proceed to Step 3 (Review).
- PARTIAL → proceed to Step 3 (Review), then re-spawn the implementation agent with the remaining steps if Step 3 doesn't object. Repeat at most twice; if still PARTIAL, stop and ask the user.
- FAILED → stop and ask the user. Do not proceed.

## 6. Sub-agent: Review

**Purpose**: independent review of the top commit against the task doc. Catches: missing test coverage, security issues, regressions, deviations from the spec, scope creep.

Spawn this agent (use `subagent_type: general-purpose`):

```
You are the review agent for ${TASK_ID} (${TASK_REF}) of the cognee-rust HTTP API v2 port.

Working dir: /home/dmytro/dev/cognee/cognee-rust

Your task doc: docs/http-api-v2/tasks/${TASK_DOC}
The commit under review: ${COMMIT_SHA} (the implementation agent reported this; "no code change required" tasks have no commit — record APPROVED with that note).

**Steps**:

1. **Orient yourself** (same reading list as the investigation/implementation agents): the task doc, the v2 root README + §1.1 wire conventions + §1.2 acknowledged divergences, IMPLEMENTATION-PROMPT §0.5 decisions log, and the Python source-of-truth files the task cites.

2. Inspect the commit:
   - `git show ${COMMIT_SHA}` for the full diff.
   - `git diff ${COMMIT_SHA}~1 ${COMMIT_SHA} --stat` for the file list.
   - For every file in the diff, read its full current content (not just the diff hunks) so you understand the surrounding code.

3. Run all the verification commands the task doc lists in §6 (acceptance criteria). Confirm every checkbox actually passes:
   - `cargo check --all-targets`
   - `cargo test --workspace`
   - `scripts/check_all.sh`
   - any task-specific commands (e.g. `cargo test -p cognee-database --test test_session_lifecycle_repo` for LIB-05).

4. Review checklist (apply to every commit):
   - **Spec match**: every step in the task doc's §4 is present in the diff. Nothing extra is present (no scope creep). If a step is "to-be-added in a later task" with a TODO marker, the marker is there.
   - **Decisions respected**: every "Decision (2026-04-29) — Decision N" note in the task doc is honored. If the commit silently re-litigates a decision (e.g. uses snake_case where Decision 10 says camelCase), that's a hard reject.
   - **Tests**: every test file in the task doc's §5 exists, contains the cases the doc lists, and passes.
   - **Coverage**: every wire-visible behavior (status code, header, body shape) cited in the task is exercised by at least one test.
   - **Wire conventions (Decisions 6, 7, 10)**: every new/modified body or response DTO uses `#[serde(rename_all = "camelCase")]`; multi-word input fields carry `#[serde(alias = "<snake_form>")]`; `DateTime<Utc>` fields use `#[serde(with = "crate::dto::util::iso8601_offset")]`; validation rejections produce status `400` with the `{"detail": [{"loc","msg","type"}], "body": <raw>}` envelope.
   - **Security**: auth-bearing endpoints are gated. Permission gates use the established pattern from v1 (`AclDb::has_permission_with_roles` or `state.lib.permissions().user_can(...)`). No secrets logged. SQL safe (no `format!` into queries).
   - **No `unwrap()` in non-test code**. `expect("reason")` only with a why-it-can't-fail comment. Lock poison `unwrap()` is OK.
   - **Strict Python parity**: no Rust-side improvements outside the documented divergences (v1's two in `docs/http-server/plan.md §1` + v2's D-1 in README §1.2). If the commit improves on Python beyond those, flag it — even if the improvement is good.
   - **Wire-shape parity**: for new/modified DTOs, confirm field names, defaults, and serialization match Python's pydantic output byte-for-byte. Where the task doc cites a JSON example, run it through serde and compare.
   - **OpenAPI regression**: if CLEAN-01 has landed (it should have, by Phase A), the camelCase regression test (`openapi_property_names_are_all_camelcase`) must still pass — confirm it does.
   - **Regressions**: previously-passing tests still pass.
   - **Commit message**: format per §8.2 of this prompt; cites the task doc; uses the `http-api-v2:` prefix.

5. **If you find concerns**: amend the commit (do NOT create a new commit; the user wants a single tidy commit per task).
   - Make the fixes.
   - Re-run `cargo fmt`, `cargo check --all-targets`, `cargo test --workspace`, `scripts/check_all.sh`.
   - `git commit --amend --no-edit` (or `--no-edit` replaced with an updated message if the message itself is wrong).
   - Re-verify all the checklist items.

6. Final report: one of these three verdicts:
   - **APPROVED** — commit is clean as-is. No amendments needed. (Or "no code change required" task — APPROVED with that note.)
   - **AMENDED** — amended the commit; describe what was fixed. Include the new commit SHA (it will differ from the input).
   - **REJECTED** — concerns are unfixable at this level (e.g. wrong design decision, requires a doc change, or scope is wrong). Document the concerns. Do NOT amend. The orchestrator will escalate to the user.

**Constraints**:
- You MAY edit code (only to amend the commit). You may NOT push.
- You MAY edit tests (to add missing coverage during amendment).
- You MAY NOT edit docs under `docs/http-api-v2/`. The doc-update agent owns those.
- Use only `git commit --amend`, never `git commit -m` (no new commits) — except when the implementation agent reported PARTIAL with no commit and your amendment is the first commit. In that one case, create a fresh commit using the format from §8.2 of this prompt.
- If the diff is empty (e.g. all changes already merged elsewhere), report APPROVED with a note.
- Do NOT spawn nested agents.
```

After this agent returns:
- APPROVED or AMENDED → proceed to Step 4 (Doc Update).
- REJECTED → stop and ask the user. Do not proceed.

## 7. Sub-agent: Doc Update

**Purpose**: propagate the "Done" status across the doc tree. Updates README.md status tables and any cross-doc reference that needs a touch-up. Keeps the doc tree in sync with the codebase reality.

Spawn this agent (use `subagent_type: general-purpose`):

```
You are the doc-update agent for ${TASK_ID} (${TASK_REF}) of the cognee-rust HTTP API v2 port.

Working dir: /home/dmytro/dev/cognee/cognee-rust

Your task doc: docs/http-api-v2/tasks/${TASK_DOC}
The commit that landed: ${COMMIT_SHA}  (or "no code change required" if there isn't one)

**Steps**:

1. Update `docs/http-api-v2/README.md`:
   - In the §3 Pre-port cleanup & enablers OR Library prerequisites OR Endpoints table (whichever owns this task), flip the Status column from `In Progress` to `Done`. Append `(commit ${COMMIT_SHA_SHORT})` to the row, or `(verified, no code change)` if there was no commit. **Note**: LIB-06 is dual-listed (Phase 0 for execution order, Library prerequisites for category) — flip the status in **both** tables when LIB-06 lands.
   - Update the §3 "Status roll-up" counts:
     - Decrement the count of the previous bucket the task was in (Not Started / Missing / Partial / Implemented).
     - Add or increment a `Done` row in the roll-up (add the row if it doesn't exist yet).
     - Re-verify the grand-total line at the bottom of the roll-up still reads `19 tasks (1 cleanup + 6 library + 12 endpoints)`.

2. Update `docs/http-api-v2/IMPLEMENTATION-PROMPT.md` §0 ("Current state"):
   - Flip the row for this task in the §0 phase table from `Not Started` to `Done` with the commit SHA short hash.
   - Update the resume point at the top of §0: "Resume point: TASK ${NEXT_TASK_ID} (${NEXT_TASK_REF})." Use the next row in §0 that is still Not Started. If this was the last task, change to "All 19 tasks complete. No resume point — the v2 port is done."

3. Update the task doc itself (`docs/http-api-v2/tasks/${TASK_DOC}`):
   - Flip the Status field in the table at the top from `Not Started` to `Done` (or `Done — no code change` for verify-only).
   - Tick every acceptance-criteria checkbox in §6 (or wherever the task doc places them) that the commit satisfied.
   - If the implementation discovered something the task doc got wrong, fix the task doc inline.
   - Close any open questions that were resolved during implementation.

4. Cross-doc consistency sweep:
   - `grep -n "Not Started\|In Progress" docs/http-api-v2/README.md` — only Not-Started rows for tasks that are genuinely not started should remain. No In-Progress rows.
   - `grep -rn "// TODO(${TASK_REF})\|// TODO(${TASK_ID})" crates/` — should be empty (those should have been resolved by the implementation agent).
   - No anchor refs to sections that were renumbered during implementation.
   - For the first few tasks: confirm the §0.5 "Project-wide infrastructure" table accurately reflects what's been landed (the `iso8601_offset` helper appears in the codebase after E-03; `ValidatedQuery<T>` after E-09; etc.). If a task ships infrastructure not yet listed, add a row.

5. Commit the doc updates. Single small commit:
   ```
   docs/http-api-v2: mark ${TASK_REF} done

   Status table flips after commit ${COMMIT_SHA_SHORT}.

   Refs: docs/http-api-v2/tasks/${TASK_DOC}
   ```

   Note: this commit also uses the `http-api-v2:` prefix family (more precisely `docs/http-api-v2:` per the v1 doc-update commit pattern). Either form is acceptable as long as it's distinct from the implementation commit message and clearly identifies a docs-only update.

6. Final report: a one-paragraph summary of which docs were updated, which status flips happened, and the new resume point.

**Constraints**:
- Do NOT edit code under `crates/`. You only touch `docs/`.
- The implementation commit is already amended-and-final. Do NOT touch it.
- Do NOT spawn nested agents.
```

After this agent returns: the task is fully done. Move to the next task in §0 above.

## 8. Conventions

### 8.1 Branch policy

All work lands directly on `main` — no per-task or per-phase feature branches. Each task produces 1–2 commits on `main`. The user pushes manually when satisfied. Confirm `git rev-parse --abbrev-ref HEAD` returns `main` before each task; if it doesn't, stop and ask the user (someone may have checked out a feature branch and the on-`main` policy needs explicit override).

**Pre-task hygiene** (`main`-direct policy makes this stricter than v1's long-lived-branch model):

- `git status` must be clean at task start. Uncommitted changes on `main` are someone else's in-flight work — stop and ask, don't stash.
- `git pull --ff-only` before the investigation agent runs, in case the user (or another contributor) pushed since the last task. If the fast-forward fails, stop and ask — do NOT auto-rebase.
- Each task's commits must compile and pass tests in isolation — `main` is an always-shippable branch, so a half-landed task is a regression in the public history. The implementation agent's verification gate (§8.3) enforces this.

### 8.2 Commit message

> **Decision (2026-04-29) — Decision 14**: confirmed `http-api-v2:` prefix for **all 19 tasks**, including CLEAN-01 (technically a v1 wire-shape fix, bookkept as task `0-1`) and LIB-06 (added 2026-04-29 as task `0-2`). Bundling everything under one prefix makes the "what changed in the v2 port?" answer trivially `git log --grep=^http-api-v2:`. Investigation agent: do not re-litigate.

Use a heredoc to preserve formatting:

```bash
git commit -m "$(cat <<'EOF'
http-api-v2: ${TASK_REF} <one-line summary>

<2–4 sentence body>
<Acceptance criteria that now pass.>

Refs: docs/http-api-v2/tasks/${TASK_DOC}

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Note the prefix change vs v1: `http-api-v2:` not `http-server:`. This makes `git log --oneline | grep '^http-api-v2:'` a clean view of the v2 port's history. CLEAN-01 also uses `http-api-v2:` for the same reason — even though it touches v1 DTOs, it's a v2-port prerequisite and belongs in the port's history.

### 8.3 Verification gates

Three commands must pass before any commit:
- `cargo fmt --check`
- `cargo check --all-targets`
- `cargo test --workspace` (debug mode)
- `scripts/check_all.sh`

If any fails, fix before committing — do not commit a broken state.

### 8.4 Scope discipline

- One task per pipeline run. The implementation agent does not pull work from a later task forward.
- Out-of-scope changes (typo fixes, doc tweaks unrelated to the current task, refactors of unrelated code) are NOT committed during a task. Defer them to a separate cleanup commit at the end of the v2 port.
- The review agent enforces this — it rejects scope creep.
- **Special case for Phase A**: if the investigation agent finds a parity divergence in an "Implemented" endpoint, do NOT silently fix it as part of the verify task. Escalate the divergence into a new task doc (or amendment to the existing one) and run the four-agent pipeline against the amended scope.

### 8.5 Status tracking

Three places hold status:
1. [README.md §3](README.md#3-tasks--implementation-status) — Pre-port cleanup & enablers + Library prerequisites + Endpoints tables + status roll-up. **Authoritative.**
2. [IMPLEMENTATION-PROMPT.md §0](IMPLEMENTATION-PROMPT.md#0-current-state) — phase table + resume point at the top.
3. Each [task doc](tasks/) — Status field at the top + acceptance checkboxes at the bottom.

All three must agree at the end of every doc-update step.

## 9. Failure modes & escalation

Stop and ask the user when:

- Investigation agent reports **OBSOLETE** for any task. The user must decide whether to skip, edit the doc, or rewrite the task.
- Investigation agent reports **BLOCKED** (a prerequisite task wasn't run). The user must decide whether to back up to the prerequisite or accept the gap.
- Implementation agent reports **FAILED**. The user must inspect the blocker.
- Implementation agent reports **PARTIAL** twice in a row. Same task is stuck.
- Review agent reports **REJECTED**. The user must decide whether to amend the doc, accept the deviation, or re-do the implementation.
- Any of `cargo check`, `cargo test`, `scripts/check_all.sh` fail unexpectedly mid-task and the implementation agent cannot resolve them within one retry.
- A merge conflict appears (the user is doing concurrent work on the branch). Stop and ask.

When you ask the user, include:
- The task name and `${TASK_ID}` / `${TASK_REF}`.
- The agent that reported the issue.
- The exact verdict the agent returned (paste it).
- The current git state (`git status`, last commit SHA).
- A specific question with 2–3 concrete options for how to proceed.

## 10. Session conventions

This driver is meant for an interactive "check-in" cadence — you run **one full task** (all four agents) per session, then stop and let the user review before starting the next task. **Do not** chain task A-1 → A-2 → A-3 in a single autonomous run; the user wants to inspect the diff after each task.

If you are running in a `/loop` skill or autonomous mode where the user has explicitly asked you to keep going, then proceed to the next task without stopping. Otherwise, after Step 4 of one task, your message to the user is:

```
Task ${TASK_ID} (${TASK_REF}) complete. Status:
- Investigation: <verdict>
- Implementation: commit ${COMMIT_SHA_SHORT}  (or "no code change required")
- Review: <APPROVED | AMENDED with new SHA>
- Doc update: commit ${DOC_COMMIT_SHA_SHORT}

Next task: ${NEXT_TASK_ID} (${NEXT_TASK_REF}, ${NEXT_TASK_DOC}).
Reply "go" to proceed, or hand back for review.
```

## 11. References (read these once, at session start)

- [v2 root README](README.md) — analysis + status tables (the authoritative status source).
- [README §1.1 Wire conventions](README.md#11-wire-conventions-project-wide-set-by-decision-6) — project-wide rules every DTO obeys.
- [README §1.2 v2 acknowledged divergences](README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output) — the one v2 wire-shape divergence (D-1).
- [§0 of this file](#0-current-state) — phase table + resume point.
- [§0.5 Decisions log](#05-decisions-log) — index of all 15 decisions and the project-wide infrastructure they introduced.
- [Tasks directory](tasks/) — 19 task docs (1 CLEAN + 6 LIB + 12 E).
- [v1 strict-parity rule](../http-server/plan.md#1-goal) — the rule v2 inherits.
- [v1 IMPLEMENTATION-PROMPT](../http-server/implementation/IMPLEMENTATION-PROMPT.md) — sibling driver this prompt is modelled on; reference for any pattern not explicit here.
- [SDK v2 gap analysis](../api-v2/README.md) — the in-process Rust API parity baseline (most v2 SDK functions are already implemented).
- [Project guide](../../.claude/CLAUDE.md) at project root `.claude/CLAUDE.md` — coding conventions, build commands, test patterns. **Note**: the agent prompts say `.claude/CLAUDE.md` (working dir is the project root), NOT `docs/.claude/CLAUDE.md`.
- Python source-of-truth: [`cognee/api/v1/{remember,recall,improve,forget,sessions,visualize}/routers/`](https://github.com/topoteretes/cognee/tree/main/cognee/api/v1) — clone with `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python` if not already present.

## 12. Failure modes the agents cannot fix on their own

Some failures require the user. Do not let an agent silently work around them:

- **A library function the task depends on does not exist and is not described in any task doc.** Means the task tree has a gap. Ask the user.
- **A migration would conflict with existing schema** (LIB-03 + LIB-05 are the most likely candidates — LIB-03 adds the `session_records` and `session_model_usage` tables that don't currently exist; verify there is no leftover state from an aborted earlier attempt). Ask before running.
- **A test reveals a Python-Rust wire incompatibility that the task doc does not anticipate** (e.g. a default value Python pydantic emits differs from what serde produces). Ask before "fixing" — the right answer might be to amend the task doc, the Python upstream, or accept a new documented divergence in README §1.2.
- **The codebase has uncommitted changes when a task starts.** Stop and ask — those changes might be in-flight from a different effort.
- **The Python upstream has changed since the task doc was written.** Run `git -C /tmp/cognee-python log --oneline -5 cognee/api/v1/<area>/` and compare against the SHAs the task doc cites (if any). If material changes exist, update the task doc before proceeding.
- **A decision (Decision N) appears to be wrong** — e.g. a Python file the decision cites no longer exists. Stop and ask the user; do NOT silently flip the decision.

When in doubt, stop and ask. The cost of one user round-trip is much lower than the cost of an unintended commit.
