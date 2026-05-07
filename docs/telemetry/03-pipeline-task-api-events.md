# 03 — Pipeline / Task / API Operation Events

> **Status (2026-05-07):** Scope was narrowed after gap 02 landed. The
> SDK-API events (`cognee.recall`, `cognee.improve`, `cognee.forget`,
> `cognee.remember`, and the three `Ok(...)` paths of `cognee.search
> EXECUTION COMPLETED`) are **already wired** by gap 02-07
> (commit `8b096bb`). What remains is the high-volume **pipeline +
> task lifecycle** events, the missing `cognee.search EXECUTION
> STARTED` paired event, the `cognee.api.improve` OTEL span (Python
> parity nicety), and the supporting plumbing (`tenant_id`,
> `Task::python_task_type`, settings snapshot allowlist). See
> [Design decisions (locked)](#design-decisions-locked) and
> [Action items](#action-items) for the binding contract.

## Overview

Python `cognee` ships a small fixed catalog of product-analytics events sent
through `send_telemetry(...)` (see [02-send-telemetry-analytics.md](02-send-telemetry-analytics.md)
for the transport-layer gap). They fall into three groups:

1. **Pipeline lifecycle** — fired around `run_tasks_with_telemetry()` for every
   pipeline run (cognify, memify, add, etc.):
   `Pipeline Run Started`, `Pipeline Run Completed`, `Pipeline Run Errored`.
2. **Task lifecycle** — fired around every individual task in `handle_task()`:
   `${task_type} Task Started`, `${task_type} Task Completed`,
   `${task_type} Task Errored` where `task_type` is one of
   `Async Generator`, `Generator`, `Coroutine`, `Function`.
3. **Top-level SDK API operations** — high-level user-facing entry points:
   `cognee.recall`, `cognee.improve`, `cognee.forget`, `cognee.remember`,
   `cognee.search EXECUTION STARTED`, `cognee.search EXECUTION COMPLETED`.

This document is **only** concerned with *where* each event is emitted and
*what payload* it carries. The HTTP transport (auth headers, redaction, retry,
opt-out) is the subject of [02-send-telemetry-analytics.md](02-send-telemetry-analytics.md);
this document depends on that client existing (it does — gap 02 closed on 2026-05-06).

A separate concern is OpenTelemetry spans. Python emits **both** an OTEL span
*and* a `send_telemetry(...)` call at most of these sites — the two are
complementary and the Rust port already has the OTEL spans
(`cognee.api.recall`, `cognee.api.improve`, `cognee.pipeline.task`, etc.).
What is missing is the analytics counterpart.

---

## Event catalog

Filenames are relative to either `/tmp/cognee-python/cognee/` (Python clone) or
`crates/` under the Rust workspace. Line numbers are accurate at the time of
writing.

| Event name | Python emission site | Property keys (besides identity) | Rust target site | Current Rust state |
|---|---|---|---|---|
| `Pipeline Run Started` | [`modules/pipelines/operations/run_tasks_with_telemetry.py:27`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_with_telemetry.py#L27) | `pipeline_name`, `cognee_version`, `tenant_id`, plus the entire `get_current_settings()` dict (`vector_db`, `graph_db`, `relational_db`, `llm`, `embedding`, etc.) merged in via `\| config` | [`crates/core/src/pipeline.rs:532`](../../crates/core/src/pipeline.rs#L532) — right after `watcher.on_pipeline_run_started(&run_info)` | **None.** Watcher fires but no analytics emission. |
| `Pipeline Run Completed` | [`run_tasks_with_telemetry.py:42`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_with_telemetry.py#L42) | same as Started | [`crates/core/src/pipeline.rs:574`](../../crates/core/src/pipeline.rs#L574) — `Ok(_)` branch after `on_pipeline_run_completed` | **None.** |
| `Pipeline Run Errored` | [`run_tasks_with_telemetry.py:59`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_with_telemetry.py#L59) | same as Started + the exception is logged but **not** sent (no `error` property in the analytics call) | [`crates/core/src/pipeline.rs:584`](../../crates/core/src/pipeline.rs#L584) (Cancelled) and [`:604`](../../crates/core/src/pipeline.rs#L604) (Failed) | **None.** |
| `${task_type} Task Started` | [`run_tasks_base.py:135`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_base.py#L135) | `task_name` (= `running_task.executable.__name__`), `cognee_version`, `tenant_id` | [`crates/core/src/pipeline.rs:988`](../../crates/core/src/pipeline.rs#L988) — start of the retry loop in `call_with_retry` | **None.** OTEL span exists ([line 971](../../crates/core/src/pipeline.rs#L971)). |
| `${task_type} Task Completed` | [`run_tasks_base.py:192`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_base.py#L192) | `task_name`, `cognee_version`, `tenant_id` | [`crates/core/src/pipeline.rs:1033`](../../crates/core/src/pipeline.rs#L1033) — `Ok(resolved)` branch | **None.** OTEL span attribute set. |
| `${task_type} Task Errored` | [`run_tasks_base.py:210`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_base.py#L210) | `task_name`, `cognee_version`, `tenant_id`. **No error string property.** | [`crates/core/src/pipeline.rs:1065-1069`](../../crates/core/src/pipeline.rs#L1065) — terminal failure after retries exhausted | **None.** |
| `cognee.recall` | [`api/v1/recall/recall.py:402`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/recall.py#L402) — emitted **before** the OTEL span / cloud dispatch | `query_length`, `scope`, `auto_route`, `top_k`, `search_type` (`str(query_type.value)` or `"auto"`), `session_id`, `datasets` (comma-joined names), `dataset_ids` (comma-joined UUIDs), `cognee_version` | [`crates/lib/src/api/recall.rs:230`](../../crates/lib/src/api/recall.rs#L230) — after the body, before returning | **Done** in commit `8b096bb` (gap 02-07). |
| `cognee.improve` | [`api/v1/improve/improve.py:91`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/improve.py#L91) — before any work, before the OTEL span | `dataset` (str), `session_count`, `session_ids` (comma-joined), `run_in_background`, `cognee_version` | [`crates/lib/src/api/improve.rs:161`](../../crates/lib/src/api/improve.rs#L161) — early in `pub async fn improve(...)` | **Partial — done** in commit `8b096bb` (gap 02-07); OTEL span still missing — see [task 03-07](03/07-improve-otel-span.md). |
| `cognee.forget` | [`api/v1/forget/forget.py:79`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L79) — before the OTEL span | `target` (one of `everything` / `data_item_memory_only` / `dataset_memory_only` / `data_item` / `dataset` / `unknown`), `dataset` (str), `data_id` (str), `cognee_version` | [`crates/lib/src/api/forget.rs:114`](../../crates/lib/src/api/forget.rs#L114) | **Done** in commit `8b096bb` (gap 02-07). The 3-value Rust `ForgetTarget` enum (`item`/`dataset`/`everything`) does not distinguish memory-only deletes — locked decision 2 below. |
| `cognee.remember` | [`api/v1/remember/remember.py:624`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L624) — inside the `cognee.api.remember` OTEL span | `mode` (`session` / `permanent`), `dataset_name`, `data_size_bytes`, `item_count`, `session_id`, `self_improvement`, `run_in_background`, `cognee_version` | [`crates/lib/src/api/remember.rs:237`](../../crates/lib/src/api/remember.rs#L237) | **Done** in commit `8b096bb` (gap 02-07). |
| `cognee.search EXECUTION STARTED` | [`modules/search/methods/search.py:74`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/methods/search.py#L74) | `cognee_version`, `tenant_id` | Top of [`SearchOrchestrator::search`](../../crates/search/src/orchestration/search_orchestrator.rs#L136) — paired with the existing `EXECUTION COMPLETED` emitter. | **Missing — pending [task 03-06](03/06-search-execution-events.md).** Decision 3 below: implement for Python parity. |
| `cognee.search EXECUTION COMPLETED` | [`modules/search/methods/search.py:115`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/methods/search.py#L115) | `cognee_version`, `tenant_id` | [`SearchOrchestrator::search`](../../crates/search/src/orchestration/search_orchestrator.rs#L18) `emit_search_completed()` (3 `Ok` paths). | **Done** in commit `8b096bb` (gap 02-07). Will be backfilled with real `tenant_id` by [task 03-01](03/01-tenant-id-plumbing.md). |

> Notes on the "EXECUTION STARTED/COMPLETED" pair: this lives in the
> internal Python `search()` method that `recall()` ultimately calls. It is
> redundant with `cognee.recall` for the SDK surface but is kept by
> **decision 3** below for byte-equal Python parity. `EXECUTION COMPLETED`
> is already emitted by `crates/search/src/orchestration/search_orchestrator.rs`;
> [task 03-06](03/06-search-execution-events.md) adds the `STARTED` paired
> emission at the top of `SearchOrchestrator::search`.

### Other `send_telemetry` call sites (out of scope, listed for completeness)

These are HTTP-router-level events fired by FastAPI handlers in Python:

- [`api/v1/add/routers/get_add_router.py:83`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py#L83) — `cognee.add API Call`
- [`api/v1/cognify/routers/get_cognify_router.py:123`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L123) — `cognee.cognify API Call`
- [`api/v1/delete/routers/get_delete_router.py:44`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py#L44) — `cognee.delete API Call`
- many more under `api/v1/*/routers/`

These are observed when running the **Python FastAPI server**, not when
calling the SDK. The Rust port's HTTP layer (`crates/lib/src/api/serve.rs`
and the http-api-v2 work) would be the place to add equivalents — see
the **Open questions** section.

---

## Property semantics

### Identity properties (sent by every event — see [01-analytics-client.md](01-analytics-client.md))

These are **not** part of `additional_properties`; they are added by the
client itself. Listed here for context.

- `user_id` — the second positional argument to `send_telemetry`. Either a
  UUID string or the literal `"sdk"` when no user object is available.
- `anonymous_id`, `persistent_id`, `api_key_tracking_id` — derived
  by `_attach_identity()` inside `send_telemetry`.

### Event-specific properties

#### Pipeline lifecycle events

- **`pipeline_name`** — string, e.g. `"cognify_pipeline"`,
  `"add_pipeline"`, `"memify_pipeline"`. Always passed as
  `str(pipeline_name)`. In Rust this is `pipeline.name` from
  `crates/core/src/pipeline.rs` (already in `PipelineRunInfo.pipeline_name`).
- **`cognee_version`** — `cognee.__version__`. In Rust, source from
  `env!("CARGO_PKG_VERSION")` evaluated **inside `cognee-lib`** so the
  reported version matches the published library, not whichever crate
  invoked the helper. Expose this as a `pub const COGNEE_VERSION: &str`
  re-export.
- **`tenant_id`** — `str(user.tenant_id)` if present, else the literal
  string `"Single User Tenant"`. The Rust port's `PipelineContext`
  carries `user_id` but not `tenant_id` today; either thread it through
  or always emit `"Single User Tenant"` (see Open questions).
- **`| config`** — Python merges `get_current_settings()` into the
  property dict. That is a snapshot of vector/graph/relational backends,
  the LLM provider, embedding settings, etc. The Rust equivalent is
  `cognee_lib::config::Config`; we should expose a serializable
  `to_telemetry_snapshot()` returning a `serde_json::Map` so the same
  keys flow on the wire.

#### Task lifecycle events

- **`task_type`** — derived from the executable kind. Python uses
  `inspect.isasyncgenfunction` etc. ([`tasks/task.py:194-207`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/tasks/task.py#L194)),
  producing `"Async Generator"`, `"Generator"`, `"Coroutine"`, or
  `"Function"`.
  **Rust mapping** — `crates/core/src/task.rs:237` defines an enum:
  | Rust `Task` variant | Python `task_type` |
  |---|---|
  | `Task::Async`, `Task::AsyncBatch` | `Coroutine` |
  | `Task::Sync`, `Task::SyncBatch` | `Function` |
  | `Task::SyncIter`, `Task::SyncIterBatch` | `Generator` |
  | `Task::AsyncStream`, `Task::AsyncStreamBatch` | `Async Generator` |

  Add a method `Task::python_task_type(&self) -> &'static str`
  on the enum to centralise the mapping. The event name template
  `"{task_type} Task Started"` is then trivially rendered.
- **`task_name`** — Python uses `running_task.executable.__name__`. Rust
  needs to surface the same. The retry loop already has
  `task_name: Option<&str>` (parameter at
  [`pipeline.rs:964`](../../crates/core/src/pipeline.rs#L964)) and uses
  `"unknown"` as the fallback in the OTEL span — reuse it here.
- **`cognee_version`**, **`tenant_id`** — same as above.

#### `cognee.recall`

| Property | Source | Notes |
|---|---|---|
| `query_length` | `len(query_text)` | Length in **characters** (Python `len(str)`) |
| `scope` | `","`-joined sources after `normalize_scope` | One of `graph`, `session`, `trace`, `graph_context`, or comma combos |
| `auto_route` | bool | Whether the query router was used |
| `top_k` | int | |
| `search_type` | `str(query_type.value)` or `"auto"` | `query_type` is a `SearchType` enum |
| `session_id` | str or `""` | |
| `datasets` | `","`-joined names or `""` | |
| `dataset_ids` | `","`-joined UUIDs or `""` | |
| `cognee_version` | const | |

The Rust `recall.rs` already computes `span_scope: String` and has
`session_id: Option<&str>`, `datasets`/`dataset_ids` parameters — all
the inputs needed are within scope of the function.

#### `cognee.improve`

| Property | Source | Notes |
|---|---|---|
| `dataset` | `str(dataset)` | UUID or name as string |
| `session_count` | `len(session_ids)` or `0` | |
| `session_ids` | `","`-joined or `""` | |
| `run_in_background` | bool | |
| `cognee_version` | const | |

#### `cognee.forget`

| Property | Source | Notes |
|---|---|---|
| `target` | derived from arg combination | Six values listed in the catalog table |
| `dataset` | `str(dataset)` or `""` | |
| `data_id` | `str(data_id)` or `""` | |
| `cognee_version` | const | |

The Rust `ForgetTarget` enum maps cleanly:

```rust
match target {
    ForgetTarget::All                                            => "everything",
    ForgetTarget::Dataset { .. } /* with memory_only flag */     => "dataset_memory_only",
    ForgetTarget::Dataset { .. }                                 => "dataset",
    ForgetTarget::Item    { .. } /* with memory_only flag */     => "data_item_memory_only",
    ForgetTarget::Item    { .. }                                 => "data_item",
}
```

The current Rust `ForgetTarget` does **not** carry a `memory_only`
distinction — confirm whether memory-only forgets are routed through
the same enum or a separate code path. (See Open questions.)

---

## Detailed gap analysis

### Pipeline lifecycle — fully missing

The Rust pipeline runner has a richer `PipelineWatcher` trait
(`on_pipeline_run_started`, `on_pipeline_run_completed`,
`on_pipeline_run_errored`) which is **observability**, not analytics.
Hooking emission into the watcher is tempting, but mixing wire-format
analytics into a public extension trait conflates concerns: a
downstream consumer who implements `PipelineWatcher` should not
unwittingly cause analytics POSTs.

**Decision:** emit directly from `execute()` in
`crates/core/src/pipeline.rs`, *next to* the watcher calls, gated on
the `telemetry` feature. The watcher remains a structural callback.

### Task lifecycle — fully missing

The retry loop (`call_with_retry`) is the right emission site:

- "Started" fires once before the first attempt (Python emits once,
  not per-attempt).
- "Completed" fires on the first successful attempt.
- "Errored" fires only after retries are exhausted (matches Python:
  Python re-raises after one attempt because there is no retry policy;
  Rust's retry policy does not change the "Errored" semantics on the
  user-facing event because retries are an internal concern).

### API operation events

- **`cognee.recall`** — span exists, analytics call missing. Drop the
  emission immediately before the `tracing::info_span!("cognee.api.recall")`
  call so it appears even if the span is suppressed.
- **`cognee.improve`** — neither the OTEL span nor the analytics call
  exists. Adding the analytics call is the focus of this gap; adding
  a `cognee.api.improve` span is a separate small task that could be
  bundled.
- **`cognee.forget`** — the existing `tracing::info!(target: "cognee.telemetry", ...)`
  block is functionally a *log line*, not a wire-format event. It does
  not POST anywhere and its property keys do not match Python
  (`forget_target` vs `target`, missing `event` field on Python side).
  Replace with the new emitter; see compat section below.

---

## Proposed design

### Helper location

The transport client lives in (per Gap 2)
`crates/utils/src/telemetry/` (or a new `crates/telemetry/` crate).
Add an `EventEmitter` facade with an enum of typed events:

```rust
// crates/utils/src/telemetry/events.rs (sketch)
pub enum AnalyticsEvent<'a> {
    PipelineRunStarted { pipeline_name: &'a str, settings: serde_json::Map<String, serde_json::Value> },
    PipelineRunCompleted { pipeline_name: &'a str, settings: serde_json::Map<String, serde_json::Value> },
    PipelineRunErrored { pipeline_name: &'a str, settings: serde_json::Map<String, serde_json::Value> },
    TaskStarted { task_type: &'static str, task_name: &'a str },
    TaskCompleted { task_type: &'static str, task_name: &'a str },
    TaskErrored { task_type: &'static str, task_name: &'a str },
    Recall { /* per-event fields */ },
    Improve { /* ... */ },
    Forget { /* ... */ },
}

impl AnalyticsEvent<'_> {
    pub fn emit(&self, user_id: Option<Uuid>, tenant_id: Option<Uuid>);
}
```

`emit()` is **synchronous and fire-and-forget** — it queues onto the
analytics client's background task and returns immediately. Errors are
swallowed and logged at `debug!` level. This is critical: the proxy may
be unreachable or slow, and pipeline / task hot paths must not block.

`cognee_version`, `tenant_id` formatting, and the `| config` merge are
applied inside the helper so call sites stay tidy.

### Wiring the emission points

| Site | What to add |
|---|---|
| [`crates/core/src/pipeline.rs:486`](../../crates/core/src/pipeline.rs#L486) `execute()` | After `watcher.on_pipeline_run_started`, emit `AnalyticsEvent::PipelineRunStarted`. In each terminal arm (`Ok`, `Cancelled`, `Err`), emit Completed or Errored. Pull `pipeline_name` from `pipeline.name`, `settings` from a new `crate::settings::snapshot_for_telemetry()` injected via `TaskContext` or read from `cognee_lib::config::Config::current()`. |
| [`crates/core/src/pipeline.rs:988`](../../crates/core/src/pipeline.rs#L988) `call_with_retry()` | Emit `TaskStarted` once before the retry loop. Emit `TaskCompleted` in the `Ok(resolved)` arm. Emit `TaskErrored` after the final `last_error` extraction. Use `Task::python_task_type()` (new helper). |
| [`crates/lib/src/api/recall.rs:128`](../../crates/lib/src/api/recall.rs#L128) | Emit `AnalyticsEvent::Recall` immediately before the `info_span!`. All inputs are already in scope. |
| [`crates/lib/src/api/improve.rs:125`](../../crates/lib/src/api/improve.rs#L125) | Emit `AnalyticsEvent::Improve` at the top of `pub async fn improve(...)`, before stage 1. Optionally also wrap the body in a `tracing::info_span!("cognee.api.improve", ...)` to bring OTEL parity with Python. |
| [`crates/lib/src/api/forget.rs:103-123`](../../crates/lib/src/api/forget.rs#L103) | Replace the existing `tracing::info!(target: "cognee.telemetry", ...)` block with `AnalyticsEvent::Forget { ... }.emit()`. |

### Sourcing `cognee_version`

```rust
// crates/lib/src/lib.rs
pub const COGNEE_VERSION: &str = env!("CARGO_PKG_VERSION");
```

The analytics helper reads it from a `cognee_utils::telemetry::version()`
free function that is wired during `cognee_lib` initialisation, so the
helper does not have to depend on `cognee-lib` (avoiding a cycle).

### Sourcing `tenant_id` and `user_id`

`TaskContext` carries `pipeline_ctx.user_id: Option<Uuid>` already
([`pipeline.rs:509`](../../crates/core/src/pipeline.rs#L509)). For task
events, read it from `env.ctx`. For pipeline events, read it from
`run_info.user_id`. For API events, the function arguments already
expose `owner_id` / a `User` (recall takes `user_id`, forget takes
`owner_id`, improve takes `&User`).

`tenant_id` is **not** modelled in Rust today (`PipelineContext` does
not have a `tenant_id` field). For the first cut, emit
`"Single User Tenant"` unconditionally; add a `tenant_id` field to
`PipelineContext` and the API param structs as a follow-up.

### Error capture

The analytics helper must:

1. Never propagate errors (caller uses `_ = event.emit(...)` or a
   non-`Result` return).
2. Spawn the actual HTTP POST on a detached `tokio::task` and rate-limit
   with the per-process channel from Gap 2.
3. When the channel is full or the proxy is unreachable, drop the event
   and log at `debug!` level.

This is what makes pipeline/task emission safe to fire on every iteration
of a hot loop.

### Relationship to OTEL spans

Python emits both. Rust should keep both. The OTEL span is for **operators**
(distributed tracing dashboards) and the analytics event is for **product
telemetry** (anonymous usage statistics). They have different consumers and
different lifecycle:

- OTEL span: structured, has `start/end/duration`, may carry rich attributes
  including PII-like data (search query preview), is sampled.
- Analytics event: flat property dict, fire-and-forget, has no duration,
  **must** be redacted before send.

The `tracing` ecosystem already lets you set `target: "cognee.telemetry"`
and route via a custom subscriber layer, but using
`tracing` for analytics events is fragile (subscribers can be replaced
at runtime, layers can drop events silently, attribute extraction from
`tracing::Event` is awkward). A separate `AnalyticsEvent::emit` API is
cleaner.

---

## Action items

Each item below has a dedicated implementation sub-document under [`03/`](03/)
with rationale, prerequisites, step-by-step source-level changes, verification
commands, files modified, and risks. **The sub-docs are authoritative**: where
they refine details based on the locked design decisions, follow the sub-doc
rather than this high-level summary.

| # | Action item | Sub-doc | Depends on | Status |
|---|---|---|---|---|
| 1 | Thread a real `tenant_id` through `PipelineContext` and `PipelineRunInfo`. Add `tenant_id: Option<Uuid>` field, populate it in `execute()`, fix the 3 literal-construction test sites. Lifecycle event emitters fall back to `"Single User Tenant"` when `None`. | [03/01-tenant-id-plumbing.md](03/01-tenant-id-plumbing.md) | — | ✅ 70e2d8e |
| 2 | Add `Task::python_task_type(&self) -> &'static str` in `crates/core/src/task.rs` (8 variants → 4 strings) and a `cognee_telemetry::cognee_version()` accessor. | [03/02-task-type-mapping.md](03/02-task-type-mapping.md) | — | ⬜ |
| 3 | Implement `cognee_lib::config::Config::telemetry_snapshot()` returning a `serde_json::Map` with the locked allowlist of provider/model fields (decision 5). Used as the `\| config` merge for pipeline events. | [03/03-settings-snapshot.md](03/03-settings-snapshot.md) | — | ⬜ |
| 4 | Wire `Pipeline Run Started/Completed/Errored` in `crates/core/src/pipeline.rs::execute()` (lines 532 / 574 / 584 + 604). Pulls `pipeline_name` from `pipeline.name`, `tenant_id` from `run_info`, and the settings snapshot via the helper from task 03. | [03/04-pipeline-lifecycle-events.md](03/04-pipeline-lifecycle-events.md) | 1, 3 | ⬜ |
| 5 | Wire `${task_type} Task Started/Completed/Errored` in `crates/core/src/pipeline.rs::call_with_retry()`. Once per task, not per attempt: `Started` before the retry loop, `Completed` on first success, `Errored` only after retries exhausted. Uses `Task::python_task_type()` for the event-name template. | [03/05-task-lifecycle-events.md](03/05-task-lifecycle-events.md) | 1, 2 | ⬜ |
| 6 | Emit `cognee.search EXECUTION STARTED` at the top of `SearchOrchestrator::search` to pair with the existing `EXECUTION COMPLETED` emitter. Backfill `tenant_id` on both. | [03/06-search-execution-events.md](03/06-search-execution-events.md) | 1 | ⬜ |
| 7 | Wrap the body of `cognee_lib::api::improve::improve()` in a `tracing::info_span!("cognee.api.improve", ...)` to bring OTEL parity with Python. Bundled into this gap per decision 4. | [03/07-improve-otel-span.md](03/07-improve-otel-span.md) | — | ⬜ |
| 8 | Unit + integration tests: `python_task_type` mapping (8→4); settings allowlist snapshot tests; mockito-driven full-pipeline integration test asserting the 4-event sequence (`Pipeline Run Started`, `Coroutine Task Started`, `Coroutine Task Completed`, `Pipeline Run Completed`); error path; opt-out test; fire-and-forget timing test (proxy stalls 5 s, dispatch returns < 100 ms). | [03/08-tests.md](03/08-tests.md) | 4, 5, 6 | ⬜ |
| 9 | User-facing docs: extend `docs/observability/send_telemetry.md` with the new event catalog (pipeline + task + search). CI updates if needed (existing `cognee-telemetry` lanes from gap 02-12 already cover the crate). | [03/09-docs-and-ci.md](03/09-docs-and-ci.md) | 4, 5, 6, 7 | ⬜ |

### Suggested execution order

A clean PR sequence based on the dependency graph above:

1. **PR 1** (foundation): tasks 01 + 02 + 03 — `tenant_id` plumbing,
   `python_task_type` + version accessor, settings snapshot helper.
   No new event emissions, just the supporting machinery.
2. **PR 2** (lifecycle): tasks 04 + 05 + 06 — pipeline + task + search
   `STARTED` events, all on top of the helpers from PR 1.
3. **PR 3** (parity nice-to-have): task 07 — `cognee.api.improve` OTEL
   span. Independent; could land in PR 1 or 2 as well.
4. **PR 4** (validation): task 08 — unit + integration tests.
5. **PR 5** (closeout): task 09 — user docs + any CI tweaks.

## Design decisions (locked)

These supersede the [Open questions](#open-questions) below — answers were
obtained from the project owner on 2026-05-07 and are the binding contract
for all per-task sub-docs under [`03/`](03/). They build on the gap-02
locked decisions (especially #2 `sdk_runtime: "rust"`, #3 hand-curated
settings subset, #4 SDK-as-single-source, #11 `LLM_API_KEY` read at
emission time).

| # | Decision | Resolution | Implication |
|---|---|---|---|
| 1 | `tenant_id` modelling | **Thread a real value** through `cognee_core::PipelineContext` and `PipelineRunInfo`. Lifecycle emitters fall back to the literal `"Single User Tenant"` when the caller passes `None` (matches Python). Backfilling existing API events (`recall`, `forget`) with `tenant_id` is **out of scope** for gap 03. | [Task 03-01](03/01-tenant-id-plumbing.md) is dedicated to this. Existing API events keep their current (no-tenant) payloads. |
| 2 | Memory-only forget classification | **Closed.** Keep the 3-value `ForgetTarget` enum (`item` / `dataset` / `everything`). Gap 02-07 already shipped `cognee.forget` with this enum. If `data_item_memory_only` / `dataset_memory_only` distinctions are added later, that is a one-line property tweak. | No work in gap 03. |
| 3 | `cognee.search EXECUTION STARTED/COMPLETED` | **Implement both** for byte-equal Python parity. `COMPLETED` already shipped in gap 02-07; [task 03-06](03/06-search-execution-events.md) adds the `STARTED` paired emission at the top of `SearchOrchestrator::search`. | Two events, not one — match Python's flow. |
| 4 | `cognee.api.improve` OTEL span | **Bundle** into this gap as [task 03-07](03/07-improve-otel-span.md). Python has the span; Rust does not. ~30 lines and gives Python parity. | One additional task. Independent of the lifecycle work. |
| 5 | Settings snapshot allowlist (`\| config` merge) | **Hand-curated subset** (per gap-02 decision 3 — _never_ serialize the full `Config`): `vector_db_provider`, `graph_db_provider`, `relational_db_provider`, `llm_provider`, `llm_model`, `embedding_provider`, `embedding_model`, `embedding_dimensions`, `chunk_strategy`, `token_counter`. Plus `sdk_runtime: "rust"` (carried from gap-02 decision 2). | [Task 03-03](03/03-settings-snapshot.md) implements `Config::telemetry_snapshot() -> serde_json::Map`. Adding fields later requires an explicit allowlist edit + test snapshot regen. |
| 6 | `dataset_id` / `pipeline_run_id` on pipeline events | **Mirror Python — omit** from analytics payload. Both remain on the OTEL span attributes. | Pipeline events carry only `pipeline_name`, `cognee_version`, `tenant_id`, plus the curated config snapshot. |
| 7 | Per-attempt vs once-per-task task events | **Once per task.** `Started` fires before the first attempt of `call_with_retry`; `Completed` fires on the first successful attempt; `Errored` fires only after retries are exhausted. Internal retries do not surface to the analytics layer. | Matches Python's mental model (Python has no retry layer at this point). |
| 8 | Sub-doc count & numeric parity | **No numeric parity** with gap 01 / gap 02 required. 9 sub-docs cover the full scope cleanly. | Sub-docs grouped by concern, not by Python sub-feature count. |
| 9 | Commit-message scope | **`telemetry/events-03-NN`** — distinct from gap 02's `telemetry/send-02-NN` so log searches stay clean. | Always include the standard `Co-Authored-By` trailer via heredoc. |

---

## Backward compatibility with the existing `cognee.telemetry` log target

> **Status:** This concern is **closed**. Gap 02-07 (commit `8b096bb`)
> already replaced the `tracing::info!(target: "cognee.telemetry", …)`
> block in [`crates/lib/src/api/forget.rs:114`](../../crates/lib/src/api/forget.rs#L114)
> with a real `cognee_telemetry::send_telemetry` call. Property keys
> now match Python (`target` instead of `forget_target`), and the
> wire-format event name is `"cognee.forget"`. No further work needed
> in gap 03.

The original migration plan kept here for reference:

- Remove the `target: "cognee.telemetry"` line in `forget.rs`. ✅ done.
- Replace it with `cognee_telemetry::send_telemetry(...)`. ✅ done.
- Align property keys with Python (`target_label` → `target`). ✅ done.
- Feature-gate behind `telemetry` so behaviour is unchanged when
  disabled. ✅ done (`#[cfg(feature = "telemetry")]`).

---

## Open questions

These were superseded by the [Design decisions (locked)](#design-decisions-locked)
table above on 2026-05-07. Kept here as a paper trail of the original
questions and the rationale considered before locking.

1. ~~**`tenant_id` modelling.**~~ Resolved by decision 1 — thread a real
   `Option<Uuid>` through `PipelineContext` + `PipelineRunInfo`; fall back
   to literal `"Single User Tenant"` when the caller passes `None`.
2. ~~**Memory-only forget classification.**~~ Resolved by decision 2 —
   keep the 3-value `ForgetTarget`; do not extend the enum in this gap.
3. **HTTP-router-level events.** Still out of scope — track separately
   in the http-api-v2 work. Not affected by gap 03.
4. ~~**`cognee.search EXECUTION STARTED/COMPLETED`.**~~ Resolved by
   decision 3 — implement both, for byte-equal Python parity.
5. ~~**Settings snapshot scope.**~~ Resolved by decision 5 — hand-curated
   allowlist of provider/model fields only.
6. ~~**Wire `cognee.api.improve` OTEL span.**~~ Resolved by decision 4 —
   bundle as [task 03-07](03/07-improve-otel-span.md).
7. ~~**Dataset ID on pipeline events.**~~ Resolved by decision 6 — match
   Python, omit.
8. ~~**`pipeline_run_id` on pipeline events.**~~ Resolved by decision 6
   — match Python, omit.
9. ~~**Per-attempt vs once-per-task task events.**~~ Resolved by decision
   7 — once per task.

---

## Testing strategy

### Unit tests

- `crates/utils/src/telemetry/events.rs` (or wherever the enum lives):
  - Each variant serialises to the expected JSON property dict.
  - `Task::python_task_type` returns the expected string for every
    `Task::*` variant (8 variants → 4 distinct strings).
  - `ForgetTarget`-to-`target`-string mapping covers all 6 values.

### Integration tests with a fake proxy

- Reuse the fake proxy server from Gap 2's tests
  (an `axum` handler that records POST bodies into a `Vec`).
- Drive a tiny pipeline through `crates/core/src/pipeline::execute()`
  with one `Task::Async` task and assert the proxy received exactly
  three events: `Pipeline Run Started`, `Coroutine Task Started`,
  `Coroutine Task Completed`, `Pipeline Run Completed` — in that
  order.
- Inject a failing task and assert `Coroutine Task Errored` and
  `Pipeline Run Errored` are emitted with the right `pipeline_name` /
  `task_name`.
- Drive `recall()`, `improve()`, `forget()` from
  `crates/lib/src/api/` against the fake proxy and assert one POST
  per call with the right event name and required keys.
- Verify with `TELEMETRY_DISABLED=1`: zero POSTs should be received
  for any of the above scenarios.

### Snapshot tests for property-dict shape

- `insta::assert_json_snapshot!` on each `AnalyticsEvent`'s rendered
  payload — locks the wire format. Re-snap on intentional changes.

### Non-blocking semantics

- Configure the fake proxy with a 5-second sleep before responding.
  Pipeline execution must not stall (assert wall-clock < 100 ms for a
  no-op pipeline). This validates the fire-and-forget contract.

---

## References

- Python pipeline lifecycle:
  [`run_tasks_with_telemetry.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_with_telemetry.py)
- Python task lifecycle:
  [`run_tasks_base.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks_base.py)
- Python task type derivation:
  [`tasks/task.py:194-207`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/tasks/task.py#L194-L207)
- Python API events:
  - [`recall.py:402`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/recall.py#L402)
  - [`improve.py:91`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/improve.py#L91)
  - [`forget.py:79`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L79)
  - [`remember.py:624`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L624)
  - [`search.py:74`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/methods/search.py#L74)
- Rust target sites:
  - [`crates/core/src/pipeline.rs::execute()`](../../crates/core/src/pipeline.rs#L486)
  - [`crates/core/src/pipeline.rs::call_with_retry()`](../../crates/core/src/pipeline.rs#L960)
  - [`crates/core/src/task.rs::Task` enum](../../crates/core/src/task.rs#L237)
  - [`crates/lib/src/api/recall.rs:128`](../../crates/lib/src/api/recall.rs#L128)
  - [`crates/lib/src/api/improve.rs:125`](../../crates/lib/src/api/improve.rs#L125)
  - [`crates/lib/src/api/forget.rs:103`](../../crates/lib/src/api/forget.rs#L103)
- Companion gaps:
  - [02-send-telemetry-analytics.md](02-send-telemetry-analytics.md) — transport, identity, opt-out (closed 2026-05-06)
  - [gap-analysis.md](gap-analysis.md) — the parent index (do not edit)
- Per-task sub-docs: [03/](03/)
- Implementation runbook: [03/00-implementation-runbook.md](03/00-implementation-runbook.md)
