# 03 — Pipeline / Task / API Operation Events

## Overview

Python `cognee` ships a small fixed catalog of product-analytics events sent
through `send_telemetry(...)` (see [01-analytics-client.md](01-analytics-client.md)
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
opt-out) is the subject of [01-analytics-client.md](01-analytics-client.md);
this document depends on that client existing.

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
| `cognee.recall` | [`api/v1/recall/recall.py:402`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/recall.py#L402) — emitted **before** the OTEL span / cloud dispatch | `query_length`, `scope`, `auto_route`, `top_k`, `search_type` (`str(query_type.value)` or `"auto"`), `session_id`, `datasets` (comma-joined names), `dataset_ids` (comma-joined UUIDs), `cognee_version` | [`crates/lib/src/api/recall.rs:128`](../../crates/lib/src/api/recall.rs#L128) — just before `tracing::info_span!("cognee.api.recall", ...)` | **None.** OTEL span present. |
| `cognee.improve` | [`api/v1/improve/improve.py:91`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/improve.py#L91) — before any work, before the OTEL span | `dataset` (str), `session_count`, `session_ids` (comma-joined), `run_in_background`, `cognee_version` | [`crates/lib/src/api/improve.rs:125`](../../crates/lib/src/api/improve.rs#L125) — top of `pub async fn improve(...)` | **None.** No OTEL span either; only per-stage `info!`/`warn!` log lines. |
| `cognee.forget` | [`api/v1/forget/forget.py:79`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L79) — before the OTEL span | `target` (one of `everything` / `data_item_memory_only` / `dataset_memory_only` / `data_item` / `dataset` / `unknown`), `dataset` (str), `data_id` (str), `cognee_version` | [`crates/lib/src/api/forget.rs:103-123`](../../crates/lib/src/api/forget.rs#L103) | **Partial.** A `tracing::info!(target: "cognee.telemetry", ...)` shim exists. The property keys differ (`forget_target` vs Python's `target`, no `cognee.forget` event name on the wire). |
| `cognee.remember` | [`api/v1/remember/remember.py:624`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L624) — inside the `cognee.api.remember` OTEL span | `mode` (`session` / `permanent`), `dataset_name`, `data_size_bytes`, `item_count`, `session_id`, `self_improvement`, `run_in_background`, `cognee_version` | [`crates/lib/src/api/remember.rs`](../../crates/lib/src/api/remember.rs) — top of public `remember()` | **None.** Not in scope for this gap per task brief but listed for completeness. |
| `cognee.search EXECUTION STARTED` | [`modules/search/methods/search.py:74`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/methods/search.py#L74) | `cognee_version`, `tenant_id` | Search has no Rust analog at this layer; the Rust port routes search through `cognee.recall` (which already has its own event). | **N/A.** |
| `cognee.search EXECUTION COMPLETED` | [`modules/search/methods/search.py:115`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/methods/search.py#L115) | `cognee_version`, `tenant_id` | same as above | **N/A.** |

> Notes on the "EXECUTION STARTED/COMPLETED" pair: this lives in the
> internal Python `search()` method that `recall()` ultimately calls. It is
> redundant with `cognee.recall` for the SDK surface and the Rust port does
> not have a separate corresponding internal entry point — recommend not
> implementing them.

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

1. **Define the event enum** in
   `crates/utils/src/telemetry/events.rs` (or wherever the Gap 2 client
   lands). One variant per event listed in the catalog.
2. **Implement `emit()`** as a non-blocking fire-and-forget that
   forwards into the Gap 2 client's send queue.
3. **Add `Task::python_task_type(&self) -> &'static str`** in
   `crates/core/src/task.rs` near the existing `pub enum Task`.
4. **Add `cognee_version` accessor.** Either:
   - `pub const COGNEE_VERSION: &str = env!("CARGO_PKG_VERSION")` in
     `crates/lib/src/lib.rs`, or
   - free function `cognee_utils::telemetry::cognee_version()` set at
     init via a `OnceLock`.
5. **Pipeline lifecycle wiring** — modify
   `crates/core/src/pipeline.rs::execute()`:
   - line 532: add `PipelineRunStarted` emit
   - line 574: add `PipelineRunCompleted` emit
   - lines 584 & 604: add `PipelineRunErrored` emits (one shared helper)
6. **Task lifecycle wiring** — modify
   `crates/core/src/pipeline.rs::call_with_retry()`:
   - just before line 988 (loop start): emit `TaskStarted`
   - line 1033 (success path): emit `TaskCompleted`
   - lines 1065-1069 (terminal failure): emit `TaskErrored`
7. **Recall API event** — add at
   `crates/lib/src/api/recall.rs:128` (before the span), wired to
   the `RecallParams`-derived field set.
8. **Improve API event** — add at
   `crates/lib/src/api/improve.rs:125` (top of `improve()`).
   Optionally also add a `cognee.api.improve` `tracing::info_span!`.
9. **Forget API event** — replace the existing
   `tracing::info!(target: "cognee.telemetry", ...)` shim at
   `crates/lib/src/api/forget.rs:103-123` with the new emitter.
   Update property keys: `forget_target` → `target`, add the literal
   event name `"cognee.forget"`.
10. **Telemetry settings snapshot** — add
    `cognee_lib::config::Config::telemetry_snapshot()` returning a
    `serde_json::Map` with the same shape as Python's
    `get_current_settings()`. This is the `| config` merge.
11. **Add `tenant_id` (follow-up).** Thread through
    `PipelineContext` and API params; until done, emit
    `"Single User Tenant"`.
12. **Tests** — see Testing strategy.

---

## Backward compatibility with the existing `cognee.telemetry` log target

The current Rust port has **one** telemetry-shaped emission: a
`tracing::info!(target: "cognee.telemetry", ...)` block in
[`crates/lib/src/api/forget.rs:103-123`](../../crates/lib/src/api/forget.rs#L103).
It does not POST anywhere — it is a structured log line that an
operator could route via a custom `tracing-subscriber` layer.

**Decision: replace, do not duplicate.**

- Remove the `target: "cognee.telemetry"` line in `forget.rs`.
- Replace it with `AnalyticsEvent::Forget { ... }.emit(...)`.
- Property naming aligns with Python: rename the local variable
  `target_label` → just pass `target` through.
- The new emitter is feature-gated behind the same `telemetry`
  feature flag, so behaviour is unchanged when the feature is
  disabled at compile time.

If there is a downstream consumer using `target: "cognee.telemetry"`
as a log filter, document it as a breaking change in the release
notes for the version that lands the new emitter. (No such consumer
is known internally.)

---

## Open questions

1. **`tenant_id` modelling** — Python's `User` has a `tenant_id`
   column. The Rust `User` model in
   `crates/models/src/user/` does not have one (verify). Until it
   does, the literal `"Single User Tenant"` is emitted. Confirm
   priority of adding tenant scoping vs deferring.
2. **Memory-only forget classification** — the current Rust
   `ForgetTarget` enum has `Item`, `Dataset`, `All`. Python
   distinguishes `data_item_memory_only` and `dataset_memory_only`.
   Where does the Rust port carry the `memory_only` flag? Either
   extend the enum or read a separate flag.
3. **HTTP-router-level events.** Python emits per-route
   `cognee.<verb> API Call` events from FastAPI handlers. The Rust
   `serve.rs` HTTP server is a separate code path. Out of scope for
   this gap; track separately in the http-api-v2 work.
4. **`cognee.search EXECUTION STARTED/COMPLETED`.** Recommend
   *not* implementing — the Rust SDK collapses internal `search()`
   into `recall()` and the `cognee.recall` event already covers it.
5. **Settings snapshot scope** — Python merges the *entire* settings
   dict into `Pipeline Run *` events. Some fields (DB URLs, model
   paths) may contain semi-sensitive data. The Gap 2 redactor is
   responsible for stripping them, but we should agree on a
   redaction allow-list before turning it on.
6. **Wire `cognee.api.improve` OTEL span** — Python has it; Rust does
   not. Bundle with this gap, or split out?
7. **Dataset ID on pipeline events.** Python does not include
   `dataset_id` in the analytics payload, only on the OTEL span. The
   task brief asked specifically — the answer is **no**, Python omits
   it. Match that.
8. **`pipeline_run_id` on pipeline events.** Same as above — Python
   does not emit it on the analytics call, only on the OTEL span. We
   should mirror Python; if we want to add it as an extension that is
   a separate decision.
9. **Per-attempt vs once-per-task task events.** Python has no
   retry policy at this layer, so the question does not arise. We have
   chosen "once per task" (matching the user's mental model of a task)
   rather than "once per attempt" (matching the wire-level retries).

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
  - [01-analytics-client.md](01-analytics-client.md) — transport, identity, opt-out
  - [gap-analysis.md](gap-analysis.md) — the parent index (do not edit)
