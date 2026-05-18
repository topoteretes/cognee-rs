# LIB-06-06 — Tests + cross-SDK parity + closure summary

**Status**: implemented in commit <LIB-06-06-SHA>
**Owner**: _unassigned_
**Depends on**: LIB-06-01, LIB-06-02, LIB-06-03, LIB-06-04, LIB-06-05 — all preceding sub-tasks committed.
**Blocks**: gap-08 task 07 (`docs/telemetry/08/07-library-pipeline-wiring.md`) unblocks once this task closes LIB-06.

**Parent doc**: [LIB-06 — Executor-Routed Convenience Pipelines](../lib-06-executor-routed-convenience.md)
**Locked decisions consulted**: 3 (provenance equivalence verified), 10 (existing fixtures kept), 15 (cross-SDK harness is the closure gate).

---

## 1. Problem statement

LIB-06 sub-tasks 01-05 land the executor-route refactor and remove the
TODO markers. This task is the final gate:

1. Add new tests covering executor-route invariants (where pre-existing
   tests don't already cover them).
2. Run the full E2E suite + cross-SDK harness one more time (after the
   cleanup pass) to confirm nothing regressed.
3. Update the gap-analysis "Future work" bullet about
   `Pipeline::telemetry_settings` to note LIB-06 closure (this is now
   *also* unblocked — gap-08 task 07 will land the `telemetry_settings`
   wiring as part of `DbPipelineWatcher`).
4. Write the closure summary at the bottom of the parent doc.

## 2. Locked decisions consulted

- **Decision 3** — Provenance equivalence is the gating invariant.
- **Decision 10** — New tests live alongside existing fixtures; do not
  invent a parallel "executor-route-only" test crate.
- **Decision 15** — Cross-SDK harness is the equivalence gate. This
  task runs it one final time end-to-end.

## 3. Pre-conditions

- LIB-06-01 through LIB-06-05 committed.
- `rg "LIB-06 follow-up" crates/` returns zero matches.
- `cargo check --all-targets` clean.
- `scripts/check_all.sh` clean (run from the previous sub-task's
  sub-agent C).

## 4. Step-by-step

### 4.1 Identify coverage gaps

Most invariants are already covered:

- **Executor-route correctness**: covered by the existing cognify /
  memify / ingestion integration tests once sub-tasks 01-04 updated
  them.
- **Provenance equivalence**: covered by
  `bash scripts/run_tests_with_openai.sh test_fact_extraction` +
  `e2e-cross-sdk/test_cognify_structural.py` (run as Decision 3 gate
  inside sub-tasks 03-04).
- **Cross-SDK structural similarity**: covered by `e2e-cross-sdk`
  (Decision 15).

Sub-agent A audits which invariants from the parent doc's "Locked
decisions" table are not yet pinned by a test:

- Decision 5 (`extract_dlt_fk_edges` runs after `execute()` returns).
  Add a unit test in `crates/cognify/tests/dlt_teardown.rs` that asserts
  the call order via a `MockGraphDB` recording call sequence.
- Decision 7 (`AddParams` injected via task closure). Add a test in
  `crates/ingestion/tests/add_params_injection.rs` that runs
  `AddPipeline::add_with_params` with non-default `node_set` and
  `importance_weight` and asserts the persisted `Data` carries both.
- Decision 8 (memify takes a placeholder input — or, if §4.3 design
  refinement was accepted in LIB-06-02, takes `Vec<Triplet>` directly).
  Cover by the existing memify integration test.
- Decision 11 (`NoopWatcher` only — no `pipeline_runs` rows from CLI).
  Add a CLI E2E test in `crates/cli/tests/cli_e2e/` that runs `cognify`
  end-to-end and asserts the `pipeline_runs` table is empty (after
  LIB-06; gap-08 task 07 will flip this assertion later).

### 4.2 Run the full smoke suite

```bash
# Format / clippy / per-crate check.
scripts/check_all.sh

# Workspace tests in debug.
cargo test --workspace

# Full LLM-using cognify suite.
bash scripts/run_tests_with_openai.sh

# Cross-SDK parity harness.
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit && cd -
```

If any step regresses vs the post-LIB-06-05 baseline, this task does
NOT close. Sub-agent C investigates; the fix lands here (or, if it
genuinely belongs in 01-04, escalates to the user for an amend-via-new-
commit decision).

### 4.3 Update gap-analysis

Edit [`docs/telemetry/gap-analysis.md`](../gap-analysis.md):

- The "Future work / out of scope" bullet at line 350 reads:

  > **Wire `Pipeline::telemetry_settings` from production SDK paths.**
  > Gap 03-04 added the `Pipeline.telemetry_settings` carrier and
  > emits `Pipeline Run Started/Completed/Errored` from
  > `cognee_core::pipeline::execute()`, but cognify, memify, and
  > ingestion currently bypass `execute()` so the events never fire
  > for those paths. The wiring belongs in the library-side
  > `pipeline_runs` work in
  > [08-pipeline-run-status.md → §D](08-pipeline-run-status.md#d-library-side-wiring-write-pipeline_runs-rows-from-cli-runs);
  > listed here only for cross-reference.

  Replace with:

  > **Wire `Pipeline::telemetry_settings` from production SDK paths.**
  > LIB-06 (`docs/telemetry/lib-06-executor-routed-convenience.md`)
  > closed on `<SHA>` and routed `cognify`, `memify`, `AddPipeline::add`
  > through `cognee_core::pipeline::execute()`. The
  > `Pipeline.telemetry_settings` carrier now fires for library paths
  > as part of the `Pipeline Run *` emission inside `execute()`. The
  > companion `DbPipelineWatcher` wiring is gap-08 task 07.

  Fill `<SHA>` with the LIB-06-06 closure commit.

- Add a new "Completed work" entry:

  > - ✅ **Route convenience pipelines through the executor (LIB-06).**
  >   `cognify::cognify`, `cognify::memify::memify`, and
  >   `ingestion::AddPipeline::add` now call
  >   `cognee_core::pipeline::execute` instead of running tasks inline.
  >   Unblocks `PipelineWatcher` lifecycle events for library callers,
  >   prerequisite for gap-08 task 07 (`pipeline_runs` audit trail) and
  >   the LIB-06 payload-event mechanism. →
  >   [lib-06-executor-routed-convenience.md](lib-06-executor-routed-convenience.md)
  >   (complete — see the
  >   [closure summary](lib-06-executor-routed-convenience.md#closure-summary)).

### 4.4 Write the closure summary

Append to the bottom of
[`docs/telemetry/lib-06-executor-routed-convenience.md`](../lib-06-executor-routed-convenience.md):

```markdown
---

## Closure summary

LIB-06 closed on <DATE> with the following commits, in landing order:

| # | Task | Commit | Subject |
|---|---|---|---|
| 1 | LIB-06-01 | `<SHA>` | Route `AddPipeline::add` through `pipeline::execute` |
| 2 | LIB-06-02 | `<SHA>` | Route `memify::memify` through `pipeline::execute` |
| 3 | LIB-06-03 | `<SHA>` | Route `cognify::cognify` standard branch through `pipeline::execute` |
| 4 | LIB-06-04 | `<SHA>` | Route cognify temporal branch through `pipeline::execute` |
| 5 | LIB-06-05 | `<SHA>` | Remove `TODO(LIB-06 follow-up)` markers |
| 6 | LIB-06-06 | `<SHA>` | Tests + cross-SDK parity + closure summary |

### Verification

- `scripts/check_all.sh` clean on the final commit.
- `bash scripts/run_tests_with_openai.sh` passes the full cognify suite
  (fact extraction, temporal, summarisation).
- `cd e2e-cross-sdk && docker compose up --build` passes
  `test_cognify_structural.py` within the existing 50% / 0.3-Jaccard
  tolerances.
- `rg "LIB-06 follow-up" crates/` returns zero matches.

### Known follow-ups

- **Gap-08 task 07** (`docs/telemetry/08/07-library-pipeline-wiring.md`)
  is now unblocked: convenience functions accept an
  `Arc<dyn PipelineRunRepository>` parameter and construct a
  `DbPipelineWatcher` that produces the four-state `pipeline_runs`
  trail. LIB-06 deliberately stops at `NoopWatcher` per
  [Decision 11](#design-decisions-locked).
- **LIB-06 payload-event mechanism**
  ([`docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md`](../http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md))
  is now also unblocked: tasks running inside `execute()` can call
  `TaskContext::publish_payload_field` and the watcher will pick it
  up. Production wiring of a real payload-emitting task is a
  follow-up.
- **`extract_dlt_fk_edges` as a typed task.** Decision 5 kept it as
  post-pipeline teardown for LIB-06. Converting it to a typed task
  (so its execution participates in the watcher's task-level events
  and any future failure shows up as `pipeline_runs.status = ERRORED`)
  is a follow-up.
- **`data_id_fn` for cognify and memify.** Decision 4 left them as
  `None` for now; gap-08 task 07 surfaces `data_ids` via a new
  `PipelineContext::data_ids` field (or equivalent).
```

Fill `<DATE>` and the six `<SHA>` placeholders with the actual commit
SHAs.

### 4.5 Re-flip the action-items table

In the parent doc's "Action items" table, flip all six rows from `⬜`
to `✅ <SHA>`. Sub-agent E for task 06 does this; sub-agents A-D for
06 do not touch the table.

## 5. Files modified

- [`docs/telemetry/lib-06-executor-routed-convenience.md`](../lib-06-executor-routed-convenience.md)
  — closure summary + action-items table flips.
- [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) — update
  future-work bullet + add completed-work entry.
- Possibly new test files:
  - `crates/cognify/tests/dlt_teardown.rs`
  - `crates/ingestion/tests/add_params_injection.rs`
  - `crates/cli/tests/cli_e2e/cognify_no_pipeline_runs.rs` (or
    integrate into existing `cli_e2e`).

## 6. Verification

```bash
# 1. Tree clean except for the new test files + doc updates.
git status

# 2. Tests pass.
cargo test --workspace

# 3. Full LLM suite.
bash scripts/run_tests_with_openai.sh

# 4. Cross-SDK harness.
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit && cd -

# 5. Full check.
scripts/check_all.sh

# 6. No TODOs remaining.
rg "LIB-06 follow-up" crates/

# 7. Doc references are intact.
rg "lib-06-executor-routed-convenience\|lib-06/" docs/
```

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Cross-SDK harness flakes on the final run | Medium | Re-run once. If two consecutive runs fail with the same metric below threshold, the regression is real; investigate. |
| New tests pin behaviour gap-08 task 07 will need to change (e.g. "no `pipeline_runs` rows from CLI") | Medium — desired temporary state | Comment in the test explaining that gap-08 task 07 flips the assertion. Reference the gap-08 sub-doc. |
| Closure summary SHA placeholders are not filled in before commit | High if E is sloppy | Sub-agent E reviews the diff before staging. |
| `gap-analysis.md` line numbers shift between LIB-06's landing and now | Low | Sub-agent A confirms the "Future work" bullet still lives at the same location; if not, search by content. |
| The LIB-06 payload-event sidecar doc isn't updated to acknowledge closure | Low | LIB-06-05 handled the sidecar; LIB-06-06 confirms the closure note is in place. |

## 8. Out of scope

- Implementing gap-08 task 07. That's the next gap.
- Implementing the LIB-06 payload-event consumer tasks. Separate gap.
- Removing `Pipeline::with_telemetry_settings` carrier — it's now wired
  for real, not "carrier-only", and stays.
- Renaming the LIB-06 doc directory or moving anything out of
  `docs/telemetry/`.
