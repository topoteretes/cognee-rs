# Task 08-04 — Emit `INITIATED` from `pipeline::execute`

**Status**: not yet implemented (⬜)
**Owner**: _unassigned_
**Depends on**: 08-03.
**Blocks**:
- [Task 08-05 — Reset helpers](05-reset-helpers.md) (reset semantics rely on `INITIATED` actually persisting).
- [Task 08-07 — Library wiring](07-library-pipeline-wiring.md) (library pipelines must produce the four-state trail, including INITIATED).
- [Task 08-08 — Qualification check](08-check-qualification.md) (qualification reads the latest status; an `INITIATED` row after a reset must shadow a prior `COMPLETED`).

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: 1 (INITIATED emitted by `pipeline::execute`, Option A), 13 (no new `RunEventKind` variant — DB-only).

---

## 1. Goal

Make `cognee_core::pipeline::execute` emit `INITIATED` as the first lifecycle state, before transitioning to `STARTED`:

1. Add `PipelineWatcher::on_pipeline_run_initiated` with a default no-op body.
2. In `execute`, construct `PipelineRunInfo` with `status = Initiated`, fire `watcher.on_pipeline_run_initiated(&run_info).await`, then transition to `Started` immediately before kicking off task execution.
3. Implement `on_pipeline_run_initiated` on `ScopedRunWatcher` to write an `INITIATED` row with `run_info = {}` (per Python's [`log_pipeline_run_initiated.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/log_pipeline_run_initiated.py)).
4. **Do not** broadcast a `RunEvent` for `INITIATED` (decision 13). The phase watch remains at its initial `Pending` value until `STARTED` flips it to `Running`.

After this task, every `pipeline::execute` call produces a four-row audit trail (INITIATED → STARTED → COMPLETED|ERRORED) by default.

## 2. Rationale

Today the executor jumps straight to `Started` ([`pipeline.rs:647`](../../crates/core/src/pipeline.rs#L647)). Python's `log_pipeline_run_initiated.py` writes `INITIATED` from `reset_pipeline_run_status.py` to invalidate a previously-completed run so a re-cognify doesn't get skipped by `check_pipeline_run_qualification`. Without an `INITIATED` row, the qualification check in task 08-08 sees the prior `COMPLETED` and short-circuits.

Decision 1 picked Option A (executor-level): every non-HTTP caller (library `cognify`/`memify`/ingestion via task 08-07) automatically gets the four-state trail without per-caller registration plumbing. The trade-off — Option B's tidier separation between "queued" and "running" — is sacrificed because Rust's executor is the only universal entry point.

## 3. Pre-conditions

- Tasks 08-01, 08-02, 08-03 committed.
- `run_info_for_initiated()` helper available (added in task 08-03 §4.1).
- `PipelineRunInfo.data_ids: Vec<Uuid>` carrier present (task 08-02). The `INITIATED` row's `run_info` is `{}`, so `data_ids` is not consumed for this state — but the field must exist on the struct.

## 4. Step-by-step

### 4.1 Add `on_pipeline_run_initiated` to the trait

Edit [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs) `PipelineWatcher` trait around line 437:

```rust
// Insert before on_pipeline_run_started:

/// Called before any task runs. Persists the initial `INITIATED` row in
/// the Python lifecycle. Default no-op — watchers that don't persist runs
/// can ignore this.
///
/// Does NOT broadcast a `RunEvent` — the in-memory event stream remains
/// four-kinded (`Started`/`Yield`/`Completed`/`Errored`/`AlreadyCompleted`).
/// Subscribers only see the run "exists" once `Started` fires.
async fn on_pipeline_run_initiated(&self, _run: &PipelineRunInfo) {}
```

### 4.2 Emit `INITIATED` from `execute`

Edit [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs) around line 640-670:

```rust
let mut run_info = PipelineRunInfo {
    run_id,
    pipeline_id,
    pipeline_name: pipeline.name.clone().unwrap_or_default(),
    user_id,
    tenant_id,
    dataset_id,
    data_ids: data_ids.clone(), // populated in task 08-02 §4.4
    status: PipelineRunStatus::Initiated, // ← was Started
    started_at: chrono::Utc::now(),
    completed_at: None,
};

// Propagate `run_id` into the pipeline context so tasks can attribute
// payload events via `TaskContext::publish_payload_field`.
let ctx = ctx.with_run_id(run_id);

// Clear the per-run provenance visited-set so each pipeline run is
// isolated. See gap 05-06 §4.6.
if let Some(pctx) = ctx.pipeline_ctx.as_ref() {
    // lock poison is unrecoverable
    pctx.provenance_visited.lock().unwrap().clear();
}

// ── INITIATED ──────────────────────────────────────────────────────────
// Write the audit row BEFORE the task subtoken setup so a malformed
// pipeline still produces an INITIATED record (and the subsequent
// `InvalidConfig` error would trigger an ERRORED row through the normal
// failure path below).
watcher.on_pipeline_run_initiated(&run_info).await;

// ── STARTED ────────────────────────────────────────────────────────────
run_info.status = PipelineRunStatus::Started;
watcher
    .on_pipeline(pipeline_id, PipelineStatus::Started { task_count })
    .await;
watcher.on_pipeline_run_started(&run_info).await;

// ── Analytics: Pipeline Run Started ─────────────────────────────────
emit_pipeline_event(
    "Pipeline Run Started",
    user_id,
    &run_info.pipeline_name,
    tenant_id,
    pipeline.telemetry_settings.as_ref(),
);

// ... rest of execute unchanged ...
```

> **Note:** the `NoTasks` early return at the top of `execute` (line ~620 — search for `if pipeline.tasks.is_empty()`) currently returns before any watcher event fires. After this change, decide whether `INITIATED` should fire even for the `NoTasks` case. The Python equivalent never calls `run_tasks` with zero tasks, so the question is academic; **the chosen behaviour: emit `INITIATED` but skip `STARTED`, then bubble the `NoTasks` error and emit `ERRORED`** so the audit trail reflects the failed configuration. Move the `INITIATED` emission to *after* the `NoTasks` guard — if the pipeline has no tasks, no INITIATED row is written either; this keeps the executor's contract that "no tasks → no run".
>
> Final placement: directly after `let mut run_info = PipelineRunInfo { ... };` and *before* the task subtoken split, but *after* the `if pipeline.tasks.is_empty()` early return at the top of the function.

### 4.3 Implement `on_pipeline_run_initiated` on `ScopedRunWatcher`

Edit [`crates/core/src/pipeline_run_registry/scoped_watcher.rs`](../../crates/core/src/pipeline_run_registry/scoped_watcher.rs) inside `impl PipelineWatcher for ScopedRunWatcher` (around line 94):

```rust
async fn on_pipeline_run_initiated(&self, run: &PipelineRunInfo) {
    // run_info = {} per Python's log_pipeline_run_initiated.py
    let db_result = self
        .db
        .log_pipeline_run(
            run.run_id,
            run.pipeline_id,
            &run.pipeline_name,
            run.dataset_id,
            DbStatus::Initiated,
            Some(super::run_info_for_initiated()),
        )
        .await;
    if let Err(e) = db_result {
        tracing::warn!(
            run_id = %self.run_id,
            "ScopedRunWatcher: DB write for Initiated failed (non-fatal): {e}"
        );
    }
    // Decision 13: no RunEvent broadcast for INITIATED. The phase watch
    // stays at its initial `Pending` value (set when the slot was created
    // in DefaultPipelineRunRegistry) until on_pipeline_run_started flips
    // it to Running.
}
```

### 4.4 Remove the pre-emptive `INITIATED` write from the registry, if any

Inspect [`crates/core/src/pipeline_run_registry/default_impl.rs`](../../crates/core/src/pipeline_run_registry/default_impl.rs) `register_inline`/`register_background` to see whether they write any row before spawning the work future. The current impl writes `STARTED` from inside `run_work_inline`. After this task, the executor's `INITIATED` emission supersedes any registry-level pre-write — the registry must NOT write its own `INITIATED` row, or you get two rows per logical state.

If the registry today writes any row outside of `ScopedRunWatcher`, remove that write. The four-state trail must come from the executor → watcher chain only.

### 4.5 Update `core_to_db_status` if necessary

[`scoped_watcher.rs:84-91`](../../crates/core/src/pipeline_run_registry/scoped_watcher.rs#L84-L91) already maps `CoreStatus::Initiated → DbStatus::Initiated`. No change needed.

### 4.6 Build + test

```bash
cargo check --all-targets
cargo test -p cognee-core
```

The existing executor tests will now observe an extra `INITIATED` row. Update test fixtures that count rows:

```bash
rg "pipeline_runs.*count|status.*Initiated" crates/core/tests/ crates/database/tests/ crates/http-server/tests/
```

### 4.7 Update HTTP server snapshot if applicable

The HTTP `GET /api/v1/activity/pipeline-runs` endpoint returns up to 50 rows by `created_at DESC`. A completed run now produces 3 rows (INITIATED → STARTED → COMPLETED) instead of 2. Existing HTTP tests at [`crates/http-server/tests/activity_router.rs`](../../crates/http-server/tests/activity_router.rs) (if present) need their row-count assertions updated.

## 5. Verification

```bash
# 1. Compiles.
cargo check --all-targets

# 2. Executor unit tests pass.
cargo test -p cognee-core --lib

# 3. Registry tests pass (existing tests must continue to see STARTED first).
cargo test -p cognee-core --test pipeline_runs

# 4. HTTP-server activity test — row count assertions updated.
cargo test -p cognee-http-server --test activity_router

# 5. Run a CLI cognify against the test fixture and confirm three rows:
#    INITIATED, STARTED, COMPLETED. (See task 09 for the dedicated test.)
cargo test -p cognee-core --test pipeline_run_lifecycle  # ← lands in task 09

# 6. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs) — `PipelineWatcher::on_pipeline_run_initiated` trait method; `execute` emits `INITIATED` before `STARTED`.
- [`crates/core/src/pipeline_run_registry/scoped_watcher.rs`](../../crates/core/src/pipeline_run_registry/scoped_watcher.rs) — impl `on_pipeline_run_initiated`.
- [`crates/core/src/pipeline_run_registry/default_impl.rs`](../../crates/core/src/pipeline_run_registry/default_impl.rs) — remove any pre-emptive `INITIATED` / `STARTED` row write outside `ScopedRunWatcher`.
- Existing test files in `crates/core/tests/` and `crates/http-server/tests/` — update row-count and status-sequence assertions.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Existing tests assert "exactly 1 row after Started" or "exactly 2 rows after Completed" → break | High — desired. | Update them in this task. Most go from N to N+1. |
| Adding a trait method with a default body is technically backwards-compatible but `RecordingWatcher` mock impls in test files that re-implement everything must add it | Medium | The compiler flags them as unused defaults; tests still compile thanks to the default body. Adopt the new method only where assertions need it. |
| Registry's HTTP path now writes a 3rd row → activity endpoint returns 3 rows per HTTP cognify; existing UI assumes 2 | Low — UI consumers do not pin row counts. | Document in 08-10 closure summary; flag in commit body. |
| `INITIATED` event broadcasts to subscribers and confuses them | Avoided by design — decision 13 disallows it. | Not implemented. |
| The `NoTasks` early return now produces an `INITIATED` row without a corresponding terminal row → orphaned `INITIATED` | Acknowledged. | The chosen placement (§4.2 final note) places the emission *after* the `NoTasks` guard so empty pipelines write zero rows. |
| Cross-SDK test in 08-09 must assert four rows on the full lifecycle, not three | Acknowledged. | Decision recorded in 08-09. |

## 8. Out of scope

- A `pipeline.disable_initiated()` toggle. Decision 1 forces the trail on for every caller.
- Broadcasting an `Initiated` event on `RunEvent`. Decision 13 keeps the in-memory event channel unchanged.
- Backfilling `INITIATED` rows for historical runs. Audit history starts fresh.
- Adding `pipeline.skip_qualification_check()` — that's task 08-08's domain.
