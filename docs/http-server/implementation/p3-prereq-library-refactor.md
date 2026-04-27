# Implementation: P3 prereq — Library refactor + `PipelineRunRegistry`

## 1. Goal

Land the three library-level changes that P3 (HTTP `/cognify`, `/memify`, `/remember`, `/improve` + WebSocket) depends on, **without touching `crates/http-server/`**:

1. Drop `run_in_background` and the `RememberResult { join_handle, … } / await_completion()` machinery from `cognee_lib::api::remember::remember()`.
2. Drop `run_in_background` (and the `has_sessions && !run_in_background` Stage 4 skip) from `cognee_lib::api::improve::improve()`.
3. Add `cognee_core::PipelineRunRegistry` (trait + default impl + `RunSpec` / `RunHandle` / `RunEvent` / `RunPhase` / `RegistryConfig` / `PipelineFuture`) and the `PipelineRunRepository` trait in `cognee-database` with a SeaORM-backed impl. Add the `pipeline_watcher` slot to `TaskContext`.

After this phase, `cognee-lib` is synchronous-only: HTTP P3 is the sole place that decides "spawn vs await", driven by the new registry. See [pipelines.md §2](../pipelines.md#2-library-refactor-prerequisite) for rationale.

## 2. References

- [pipelines.md](../pipelines.md) — full spec. Especially:
  - [§2](../pipelines.md#2-library-refactor-prerequisite) — refactor targets.
  - [§3](../pipelines.md#3-status-taxonomy-and-wire-mapping) — status taxonomy.
  - [§5.2](../pipelines.md#52-the-pipelinerunrepository-trait) — repository trait.
  - [§6](../pipelines.md#6-cognee_corepipelinerunregistry--the-new-component) — registry public types and channel impl.
  - [§14](../pipelines.md#14-testing-strategy) — test matrix.
- [plan.md §5](../plan.md#5-library-refactor-prerequisite) — phase scoping.
- [implementation/README.md](README.md) — template + invariants.
- Existing core primitives: [crates/core/src/pipeline.rs](../../crates/core/src/pipeline.rs) (`PipelineWatcher`, `PipelineRunInfo`, `PipelineRunStatus`, `deterministic_pipeline_id`); [crates/core/src/task_context.rs](../../crates/core/src/task_context.rs) (`TaskContext`, `TaskContextBuilder`).
- Existing database primitives: [crates/database/src/entities/pipeline_run.rs](../../crates/database/src/entities/pipeline_run.rs), [crates/database/src/ops/pipeline_runs.rs](../../crates/database/src/ops/pipeline_runs.rs), [crates/database/src/migrator/m20250101_000001_initial_schema.rs](../../crates/database/src/migrator/m20250101_000001_initial_schema.rs) (the `pipeline_runs` table is already created here — no new migration needed).
- Refactor targets: [crates/lib/src/api/remember.rs](../../crates/lib/src/api/remember.rs), [crates/lib/src/api/improve.rs](../../crates/lib/src/api/improve.rs).

## 3. Prerequisites

None for the library work itself — this lands **before** HTTP P3 starts. P0 (skeleton) and P1/P2 (HTTP routers that don't run pipelines) can land independently.

## 4. Step-by-step

Each step is a single commit; if a step would produce a >300-line diff, split it.

### Step 1 — Add `PipelineRunRepository` trait module in `cognee-database`

- Create `crates/database/src/pipelines/mod.rs` and `crates/database/src/pipelines/repository.rs`. The trait signature is fixed by [pipelines.md §5.2](../pipelines.md#52-the-pipelinerunrepository-trait): `log_pipeline_run`, `latest_status`, `list_recent`, `reset_orphans`.
- Re-use existing types: `crate::types::{DatabaseError, PipelineRunStatus, PipelineRun}`. Define a `pub type PipelineRunRow = PipelineRun;` alias inside `pipelines::repository` so the trait signature in pipelines.md §5.2 is byte-for-byte usable.
- The trait method `log_pipeline_run` returns `Uuid` — the new row's primary key (i.e. the value stored in `pipeline_runs.id`, which is `Uuid::new_v4()`-generated inside the impl).
- The trait must be `Send + Sync` and use `#[async_trait::async_trait]`. Use `cognee-database`'s already-pinned `async-trait` workspace dep.
- Add `pub mod pipelines;` to `crates/database/src/lib.rs` and re-export the trait at the crate root: `pub use pipelines::PipelineRunRepository;`.
- **Do not** implement the trait yet — that's Step 3.
- **Verify**: `cargo check -p cognee-database`.

### Step 2 — Confirm the `pipeline_runs` schema already matches the spec

- The `pipeline_runs` table is created in `m20250101_000001_initial_schema.rs` (line ~360) with all the columns required by [pipelines.md §5](../pipelines.md#5-database-persistence--pipeline_runs-table) (`id`, `created_at`, `status`, `pipeline_run_id`, `pipeline_name`, `pipeline_id`, `dataset_id`, `run_info` JSON) and the three indexes (`pipeline_run_id`, `pipeline_id`, `dataset_id`). **No migration required.**
- If a column is missing during implementation, write a new migration `crates/database/src/migrator/m20260501_000001_pipeline_run_repository.rs` and register it in `migrator/mod.rs`. Otherwise skip.
- **Verify**: `cargo test -p cognee-database` (existing migration tests must still pass).

### Step 3 — SeaORM-backed `PipelineRunRepository` impl

- Create `crates/database/src/pipelines/sea_orm_impl.rs`. The impl wraps `Arc<sea_orm::DatabaseConnection>` and implements `PipelineRunRepository`:
  - `log_pipeline_run` → INSERT into `pipeline_runs` with a freshly generated `Uuid::new_v4()` for the primary key (matches the "new row per status transition" pattern in [pipelines.md §5.1](../pipelines.md#51-writing-pattern)). Use `crate::uuid_hex::to_hex` for column writes (existing convention).
  - `latest_status` → window-function-equivalent: select the most recent row per `dataset_id` for the given `pipeline_name`, ordered by `created_at DESC`. The existing `crate::ops::pipeline_runs::get_latest_pipeline_status` is the single-dataset variant; the trait's batched variant should accept `&[Uuid]` and return `HashMap<Uuid, PipelineRunStatus>` per [pipelines.md §5.2](../pipelines.md#52-the-pipelinerunrepository-trait).
  - `list_recent` → SELECT with optional `dataset_id` filter, ordered by `created_at DESC`, limited.
  - `reset_orphans` → UPDATE every row in `INITIATED` / `STARTED` whose `(pipeline_run_id, created_at)` is the latest for its `pipeline_run_id` to `ERRORED` with `run_info = json!({"reason": <reason>})`. Matches Python's [`reset_dataset_pipeline_run_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/reset_dataset_pipeline_run_status.py).
- Re-export the impl: `pub use pipelines::sea_orm_impl::SeaOrmPipelineRunRepository;` in `crates/database/src/lib.rs`.
- Map the `cognee_core`-side `PipelineRunStatus` (which lives in `cognee-core`'s `pipeline.rs`) to the database-side enum at the trait boundary. The trait method takes `crate::types::PipelineRunStatus` (the database enum) so callers from `cognee-core` translate at the seam — there is **no** dependency edge from `cognee-database` back to `cognee-core`.
- **Verify**: `cargo check -p cognee-database --all-targets`.

### Step 4 — Add `pipeline_watcher` slot to `TaskContext`

- In [crates/core/src/task_context.rs](../../crates/core/src/task_context.rs):
  - Add a new public field to `TaskContext`: `pub pipeline_watcher: Option<Arc<dyn PipelineWatcher>>`. Place it after `exec_status` so the field order is stable for any pattern matches that may exist (none today, but keep it explicit).
  - Add `pipeline_watcher: Option<Arc<dyn PipelineWatcher>>` field to `TaskContextBuilder` and a setter method `pipeline_watcher(mut self, w: Arc<dyn PipelineWatcher>) -> Self`.
  - Update **both** `with_progress` and `with_current_data` to thread the new field through (`pipeline_watcher: self.pipeline_watcher.clone()`).
  - Default in `build()` is `None` (callers that don't set it see no behavior change). Library code that already accepts an explicit `&dyn PipelineWatcher` argument (the existing `execute(...)` signature in `pipeline.rs`) is unaffected; the slot is for cases where the watcher should be injected via the context rather than threaded as a function arg — which is what the registry needs.
- Import `crate::pipeline::PipelineWatcher` at the top of `task_context.rs`.
- The slot is **not** feature-gated. The field exists on every `TaskContext`; only the registry that fills it is feature-gated. This avoids a public-API split.
- **Verify**: `cargo check -p cognee-core --all-targets`. No existing call site should break — every existing builder use ends with `.build()`, which now defaults the field to `None`.

### Step 5 — `cognee-core` feature flag and module wiring

- In `crates/core/Cargo.toml`:
  - Add `[features] pipeline-run-registry = []`.
  - The crate already depends on `cognee-database` (for `DatabaseConnection` in `TaskContext`), so no new dep — the registry just imports `PipelineRunRepository` from there.
- Create `crates/core/src/pipeline_run_registry/mod.rs` (gated by `#[cfg(feature = "pipeline-run-registry")]`) and a `mod pipeline_run_registry;` line in `lib.rs`. Re-export the public types at the crate root behind the same feature.
- The feature is **off by default** in `cognee-core` itself, but `cognee-lib` and `cognee-http-server` will turn it on (the Cargo wiring on those crates lands in P3, not here — this phase only needs the feature to exist).
- **Verify**: `cargo check -p cognee-core --features pipeline-run-registry --all-targets`.

### Step 6 — Public types: `RunHandle`, `RunEvent`, `RunPhase`, `RunSpec`, `RegistryConfig`

- In `crates/core/src/pipeline_run_registry/types.rs`, add the structs and enums verbatim from [pipelines.md §6.2](../pipelines.md#62-public-types). Keep them small data types: `Clone + Debug` where the spec demands it; `RunSpec` does not need `Clone`.
- `RegistryConfig::default()` produces the values listed in §6.2 (`max_in_memory_runs = 4096`, `finished_retention = 1 hour`, `channel_capacity = 64`, `yield_throttle = None`, `abort_writes_errored_row = true`). Implement `Default` manually — derive does not produce these values.
- Define `pub type PipelineFuture = Pin<Box<dyn Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'static>>;` and re-export it from the crate root.
- Define `pub enum RegistryError` with `#[derive(Debug, thiserror::Error)]`. Variants:
  - `#[error("unknown run id: {0}")] UnknownRun(Uuid)`
  - `#[error("run aborted")] Aborted`
  - `#[error("registry shut down")] Shutdown`
  - `#[error("repository error: {0}")] Repository(#[from] cognee_database::DatabaseError)`
  - `#[error("registry full and no finished runs to evict")] RegistryFull`
- Define a `pub struct RunOutcome { pub run_id: Uuid, pub phase: RunPhase }` — the value returned by `register_inline` once the work future completes. Keep it small; the HTTP layer can compose the wider response from it.
- **Verify**: `cargo check -p cognee-core --features pipeline-run-registry`.

### Step 7 — `PipelineRunRegistry` trait

- In `crates/core/src/pipeline_run_registry/trait_def.rs`, add the `#[async_trait]` trait shape from [pipelines.md §6.2](../pipelines.md#62-public-types):
  - `register_inline(&self, spec: RunSpec, work: PipelineFuture) -> Result<RunOutcome, RegistryError>`
  - `register_background(&self, spec: RunSpec, work: PipelineFuture) -> Result<RunHandle, RegistryError>`
  - `subscribe(&self, run_id: Uuid) -> Pin<Box<dyn Stream<Item = RunEvent> + Send + 'static>>` (synchronous — does not block).
  - `snapshot_status(&self, run_id: Uuid) -> Option<RunPhase>`
  - `abort(&self, run_id: Uuid) -> Result<(), RegistryError>`
  - `shutdown(&self) -> Result<(), RegistryError>`
- `subscribe` returns a runtime-agnostic `Pin<Box<dyn Stream<Item = RunEvent> + Send + 'static>>`. The internal channel is `tokio::sync::broadcast` (see Step 9), but it does not appear in the public signature. This makes it possible for non-tokio consumers (or tests using `futures::executor`) to drain the stream without pulling in tokio macros.
- For unknown run IDs, `subscribe` lazily creates a placeholder slot and returns its receiver, so a producer registering after the subscriber attaches still delivers events. This matches Python's `initialize_queue` behavior where attaching to an unknown id yields an empty queue rather than an error, see [pipelines.md §10](../pipelines.md#10-websocket-integration).
- The trait is `Send + Sync` and `'static`-friendly; consumers store `Arc<dyn PipelineRunRegistry>` in `AppState`.
- **Verify**: `cargo check -p cognee-core --features pipeline-run-registry`.

### Step 8 — `ScopedRunWatcher` (the registry's `PipelineWatcher` proxy)

- In `crates/core/src/pipeline_run_registry/scoped_watcher.rs`, add `ScopedRunWatcher` per [pipelines.md §6.3](../pipelines.md#63-the-registry-implements-pipelinewatcher).
- Implement `cognee_core::PipelineWatcher` for it. The non-default methods (`on_pipeline`, `on_task`) forward to a no-op; the rich `on_pipeline_run_started` / `_completed` / `_errored` methods do two things:
  1. `repo.log_pipeline_run(...).await` to write the durable row (translating `cognee_core::PipelineRunStatus` → `cognee_database::types::PipelineRunStatus` at the seam).
  2. `sink.publish(RunEvent { … })` on the per-run broadcast sink.
- A repository-write failure must **not** abort the pipeline. Log via `tracing::warn!` and continue. (Matches Python's behavior — DB failures don't fail the pipeline.)
- **Verify**: unit test the watcher in isolation by passing a `MockPipelineRunRepository`.

### Step 9 — `DefaultPipelineRunRegistry` impl

- In `crates/core/src/pipeline_run_registry/default_impl.rs`, add the concrete struct.
- Internal state (per [pipelines.md §6.4](../pipelines.md#64-channel-implementation)):
  - `runs: tokio::sync::RwLock<HashMap<Uuid, RunSlot>>` where `RunSlot` holds:
    - `event_tx: broadcast::Sender<RunEvent>` — fan-out for subscribers.
    - `phase_tx: watch::Sender<RunPhase>` — current phase snapshot.
    - `started_at: DateTime<Utc>`, `finished_at: Option<DateTime<Utc>>`.
    - `abort_handle: Option<tokio::task::AbortHandle>` — present only for background runs.
    - `meta: RunHandle` — cheap clone returned by `register_*` and used by the watcher.
  - `repo: Arc<dyn PipelineRunRepository>` injected via `pub fn new(repo: Arc<dyn PipelineRunRepository>, cfg: RegistryConfig) -> Self`.
  - `eviction_order: Mutex<VecDeque<Uuid>>` — append on register, drain on retention/eviction. Eviction never removes a slot whose `finished_at` is still `None`.
  - `cfg: RegistryConfig` — stored as-is.
- New helper method `pub fn watcher_for(&self, run_id: Uuid) -> Arc<dyn PipelineWatcher>` — constructs a `ScopedRunWatcher` for the given run id, capturing `Arc<dyn PipelineRunRepository>` and a per-run sink that publishes into the slot's `event_tx`.
- `register_inline`:
  1. Build the slot (insert into `runs`).
  2. Resolve the watcher with `self.watcher_for(run_id)`.
  3. Emit `RunEvent { kind: Started }` on the event channel and call `repo.log_pipeline_run(..., Started, None)`.
  4. `await work` directly in the current task. On `Ok(())` emit `Completed`; on `Err(e)` emit `Errored { message: e.to_string() }`.
  5. Update `finished_at`, push the run id onto `eviction_order`, return a `RunOutcome` describing success/error.
- `register_background`:
  1. Build the slot, including a `JoinHandle::abort_handle()` reservation slot.
  2. `tokio::spawn(async move { run_inline_logic_above })`. Save the `AbortHandle` into the slot. Return the cloned `RunHandle` immediately.
- `subscribe`:
  1. Acquire a read lock on `runs`. If the slot exists, call `event_tx.subscribe()` to get a `broadcast::Receiver<RunEvent>`.
  2. If the slot does not yet exist, **lazily create a placeholder slot** so a subscriber that races ahead of the producer still gets a live receiver. The producer will populate the slot's `meta` later (see Step 7's "initialize_queue parity" requirement).
  3. Wrap the receiver with `tokio_stream::wrappers::BroadcastStream::new(rx)` and `Box::pin` it. Map `Err(BroadcastStreamRecvError::Lagged(_))` to a synthetic `RunEvent { kind: Errored { message: "subscriber lagged" }, … }` (per [pipelines.md §6.4](../pipelines.md#64-channel-implementation)) so the consumer can surface it as a 1011 close.
- `snapshot_status`: read `watch::Receiver::borrow()` clone, returning `None` for unknown slots.
- `abort(run_id)`:
  1. If the slot has an `AbortHandle`, call `.abort()`.
  2. If `cfg.abort_writes_errored_row=true` (default), call `repo.log_pipeline_run(..., Errored, Some(json!({"reason": "abort"})))`.
  3. Publish `RunEvent { kind: Errored { message: "aborted" }, … }` and mark `finished_at = now()`.
- `shutdown`:
  1. Snapshot every still-running slot (those with `finished_at == None && abort_handle.is_some()`).
  2. For each, call `self.abort(run_id).await` with `run_info = json!({"reason": "server_shutdown"})` — see [pipelines.md §12](../pipelines.md#12-crash--restart-recovery).
  3. Drop the broadcast senders so dangling subscribers see `Ended`.
- A separate `tokio::spawn`ed retention task started in `new()` runs every 60s and removes finished slots whose `finished_at + cfg.finished_retention < now`.
- **Constructor that runs reset_orphans on startup**: provide `pub async fn new_with_orphan_reset(repo, cfg) -> Result<Self, RegistryError>` that calls `repo.reset_orphans("server_restart_orphan")` once before returning, per [pipelines.md §12](../pipelines.md#12-crash--restart-recovery). The HTTP server uses this constructor.
- **Verify**: `cargo check -p cognee-core --features pipeline-run-registry --all-targets`.

### Step 10 — Re-exports and crate-level `pub use`

- Add to `crates/core/src/lib.rs` (gated by `#[cfg(feature = "pipeline-run-registry")]`):
  ```rust,ignore
  pub mod pipeline_run_registry;
  pub use pipeline_run_registry::{
      RunHandle, RunEvent, RunEventKind, RunPhase, RunSpec,
      RegistryConfig, PipelineRunRegistry, DefaultPipelineRunRegistry,
      PipelineFuture, RegistryError, ScopedRunWatcher,
  };
  ```
- Re-export `cognee_database::PipelineRunRepository` from `cognee-core` for ergonomics: `pub use cognee_database::PipelineRunRepository;` (ungated — the trait costs nothing if unused).
- **Verify**: `cargo check -p cognee-core --features pipeline-run-registry --all-targets`.

### Step 11 — Drop background path from `remember()`

- In [crates/lib/src/api/remember.rs](../../crates/lib/src/api/remember.rs):
  - Remove the `run_in_background: bool` parameter from `pub async fn remember(...)` (currently line 241). The next argument `owner_id: Uuid` becomes the parameter at the same position.
  - Delete the `if run_in_background { return remember_permanent_background(...); }` branch (lines 286–308) — the function falls straight through to `remember_permanent_blocking(...)`.
  - Delete the `remember_permanent_background` function entirely (lines 506–590).
  - Delete `RememberResultInner` (lines 60–76) — no struct in the new design needs it.
  - Delete the `await_completion()` method (lines 131–177).
  - Delete the `inner: Option<Arc<AsyncMutex<RememberResultInner>>>` field on `RememberResult` (line 104) and every `inner: None` / `inner: Some(...)` initializer.
  - Delete `RememberStatus::Running` (line 38). Update the `done()` method (line 123) — without `Running`, every status is terminal, so `done()` returns `true` unconditionally. Update the `is_success()` doc to drop the "Running" reference. Update the `remember_status_serde_roundtrip_running` and `is_success_running_and_errored` tests inside `mod tests` to use a different status (e.g. test `Errored` round-trip and assert `done() == true` for each variant).
  - In `remember_session` (line 601), delete the entire `if self_improvement { let inner = … tokio::spawn(...) … Some(inner) } else { None }` block (lines 653–724). Replace it with a synchronous `if self_improvement { improve(...).await? }` call, then return a `RememberResult` with `status: SessionStored`. Errors from the inline `improve(...)` follow Python's "non-fatal" semantics: log via `tracing::warn!` and store the message in `RememberResult.error`, but **do not** propagate as an `Err`.
  - Drop the unused `tokio::sync::Mutex as AsyncMutex` import if no longer referenced; let `cargo check` flag it.
- **Verify**: `cargo check -p cognee-lib --all-targets`.

### Step 12 — Drop background path from `improve()`

- In [crates/lib/src/api/improve.rs](../../crates/lib/src/api/improve.rs):
  - Remove the `run_in_background: bool` parameter from `pub async fn improve(...)` (line 59). The argument list collapses by one.
  - Replace the `if has_sessions && !run_in_background` branch (line 198) with `if has_sessions` — Stage 4 always runs when sessions are present, since the function is now sync. The Python "skip Stage 4 in background" semantics no longer apply because the library function does not know it's running in a background task; the HTTP layer schedules accordingly.
  - Remove the `// Skipped when run_in_background=true (Python improve.py:152).` comment (line 197). Replace it with a single line: `// Stage 4 always runs when sessions are present (background dispatch is host-side).`.
  - The collapse is intentional and matches [pipelines.md §2](../pipelines.md#2-library-refactor-prerequisite): "the `has_sessions && !run_in_background` skip-condition collapses (always run when sessions are present, since the function is now sync)". HTTP P3 will still expose `run_in_background` as a wire flag, but it controls **dispatch** (spawn vs await), not Stage-4 inclusion.
- **Verify**: `cargo check -p cognee-lib --all-targets`.

### Step 13 — Update remaining callers of `remember()` and `improve()`

Workspace search confirms the only direct callers are the lib's own test files; the bindings (`python/`, `js/`, `capi/`) do not currently expose `remember`/`improve` and `cognee-cli` does not invoke them. `crates/cloud/src/cloud_client.rs::remember`/`improve` are HTTP-wrapper methods (they POST to a remote server), not callers of the lib functions.

Callsites that pass `run_in_background` and must drop the argument:

- [crates/lib/tests/remember_tests.rs](../../crates/lib/tests/remember_tests.rs) — five call sites at lines 101, 150, 193, 227, 303. Remove the `false` / `true` argument. Delete the test `remember_permanent_background_returns_running_then_completes` (around line 296) — the test asserts deprecated background behavior; the new sync semantics are covered by `remember_sync_only.rs` (Step 5 of this section), and the registry's spawn path is covered by `pipeline_run_registry.rs` (Step 5 — Tests).
- [crates/lib/tests/improve_e2e.rs](../../crates/lib/tests/improve_e2e.rs) — three call sites at lines 88, 149, 187. Remove the `run_in_background: bool` argument from each. Delete `improve_with_run_in_background_skips_stage4` (around line 117) — Stage 4 now always runs when sessions are present; the new behavior is covered by `improve_sync_only.rs`.
- The internal `improve(...)` call inside `remember_session` (now inlined per Step 11) drops the `false` argument it currently passes (line 679).
- **Cloud client compatibility**: `crates/cloud/src/cloud_client.rs` is an HTTP wrapper that talks to a *remote* server — its `run_in_background` is a wire field on the cloud API, not a local lib parameter, and **stays as-is**.
- **Bindings**: `python/`, `js/`, `capi/` do not currently bind `remember`/`improve`. If a future PR adds such bindings, those wrappers may expose their own `run_in_background` parameter and decide locally whether to `tokio::spawn` (or a binding-equivalent) — that is independent of the now-sync lib API.
- **Verify**: `cargo check -p cognee-lib --all-targets && cargo test -p cognee-lib --no-run`.

### Step 14 — Bindings sweep

- `grep -rn "remember\|improve" python/ js/ capi/ --include="*.rs"` to find any binding-side wrappers.
- For each match, drop `run_in_background` from the bound signature and let the binding's own background-mode (e.g. PyO3 returning a `Future` future, or Neon spawning) handle scheduling locally — bindings are independent of the lib's sync-only API.
- If a binding currently lacks a wrapper for `remember`/`improve`, **leave it alone** — adding bindings is out of scope for this phase.
- **Verify**: `python/scripts/check.sh && js/scripts/check.sh && capi/scripts/check.sh`. Each must pass.

### Step 15 — Verification sweep

Run from the repository root:

```bash
grep -rn "run_in_background" crates/lib crates/cognify crates/ingestion
```

Must return zero matches. The grep can still hit `crates/cloud/src/cloud_client.rs` (forwarding to remote HTTP server) — that's expected and **not in the scoped paths**.

Other `tokio::spawn` calls in `crates/cognify/src/{summarization,fact_extraction,tasks}.rs` are intra-pipeline parallelism and **stay** per [pipelines.md §2](../pipelines.md#2-library-refactor-prerequisite).

- **Verify**: `cargo test --workspace` (debug mode, no `--release`).

### Step 16 — Final formatting & full check

- `cargo fmt`.
- `scripts/check_all.sh` — runs `cargo fmt --check`, `cargo check --all-targets`, `cargo clippy -- -D warnings`, then the C/Python/JS binding check scripts.
- All must pass before the phase is marked Done.

## 5. Tests

Each test file is independent; aim for fast unit tests with `MockPipelineRunRepository` and `MockGraphDB`/`MockVectorDB` from `cognee-test-utils`. No real LLM or embedding model required for any test below.

| File | Covers |
|---|---|
| `crates/core/tests/pipeline_run_registry.rs` | (a) `register_inline` runs to completion and emits `Started → Completed`; (b) `register_background` spawns and the handle is returned before the work finishes; (c) two concurrent subscribers see the same event sequence; (d) a subscriber that attaches **before** the producer registers (i.e. unknown run id) sees no events lost — register the slot lazily on first `subscribe` per [pipelines.md §10](../pipelines.md#10-websocket-integration) (Python `initialize_queue` parity); (e) `abort(run_id)` drops the spawned task and emits `Errored` with `message="aborted"`; (f) `shutdown()` aborts every in-flight run and writes `ERRORED` rows when `cfg.abort_writes_errored_row=true`; (g) `RegistryConfig::default()` matches the spec values. |
| `crates/core/tests/scoped_run_watcher.rs` | Drive a fake pipeline through `Started → Yield → Yield → Completed`; assert the events flow through the broadcast channel in order **and** the corresponding rows land in the (mock) `PipelineRunRepository`. Assert `Yield` events do **not** trigger DB writes ([pipelines.md §3.3](../pipelines.md#33-live-event-status--emitted-on-the-registry-channel-and-the-websocket-frame)). Assert `RegistryConfig::yield_throttle = Some(50ms)` collapses two yields-in-quick-succession into one event. |
| `crates/database/tests/pipeline_run_repository.rs` | Round-trip via `SeaOrmPipelineRunRepository` against an in-memory SQLite (`sqlite::memory:`). Cover: (a) `log_pipeline_run` returns a fresh `Uuid` per call; (b) `latest_status` returns the most recent row when multiple rows share the same `pipeline_run_id`; (c) `latest_status` for a batch of `dataset_ids` returns the latest per dataset; (d) `reset_orphans` rewrites `INITIATED`/`STARTED` rows that are the latest for their `pipeline_run_id` to `ERRORED` with the supplied `reason`, and counts the rewrites; (e) `reset_orphans` does **not** rewrite a row that already has a `COMPLETED` successor. |
| `crates/lib/tests/remember_sync_only.rs` | Compile-time + runtime: (a) the function signature has no `run_in_background` parameter; (b) `RememberResult` has no `inner` / `await_completion()` member; (c) calling `remember(...)` for permanent mode returns `RememberStatus::Completed` synchronously; (d) calling with a `session_id` runs the inline `improve()` synchronously and returns `RememberStatus::SessionStored`. Use the existing `MockGraphDB`/`MockVectorDB`/`MockEmbeddingEngine` test fixtures. |
| `crates/lib/tests/improve_sync_only.rs` | (a) signature has no `run_in_background`; (b) when sessions are supplied, all four stages run; (c) when sessions are not supplied, only Stage 3 (memify) runs. |

Existing tests (`crates/lib/tests/remember_tests.rs`, `crates/lib/tests/improve_e2e.rs`) are updated per Step 13; tests asserting deprecated background behavior are deleted (the registry tests cover the equivalent now-correct behavior).

## 6. Acceptance criteria

- [ ] `cargo check --all-targets` passes (no `--release`).
- [ ] `cargo test --workspace` passes in debug mode.
- [ ] `cargo check -p cognee-core --features pipeline-run-registry --all-targets` passes.
- [ ] `grep -rn "run_in_background" crates/lib crates/cognify crates/ingestion` returns zero hits.
- [ ] All bindings (`python/scripts/check.sh`, `js/scripts/check.sh`, `capi/scripts/check.sh`) pass against the new signatures.
- [ ] `scripts/check_all.sh` passes (fmt + check + clippy + binding checks).
- [ ] `cognee-lib::api::remember::remember()` signature has no `run_in_background` parameter and `RememberResult` has no `inner` / `await_completion()` API.
- [ ] `cognee-lib::api::improve::improve()` signature has no `run_in_background` parameter; Stage 4 runs whenever sessions are present.
- [ ] `cognee_core::PipelineRunRegistry`, `RunHandle`, `RunEvent`, `RunPhase`, `RunSpec`, `RegistryConfig`, `PipelineFuture`, `DefaultPipelineRunRegistry`, `ScopedRunWatcher` are public from `cognee-core` under the `pipeline-run-registry` feature.
- [ ] `cognee_database::PipelineRunRepository` is public from `cognee-database` and re-exported from `cognee-core`.
- [ ] `TaskContext` has a `pipeline_watcher: Option<Arc<dyn PipelineWatcher>>` slot wired through `TaskContextBuilder`.
- [ ] No edits to `crates/http-server/` in this phase.

## 7. Files touched

**New**:
- `crates/database/src/pipelines/mod.rs`
- `crates/database/src/pipelines/repository.rs` — `PipelineRunRepository` trait.
- `crates/database/src/pipelines/sea_orm_impl.rs` — `SeaOrmPipelineRunRepository`.
- `crates/core/src/pipeline_run_registry/mod.rs`
- `crates/core/src/pipeline_run_registry/types.rs` — public types.
- `crates/core/src/pipeline_run_registry/trait_def.rs` — `PipelineRunRegistry` trait.
- `crates/core/src/pipeline_run_registry/scoped_watcher.rs` — `ScopedRunWatcher`.
- `crates/core/src/pipeline_run_registry/default_impl.rs` — `DefaultPipelineRunRegistry`.
- `crates/core/tests/pipeline_run_registry.rs`
- `crates/core/tests/scoped_run_watcher.rs`
- `crates/database/tests/pipeline_run_repository.rs`
- `crates/lib/tests/remember_sync_only.rs`
- `crates/lib/tests/improve_sync_only.rs`

**Modified**:
- `crates/database/src/lib.rs` — re-export trait and impl.
- `crates/core/Cargo.toml` — add `pipeline-run-registry` feature.
- `crates/core/src/lib.rs` — `pub mod pipeline_run_registry;` and re-exports under feature gate.
- `crates/core/src/task_context.rs` — add `pipeline_watcher` slot to `TaskContext` and `TaskContextBuilder`.
- `crates/lib/src/api/remember.rs` — drop `run_in_background`, drop `RememberResultInner.join_handle`, drop `await_completion()`, drop `remember_permanent_background`, inline the `improve(...)` call in `remember_session`.
- `crates/lib/src/api/improve.rs` — drop `run_in_background`, collapse Stage 4 condition to `has_sessions`.
- `crates/lib/tests/remember_tests.rs` — drop `run_in_background` arg from five call sites; delete the background-mode test.
- `crates/lib/tests/improve_e2e.rs` — drop `run_in_background` arg from three call sites; delete the Stage-4-skip test.
- (If applicable) binding wrappers in `python/`, `js/`, `capi/` that today reference `run_in_background` against the lib facade.

**Not touched** (out of scope):
- `crates/http-server/` — P3 owns HTTP wiring.
- `crates/cloud/src/cloud_client.rs` — `run_in_background` is an HTTP wire field on a *remote* call, not a local lib parameter.
- `crates/cognify/src/{summarization,fact_extraction,tasks}.rs` — intra-pipeline `tokio::spawn` is parallelism, not background-mode dispatch ([pipelines.md §2](../pipelines.md#2-library-refactor-prerequisite)).
