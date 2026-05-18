# HTTP Server — Pipeline Runs & Background Tasks

This document specifies how the Rust HTTP server tracks long-running pipeline operations (`/cognify`, `/memify`, `/remember`, `/improve`, `/sync`, `/add`) when the caller passes `run_in_background=true`. The component that owns this lifecycle is **`cognee_core::PipelineRunRegistry`** — a runtime-agnostic registry that hosts a per-run in-memory event channel, satisfies the existing `cognee_core::PipelineWatcher` trait so library functions can publish lifecycle events without knowing about it, and persists durable status rows via an injected `PipelineRunRepository` trait. The HTTP server consumes the registry through `AppState` and binds it as the `PipelineWatcher` in every `TaskContext` it builds.

Library functions in `cognee-lib` stay synchronous (callers `.await` them to completion). The `run_in_background` flag is purely a **hosting concern**: the HTTP server decides whether to await the future inline or hand it to the registry's spawn path. There is no `run_in_background` flag in the library API.

Companion docs: [plan.md](plan.md), [architecture.md](architecture.md), [auth.md](auth.md), [websocket.md](websocket.md) (consumes the channel exposed here), [routers/cognify.md](routers/cognify.md), [routers/memify.md](routers/memify.md), [routers/remember.md](routers/remember.md), [routers/improve.md](routers/improve.md).

## 1. Goals & non-goals

### Goals

- **One reusable component** at `cognee_core::PipelineRunRegistry` — usable by the HTTP server, the CLI, the MCP, embedders, and any other host that needs background lifecycle tracking.
- **Two-tier visibility, matching Python**: a *durable* `pipeline_runs` table that survives restarts and powers historical queries (`GET /api/v1/datasets/status`, `GET /api/v1/activity/pipeline-runs`), and a *volatile* per-run event channel that powers live WebSocket subscriptions during a run.
- **Library API stays unchanged**: `cognee_lib::cognify::cognify(...)`, `cognee_lib::cognify::memify::memify(...)`, `cognee_ingestion::AddPipeline::run(...)`, etc. remain synchronous (note: `cognee_lib::add` is a *module*, not a function — the entry point is `AddPipeline::run`). They publish lifecycle events through the existing `cognee_core::PipelineWatcher` trait.
- **Wire-compatible status enum on the wire**: the `pipeline_runs.status` column and the WebSocket frame's `status` field both use Python's `DATASET_PROCESSING_*` and `PipelineRun*` strings. The mapping from `cognee_core::PipelineRunStatus` to those wire strings happens in the HTTP DTO layer.
- **Wire-compatible event shape**: events emitted to the WebSocket carry the same `{pipeline_run_id, status, payload}` JSON used by [Python's WS handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L312-L345).
- **Deterministic IDs available**: when the caller has them, `pipeline_run_id` is `uuid5(NAMESPACE_OID, "{pipeline_id}_{dataset_id}")` (Python parity). When the caller does not (e.g. an embedder running ad-hoc work), the registry auto-generates one via `Uuid::new_v4()`.
- **Bounded resource use, configurable**: the in-memory registry has a generous default cap; eviction evicts oldest *finished* runs first, never running ones; operators can raise the cap to unbounded.
- **Recoverable**: on graceful shutdown, in-flight runs are `abort()`-ed and written as `DATASET_PROCESSING_ERRORED` rows so a restart presents an honest state. **This is one acknowledged divergence from Python**, where shutdown leaves the rows as `STARTED` indefinitely.

### Non-goals

- **No `run_in_background` in `cognee-lib`.** Library callers who want background execution `tokio::spawn` it themselves or use the registry directly. The HTTP server is the only first-class consumer of the background path.
- **Cross-process / cross-replica fan-out**: the in-memory registry is local to one process. Multi-replica deployments need either sticky-session WebSocket routing or a Redis-backed channel — out of scope.
- **Pause / resume / cancel from HTTP**: not exposed. The registry has `abort(run_id)` for shutdown, but no public HTTP endpoint to invoke it. Once a run starts, it runs to completion or errors out (matches Python).
- **Streaming logs to the client**: the WebSocket emits *status events* with formatted graph payloads, not per-task log lines. Stdout/stderr stays in `tracing`.

## 2. Library refactor prerequisite

Two existing library functions ship their own background machinery and must be refactored before the registry becomes the single source of truth. The refactor is part of this work, not a follow-up.

| Library function | What needs to change | Source |
|---|---|---|
| `cognee_lib::api::remember::remember()` | Drops the `run_in_background: bool` parameter and the bespoke `RememberResult` / `JoinHandle` shared-state machinery. Returns a synchronous `Result<RememberResult, Error>` that always reflects the completed-or-errored run. The HTTP `/remember` handler is what spawns the background task via `PipelineRunRegistry::register_background(...)`. The `RememberResult` struct keeps its observable fields (`status`, `data_size`, `pipeline_run_id`, `error`, etc.) but loses the `JoinHandle` and the `await_completion()` method. | [crates/lib/src/api/remember.rs:75-107](../../crates/lib/src/api/remember.rs#L75-L107), [:236-336](../../crates/lib/src/api/remember.rs#L236-L336), [:503-700](../../crates/lib/src/api/remember.rs#L503-L700) |
| `cognee_lib::api::improve::improve()` | Drops the `run_in_background: bool` parameter. The `has_sessions && !run_in_background` skip-condition collapses (always run when sessions are present, since the function is now sync). The HTTP `/improve` handler is what spawns the background task via the registry. | [crates/lib/src/api/improve.rs:59](../../crates/lib/src/api/improve.rs#L59), [:197-198](../../crates/lib/src/api/improve.rs#L197-L198) |

After the refactor, `grep -rn run_in_background crates/lib crates/cognify crates/ingestion` returns zero matches. Other `tokio::spawn` calls in the library tree (`crates/cognify/src/summarization/extractor.rs`, `crates/cognify/src/fact_extraction/extractor.rs`, `crates/cognify/src/tasks.rs`) are **internal parallelism** within a pipeline — not background-mode dispatch — and stay as-is.

## 3. Status taxonomy and wire mapping

Three distinct enums coexist; the mapping is well-defined.

### 3.1 `cognee_core::PipelineRunStatus` (already exists)

The library's lifecycle enum, defined at [crates/core/src/pipeline.rs:311](../../crates/core/src/pipeline.rs#L311):

```rust
pub enum PipelineRunStatus {
    Initiated,
    Started,
    Completed,
    Errored,
}
```

`Display` emits `INITIATED` / `STARTED` / `COMPLETED` / `ERRORED`. This is the form the registry uses internally.

### 3.2 Durable status — written to `pipeline_runs.status`

Python's wire format prefixes everything with `DATASET_PROCESSING_`. The HTTP DTO layer + the SeaORM column mapper translate `cognee_core::PipelineRunStatus` to Python-prefixed strings:

| `cognee_core::PipelineRunStatus` | DB column value (Python parity) |
|---|---|
| `Initiated` | `"DATASET_PROCESSING_INITIATED"` |
| `Started` | `"DATASET_PROCESSING_STARTED"` |
| `Completed` | `"DATASET_PROCESSING_COMPLETED"` |
| `Errored` | `"DATASET_PROCESSING_ERRORED"` |

Mirrors Python's [`PipelineRunStatus`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py#L8-L12) byte-for-byte.

### 3.3 Live event status — emitted on the registry channel and the WebSocket frame

Python's [`PipelineRunInfo` subclasses](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py) define the wire enum:

| Value (string in JSON) | When emitted |
|---|---|
| `PipelineRunStarted` | First event after the task begins. |
| `PipelineRunYield` | Mid-pipeline progress (per-batch / per-stage). |
| `PipelineRunCompleted` | Terminal success — closes the channel for `Completed`-only WS clients (Python parity, see [websocket.md §6](websocket.md#6-status-semantics--terminal-close)). |
| `PipelineRunErrored` | Terminal failure — does **not** close the channel (Python parity); the error is conveyed in the JSON `status` field. |
| `PipelineRunAlreadyCompleted` | The run had already completed when re-invoked; emitted once. Does not close the channel (Python parity). |

The mapping to durable status:

| Live event | Durable status |
|---|---|
| `PipelineRunStarted` | `DATASET_PROCESSING_STARTED` |
| `PipelineRunYield` | (no DB write — too noisy, matches Python) |
| `PipelineRunCompleted` | `DATASET_PROCESSING_COMPLETED` |
| `PipelineRunErrored` | `DATASET_PROCESSING_ERRORED` |
| `PipelineRunAlreadyCompleted` | (no DB write — DB already says completed) |

The HTTP DTO layer in `crates/http-server/src/dto/pipeline_run.rs` owns both translations (`PipelineRunStatus` ↔ `DATASET_PROCESSING_*`, registry event ↔ `PipelineRun*`).

### 3.4 Four-state lifecycle on the `pipeline_runs` table

After [telemetry gap 08](../telemetry/08-pipeline-run-status.md), every pipeline (cognify, memify, ingestion) — whether invoked through the HTTP server, the CLI, or as a library call — writes the full Python-faithful four-state trail to the `pipeline_runs` table. Each transition is a **new row** sharing the same `pipeline_run_id`; the latest row by `created_at` defines the current state.

```
INITIATED → STARTED → (COMPLETED | ERRORED)
```

| State | When written | `run_info` JSON body |
|---|---|---|
| `INITIATED` | `cognee_core::pipeline::execute` emits before the first task begins (Option A; Decision 1). | `{}` |
| `STARTED` | `pipeline::execute` emits as the first task starts running. | `{"data": ["<uuid>", "<uuid>", …]}` or `{"data": "None"}` when the input has no `Data` items. |
| `COMPLETED` | `pipeline::execute` emits after the last task succeeds. | Same shape as `STARTED`. |
| `ERRORED` | `pipeline::execute` emits when any task fails (top-level error handler). | `{"data": [...], "error": "<message>"}` |

`data_info` (the JSON serialised under `"data"`) is byte-identical to Python's helper: a `[String]` array of stringified `Data.id`s for `Vec<Data>` inputs, the literal string `"None"` for empty inputs, and `format!("{:?}", input)` for repr-fallback inputs. The helper lives at `cognee_core::pipeline_run_registry::data_info`.

`GET /api/v1/activity/pipeline-runs` projects the latest row per `(pipeline_name, dataset_id)` and returns the `DATASET_PROCESSING_*` wire string for `status`. The library-side API surface (`reset_pipeline_run_status`, `reset_dataset_pipeline_run_status`, the reader trio on `PipelineRunRepository`, and the `check_pipeline_run_qualification` gate) is documented under [`docs/telemetry/08-pipeline-run-status.md` § Closure summary](../telemetry/08-pipeline-run-status.md#closure-summary).

## 4. Identifiers

### 4.1 `pipeline_id` (deterministic)

`cognee-core` already exposes the helper at [crates/core/src/pipeline.rs:333](../../crates/core/src/pipeline.rs#L333) (`deterministic_pipeline_id`). The HTTP layer wraps it as a public free function for ergonomics:

```rust
pub fn pipeline_id(user_id: Uuid, dataset_id: Uuid, pipeline_name: &str) -> Uuid {
    let s = format!("{}{}{}", user_id, pipeline_name, dataset_id);
    Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes())
}
```

Same algorithm as [Python](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_id.py).

### 4.2 `pipeline_run_id` (deterministic, derived)

```rust
pub fn pipeline_run_id(pipeline_id: Uuid, dataset_id: Uuid) -> Uuid {
    let s = format!("{}_{}", pipeline_id, dataset_id);
    Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes())
}
```

Same as [Python](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_run_id.py). Note: this id is **not unique across separate runs** of the same pipeline — Python intentionally reuses it so a re-cognify of the same dataset emits the same `pipeline_run_id`. Multiple `pipeline_runs` rows can share the same `pipeline_run_id` value with different `created_at`. Don't make `pipeline_run_id` a primary key; the `id` column does that.

### 4.3 Caller-provided vs auto-generated

The registry's `register*` methods accept `Option<Uuid>` for the run id:

- **`Some(id)`** — caller computed it via §4.2. The HTTP server always supplies the deterministic id so cross-SDK reads work.
- **`None`** — registry calls `Uuid::new_v4()` and returns it on the handle. Used by ad-hoc embedders that don't care about id stability.

## 5. Database persistence — `pipeline_runs` table

Mirrors [Python](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py#L15-L26):

| Column | Type | Notes |
|---|---|---|
| `id` | UUID PK | `Uuid::new_v4()`, one row per status transition. |
| `created_at` | TIMESTAMPTZ NOT NULL DEFAULT NOW() | — |
| `status` | TEXT NOT NULL | One of the `DATASET_PROCESSING_*` strings (§3.2). Indexed. |
| `pipeline_run_id` | UUID NOT NULL INDEX | Deterministic from `(pipeline_id, dataset_id)`, or `Uuid::new_v4()` for ad-hoc runs. |
| `pipeline_name` | TEXT NOT NULL | `"cognify_pipeline"`, `"memify_pipeline"`, etc. |
| `pipeline_id` | UUID NOT NULL INDEX | Deterministic from `(user, dataset, pipeline_name)`. |
| `dataset_id` | UUID NOT NULL INDEX | — |
| `run_info` | JSONB NULL | Free-form: error message, item counts, etc. |

SeaORM migration in `crates/database/src/migrator/`. Indexes on `pipeline_run_id`, `pipeline_id`, `dataset_id` to make `/datasets/status` and `/activity/pipeline-runs` cheap.

### 5.1 Writing pattern

Each status transition writes a **new row** rather than updating in place. This matches Python and gives us an audit trail for free:

```sql
INSERT INTO pipeline_runs (id, status, pipeline_run_id, pipeline_id, pipeline_name, dataset_id, run_info)
VALUES ($1, 'DATASET_PROCESSING_STARTED', $2, $3, $4, $5, $6);
```

`/datasets/status?dataset=…` reads the *latest* row per `dataset_id` filtered by `pipeline_name` — same query Python uses (window function, see [`get_pipeline_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/get_pipeline_status.py)).

### 5.2 The `PipelineRunRepository` trait

To keep `cognee-core` free of a hard dependency on `cognee-database`, the registry takes the persistence layer as an injected trait. The trait lives in `cognee-database` and is satisfied by a SeaORM-backed implementation; consumers can substitute test doubles.

```rust
// crates/database/src/pipelines/repository.rs
#[async_trait]
pub trait PipelineRunRepository: Send + Sync {
    /// Insert one row representing a status transition. Returns the row's `id`.
    async fn log_pipeline_run(
        &self,
        pipeline_run_id: Uuid,
        pipeline_id: Uuid,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
        status: PipelineRunStatus,
        run_info: Option<serde_json::Value>,
    ) -> Result<Uuid, DbError>;

    /// Latest status per dataset for a given pipeline name (window-function query).
    async fn latest_status(
        &self,
        dataset_ids: &[Uuid],
        pipeline_name: &str,
    ) -> Result<HashMap<Uuid, PipelineRunStatus>, DbError>;

    /// Recent runs for the activity router, with optional dataset filter.
    async fn list_recent(
        &self,
        dataset_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<PipelineRunRow>, DbError>;

    /// Restart-orphan reset: rewrite any row stuck in `INITIATED`/`STARTED`
    /// without a successor to `ERRORED` with the given `reason`.
    async fn reset_orphans(&self, reason: &str) -> Result<u64, DbError>;
}
```

`cognee-core` declares `PipelineRunRepository` in its public re-exports as a *re-export from `cognee-database`*; the registry holds an `Arc<dyn PipelineRunRepository>`. **`cognee-core` does not depend on `cognee-database` directly** — both depend on a shared `cognee-database-traits` crate (or the trait lives in `cognee-core` and the impl in `cognee-database`; the precise crate boundary is an implementation detail). The decisive constraint: the registry only sees the trait.

## 6. `cognee_core::PipelineRunRegistry` — the new component

### 6.1 Location and feature gating

| Property | Value |
|---|---|
| Crate | `cognee-core` |
| Module | `cognee_core::pipeline_run_registry` (re-exported at the crate root) |
| Feature flag | `pipeline-run-registry` (off by default to keep the core's footprint small for embedders that don't need it; enabled by default in `cognee-lib` and `cognee-http-server`) |

### 6.2 Public types

```rust
// crates/core/src/pipeline_run_registry.rs

use std::pin::Pin;
use std::sync::Arc;
use chrono::{DateTime, Utc};
use futures::Stream;
use uuid::Uuid;

/// Per-run handle returned by `register_*`. Cheap to clone and share.
#[derive(Clone)]
pub struct RunHandle {
    pub run_id:       Uuid,
    pub task_run_id:  Uuid,         // pipeline_runs.id of the latest row written
    pub user_id:      Option<Uuid>,
    pub dataset_id:   Option<Uuid>,
    pub pipeline_name: String,
    pub started_at:   DateTime<Utc>,
}

/// One event in a run's lifecycle.
#[derive(Clone, Debug)]
pub struct RunEvent {
    pub run_id:   Uuid,
    pub kind:     RunEventKind,
    /// Free-form payload. The HTTP layer fills this with the formatted graph
    /// snapshot for cognify; other pipelines can leave it empty.
    pub payload:  serde_json::Value,
    pub at:       DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub enum RunEventKind {
    Started,
    Yield,
    Completed,
    Errored { message: String },
    AlreadyCompleted,
}

/// Snapshot of a run's high-level phase. Cheap to read; never blocks the producer.
#[derive(Clone, Debug)]
pub enum RunPhase {
    Pending,
    Running,
    Completed,
    Errored { message: String },
}

/// Builder-style metadata for a new run.
pub struct RunSpec {
    pub run_id:        Option<Uuid>,    // None → auto-generate uuid v4
    pub pipeline_name: String,
    pub user_id:       Option<Uuid>,
    pub dataset_id:    Option<Uuid>,
}

/// Configurable bounds. Defaults are documented inline.
#[derive(Clone)]
pub struct RegistryConfig {
    /// Max in-memory active+finished runs. Default: 4096. Set to `usize::MAX` for unbounded.
    pub max_in_memory_runs: usize,
    /// How long to retain finished runs in memory after their terminal event. Default: 1 hour.
    pub finished_retention: std::time::Duration,
    /// Per-run event channel capacity. Default: 64. Slow subscribers past this limit are dropped.
    pub channel_capacity: usize,
    /// Optional yield-event throttle. Default: None (emit every yield).
    pub yield_throttle:   Option<std::time::Duration>,
    /// Whether to write `DATASET_PROCESSING_ERRORED` rows on `abort()` during shutdown. Default: true.
    /// Set to `false` to match Python's "leave orphan rows" behavior strictly.
    pub abort_writes_errored_row: bool,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            max_in_memory_runs:         4096,
            finished_retention:         std::time::Duration::from_secs(3600),
            channel_capacity:           64,
            yield_throttle:             None,
            abort_writes_errored_row:   true,
        }
    }
}

/// The registry trait. Implementation lives in `cognee-core`.
#[async_trait::async_trait]
pub trait PipelineRunRegistry: Send + Sync {
    /// Register a new run and run its `work` future inline (caller awaits to completion).
    /// The future is wrapped so its lifecycle events flow through the registry's `PipelineWatcher`.
    async fn register_inline(
        &self,
        spec: RunSpec,
        work: PipelineFuture,
    ) -> Result<RunOutcome, RegistryError>;

    /// Register a new run and spawn `work` on the runtime. Returns immediately
    /// with the handle; `subscribe(handle.run_id)` lets observers tail events.
    async fn register_background(
        &self,
        spec: RunSpec,
        work: PipelineFuture,
    ) -> Result<RunHandle, RegistryError>;

    /// Subscribe to the event stream for an existing run. Returns an empty
    /// placeholder if the run id is unknown (matches Python's `initialize_queue` semantics).
    fn subscribe(
        &self,
        run_id: Uuid,
    ) -> Pin<Box<dyn Stream<Item = RunEvent> + Send + 'static>>;

    /// Snapshot the current high-level phase. Returns `None` for unknown runs.
    fn snapshot_status(&self, run_id: Uuid) -> Option<RunPhase>;

    /// Abort an in-flight run. Used by graceful shutdown and tests.
    async fn abort(&self, run_id: Uuid) -> Result<(), RegistryError>;

    /// Shut down the registry: abort every in-flight run, write `ERRORED` rows
    /// (when `abort_writes_errored_row=true`), and drain the channels.
    async fn shutdown(&self) -> Result<(), RegistryError>;
}

/// A boxed future that returns a result through the watcher; the registry
/// does not require the future to return a meaningful value, only that it
/// reaches a terminal `PipelineWatcher` event.
pub type PipelineFuture =
    Pin<Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>>
        + Send + 'static>>;

#[derive(Debug)]
pub enum RegistryError { /* ... */ }
```

### 6.3 The registry implements `PipelineWatcher`

The internal trick: when `register_*` wraps `work`, it also constructs a per-run `PipelineWatcher` proxy that forwards every event to the run's slot. Library functions don't see the registry — they see a `PipelineWatcher` injected via the `TaskContext` builder. **Note: the current `TaskContext` ([crates/core/src/task_context.rs:53](../../crates/core/src/task_context.rs#L53)) holds an `exec_status` field but no `pipeline_watcher` slot — adding one is part of the cognee-core changes that ship alongside `PipelineRunRegistry` (also in P3).** Concretely (post-refactor):

```rust
pub struct ScopedRunWatcher {
    run_id: Uuid,
    sink:   PerRunSink,            // owned by the registry
    db:     Arc<dyn PipelineRunRepository>,
}

#[async_trait]
impl PipelineWatcher for ScopedRunWatcher {
    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        self.db.log_pipeline_run(run.run_id, run.pipeline_id, &run.pipeline_name,
                                 run.dataset_id, PipelineRunStatus::Started, None).await.ok();
        self.sink.publish(RunEvent { run_id: self.run_id, kind: RunEventKind::Started, ... }).await;
    }
    async fn on_pipeline_run_completed(&self, run: &PipelineRunInfo, _: usize) { … }
    async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, err: &str) { … }
    // task-level events are passed through but not surfaced to the registry channel
}
```

Library code that already calls `watcher.on_pipeline_run_started(...)` (etc.) automatically participates — no library API change. The dispatcher in §7 attaches `ScopedRunWatcher` to the `TaskContext` it builds for every spawn.

### 6.4 Channel implementation

Internally the registry uses `tokio::sync::broadcast` for per-run fan-out (multiple WS subscribers). At the *public API* it returns a runtime-agnostic `Pin<Box<dyn Stream<Item = RunEvent> + Send>>`. The `Stream` impl wraps a `BroadcastStream` that drops `Lagged` errors and surfaces them as a `RunEvent { kind: Errored { message: "subscriber lagged" }, .. }` so consumers (the WS handler) can map it to a 1011 close frame. `tokio` is a transitive dependency of `cognee-core` already, so this introduces no new deps.

## 7. Background task lifecycle (HTTP server side)

The HTTP server's per-handler dispatcher (in `crates/http-server/src/pipelines/dispatch.rs`) owns the tiny amount of glue between `AppState`, the registry, and the library function:

```rust
async fn dispatch_pipeline<F, Fut>(
    state: &AppState,
    user: &AuthenticatedUser,
    pipeline_name: &str,
    dataset_id: Option<Uuid>,
    run_in_background: bool,
    work: F,
) -> Result<DispatchOutcome, ApiError>
where
    F: FnOnce(Arc<TaskContext>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), PipelineError>> + Send + 'static,
{
    let pid  = pipeline_id(user.id, dataset_id.unwrap_or_default(), pipeline_name);
    let prid = dataset_id.map(|d| pipeline_run_id(pid, d));   // None for ad-hoc

    let spec = RunSpec {
        run_id: prid,             // Some(...) when the caller has a deterministic id
        pipeline_name: pipeline_name.into(),
        user_id: Some(user.id),
        dataset_id,
    };

    if run_in_background {
        let handle = state.pipelines.register_background(spec, Box::pin(async move {
            let ctx = state.lib.task_context_with_watcher(state.pipelines.watcher_for(prid)).await?;
            work(ctx).await.map_err(|e| Box::new(e) as _)
        })).await?;
        Ok(DispatchOutcome::Background { handle })
    } else {
        let outcome = state.pipelines.register_inline(spec, /* same wrapping */).await?;
        Ok(DispatchOutcome::Blocking { outcome })
    }
}
```

The library function (the `work` future) does not see the flag. It just runs against the `TaskContext`'s watcher slot, which the registry has bound to a per-run sink.

## 8. Status transitions

```
Pending ──► Running ──► Completed
                    └─► Errored
        └─► AlreadyCompleted    (idempotent re-cognify; no work performed)
        └─► Aborted (shutdown)  (writes Errored to durable + channel; matches "shutdown_grace" error)
```

Each transition writes a new `pipeline_runs` row (§5) and emits a `RunEvent` on the registry channel.

`PipelineRunYield` events emit on the channel only, not the DB. Yield events from inside batch loops can be throttled by `yield_throttle` to avoid drowning slow WebSocket consumers; the default is no throttling (matches Python).

## 9. Sync vs background dispatch (HTTP wire shapes)

The two response shapes follow Python verbatim — only the dispatcher branch differs.

### 9.1 Blocking (`run_in_background=false`, default)

- Handler awaits the work to completion.
- On success: returns the aggregated `Dict[dataset_id_str -> PipelineRunInfo]` shape ([Python parity](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_pipeline.py)).
- On error: returns `500 {"error": "Pipeline run errored", "detail": "<msg>"}` ([Python parity](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L237-L249)). Note: `/improve` returns `420` for errors per [routers/improve.md](routers/improve.md).
- WebSocket subscribers can still attach during a blocking run if they know the `pipeline_run_id`. Useful for the frontend that may have spawned a sync request and wants live updates anyway.

### 9.2 Background (`run_in_background=true`)

- Handler returns immediately after the first event (`PipelineRunStarted`).
- Response shape: `{<dataset_id_str>: {"pipeline_run_id": "<uuid>", "status": "PipelineRunStarted", "dataset_id": "<uuid>", "dataset_name": "<str>", "payload": []}}` — payload is **always empty list** (Python clears it; see [`run_pipeline.py:97-102`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_pipeline.py)).
- The work continues in a `tokio::spawn`ed task; events flow into the registry channel; subscribers receive them via the WebSocket.

## 10. WebSocket integration

Detailed protocol in [websocket.md](websocket.md). The WS handler at `/api/v1/cognify/subscribe/{pipeline_run_id}` calls:

```rust
let mut events = state.pipelines.subscribe(run_id);
while let Some(event) = events.next().await {
    let payload = state.lib.formatted_graph_data(dataset_id, &user)
        .await
        .unwrap_or_else(|_| serde_json::json!({}));
    let frame = serde_json::json!({
        "pipeline_run_id": event.run_id,
        "status":          event_kind_to_python_string(&event.kind),
        "payload":         payload,
    });
    socket.send(Message::Text(frame.to_string())).await?;
    if matches!(event.kind, RunEventKind::Completed) {
        socket.close(1000, "").await;
        return;
    }
    // Errored / AlreadyCompleted forward but DO NOT close (Python parity).
}
```

`event_kind_to_python_string` maps `Started → "PipelineRunStarted"`, `Yield → "PipelineRunYield"`, etc. — the wire-string translation lives only at this seam.

## 11. Eviction & resource budget

Default config: `max_in_memory_runs = 4096`, `finished_retention = 1 hour`. On each `register_*` call:

```
if registry.runs.len() >= max_in_memory_runs:
    evict the oldest finished run from eviction_order
    if no finished runs available, log a warning and skip eviction
      (we never evict a still-running handle)
```

A separate background task runs every 60s and removes any finished run whose `finished_at + finished_retention < now`.

Operators wanting strict bug-for-bug parity with Python's "queue leaks until `remove_queue`" can configure `max_in_memory_runs = usize::MAX` and `finished_retention = Duration::MAX`; the in-memory leak then mirrors Python exactly. Default behavior keeps memory bounded and is the recommended setting.

## 12. Crash & restart recovery

On graceful shutdown (SIGTERM / SIGINT), the HTTP server calls `state.pipelines.shutdown().await` which:

1. For each in-flight run, calls `abort(run_id)`:
   - Drops the spawned task via its `tokio::task::JoinHandle::abort_handle()`.
   - When `cfg.abort_writes_errored_row = true` (default): writes a `DATASET_PROCESSING_ERRORED` row with `run_info = {"reason": "server_shutdown"}` so the durable view reflects reality.
   - Sends a `RunEvent { kind: Errored { message: "server shutdown" }, .. }` so any WS subscriber gets a final frame.
2. Drains the per-run channels.
3. Returns. The HTTP server then waits up to `shutdown_grace_period` (default 30s) for any remaining in-flight requests, then exits.

**Acknowledged Python divergence**: Python on shutdown leaves rows stuck in `STARTED` indefinitely. Rust writes `ERRORED` with `reason = "server_shutdown"`. This is a behavior add (not a wire deviation in steady-state), accepted per the user's confirmation.

On startup, the registry calls `repo.reset_orphans("server_restart_orphan")` which rewrites any `DATASET_PROCESSING_INITIATED` / `DATASET_PROCESSING_STARTED` rows with no successor to `DATASET_PROCESSING_ERRORED`. This matches Python's [`reset_dataset_pipeline_run_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/reset_dataset_pipeline_run_status.py).

## 13. Concurrency model

- **One tokio task per pipeline run.** Pipelines are CPU+I/O mixed; the multi-thread runtime distributes them.
- **No global pipeline lock.** Each run is isolated. Two concurrent `cognify` calls on different datasets run in parallel; same dataset gets the same `pipeline_run_id` twice and the second call sees the first's row.
- **DB writes are serialized per `pipeline_run_id`** via `SELECT … FOR UPDATE` in the repository when on Postgres; SQLite uses transactional UPSERT.
- **Channel send is non-blocking.** A full broadcast channel returns `SendError(Lagged)`; the registry maps it to a final `Errored` event for that subscriber and closes its slice.

## 14. Testing strategy

| Layer | Tests |
|---|---|
| Unit | `pipeline_id` and `pipeline_run_id` deterministic against canned (user, dataset, name) tuples; `RunPhase` transitions; eviction picks finished runs first; status enum mapping (`PipelineRunStatus` ↔ `DATASET_PROCESSING_*` ↔ `PipelineRun*`). |
| `PipelineRunRepository` impl | Round-trip a sequence of status writes; `latest_status` returns the most recent by `created_at`; `reset_orphans` rewrites `INITIATED`/`STARTED` to `ERRORED` and counts the rewrites. |
| Registry | Spawn a fake pipeline that emits `Started → Yield → Yield → Completed`; subscribe two consumers concurrently and assert both see the same event sequence; abort mid-flight and assert `Errored` is emitted to all subscribers; subscribe to an unknown run_id, then register, assert no events lost. |
| Integration (HTTP) | Hit `/cognify` with `run_in_background=true`, snapshot the response, attach a WS to `/cognify/subscribe/{id}`, assert the events arrive in order and the WS closes with 1000 on `Completed` only. |
| Recovery | Insert an `INITIATED` row, restart the registry, assert the row is rewritten to `ERRORED` with `reason="server_restart_orphan"`. |
| Library refactor | After dropping `run_in_background` from `remember()` and `improve()`, the function signatures compile against every existing caller in the workspace; cross-SDK parity tests for `/remember` and `/improve` continue to pass. |
| Cross-SDK | Snapshot the JSON shape of a `PipelineRunStarted` event from both Python and Rust; diff. Same for `Completed` and `Errored`. |

## 15. Open questions

1. **`PipelineRunRepository` placement** — does the trait live in `cognee-database` (and `cognee-core` re-exports it via a feature flag) or in a new `cognee-database-traits` micro-crate? The latter is cleaner but adds a crate; the former works but creates a circular-ish dependency unless `cognee-core` only imports from `cognee-database` behind the `pipeline-run-registry` feature. Lean: trait in `cognee-database`, gate the cognee-core dependency behind the feature.
2. **Multi-replica deployments**: a process-local registry doesn't fan out across replicas. WebSocket subscribers bound to a different replica from the one running the pipeline see no events. Three options: (a) sticky WS routing, (b) Redis pub/sub backing the channel, (c) document the constraint and rely on operator-side load-balancer config. Lean (c) for phase 1.
3. **Yield event throttling — already wired, future tuning**: `RegistryConfig::yield_throttle` defaults to `None` (no throttling, matches Python). The infrastructure is in place; the open question is whether the cross-SDK harness reveals a need to flip a per-pipeline default before Phase 8.
4. **`run_info` JSON schema**: Python writes free-form JSON. Rust matches. Not standardizing for phase 1.
5. **Backpressure on broadcast lag**: closing a slow WebSocket is correct but harsh. An alternative is to buffer-and-replay from the durable `pipeline_runs` table — but that loses the live-payload semantics. Defer.
6. **Removal of library `run_in_background` flags**: see §2. Coordinate the `remember()` and `improve()` library refactors with the HTTP server work so cross-SDK tests don't lag behind. The refactor is doc-tracked here; the library PR is the implementation.

## 16. References

- Python pipeline run model: [`cognee/modules/pipelines/models/PipelineRun.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py).
- Python pipeline run info enum: [`cognee/modules/pipelines/models/PipelineRunInfo.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py).
- Sync vs background dispatch: [`cognee/modules/pipelines/operations/run_pipeline.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_pipeline.py).
- Per-run in-memory queues: [`cognee/modules/pipelines/queues/pipeline_run_info_queues.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/queues/pipeline_run_info_queues.py).
- WebSocket handler: [`cognee/api/v1/cognify/routers/get_cognify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py) (the `subscribe_to_cognify_info` block).
- Status query: [`cognee/modules/pipelines/operations/get_pipeline_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/get_pipeline_status.py).
- ID generation: [`cognee/modules/pipelines/utils/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/pipelines/utils).
- Restart-time orphan reset: [`cognee/modules/pipelines/layers/reset_dataset_pipeline_run_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/reset_dataset_pipeline_run_status.py).
- Existing `cognee-core` primitives: [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs) (`PipelineWatcher`, `PipelineRunInfo`, `PipelineRunStatus`), [`crates/core/src/exec_status.rs`](../../crates/core/src/exec_status.rs) (`ExecStatusManager`), [`crates/core/src/task_context.rs`](../../crates/core/src/task_context.rs) (`TaskContext` builder).
- Library refactor targets: [`crates/lib/src/api/remember.rs`](../../crates/lib/src/api/remember.rs), [`crates/lib/src/api/improve.rs`](../../crates/lib/src/api/improve.rs).
