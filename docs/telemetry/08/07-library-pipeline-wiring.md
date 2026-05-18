# Task 08-07 — Wire `PipelineRunRepository` through library pipelines

**Status**: implemented in commit f64fcac (LIB-06 pre-requisite made the wiring a one-line-per-function `&NoopWatcher` → `&DbPipelineWatcher::new(repo)` swap; the only sub-doc divergence was scope expansion to 41 files — 17 test fixtures + 3 lib::api callers + 4 cognify internals — forced by adding the required `pipeline_run_repo` parameter to the three convenience functions)
**Owner**: _unassigned_
**Depends on**: 08-04, **LIB-06 (closure commit `b5ccc96`)** — all three convenience functions now route through `cognee_core::pipeline::execute` with `&NoopWatcher`. This task swaps the watcher only.
**Blocks**:
- [Task 08-08 — Qualification check](08-check-qualification.md) (qualification runs inside cognify/memify and needs the wired-through repo).
- [Task 08-09 — Tests](09-tests.md) (cross-SDK test asserts CLI cognify produces the four-state trail).

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: 2 (always-on registry for library pipelines; `NoopPipelineRunRepository` default), 11 (`DbPipelineWatcher` is the watcher type library pipelines use, located at `crates/core/src/pipeline_run_registry/db_watcher.rs` per commit `56f020e`).

> **Post-LIB-06 simplification.** This doc was drafted before LIB-06 routed
> `cognify`, `memify`, and `AddPipeline::add` through `pipeline::execute`. Now
> that they do (with `&NoopWatcher`), each convenience function only needs
> to **replace its `&NoopWatcher` argument with `&DbPipelineWatcher::new(repo)`**.
> No manual hook plumbing at the top/bottom of the function body is needed —
> `pipeline::execute` already emits INITIATED (task 08-04, commit `29a99f8`),
> STARTED, COMPLETED, and ERRORED.

---

## 1. Goal

Make `cognify`, `memify`, and the ingestion `AddPipeline` produce the four-state `pipeline_runs` trail by default — not just HTTP-driven runs.

1. Add `NoopPipelineRunRepository` to `cognee-database` (in-memory, returns `Ok(...)` for writes, `Ok(None)` / `Ok(vec![])` for reads).
2. Add `DbPipelineWatcher` to `crates/core/src/pipeline_run_registry/db_watcher.rs` — a `PipelineWatcher` impl that owns `Arc<dyn PipelineRunRepository>` and writes the same `INITIATED` / `STARTED` / `COMPLETED` / `ERRORED` rows the `ScopedRunWatcher` does, but **without** the in-memory event channel.
3. Add an `Arc<dyn PipelineRunRepository>` parameter (required, not `Option`) to:
   - `cognify::cognify` ([`crates/cognify/src/tasks.rs:1773`](../../crates/cognify/src/tasks.rs#L1773))
   - `cognify::memify::memify` ([`crates/cognify/src/memify/pipeline.rs:57`](../../crates/cognify/src/memify/pipeline.rs#L57))
   - `cognify::dataset_resolver::cognify_datasets` ([`crates/cognify/src/dataset_resolver.rs:103`](../../crates/cognify/src/dataset_resolver.rs#L103))
   - `ingestion::AddPipeline::add` ([`crates/ingestion/src/pipeline.rs:786`](../../crates/ingestion/src/pipeline.rs#L786))
4. Construct the `DbPipelineWatcher` inside each entry point and pass it to `cognee_core::pipeline::execute` as the `watcher`.
5. Update the CLI subcommands (`cognify`, `memify`, `add`, `add_and_cognify`, `run_sequence`) to construct the real `SeaOrmPipelineRunRepository` from the existing `DatabaseConnection` and pass it in.
6. Examples and embedded library users that don't have a DB can pass `Arc::new(NoopPipelineRunRepository::default())`.

## 2. Rationale

Decision 2 settled "always-on": library pipelines always run through a repo. The default is `NoopPipelineRunRepository` so embedded users (no DB available) can still call `cognify` without setting up SQLite. CLI users get the real repo, which means `cognee-cli cognify ...` now produces `pipeline_runs` rows the HTTP activity endpoint surfaces.

Decision 11 keeps the single-point-of-truth invariant: both the HTTP-driven `ScopedRunWatcher` and the new library `DbPipelineWatcher` share helper functions from task 08-03 (`run_info_for_running`, `run_info_for_initiated`, etc.), so the wire shape is identical regardless of entry point.

## 3. Pre-conditions

- Tasks 08-01, 08-02, 08-03, 08-04 committed.
- `run_info_for_*` helpers exposed from `cognee_core::pipeline_run_registry` (tasks 08-02, 08-03). **Verified** at `crates/core/src/pipeline_run_registry/data_info.rs:50,66,83` and re-exported via `crates/core/src/pipeline_run_registry/mod.rs:9`.
- `PipelineWatcher::on_pipeline_run_initiated` trait method present (task 08-04). **Verified** at `crates/core/src/pipeline.rs:495` and called from `pipeline::execute` at `crates/core/src/pipeline.rs:749`.
- `PipelineRunRepository` accepts `Option<Uuid>` for `dataset_id` (task 08-01). **Verified** at `crates/database/src/pipelines/repository.rs:43-51`.
- **LIB-06 fully landed** (verified: main is at `da04bd6`, closure SHA `b5ccc96` on `git log --oneline`). The three convenience functions reach `pipeline::execute` with `&NoopWatcher`:
  - `crates/cognify/src/tasks.rs:1897` — `cognee_core::pipeline::execute(&pipeline, inputs, ctx, &NoopWatcher)`
  - `crates/cognify/src/memify/pipeline.rs:239` — same shape
  - `crates/ingestion/src/pipeline.rs:1029` — same shape

## 4. Step-by-step

### 4.1 `NoopPipelineRunRepository`

> **Trait drift note (post task 08-05/08-06).** The trait signatures in this section's skeleton predate the most recent reader/orphan changes. Implementor must align to the actual trait at [`crates/database/src/pipelines/repository.rs`](../../crates/database/src/pipelines/repository.rs):
> - `reset_orphans(&self, reason: &str) -> Result<u64, DbError>` — takes a `reason` and returns the rewritten-row count, not `()`.
> - `list_recent` returns `Vec<PipelineRunRow>`, not `Vec<PipelineRun>`.
> - `get_payload` returns `serde_json::Map<String, serde_json::Value>`, not `HashMap`.
> - Error type is `DbError`, not `DatabaseError`.
> - `list_recent_with_attribution` already has a default impl that falls back to `list_recent`; the noop can omit it.
> - Also promote `NoOpPipelineRunRepository` from `crates/http-server/src/state.rs:136` (currently `pub(crate)`) into the new public type in `cognee-database`, rename to `NoopPipelineRunRepository` (Rust convention), and replace the http-server's private copy with a re-export.

Add to [`crates/database/src/pipelines/`](../../crates/database/src/pipelines/) as a new file `noop_impl.rs` (template — adjust to current trait):

```rust
//! In-memory no-op `PipelineRunRepository` for embedded uses without a DB.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::pipelines::repository::{
    PipelineRunRepository, PipelineRunRow, PipelineRunWithAttributionRow,
};
use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};

/// `PipelineRunRepository` that ignores all writes and returns empty
/// results for reads. Suitable for tests and embedded library users that
/// don't have a SQL database.
///
/// Reads (`get_*`, `list_*`, `latest_status`) return empty. Writes
/// (`log_pipeline_run`, `set_payload_field`, `reset_orphans`) return
/// `Ok(...)` with a synthesised UUID where applicable.
#[derive(Default)]
pub struct NoopPipelineRunRepository;

impl NoopPipelineRunRepository {
    pub fn new() -> Self {
        Self
    }

    /// Convenience: return as `Arc<dyn PipelineRunRepository>`.
    pub fn arc() -> Arc<dyn PipelineRunRepository> {
        Arc::new(Self)
    }
}

#[async_trait]
impl PipelineRunRepository for NoopPipelineRunRepository {
    async fn log_pipeline_run(
        &self,
        _pipeline_run_id: Uuid,
        _pipeline_id: Uuid,
        _pipeline_name: &str,
        _dataset_id: Option<Uuid>,
        _status: PipelineRunStatus,
        _run_info: Option<serde_json::Value>,
    ) -> Result<Uuid, DatabaseError> {
        Ok(Uuid::new_v4())
    }

    async fn latest_status(
        &self,
        _dataset_ids: &[Uuid],
        _pipeline_name: &str,
    ) -> Result<HashMap<Uuid, PipelineRunStatus>, DatabaseError> {
        Ok(HashMap::new())
    }

    async fn list_recent(
        &self,
        _dataset_id: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<PipelineRun>, DatabaseError> {
        Ok(Vec::new())
    }

    async fn list_recent_with_attribution(
        &self,
        _dataset_id: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<PipelineRunWithAttributionRow>, DatabaseError> {
        Ok(Vec::new())
    }

    async fn reset_orphans(&self) -> Result<(), DatabaseError> {
        Ok(())
    }

    async fn set_payload_field(
        &self,
        _run_id: Uuid,
        _key: &str,
        _value: serde_json::Value,
    ) -> Result<(), DatabaseError> {
        Ok(())
    }

    async fn get_payload(
        &self,
        _run_id: Uuid,
    ) -> Result<HashMap<String, serde_json::Value>, DatabaseError> {
        Ok(HashMap::new())
    }

    // Reader helpers from task 08-06:
    async fn get_pipeline_run(
        &self,
        _pipeline_run_id: Uuid,
    ) -> Result<Option<PipelineRun>, DatabaseError> {
        Ok(None)
    }

    async fn get_pipeline_run_by_dataset(
        &self,
        _dataset_id: Uuid,
        _pipeline_name: &str,
    ) -> Result<Option<PipelineRun>, DatabaseError> {
        Ok(None)
    }

    async fn get_pipeline_runs_by_dataset(
        &self,
        _dataset_id: Uuid,
    ) -> Result<Vec<PipelineRun>, DatabaseError> {
        Ok(Vec::new())
    }
}
```

Re-export from [`crates/database/src/pipelines/mod.rs`](../../crates/database/src/pipelines/mod.rs) and [`crates/database/src/lib.rs`](../../crates/database/src/lib.rs).

### 4.2 `DbPipelineWatcher`

Add `crates/core/src/pipeline_run_registry/db_watcher.rs`:

```rust
//! `PipelineWatcher` that persists the four-state `pipeline_runs` trail
//! through `PipelineRunRepository` without an in-memory event channel.
//!
//! Used by library pipelines (cognify, memify, ingestion) that do not
//! flow through the http-server's registry but still want the audit trail.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use cognee_database::{PipelineRunRepository, PipelineRunStatus as DbStatus};

use crate::pipeline::{
    PipelineRunInfo, PipelineRunStatus as CoreStatus, PipelineStatus, PipelineWatcher, TaskStatus,
};

use super::{run_info_for_errored, run_info_for_initiated, run_info_for_running};

/// `PipelineWatcher` that writes `pipeline_runs` rows through a
/// `PipelineRunRepository`. Does NOT broadcast `RunEvent`s.
///
/// Library pipelines (cognify, memify, ingestion) construct this and pass
/// it as the `watcher` to `pipeline::execute`. The HTTP server uses
/// `ScopedRunWatcher` instead, which also publishes to the in-memory
/// event channel.
pub struct DbPipelineWatcher {
    repo: Arc<dyn PipelineRunRepository>,
}

impl DbPipelineWatcher {
    pub fn new(repo: Arc<dyn PipelineRunRepository>) -> Self {
        Self { repo }
    }

    fn core_to_db_status(status: &CoreStatus) -> DbStatus {
        match status {
            CoreStatus::Initiated => DbStatus::Initiated,
            CoreStatus::Started => DbStatus::Started,
            CoreStatus::Completed => DbStatus::Completed,
            CoreStatus::Errored => DbStatus::Errored,
        }
    }
}

#[async_trait]
impl PipelineWatcher for DbPipelineWatcher {
    async fn on_pipeline(&self, _pipeline_id: Uuid, _status: PipelineStatus) {}

    async fn on_task(
        &self,
        _pipeline_id: Uuid,
        _task_index: usize,
        _task_name: Option<&str>,
        _total_tasks: usize,
        _status: TaskStatus,
    ) {
    }

    async fn on_pipeline_run_initiated(&self, run: &PipelineRunInfo) {
        if let Err(e) = self
            .repo
            .log_pipeline_run(
                run.run_id,
                run.pipeline_id,
                &run.pipeline_name,
                run.dataset_id,
                DbStatus::Initiated,
                Some(run_info_for_initiated()),
            )
            .await
        {
            tracing::warn!("DbPipelineWatcher: Initiated write failed (non-fatal): {e}");
        }
    }

    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        if let Err(e) = self
            .repo
            .log_pipeline_run(
                run.run_id,
                run.pipeline_id,
                &run.pipeline_name,
                run.dataset_id,
                Self::core_to_db_status(&run.status),
                Some(run_info_for_running(&run.data_ids)),
            )
            .await
        {
            tracing::warn!("DbPipelineWatcher: Started write failed (non-fatal): {e}");
        }
    }

    async fn on_pipeline_run_completed(&self, run: &PipelineRunInfo, _output_count: usize) {
        if let Err(e) = self
            .repo
            .log_pipeline_run(
                run.run_id,
                run.pipeline_id,
                &run.pipeline_name,
                run.dataset_id,
                DbStatus::Completed,
                Some(run_info_for_running(&run.data_ids)),
            )
            .await
        {
            tracing::warn!("DbPipelineWatcher: Completed write failed (non-fatal): {e}");
        }
    }

    async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, error: &str) {
        if let Err(e) = self
            .repo
            .log_pipeline_run(
                run.run_id,
                run.pipeline_id,
                &run.pipeline_name,
                run.dataset_id,
                DbStatus::Errored,
                Some(run_info_for_errored(&run.data_ids, error)),
            )
            .await
        {
            tracing::warn!("DbPipelineWatcher: Errored write failed (non-fatal): {e}");
        }
    }

    async fn on_payload_field(&self, run_id: Uuid, key: &str, value: serde_json::Value) {
        if let Err(e) = self.repo.set_payload_field(run_id, key, value).await {
            tracing::warn!(run_id=%run_id, key=%key, "DbPipelineWatcher: payload field write failed: {e}");
        }
    }
}
```

Re-export from `crates/core/src/pipeline_run_registry/mod.rs`.

### 4.3 Update `cognify` signature

Edit [`crates/cognify/src/tasks.rs::cognify`](../../crates/cognify/src/tasks.rs) (line 1773). The single change in the body is at **line 1897** — replace `&NoopWatcher` with `&DbPipelineWatcher::new(repo)`:

```rust
pub async fn cognify(
    data_items: Vec<Data>,
    dataset_id: Uuid,
    user_id: Option<Uuid>,
    user_email: Option<String>,
    tenant_id: Option<Uuid>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    pipeline_run_repo: Arc<dyn PipelineRunRepository>, // ← NEW
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: &CognifyConfig,
) -> Result<CognifyResult, CognifyError> {
    // ... existing code unchanged up through line 1896 ...
    let watcher = DbPipelineWatcher::new(pipeline_run_repo.clone());
    let outputs = cognee_core::pipeline::execute(&pipeline, inputs, ctx, &watcher).await?;
    // ... rest of function unchanged ...
}
```

Post-LIB-06, there is **only one** call to `cognee_core::pipeline::execute` in `cognify` (at line 1897). No additional hook plumbing is needed — `pipeline::execute` already fires `on_pipeline_run_initiated` / `Started` / `Completed` / `Errored`.

### 4.4 Update `memify`, `cognify_datasets`, `AddPipeline::add`

Same one-line swap inside each function — replace `&NoopWatcher` with `&DbPipelineWatcher::new(repo)`:
- `crates/cognify/src/memify/pipeline.rs:239`
- `crates/ingestion/src/pipeline.rs:1029`

`cognify_datasets` and `cognify_dataset_refs` ([`crates/cognify/src/dataset_resolver.rs:104`, `:212`](../../crates/cognify/src/dataset_resolver.rs)) do not call `pipeline::execute` directly — they loop over datasets and call `cognify(...)` per dataset. They only need the new parameter added and propagated into each `cognify(...)` call.

For `AddPipeline::add` ([`crates/ingestion/src/pipeline.rs:786`](../../crates/ingestion/src/pipeline.rs#L786)), the pipeline-run trail mirrors Python's `cognee.add()` which also writes to `pipeline_runs`. The pipeline name is `"ingestion_pipeline"` (Python parity — search `cognee/modules/pipelines/operations/run_tasks.py` for the exact string).

Also remove the now-stale `// gap-08 task 07: swap NoopWatcher for DbPipelineWatcher` comment at `crates/cognify/src/memify/pipeline.rs:129` and the equivalent stub comment at `crates/cognify/src/tasks.rs:1896` once the swap lands.

### 4.5 CLI: pass the real repo

Edit [`crates/cli/src/commands/cognify.rs`](../../crates/cli/src/commands/cognify.rs) around line 155-169:

```rust
let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
    Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

let result = cognify(
    data_items,
    dataset.id,
    Some(owner_id),
    user_email,
    dataset.tenant_id,
    llm.clone(),
    Arc::clone(&storage),
    Arc::clone(&graph_db),
    Arc::clone(&vector_db),
    Arc::clone(&embedding_engine),
    Some(Arc::clone(&database)),
    pipeline_run_repo,
    Arc::clone(&ontology_resolver),
    &cognify_config,
)
.await
```

Repeat in:
- [`crates/cli/src/commands/memify.rs`](../../crates/cli/src/commands/memify.rs)
- [`crates/cli/src/commands/add.rs`](../../crates/cli/src/commands/add.rs)
- [`crates/cli/src/commands/add_and_cognify.rs`](../../crates/cli/src/commands/add_and_cognify.rs)
- [`crates/cli/src/commands/run_sequence.rs`](../../crates/cli/src/commands/run_sequence.rs)

### 4.6 HTTP-server: keep `ScopedRunWatcher` — exclusive watcher invariant

The http-server's dispatch path already uses `ScopedRunWatcher` through the registry. `pipeline::execute` accepts exactly one `&dyn PipelineWatcher`; whichever watcher is wired in is the sole writer for that run. The risk is that the HTTP path now calls `cognify(...)` / `memify(...)` / `AddPipeline::add(...)` and those functions internally construct `DbPipelineWatcher`, while the surrounding registry boxed-future also installs `ScopedRunWatcher` on a *different* `pipeline::execute` invocation — producing two row-sets per logical run.

**Required mitigation (load-bearing):** the HTTP boxed futures in [`crates/http-server/src/pipelines/`](../../crates/http-server/src/pipelines/) MUST pass `NoopPipelineRunRepository::arc()` into `cognify(...)` / `memify(...)` / `AddPipeline::add(...)`, so the internal `DbPipelineWatcher` becomes a no-op and the registry's `ScopedRunWatcher` is the sole writer. Add an integration test in task 09 that asserts exactly one row per status per HTTP cognify (four total).

> **Alternative considered (and recommended):** inside each convenience function, branch on the repo identity — if the caller passed a `NoopPipelineRunRepository`, pass `&NoopWatcher` to `pipeline::execute`; otherwise construct and pass `&DbPipelineWatcher`. This avoids the runtime cost of two no-op `await`s per status and makes the "registry-only" path explicit. But it requires a downcast / type tag; the simpler implementation (always construct `DbPipelineWatcher`) is acceptable because writes to `NoopPipelineRunRepository` are `Ok(...)` cheap.

### 4.7 Examples and tests

Run:

```bash
rg "cognify\(|memify\(|AddPipeline::new" examples/ crates/ | grep -v test
```

Every call site needs the new parameter. Examples can pass `NoopPipelineRunRepository::arc()`.

### 4.8 Build + test

```bash
cargo check --all-targets
cargo test -p cognee-cli --test cli_e2e
cargo test -p cognee-cognify
```

## 5. Verification

```bash
# 1. Compiles.
cargo check --all-targets

# 2. CLI cognify round-trip writes four rows.
cargo test -p cognee-cli --test cli_e2e -- cognify_writes_pipeline_runs

# 3. NoopRepo path doesn't error.
cargo test -p cognee-cognify -- noop_repo_smoke

# 4. HTTP-server still produces exactly four rows per cognify (registry-only).
cargo test -p cognee-http-server --test activity_pipeline_runs

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/database/src/pipelines/noop_impl.rs`](../../crates/database/src/pipelines/noop_impl.rs) — **NEW**, `NoopPipelineRunRepository`.
- [`crates/database/src/pipelines/mod.rs`](../../crates/database/src/pipelines/mod.rs) — re-export.
- [`crates/database/src/lib.rs`](../../crates/database/src/lib.rs) — re-export at crate root.
- [`crates/core/src/pipeline_run_registry/db_watcher.rs`](../../crates/core/src/pipeline_run_registry/db_watcher.rs) — **NEW**.
- [`crates/core/src/pipeline_run_registry/mod.rs`](../../crates/core/src/pipeline_run_registry/mod.rs) — re-export.
- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) — `cognify` gains `pipeline_run_repo: Arc<dyn PipelineRunRepository>`; constructs `DbPipelineWatcher`.
- [`crates/cognify/src/memify/pipeline.rs`](../../crates/cognify/src/memify/pipeline.rs) — `memify` same.
- [`crates/cognify/src/dataset_resolver.rs`](../../crates/cognify/src/dataset_resolver.rs) — `cognify_datasets` / `cognify_dataset_refs` propagate the repo through.
- [`crates/ingestion/src/pipeline.rs`](../../crates/ingestion/src/pipeline.rs) — `AddPipeline::add` same.
- [`crates/cli/src/commands/cognify.rs`](../../crates/cli/src/commands/cognify.rs), [`memify.rs`](../../crates/cli/src/commands/memify.rs), [`add.rs`](../../crates/cli/src/commands/add.rs), [`add_and_cognify.rs`](../../crates/cli/src/commands/add_and_cognify.rs), [`run_sequence.rs`](../../crates/cli/src/commands/run_sequence.rs) — construct real repo, pass through.
- HTTP-server pipeline boxed futures in `crates/http-server/src/pipelines/` — pass `NoopPipelineRunRepository::arc()` so the registry's `ScopedRunWatcher` is the sole writer.
- Examples in [`examples/`](../../examples/) — pass `NoopPipelineRunRepository::arc()`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| HTTP-server starts writing rows twice (DbPipelineWatcher inside cognify + ScopedRunWatcher in registry) | **High** if the HTTP path uses a real repo. | Pass `NoopPipelineRunRepository::arc()` from the HTTP boxed future. Add a dedicated integration test (task 09) that asserts exactly four rows per HTTP cognify. |
| Adding a required parameter to every library entry point breaks every test fixture | High — desired. | Use `NoopPipelineRunRepository::arc()` everywhere except CLI / integration tests. The compiler will enumerate fix sites. |
| `DbPipelineWatcher` duplicates `ScopedRunWatcher`'s logic; future divergence risk | Medium | Both call the same helpers (`run_info_for_*`); the only delta is `ScopedRunWatcher`'s sink. If the divergence grows, extract a shared `PersistLifecycle` mixin in a follow-up. |
| Bindings (PyO3, Neon, C) cognify wrappers need the new parameter — surface bloat | Low — bindings expose `Pipeline.run()`, not library-level `cognify(...)`. | Bindings construct `NoopPipelineRunRepository::arc()` internally; no host-facing API change. |
| `pipeline_run_repo: Arc<dyn ...>` requires a fat-pointer everywhere; harmless but visible | Low | The `Arc::clone()` is cheap. |
| `dataset_id` is `Uuid` (not `Option<Uuid>`) in the cognify signature — `Data` inputs may correspond to multiple datasets in some uses? | Low — cognify is single-dataset-per-call. | Confirmed by inspection of `CognifyInput.dataset_id`. |

## 8. Out of scope

- Surfacing `pipeline_run_repo` through bindings as a host-tunable. Bindings stay noop-only in this gap.
- Adding a `Pipeline::with_run_repo(...)` builder option on `cognee_core::Pipeline`. The watcher is the seam.
- Bulk-cognify entry point that runs multiple datasets in one process. Not in scope.
- Letting the HTTP-server path use `DbPipelineWatcher` and dropping `ScopedRunWatcher`. The event channel is load-bearing for HTTP subscribers.
