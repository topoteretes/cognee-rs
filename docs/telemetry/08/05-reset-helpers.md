# Task 08-05 — `reset_pipeline_run_status` + `reset_dataset_pipeline_run_status` helpers

**Status**: not yet implemented (⬜)
**Owner**: _unassigned_
**Depends on**: 08-04.
**Blocks**:
- [Task 08-08 — Qualification check](08-check-qualification.md) (the check reads the latest row; a `reset_*` call must shadow a prior `COMPLETED` with an `INITIATED` row so the check lets the re-cognify through).
- [Task 08-09 — Tests](09-tests.md) (reset helper has dedicated round-trip tests).

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: 5 (`run_info = {}` for `INITIATED`), 10 (synchronous writes), 11 (single point of truth via the repo).

---

## 1. Goal

Expose Python-parity reset helpers as part of `cognee_lib`'s public API:

1. `reset_pipeline_run_status(repo, user_id, dataset_id, pipeline_name)` — writes a fresh `INITIATED` row with `run_info = {}`. Matches Python's [`reset_pipeline_run_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/reset_pipeline_run_status.py).
2. `reset_dataset_pipeline_run_status(repo, user_id, dataset_id)` — iterates *all* pipeline names with a row for the dataset and calls `reset_pipeline_run_status` for each (skipping ones already at `INITIATED`). Matches Python's [`reset_dataset_pipeline_run_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/reset_dataset_pipeline_run_status.py).
3. Wire `reset_dataset_pipeline_run_status` into the prune / dataset-reset flow in [`crates/lib/src/api/prune.rs`](../../crates/lib/src/api/prune.rs) and the CLI delete command at [`crates/cli/src/commands/delete.rs`](../../crates/cli/src/commands/delete.rs).

## 2. Rationale

Without these helpers, a user has no public-API way to mark a previously-completed dataset for reprocessing. Today the only way to re-run cognify against a completed dataset is to mutate the DB directly. After task 08-08 lands, the qualification check will start short-circuiting completed runs — without a reset helper, users will hit a wall.

Python plumbs the reset into prune flows so `cognee.prune()` invalidates all pipeline runs for the targeted datasets. Decision 2 (always-on registry for library pipelines) makes the same plumbing necessary in Rust.

## 3. Pre-conditions

- Tasks 08-01, 08-02, 08-03, 08-04 committed.
- `PipelineRunRepository::log_pipeline_run` accepts `Option<Uuid>` for `dataset_id` and persists the row (silent-drop removed in task 08-01).
- `run_info_for_initiated()` available at `cognee_core::pipeline_run_registry::run_info_for_initiated`.
- `pipeline_id` / `pipeline_run_id` derivation helpers available at [`crates/http-server/src/pipelines/dispatch.rs:33-49`](../../crates/http-server/src/pipelines/dispatch.rs#L33-L49) — these need promoting into a shared library location (see §4.0).
- A way to enumerate distinct `pipeline_name` per `dataset_id`. Task 08-06 adds `get_pipeline_runs_by_dataset` — but this helper is needed independently. Use a SeaORM query in the meantime, then refactor to call `get_pipeline_runs_by_dataset` once 08-06 lands.

> **Sequencing note:** since this task depends on enumerating pipeline names per dataset, the orchestrator should reorder tasks 05/06 internally if it makes the implementation easier. The runbook keeps numeric order (05 first), and 05's implementation uses a direct repo query that 06 later replaces with the reader helper. Alternatively, swap the implementation order in the same PR.

## 4. Step-by-step

### 4.0 Promote `pipeline_id` / `pipeline_run_id` to a shared location

The deterministic ID helpers currently live in `crates/http-server/src/pipelines/dispatch.rs`. Move them to `crates/core/src/pipeline_run_registry/ids.rs` so library code can call them without depending on `cognee-http-server`:

```rust
//! Deterministic pipeline & pipeline-run IDs (Python parity).

use uuid::Uuid;

/// `pipeline_id = uuid5(OID, "{user_id}{pipeline_name}{dataset_id}")`
///
/// `dataset_id` defaults to `Uuid::nil()` when absent (ad-hoc paths).
pub fn pipeline_id(user_id: Uuid, dataset_id: Uuid, pipeline_name: &str) -> Uuid {
    let s = format!("{user_id}{pipeline_name}{dataset_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes())
}

/// `pipeline_run_id = uuid5(OID, "{pipeline_id}_{dataset_id}")`
pub fn pipeline_run_id(pipeline_id: Uuid, dataset_id: Uuid) -> Uuid {
    let s = format!("{pipeline_id}_{dataset_id}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes())
}
```

Re-export from `crates/core/src/pipeline_run_registry/mod.rs` and re-export from the http-server's dispatch (keep the existing API surface; just delegate). Update the http-server's call sites.

### 4.1 Add `crates/lib/src/api/pipeline_runs.rs`

```rust
//! Python-parity reset helpers for `pipeline_runs`.

use std::sync::Arc;

use uuid::Uuid;

use cognee_core::pipeline_run_registry::{
    ids::{pipeline_id, pipeline_run_id},
    run_info_for_initiated,
};
use cognee_database::{
    PipelineRunRepository, PipelineRunStatus,
    DatabaseError,
};

use crate::error::ApiError;

/// Insert a fresh `INITIATED` row for the `(user_id, dataset_id, pipeline_name)`
/// triple so a future re-cognify is not short-circuited by
/// `check_pipeline_run_qualification`.
///
/// Matches Python's
/// [`reset_pipeline_run_status`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/reset_pipeline_run_status.py).
///
/// `run_info` is `{}` per Python's `log_pipeline_run_initiated.py`.
pub async fn reset_pipeline_run_status(
    repo: Arc<dyn PipelineRunRepository>,
    user_id: Uuid,
    dataset_id: Uuid,
    pipeline_name: &str,
) -> Result<(), ApiError> {
    let pid = pipeline_id(user_id, dataset_id, pipeline_name);
    let prid = pipeline_run_id(pid, dataset_id);
    repo.log_pipeline_run(
        prid,
        pid,
        pipeline_name,
        Some(dataset_id),
        PipelineRunStatus::Initiated,
        Some(run_info_for_initiated()),
    )
    .await
    .map(|_| ())
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("reset_pipeline_run_status: {e}")))
}

/// Walk every distinct `(pipeline_name)` that has at least one
/// `pipeline_runs` row for the dataset and call
/// [`reset_pipeline_run_status`] for each, skipping ones whose latest
/// status is already `INITIATED`.
///
/// Matches Python's
/// [`reset_dataset_pipeline_run_status`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/reset_dataset_pipeline_run_status.py).
pub async fn reset_dataset_pipeline_run_status(
    repo: Arc<dyn PipelineRunRepository>,
    user_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), ApiError> {
    // Lands on task 08-06: this becomes `repo.get_pipeline_runs_by_dataset(dataset_id)`.
    // For now, query distinct pipeline names directly via a repo method
    // `list_pipeline_names_for_dataset(dataset_id)` introduced inline here.
    let names = repo
        .list_pipeline_names_for_dataset(dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("list_pipeline_names_for_dataset: {e}")))?;

    for (name, latest_status) in names {
        if matches!(latest_status, PipelineRunStatus::Initiated) {
            continue; // already pending — no-op
        }
        reset_pipeline_run_status(repo.clone(), user_id, dataset_id, &name).await?;
    }
    Ok(())
}
```

Re-export from `crates/lib/src/api/mod.rs`.

### 4.2 Add `list_pipeline_names_for_dataset` to the repo trait

Edit [`crates/database/src/pipelines/repository.rs`](../../crates/database/src/pipelines/repository.rs) — append to the trait:

```rust
/// Return one (`pipeline_name`, latest status) pair per distinct pipeline
/// name that has at least one row for `dataset_id`. "Latest" is by
/// `created_at DESC`.
async fn list_pipeline_names_for_dataset(
    &self,
    dataset_id: Uuid,
) -> Result<Vec<(String, PipelineRunStatus)>, DatabaseError>;
```

Implement in [`crates/database/src/pipelines/sea_orm_impl.rs`](../../crates/database/src/pipelines/sea_orm_impl.rs):

```rust
async fn list_pipeline_names_for_dataset(
    &self,
    dataset_id: Uuid,
) -> Result<Vec<(String, PipelineRunStatus)>, DatabaseError> {
    let rows = pipeline_run::Entity::find()
        .filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_desc(pipeline_run::Column::CreatedAt)
        .all(self.db.as_ref())
        .await
        .map_err(|e| {
            DatabaseError::QueryError(format!(
                "list_pipeline_names_for_dataset query failed: {e}"
            ))
        })?;

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        if seen.insert(row.pipeline_name.clone()) {
            let run: PipelineRun = row.into();
            out.push((run.pipeline_name, run.status));
        }
    }
    Ok(out)
}
```

> **Note:** task 08-06 will introduce `get_pipeline_runs_by_dataset(dataset_id) -> Vec<PipelineRun>` which is strictly more general. After 08-06 lands, refactor `reset_dataset_pipeline_run_status` to call `get_pipeline_runs_by_dataset` and remove `list_pipeline_names_for_dataset` (keep the trait minimal). Alternatively, swap implementation order in the same PR — see §3 sequencing note.

### 4.3 Wire into `prune`

Edit [`crates/lib/src/api/prune.rs`](../../crates/lib/src/api/prune.rs):

`prune_data` / `prune_system` should:

1. Enumerate datasets owned by the user (already done — search for the existing iteration).
2. For each dataset, call `reset_dataset_pipeline_run_status(repo.clone(), user_id, dataset_id)` before cascading deletes.

Concretely, locate the function body around line 67-86 and inject the reset call before the storage prune.

> Caveat: prune in Rust today actually deletes datasets via `cognee_delete::DeleteService`, which cascades through `pipeline_runs` because of the FK. After task 08-01 drops the FK, the rows survive the dataset delete — the reset call writes a fresh `INITIATED` row that points at a soon-to-be-orphan `dataset_id`. This is intentional: a re-cognify after prune always needs to start fresh; the orphan rows are harmless and surface in `list_recent_with_attribution` with `dataset_name = None`.
>
> If you prefer the rows to disappear with the dataset, add a `repo.delete_runs_for_dataset(dataset_id)` step instead. **Decision deferred — pending user input.** Lands a sub-decision in 08-10's closure summary.

For now, the reset call replaces the implicit FK cascade.

### 4.4 Wire into CLI `delete`

Edit [`crates/cli/src/commands/delete.rs`](../../crates/cli/src/commands/delete.rs). When the subcommand resolves a dataset for deletion, call `reset_dataset_pipeline_run_status` before the cascade. Use the same `Arc<dyn PipelineRunRepository>` constructed in task 08-07 for the cognify CLI command.

### 4.5 Build + test

```bash
cargo check --all-targets
cargo test -p cognee-lib --test api_pipeline_runs   # ← test lands in 08-09
cargo test -p cognee-database --test pipeline_run_repository
```

## 5. Verification

```bash
# 1. Compiles.
cargo check --all-targets

# 2. Repo trait extension round-trips.
cargo test -p cognee-database --test pipeline_run_repository -- list_pipeline_names

# 3. CLI delete invokes the reset.
cargo test -p cognee-cli --test cli_e2e -- delete_resets_runs

# 4. Prune end-to-end test still passes (existing test).
cargo test -p cognee-lib --test prune

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/core/src/pipeline_run_registry/ids.rs`](../../crates/core/src/pipeline_run_registry/ids.rs) — **NEW**, promote `pipeline_id` / `pipeline_run_id` helpers from the http-server.
- [`crates/core/src/pipeline_run_registry/mod.rs`](../../crates/core/src/pipeline_run_registry/mod.rs) — re-export the IDs.
- [`crates/http-server/src/pipelines/dispatch.rs`](../../crates/http-server/src/pipelines/dispatch.rs) — delegate to the new shared helpers.
- [`crates/database/src/pipelines/repository.rs`](../../crates/database/src/pipelines/repository.rs) — `list_pipeline_names_for_dataset` trait method.
- [`crates/database/src/pipelines/sea_orm_impl.rs`](../../crates/database/src/pipelines/sea_orm_impl.rs) — impl.
- [`crates/lib/src/api/pipeline_runs.rs`](../../crates/lib/src/api/pipeline_runs.rs) — **NEW**, both reset helpers.
- [`crates/lib/src/api/mod.rs`](../../crates/lib/src/api/mod.rs) — re-export.
- [`crates/lib/src/api/prune.rs`](../../crates/lib/src/api/prune.rs) — call `reset_dataset_pipeline_run_status` before cascade.
- [`crates/cli/src/commands/delete.rs`](../../crates/cli/src/commands/delete.rs) — same wiring.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Adding a trait method without a default body breaks every test mock that impls `PipelineRunRepository` | High — must update each. | Provide a `default impl` body returning `Ok(vec![])` is not possible on async-trait without unstable features; instead, add the method without default and update each impl explicitly. Greppable: `rg "impl PipelineRunRepository for"`. |
| `list_pipeline_names_for_dataset` becomes obsolete after task 08-06 — code churn | Medium | Alternative path: swap implementation order so 08-06 lands first and 08-05 uses `get_pipeline_runs_by_dataset` directly. The runbook tolerates this; document choice in commit body. |
| Prune ↔ reset interaction: rows survive dataset delete after task 08-01 drops the FK; `list_recent_with_attribution` surfaces orphans | Acknowledged. | Document in 08-10 closure summary. Optionally add `repo.delete_runs_for_dataset` in a follow-up; **NOT** in this gap. |
| Calling `reset_pipeline_run_status` for a dataset that already has `INITIATED` writes a duplicate row | Low — Python's helper does the same; the `INITIATED → INITIATED` transition is idempotent in spirit. | The dataset-level helper already short-circuits if latest status is `Initiated`. For the per-name helper, document that re-calling produces multiple rows — same as Python. |
| Concurrency: two `reset_*` calls racing produce two `INITIATED` rows | Low — Python is single-threaded per request. | Accept the same race. Latest by `created_at` wins. |

## 8. Out of scope

- Bulk-reset helper that iterates *all* datasets for a user (no Python equivalent).
- `delete_runs_for_dataset` repo method. Out of scope; rows survive.
- Surfacing the reset helpers through the HTTP `POST /datasets/{id}/reset` endpoint. Not in the current Rust HTTP surface; cross-route addition.
- Surfacing through bindings (Python/JS/C). Out of scope — the helpers are library-level.
