# Task 08-10 — Docs and CI for gap 08 closure

**Status**: not yet implemented (⬜)
**Owner**: _unassigned_
**Depends on**: tasks 08-01 through 08-09.
**Blocks**: —

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: all (this task documents the surface the previous tasks shipped and writes the closure summary).

---

## 1. Goal

Close gap 08 by:

1. Flipping [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) §3 status from "Partial" / "Verify" to "✅ Implemented in gap 08".
2. Adding a "Pipeline Run Lifecycle" section to [`docs/http-server/pipelines.md`](../../http-server/pipelines.md) (if it exists) or a new top-level doc otherwise, documenting the four-state DB trail, `run_info` shapes, and the qualification gate.
3. Documenting the new public helpers (`reset_pipeline_run_status`, `reset_dataset_pipeline_run_status`, the reader trio) in a short `docs/lib/pipeline-runs.md`.
4. Writing the "Closure summary" section at the bottom of [`docs/telemetry/08-pipeline-run-status.md`](../08-pipeline-run-status.md).
5. Confirming no `.github/workflows/ci.yml` change is needed — the new tests land under existing job patterns.

## 2. Rationale

- Gap-analysis update keeps the top-level status table accurate so future contributors don't re-litigate the gap.
- HTTP-server / lib docs give consumers a single place to find the lifecycle / `run_info` / qualification semantics without spelunking the source.
- Closure summary preserves the per-commit audit trail (gap-06 / gap-07 convention).

## 3. Pre-conditions

- All preceding tasks committed.
- The single CI workflow lives at [`.github/workflows/ci.yml`](../../../.github/workflows/ci.yml) (or equivalent path — confirm) and already runs `lib-tests.yml` against the workspace. Gap-08 tests under `crates/*/tests/` are picked up by `cargo test --workspace`.
- The `e2e-cross-sdk` Docker harness already runs on push to main; the new `test_pipeline_runs_parity.py` is collected automatically per the harness's `conftest.py`.

## 4. Step-by-step

### 4.1 Update `docs/telemetry/gap-analysis.md` §3

Locate §3 (Pipeline Run Status Persistence) around line 46-55. Replace the body with:

```markdown
## 3. Pipeline Run Status Persistence

✅ **Implemented in [gap 08](08-pipeline-run-status.md).** Rust now writes
the full four-state Python lifecycle (`INITIATED → STARTED → COMPLETED |
ERRORED`) for every cognify/memify/ingestion run, regardless of whether
the run originated from the HTTP server or the CLI. `dataset_id` matches
Python's nullable + FK-less schema; `run_info` JSON is byte-identical to
Python's shape (`{"data": [...]}` on STARTED/COMPLETED, `{"data": [...],
"error": "..."}` on ERRORED, `{}` on INITIATED). The Rust API exposes
`reset_pipeline_run_status` and `reset_dataset_pipeline_run_status`
helpers under `cognee_lib::api::pipeline_runs`, plus reader helpers
(`get_pipeline_run`, `get_pipeline_run_by_dataset`,
`get_pipeline_runs_by_dataset`) on `PipelineRunRepository`. The cognify
and memify entry points consult `check_pipeline_run_qualification` to
short-circuit already-completed datasets and reject already-running
ones. Cross-SDK parity is enforced by
[`e2e-cross-sdk/harness/test_pipeline_runs_parity.py`](../../e2e-cross-sdk/harness/test_pipeline_runs_parity.py).
```

### 4.2 Add `docs/lib/pipeline-runs.md`

New file documenting the public API surface for library consumers:

````markdown
# Pipeline Run Status — Library API

Cognee's Rust port persists every pipeline execution (cognify, memify,
ingestion) as a row in the `pipeline_runs` SQLite/Postgres table, matching
the Python SDK's four-state lifecycle:

```
INITIATED → STARTED → (COMPLETED | ERRORED)
```

Each state transition writes a **new row** (append-only / audit-trail).
`pipeline_run_id` is reused across re-runs of the same `(pipeline, dataset)`
pair — the latest row by `created_at` defines the current state.

## `run_info` JSON shape

| State | Body |
|-------|------|
| INITIATED | `{}` |
| STARTED | `{"data": ["<uuid>", "<uuid>", ...]}` or `{"data": "None"}` |
| COMPLETED | same as STARTED |
| ERRORED | `{"data": [...], "error": "<message>"}` |

The `"data"` value is a JSON array of stringified `Data.id`s, or the
literal string `"None"` when the run has no `Data` input. Matches
Python's `data_info` exactly.

## Where rows are written

| Caller | Repository | Watcher |
|--------|------------|---------|
| HTTP-server (`POST /datasets/{id}/cognify` etc.) | `SeaOrmPipelineRunRepository` via the registry | `ScopedRunWatcher` (broadcasts `RunEvent`s for HTTP subscribers) |
| CLI (`cognee-cli cognify ...`) | `SeaOrmPipelineRunRepository` constructed from the CLI's `DatabaseConnection` | `DbPipelineWatcher` (DB-only, no event channel) |
| Embedded library users | Caller-provided `Arc<dyn PipelineRunRepository>`; default `NoopPipelineRunRepository` writes nothing | `DbPipelineWatcher` wrapping the user-supplied repo |

## Public helpers

```rust
use cognee_lib::api::pipeline_runs::{
    reset_pipeline_run_status,
    reset_dataset_pipeline_run_status,
};

// Invalidate a single (user, dataset, pipeline_name) so a future
// cognify/memify call is not short-circuited.
reset_pipeline_run_status(repo, user_id, dataset_id, "cognify_pipeline").await?;

// Invalidate every pipeline_name with a row for the dataset.
reset_dataset_pipeline_run_status(repo, user_id, dataset_id).await?;
```

Reader helpers on `PipelineRunRepository`:

```rust
let latest: Option<PipelineRun> = repo.get_pipeline_run(pipeline_run_id).await?;
let latest_for_pipeline: Option<PipelineRun> = repo
    .get_pipeline_run_by_dataset(dataset_id, "cognify_pipeline")
    .await?;
let one_per_pipeline: Vec<PipelineRun> = repo
    .get_pipeline_runs_by_dataset(dataset_id)
    .await?;
```

## Qualification gate

`cognify(...)` and `memify(...)` consult `check_pipeline_run_qualification`
internally before running tasks:

| Latest status | Behaviour |
|---------------|-----------|
| None / INITIATED / ERRORED | Proceed normally |
| STARTED | Reject with `CognifyError::PipelineAlreadyRunning` / `MemifyError::PipelineAlreadyRunning` |
| COMPLETED | Short-circuit — return `CognifyResult { already_completed: true, .. }` without re-running |

To force a re-cognify, call `reset_pipeline_run_status` first.

## Cross-SDK compatibility

Rust and Python share the same `pipeline_runs` table when both SDKs point
at the same SQLite file. Schema and `run_info` JSON are byte-identical
after gap 08. See [`e2e-cross-sdk/harness/test_pipeline_runs_parity.py`](../../e2e-cross-sdk/harness/test_pipeline_runs_parity.py).

The Rust-only `pipeline_run_payload_fields` sidecar (LIB-06) is not
projected back into Python — it's an enrichment layer the Python SDK
does not see.
````

### 4.3 Update `docs/http-server/pipelines.md` (if present)

Search:

```bash
test -f /home/dmytro/dev/cognee/cognee-rust/docs/http-server/pipelines.md && echo found
```

If found, insert a "Lifecycle" section describing the four-state trail and how `GET /api/v1/activity/pipeline-runs` exposes it. Reference the new `docs/lib/pipeline-runs.md` for the library-side semantics.

If absent, skip this step (the library doc covers both surfaces).

### 4.4 Closure summary

After the orchestrator commits all ten tasks, append to [`docs/telemetry/08-pipeline-run-status.md`](../08-pipeline-run-status.md):

```markdown
---

## Closure summary

Gap 08 closed in N commits. The table below lists every commit in
landing order — each sub-task lands as a pair (implementation
commit + sub-doc status flip), following the gap-06 / gap-07
convention.

| # | Commit | Subject | Task |
|---|---|---|---|
| 08-00 | `<sha>` | telemetry/pipeline-runs-08-00: lock gap-08 decisions and scope action items | 08-00 |
| 08-01 | `<sha>` | telemetry/pipeline-runs-08-01: make pipeline_runs.dataset_id nullable | 08-01 |
| 08-01 | `<sha>` | telemetry/pipeline-runs-08-01: mark action item 01 complete | 08-01 |
| ... | ... | ... | ... |

### What the gap delivered

- `pipeline_runs.dataset_id` nullable; FK to `datasets(id)` dropped.
  SQLite migration rebuilds the table; Postgres uses `ALTER TABLE`.
  Removes the silent-drop branch in `SeaOrmPipelineRunRepository`.
- `data_info` helper in `cognee_core::pipeline_run_registry` plus
  `run_info_for_initiated` / `run_info_for_running` /
  `run_info_for_errored` builders matching Python byte-for-byte.
- `RunSpec.data_ids` and `PipelineRunInfo.data_ids` carriers
  populated by the executor from `pipeline.data_id_fn`.
- `INITIATED` row emitted by `cognee_core::pipeline::execute` before
  the first task runs (Option A, decision 1).
- `ScopedRunWatcher` and the new `DbPipelineWatcher` (library-side)
  write Python-shaped `run_info` JSON.
- `reset_pipeline_run_status` + `reset_dataset_pipeline_run_status`
  helpers under `cognee_lib::api::pipeline_runs`. Plumbed into the
  prune / dataset-reset CLI subcommands.
- Reader helpers (`get_pipeline_run`, `get_pipeline_run_by_dataset`,
  `get_pipeline_runs_by_dataset`) on `PipelineRunRepository`.
- Library pipelines (cognify, memify, ingestion) accept
  `Arc<dyn PipelineRunRepository>`; CLI passes the real SeaORM repo,
  embedded users default to `NoopPipelineRunRepository`.
- `check_pipeline_run_qualification` short-circuits already-completed
  datasets and rejects already-running ones (cognify + memify).
- Cross-SDK parity test under
  `e2e-cross-sdk/harness/test_pipeline_runs_parity.py`.
- New `cognee-cli internal pipeline-runs list` and
  `cognee-cli internal pipeline-run-id` debug subcommands feeding
  the cross-SDK test.

### Known follow-ups

- **Telemetry tie-in (carryover from gap 03-04).** Now that library
  pipelines run through `pipeline::execute`, the
  `Pipeline::with_telemetry_settings(...)` carrier is populated and
  `Pipeline Run Started/Completed/Errored` analytics events fire for
  cognify/memify/ingestion. Verify the per-call `Settings::telemetry_snapshot()`
  is threaded through every CLI subcommand. See
  [03-pipeline-task-api-events.md → Known follow-ups](../03-pipeline-task-api-events.md#known-follow-ups).
- **Prune ↔ pipeline_runs interaction.** After dropping the FK
  (08-01), `pipeline_runs` rows for a deleted dataset survive
  the delete. Task 08-05 writes a fresh `INITIATED` row before
  cascade, but the prior `COMPLETED` row also remains. If the
  growing audit trail becomes a problem, add a
  `delete_runs_for_dataset` repo method in a follow-up.
- **`check_pipeline_run_qualification` for ingestion.** Decision 3
  excluded ingestion because Python doesn't gate it. If the Rust
  port wants stricter dedup guarantees, gate ingestion in a
  follow-up.
- **Surfacing `CognifyResult.already_completed` through bindings.**
  PyO3 / Neon currently serialise `CognifyResult`; the new
  `already_completed: bool` field appears in payloads with
  `#[serde(default)]` so old binding clients ignore it. If a binding
  consumer wants to act on the short-circuit, add an explicit return
  type variant.
- **Postgres FK name introspection.** The migration's `DO` block
  looks up the FK name dynamically. If the constraint is ever
  manually renamed, the lookup still works; if multiple FKs exist on
  `pipeline_runs → datasets`, only the first is dropped. Re-check
  if other migrations add cross-table FKs to `pipeline_runs`.
```

(The exact SHAs and the `N` count are filled in by sub-agent E when the loop completes.)

### 4.5 No `.github/workflows/ci.yml` edit expected

The existing `lib-tests.yml` runs `cargo test --workspace` and the `cross-sdk` workflow (if separate) runs the Docker harness. Both pick up the new test files automatically. Verify by reading [`.github/workflows/`](../../../.github/workflows/) directory listing — if the workspace includes a manifest that disables cross-SDK by default, escalate.

## 5. Verification

`scripts/check_all.sh` is the canonical local gate:

```bash
scripts/check_all.sh
```

There is no markdownlint step in the project's check suite; doc edits are verified by review only.

End-to-end gap smoke pass (mirrors the runbook's "When the loop ends" section):

```bash
cargo test -p cognee-database --test pipeline_run_repository
cargo test -p cognee-core --test pipeline_run_lifecycle
cargo test -p cognee-http-server --test activity_pipeline_runs
cargo test -p cognee-cli --test cli_pipeline_runs
cargo test -p cognee-cognify --test cognify_qualification
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit && cd -
```

## 6. Files modified

- [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) — §3 status flip.
- [`docs/telemetry/08-pipeline-run-status.md`](../08-pipeline-run-status.md) — "Closure summary" section.
- [`docs/lib/pipeline-runs.md`](../../lib/pipeline-runs.md) — **NEW**.
- [`docs/http-server/pipelines.md`](../../http-server/pipelines.md) — "Lifecycle" section (if the file exists).

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| CI runtime grows due to cross-SDK Docker test | Medium — already pays Docker build cost on PRs. | The new test file adds ~5s to the harness; well below the noise floor. |
| Doc paths drift if `docs/http-server/pipelines.md` is later renamed | Low | Use relative paths only; rely on grep-able file names. |
| `docs/lib/pipeline-runs.md` duplicates info from sub-doc `00-implementation-runbook.md` | Acknowledged. | Cross-link rather than copy; runbook focuses on *how to implement*, the lib doc focuses on *how to use*. |
| `gap-analysis.md` §3 line shifts since the doc was written | Medium | Sub-agent A's update step uses `grep` to locate the §3 header rather than line numbers. |
| Closure summary's "N commits" miscount | Low | Sub-agent E counts commits from `git log --oneline --grep="telemetry/pipeline-runs-08-"`. |

## 8. Out of scope

- A user-facing migration guide for embedded library users. The
  `pipeline_run_repo: Arc<dyn PipelineRunRepository>` parameter is
  visible in `cognify(...)`'s signature and the rustdoc is sufficient.
- Updating the Python `cognee` SDK to reciprocally project Rust's
  `pipeline_run_payload_fields` rows into Python's `run_info`.
  Decision 9 explicitly left that as a Rust-only enrichment.
- A `cognee-cli pipeline-runs reset` user-facing subcommand. The
  reset helpers are library-level; if users want CLI reset, add it
  in a follow-up.
- A dashboard / Grafana board for the four-state trail. Not part of
  this gap.
