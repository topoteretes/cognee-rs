# Task 08-06 — Reader helpers on `PipelineRunRepository`

**Status**: implemented in commit 78c73c7 (also removed the provisional list_pipeline_names_for_dataset trait method from 08-05; both callers in cognee-lib + cognee-delete migrated to get_pipeline_runs_by_dataset and kept the skip-already-Initiated semantics)
**Owner**: _unassigned_
**Depends on**: 08-01.
**Blocks**:
- [Task 08-08 — Qualification check](08-check-qualification.md) (`check_pipeline_run_qualification` uses `get_pipeline_run_by_dataset`).
- [Task 08-09 — Tests](09-tests.md).

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: 7 (reader helpers ship with this gap), 12 (`pipeline_run_id` reuse — latest by `created_at` defines current state).

---

## 1. Goal

Add three Python-parity reader methods to `PipelineRunRepository` and implement them on `SeaOrmPipelineRunRepository`:

1. `get_pipeline_run(pipeline_run_id) -> Option<PipelineRun>` — return the latest row matching the given `pipeline_run_id`. Matches Python's [`get_pipeline_run.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_run.py).
2. `get_pipeline_run_by_dataset(dataset_id, pipeline_name) -> Option<PipelineRun>` — return the latest row matching the dataset + pipeline name. Matches Python's [`get_pipeline_run_by_dataset.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_run_by_dataset.py).
3. `get_pipeline_runs_by_dataset(dataset_id) -> Vec<PipelineRun>` — return one row per distinct `pipeline_name` for the dataset, each the latest by `created_at`. Matches Python's [`get_pipeline_runs_by_dataset.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_runs_by_dataset.py).

After this task, the temporary `list_pipeline_names_for_dataset` added in task 08-05 §4.2 is removed (or kept as a thin wrapper); `reset_dataset_pipeline_run_status` uses `get_pipeline_runs_by_dataset`.

## 2. Rationale

Python's modules consume these three helpers in qualification, reset, and metric flows. The Rust repo has `latest_status(dataset_ids: &[Uuid], pipeline_name) -> HashMap<Uuid, PipelineRunStatus>` which is close to (3) but loses the rest of the row and doesn't cover (1) or (2). The qualification check in task 08-08 needs the full `PipelineRun` to read `created_at` (for tiebreaking) and `status`.

## 3. Pre-conditions

- Task 08-01 committed — domain `PipelineRun.dataset_id: Option<Uuid>`.
- Existing `latest_status` impl as reference for ORDER BY semantics ([`sea_orm_impl.rs:78-106`](../../crates/database/src/pipelines/sea_orm_impl.rs#L78-L106)).

## 4. Step-by-step

### 4.1 Extend the trait

Edit [`crates/database/src/pipelines/repository.rs`](../../crates/database/src/pipelines/repository.rs):

```rust
/// Return the latest row for `pipeline_run_id` (ordered by `created_at DESC`).
///
/// Multiple rows share the same `pipeline_run_id` — Python intentionally
/// reuses it across status transitions. This method picks the most recent.
async fn get_pipeline_run(
    &self,
    pipeline_run_id: Uuid,
) -> Result<Option<PipelineRun>, DatabaseError>;

/// Return the latest run for `(dataset_id, pipeline_name)` by `created_at`.
async fn get_pipeline_run_by_dataset(
    &self,
    dataset_id: Uuid,
    pipeline_name: &str,
) -> Result<Option<PipelineRun>, DatabaseError>;

/// Return one latest row per distinct `pipeline_name` that has runs for
/// `dataset_id`. Result order is unspecified.
async fn get_pipeline_runs_by_dataset(
    &self,
    dataset_id: Uuid,
) -> Result<Vec<PipelineRun>, DatabaseError>;
```

### 4.2 Implement on `SeaOrmPipelineRunRepository`

Add to [`crates/database/src/pipelines/sea_orm_impl.rs`](../../crates/database/src/pipelines/sea_orm_impl.rs):

```rust
async fn get_pipeline_run(
    &self,
    pipeline_run_id: Uuid,
) -> Result<Option<PipelineRun>, DatabaseError> {
    let row = pipeline_run::Entity::find()
        .filter(pipeline_run::Column::PipelineRunId.eq(uuid_hex::to_hex(pipeline_run_id)))
        .order_by_desc(pipeline_run::Column::CreatedAt)
        .one(self.db.as_ref())
        .await
        .map_err(|e| DatabaseError::QueryError(format!("get_pipeline_run query failed: {e}")))?;
    Ok(row.map(PipelineRun::from))
}

async fn get_pipeline_run_by_dataset(
    &self,
    dataset_id: Uuid,
    pipeline_name: &str,
) -> Result<Option<PipelineRun>, DatabaseError> {
    let row = pipeline_run::Entity::find()
        .filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .filter(pipeline_run::Column::PipelineName.eq(pipeline_name))
        .order_by_desc(pipeline_run::Column::CreatedAt)
        .one(self.db.as_ref())
        .await
        .map_err(|e| {
            DatabaseError::QueryError(format!("get_pipeline_run_by_dataset query failed: {e}"))
        })?;
    Ok(row.map(PipelineRun::from))
}

async fn get_pipeline_runs_by_dataset(
    &self,
    dataset_id: Uuid,
) -> Result<Vec<PipelineRun>, DatabaseError> {
    let rows = pipeline_run::Entity::find()
        .filter(pipeline_run::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_desc(pipeline_run::Column::CreatedAt)
        .all(self.db.as_ref())
        .await
        .map_err(|e| {
            DatabaseError::QueryError(format!("get_pipeline_runs_by_dataset query failed: {e}"))
        })?;

    // Pick the first (latest) row per distinct pipeline_name.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        if seen.insert(row.pipeline_name.clone()) {
            out.push(PipelineRun::from(row));
        }
    }
    Ok(out)
}
```

### 4.3 Refactor `reset_dataset_pipeline_run_status` (if 08-05 already landed)

If task 08-05 introduced `list_pipeline_names_for_dataset` as a temporary helper, replace its call site in [`crates/lib/src/api/pipeline_runs.rs`](../../crates/lib/src/api/pipeline_runs.rs):

```rust
pub async fn reset_dataset_pipeline_run_status(
    repo: Arc<dyn PipelineRunRepository>,
    user_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), ApiError> {
    let runs = repo
        .get_pipeline_runs_by_dataset(dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("get_pipeline_runs_by_dataset: {e}")))?;
    for run in runs {
        if matches!(run.status, PipelineRunStatus::Initiated) {
            continue;
        }
        reset_pipeline_run_status(repo.clone(), user_id, dataset_id, &run.pipeline_name).await?;
    }
    Ok(())
}
```

A second caller of `list_pipeline_names_for_dataset` lives in [`crates/delete/src/lib.rs`](../../crates/delete/src/lib.rs#L637) (around line 637, inside `prune_pipeline_state_for_dataset`). It consumes the same `(name, latest_status)` shape and must be swapped to `get_pipeline_runs_by_dataset` in the same PR — otherwise removing `list_pipeline_names_for_dataset` from the trait leaves the delete crate uncompilable.

Remove `list_pipeline_names_for_dataset` from the repo trait + impl + any mocks that picked it up in task 08-05.

### 4.4 Update non-SeaOrm impls

[`cognee-test-utils`](../../crates/test-utils/) does not include a `MockPipelineRunRepository` today — most tests use the real `SeaOrmPipelineRunRepository` against an in-memory SQLite pool. However, the following non-SeaOrm impls of `PipelineRunRepository` exist in the tree and must gain stubs for the three new methods (and lose `list_pipeline_names_for_dataset` if/when it is dropped):

- `NoOpPipelineRunRepository` in [`crates/http-server/src/state.rs`](../../crates/http-server/src/state.rs)
- `FailingRepo` in [`crates/core/tests/scoped_watcher_payload_persistence.rs`](../../crates/core/tests/scoped_watcher_payload_persistence.rs)

Stubs should return `Ok(None)` for the two single-row readers (`get_pipeline_run`, `get_pipeline_run_by_dataset`) and `Ok(vec![])` for the multi-row reader (`get_pipeline_runs_by_dataset`). Confirm coverage via:

```bash
rg "impl PipelineRunRepository for" crates/
```

If any additional test-only impl appears, apply the same `Ok(None)` / `Ok(vec![])` pattern.

### 4.5 Build + test

```bash
cargo check --all-targets
cargo test -p cognee-database --test pipeline_run_repository
```

## 5. Verification

```bash
cargo check --all-targets
cargo test -p cognee-database --test pipeline_run_repository -- get_pipeline_run
cargo test -p cognee-database --test pipeline_run_repository -- get_pipeline_run_by_dataset
cargo test -p cognee-database --test pipeline_run_repository -- get_pipeline_runs_by_dataset
scripts/check_all.sh
```

## 6. Files modified

- [`crates/database/src/pipelines/repository.rs`](../../crates/database/src/pipelines/repository.rs) — three new trait methods.
- [`crates/database/src/pipelines/sea_orm_impl.rs`](../../crates/database/src/pipelines/sea_orm_impl.rs) — impls.
- [`crates/lib/src/api/pipeline_runs.rs`](../../crates/lib/src/api/pipeline_runs.rs) — `reset_dataset_pipeline_run_status` uses `get_pipeline_runs_by_dataset`.
- (Possibly) drop `list_pipeline_names_for_dataset` from repo trait and impl, if added in task 08-05 as a temp shim.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `get_pipeline_runs_by_dataset` ordering is unspecified — callers depending on specific order break | Low — caller (reset helper) iterates without order assumption. | Document "result order unspecified" in the trait. |
| ORDER BY `created_at DESC` is non-deterministic when two rows share a microsecond (Postgres) — picks an arbitrary one | Low — write throughput is 1 row/state transition. | Acceptable. Python has the same behaviour. |
| `get_pipeline_run` returns rows where `dataset_id IS NULL` for ad-hoc runs that happen to share a `pipeline_run_id` | Low | The `pipeline_run_id` derivation already includes `dataset_id` (UUIDv5), so collisions across datasets are not possible. |
| Adding three trait methods inflates the binding surface (PyO3 / Neon) | None — bindings don't surface the trait directly. | No impact. |
| Refactor of 08-05's `list_pipeline_names_for_dataset` requires a follow-up commit | Low — bundled in this PR. | Land both file changes in this task. |

## 8. Python-parity notes

- Python's [`get_pipeline_run`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_run.py) calls `session.scalar()` without an `ORDER BY` clause — it returns whatever row the database surfaces first. The Rust plan strictly orders by `created_at DESC` and picks the latest, which is a *stronger* guarantee consistent with locked decision 12 ("latest by `created_at` defines current state"). Intentional, not drift.

## 9. Out of scope

- A unified `find_runs(filter)` method. The three Python parity helpers are explicit; a generic finder is harder to align cross-SDK.
- Returning `pipeline_runs` joined with attribution (owner, dataset name) — that's `list_recent_with_attribution`'s domain.
- A version of these helpers that takes a user_id filter for tenant isolation. Python's helpers do not filter by user; Rust matches.
