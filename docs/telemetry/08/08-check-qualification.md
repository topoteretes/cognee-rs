# Task 08-08 — `check_pipeline_run_qualification` gate

**Status**: not yet implemented (⬜)
**Owner**: _unassigned_
**Depends on**: 08-06, 08-07.
**Blocks**:
- [Task 08-09 — Tests](09-tests.md) (qualification has dedicated tests).
- [Task 08-10 — Docs + CI](10-docs-and-ci.md).

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: 3 (qualification check ships with this gap, cognify + memify), 13 (use existing `RunEventKind::AlreadyCompleted` for the short-circuit signal).

---

## 1. Goal

Add Python-parity `check_pipeline_run_qualification` to the cognify and memify entry points so that:

- Calling `cognify(...)` on a dataset whose latest pipeline_run is `COMPLETED` **short-circuits** without re-running tasks. The function returns a `CognifyResult` derived from the last successful run (or, if that's not feasible, an empty result with a flag) and emits an `AlreadyCompleted` `RunEvent` for HTTP subscribers (via `ScopedRunWatcher`, when applicable).
- Calling `cognify(...)` on a dataset whose latest pipeline_run is `STARTED` **rejects** the call with a `CognifyError::PipelineAlreadyRunning`.
- Calling `cognify(...)` on a dataset whose latest pipeline_run is `INITIATED` or `ERRORED` (or has no rows) **proceeds normally**.

Same gating applies to `memify(...)`. Ingestion is **out of scope** (Python's ingestion path does not consult this gate — see decision 3).

## 2. Rationale

Python's [`check_pipeline_run_qualification.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/check_pipeline_run_qualification.py) is consulted by [`run_pipeline.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_pipeline.py) before calling `run_tasks`. Without the check, re-running cognify on a completed dataset spends LLM calls re-extracting the graph that's already there. Worse, partial duplicates can corrupt the graph if a re-run interleaves with a slow vector index commit.

Decision 3 ships the check in this gap because the four-state trail is the prerequisite and shipping the trail without the gate leaves the trail unused.

## 3. Pre-conditions

- Tasks 08-01 through 08-07 committed.
- `PipelineRunRepository::get_pipeline_run_by_dataset` available (task 08-06).
- `cognify`, `memify` entry points accept `Arc<dyn PipelineRunRepository>` (task 08-07).
- `RunEventKind::AlreadyCompleted` variant present at [`types.rs:40`](../../crates/core/src/pipeline_run_registry/types.rs#L40).

## 4. Step-by-step

### 4.1 Add `CognifyError::PipelineAlreadyRunning`

Edit [`crates/cognify/src/error.rs`](../../crates/cognify/src/error.rs) — append:

```rust
#[error("pipeline {pipeline_name} for dataset {dataset_id} is already running")]
PipelineAlreadyRunning {
    pipeline_name: String,
    dataset_id: Uuid,
},
```

### 4.2 Add the qualification helper

Add `crates/cognify/src/qualification.rs`:

```rust
//! Python-parity `check_pipeline_run_qualification`.
//!
//! Reads the latest `pipeline_runs` row for `(dataset_id, pipeline_name)`
//! and returns a verdict the caller acts on.

use std::sync::Arc;

use uuid::Uuid;

use cognee_database::{PipelineRun, PipelineRunRepository, PipelineRunStatus};

/// Verdict from a qualification check.
#[derive(Debug, Clone)]
pub enum Qualification {
    /// No previous run, or the latest is `INITIATED` or `ERRORED` — proceed.
    Proceed,
    /// Latest is `STARTED` — reject; caller should error.
    AlreadyRunning(PipelineRun),
    /// Latest is `COMPLETED` — short-circuit; caller should not re-run.
    AlreadyCompleted(PipelineRun),
}

/// Mirror of Python's `check_pipeline_run_qualification`.
pub async fn check_pipeline_run_qualification(
    repo: &dyn PipelineRunRepository,
    dataset_id: Uuid,
    pipeline_name: &str,
) -> Result<Qualification, cognee_database::DatabaseError> {
    let latest = repo
        .get_pipeline_run_by_dataset(dataset_id, pipeline_name)
        .await?;
    Ok(match latest {
        None => Qualification::Proceed,
        Some(run) => match run.status {
            PipelineRunStatus::Initiated | PipelineRunStatus::Errored => Qualification::Proceed,
            PipelineRunStatus::Started => Qualification::AlreadyRunning(run),
            PipelineRunStatus::Completed => Qualification::AlreadyCompleted(run),
        },
    })
}
```

Re-export from `crates/cognify/src/lib.rs`.

### 4.3 Apply the gate in `cognify`

Edit [`crates/cognify/src/tasks.rs::cognify`](../../crates/cognify/src/tasks.rs) near the top of the function (after config validation, before any task work):

```rust
use crate::qualification::{check_pipeline_run_qualification, Qualification};

let pipeline_name = "cognify_pipeline";
match check_pipeline_run_qualification(
    pipeline_run_repo.as_ref(),
    dataset_id,
    pipeline_name,
)
.await
.map_err(|e| CognifyError::DatabaseError(e.to_string()))?
{
    Qualification::AlreadyCompleted(prior) => {
        info!(
            dataset_id = %dataset_id,
            pipeline_run_id = %prior.pipeline_run_id,
            "cognify: dataset already completed; short-circuiting (Python parity)"
        );
        // Return an empty result. Callers that need the prior run's outputs
        // can query the graph/vector stores directly — the row in
        // `pipeline_runs` is the source of truth for "this dataset is done".
        return Ok(CognifyResult::already_completed(prior.pipeline_run_id));
    }
    Qualification::AlreadyRunning(prior) => {
        return Err(CognifyError::PipelineAlreadyRunning {
            pipeline_name: pipeline_name.to_string(),
            dataset_id,
        });
    }
    Qualification::Proceed => {}
}
```

Add `CognifyResult::already_completed(pipeline_run_id: Uuid)` constructor that produces an empty result tagged as already-completed:

```rust
// In crates/cognify/src/pipeline.rs (or wherever CognifyResult lives):
impl CognifyResult {
    pub fn already_completed(pipeline_run_id: Uuid) -> Self {
        Self {
            already_completed: true,
            // ... empty vectors / zero counts ...
            ..Default::default()
        }
    }
}
```

Add an `already_completed: bool` field to `CognifyResult`. Default `false`. CLI prints "already complete" when set; HTTP-server returns `200 OK` with the prior `pipeline_run_id` and `status = "DATASET_PROCESSING_COMPLETED"` in the body.

### 4.4 Apply the gate in `memify`

Same pattern in [`crates/cognify/src/memify/pipeline.rs::memify`](../../crates/cognify/src/memify/pipeline.rs#L57) with `pipeline_name = "memify_pipeline"` and a corresponding `MemifyError::PipelineAlreadyRunning` + `MemifyResult::already_completed`.

### 4.5 HTTP-server: surface the `AlreadyCompleted` event

The registry's `ScopedRunWatcher` already publishes `RunEvent { kind: RunEventKind::AlreadyCompleted, ... }` — but no current code path produces this kind. Add a watcher hook so callers can fire it on the short-circuit path.

Actually — the short-circuit happens inside `cognify(...)` *before* `pipeline::execute` is called, so the executor's watcher events don't fire. The HTTP boxed future needs to handle this:

```rust
// In crates/http-server/src/pipelines/p3_routers/cognify.rs (or analogous):
let result = cognify(...).await?;
if result.already_completed {
    // Emit an AlreadyCompleted event on the registry's run channel so
    // /api/v1/pipeline-runs/{id}/events subscribers see the short-circuit.
    state.pipelines.publish_already_completed(run_id);
}
```

Add `DefaultPipelineRunRegistry::publish_already_completed(run_id)` that publishes `RunEventKind::AlreadyCompleted` to the slot's broadcast channel (analogous to the existing publish paths).

For library-level callers (no registry), no event is fired — the `CognifyResult.already_completed` flag is the signal.

### 4.6 `Errored` status counts as Proceed — confirm Python parity

Python's `check_pipeline_run_qualification` returns `True` (proceed) for `INITIATED`, `ERRORED`, *and* missing rows. Both languages should match. Verify the Python source as of the gap implementation date:

```bash
git -C /tmp/cognee-python show HEAD:cognee/modules/pipelines/layers/check_pipeline_run_qualification.py
```

If Python's behaviour ever changes, update the Rust match arm accordingly.

### 4.7 Build + test

```bash
cargo check --all-targets
cargo test -p cognee-cognify --test qualification
```

## 5. Verification

```bash
# 1. Compiles.
cargo check --all-targets

# 2. Qualification unit tests.
cargo test -p cognee-cognify --lib -- qualification

# 3. Cognify integration test — re-running a completed dataset returns
#    already_completed=true and writes no new pipeline_runs rows.
cargo test -p cognee-cognify --test cognify_qualification

# 4. HTTP-server emits AlreadyCompleted on the registry channel for
#    short-circuit paths.
cargo test -p cognee-http-server --test cognify_short_circuit

# 5. CLI cognify on a completed dataset prints "already complete" and
#    exits 0 (no error).
cargo test -p cognee-cli --test cli_e2e -- cognify_short_circuit

# 6. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/cognify/src/error.rs`](../../crates/cognify/src/error.rs) — `PipelineAlreadyRunning` variant.
- [`crates/cognify/src/qualification.rs`](../../crates/cognify/src/qualification.rs) — **NEW**, the helper.
- [`crates/cognify/src/lib.rs`](../../crates/cognify/src/lib.rs) — re-export.
- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) — apply gate in `cognify(...)`.
- [`crates/cognify/src/pipeline.rs`](../../crates/cognify/src/pipeline.rs) — `CognifyResult.already_completed` field + constructor.
- [`crates/cognify/src/memify/pipeline.rs`](../../crates/cognify/src/memify/pipeline.rs) — apply gate, mirror `MemifyResult.already_completed`.
- [`crates/cognify/src/memify/result.rs`](../../crates/cognify/src/memify/result.rs) (or wherever `MemifyResult` lives) — same.
- [`crates/core/src/pipeline_run_registry/default_impl.rs`](../../crates/core/src/pipeline_run_registry/default_impl.rs) — `publish_already_completed(run_id)` helper.
- [`crates/core/src/pipeline_run_registry/trait_def.rs`](../../crates/core/src/pipeline_run_registry/trait_def.rs) — add to trait if other registries need it.
- [`crates/http-server/src/pipelines/`](../../crates/http-server/src/pipelines/) (boxed futures) — call `publish_already_completed` when `result.already_completed` is true.
- [`crates/cli/src/commands/cognify.rs`](../../crates/cli/src/commands/cognify.rs) — print "already complete" message when the flag is set.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `CognifyResult` shape change is a wire-shape break for any binding consumer | Medium — bindings serialize `CognifyResult` through PyO3 / Neon. | Add `already_completed: bool` with `#[serde(default)]` (defaults to false on deserialize). Binding consumers see the new field appear in payloads. |
| Short-circuit returns empty vectors but caller expects the prior run's outputs | Medium — Python returns the prior run's metadata, not the data. | Document: after short-circuit, query the graph/vector store directly. Match Python. |
| Race: two concurrent cognify calls both see `INITIATED` and both proceed → duplicate runs | Low — the executor's `INITIATED` row is written before tasks start; the second call will see `STARTED` if scheduled later. | Acceptable race window matches Python's. For strict locking, introduce a row-level lock (out of scope). |
| Errored status surfaces a stale error to the next run via the row's `run_info["error"]` | Low — we proceed on errored, the new run shadows it via a fresh `INITIATED`. | No action needed. |
| `MemifyError::PipelineAlreadyRunning` adds variant; binding consumers must handle | Low | Internal to library; bindings catch via the unified error wrapper. |
| HTTP-server's `publish_already_completed` requires registry plumbing — may not have a `run_id` at the boxed-future layer | Acknowledged. | The dispatcher already computes `run_id` and passes it through `RunSpec.run_id`; the boxed future receives it via context. If not, surface a builder hook on `ScopedRunWatcher`. |
| Telemetry / OTLP spans for `cognify` short-circuit fire a `Pipeline Run Started` analytics event that's misleading | Low — short-circuit returns before `pipeline::execute` runs, so no analytics event fires. | Acceptable. |

## 8. Out of scope

- Extending the gate to ingestion. Python excludes it.
- A per-tenant cooldown / rate limit. Out of scope.
- A force-reprocess flag on `cognify(...)` that bypasses the gate. Reset via `reset_pipeline_run_status` (task 08-05) instead.
- A queue / retry behavior for the `AlreadyRunning` reject path. Caller error; not the library's responsibility.
- Streaming the short-circuit verdict back through `RunEvent::AlreadyCompleted` for non-HTTP runs. The `CognifyResult.already_completed` flag is sufficient.
