# Telemetry Gap 08 — Pipeline Run Status Implementation Runbook

## Purpose

This document is a self-contained **prompt** for executing the ten
gap-08 implementation tasks defined in [`docs/telemetry/08/`](.)
sequentially with a fixed five-sub-agent workflow per task. Pasting
this document (or pointing a fresh Claude Code session at it) drives
the gap to completion without burning the main session's context
window on per-task investigation, code reading, or test output.

It mirrors the gap-07 runbook
([`docs/telemetry/07/00-implementation-runbook.md`](../07/00-implementation-runbook.md))
verbatim in shape — only the source-of-truth references and the
per-task decisions change.

## How to use

Open a fresh Claude Code session in `/home/dmytro/dev/cognee/cognee-rust`
and ask: *"Follow `docs/telemetry/08/00-implementation-runbook.md`."*
The orchestrator instructions begin at [Orchestrator
prompt](#orchestrator-prompt).

---

## Source-of-truth documents

The orchestrator MUST treat these as the binding contract:

- **Design decisions (locked)**:
  [`docs/telemetry/08-pipeline-run-status.md`](../08-pipeline-run-status.md)
  → "Design decisions (locked)" table. Thirteen numbered decisions
  pre-approved by the project owner on 2026-05-12. **Do not
  re-litigate them.** The most load-bearing entries:
  - **Decision 1** — INITIATED is emitted by `pipeline::execute`
    (executor-level, Option A); the watcher persists the row.
  - **Decision 2** — Library pipelines always run through a
    repository; `NoopPipelineRunRepository` is the default for
    embedded uses, CLI always wires the real SeaORM repo.
  - **Decision 3** — `check_pipeline_run_qualification` ships in this
    gap (cognify + memify).
  - **Decision 4** — `dataset_id` becomes nullable; FK dropped.
  - **Decision 5** — `run_info` JSON shape is byte-identical to
    Python; `data_info` helper lives in
    `crates/core/src/pipeline_run_registry/data_info.rs`.
  - **Decision 6** — `run_info["data"]` items are JSON strings
    (`Value::String(id.to_string())`).
  - **Decision 7** — `PipelineRunRepository` gains three reader
    helpers.
  - **Decision 8** — Cross-SDK parity test runs under the existing
    `e2e-cross-sdk/` harness.
  - **Decision 9** — The Rust-only `pipeline_run_payload_fields`
    sidecar stays Rust-only.
  - **Decision 10** — All `repo.log_pipeline_run(...)` calls are
    synchronous (awaited).
  - **Decision 11** — `SeaOrmPipelineRunRepository` is the single
    point of truth; library pipelines use `DbPipelineWatcher`.
  - **Decision 12** — `pipeline_run_id` reuse semantics preserved.
  - **Decision 13** — No new `RunEventKind` variant for INITIATED.

- **Per-task implementation plans**:
  [`01-dataset-id-nullable-migration.md`](01-dataset-id-nullable-migration.md),
  [`02-data-info-helper.md`](02-data-info-helper.md),
  [`03-run-info-shape-alignment.md`](03-run-info-shape-alignment.md),
  [`04-initiated-from-executor.md`](04-initiated-from-executor.md),
  [`05-reset-helpers.md`](05-reset-helpers.md),
  [`06-reader-helpers.md`](06-reader-helpers.md),
  [`07-library-pipeline-wiring.md`](07-library-pipeline-wiring.md),
  [`08-check-qualification.md`](08-check-qualification.md),
  [`09-tests.md`](09-tests.md),
  [`10-docs-and-ci.md`](10-docs-and-ci.md).

- **Gap parent**:
  [`docs/telemetry/08-pipeline-run-status.md`](../08-pipeline-run-status.md)
  → "Action items" + "Design decisions (locked)" tables.

- **Root gap analysis**:
  [`docs/telemetry/gap-analysis.md`](../gap-analysis.md).

- **Prior art**:
  - [`docs/telemetry/07-bindings-auto-init.md`](../07-bindings-auto-init.md)
    + [`docs/telemetry/07/`](../07/) — closed gap; this runbook's
    five-sub-agent shape and locked-decisions discipline are lifted
    verbatim from there.
  - [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
    + [`docs/telemetry/05/`](../05/) — closed gap; demonstrates the
    "schema + entity + domain + every consumer" cascade that task 08-01
    follows for the `dataset_id` nullability change.
  - The existing Python references the Rust port must remain compatible
    with:
    [`/tmp/cognee-python/cognee/modules/pipelines/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/pipelines)
    (clone via `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`).

## Project conventions to honour

From [`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) and
[`/home/dmytro/.claude/CLAUDE.md`](file:///home/dmytro/.claude/CLAUDE.md):

- Compilation check: `cargo check --all-targets`.
- Tests: debug mode (no `--release`).
- Full check before committing: `scripts/check_all.sh`.
- No `unwrap()` in non-test code. Use `expect("reason")` or `?`.
  Lock-poison `unwrap` on `Mutex`/`RwLock` is acceptable with a
  `// lock poison is unrecoverable` comment.
- Commit message convention: `<scope>: <subject>` — for this gap,
  scope is `telemetry/pipeline-runs` (e.g.
  `telemetry/pipeline-runs-08-02: add data_info helper to pipeline_run_registry`).
  Always include the
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
  trailer via heredoc.
- Never `git push`, never amend, never use `--no-verify`.
- Ask the user before any destructive or shared-state action.
- Env-mutating tests must be marked `#[serial_test::serial]` (or
  equivalent). Tests that touch the SeaORM in-memory pool concurrently
  must scope their own DB instance.

---

## Orchestrator prompt

> You are the orchestrator for telemetry gap 08 (pipeline run status
> persistence parity with Python). Your job is to drive the ten tasks
> `08-01` … `08-10` through to a clean, committed, documented state,
> **one at a time, in order, with no parallelism between tasks**.
>
> ### Top-level loop
>
> For each task `N` from 01 to 10, in strict numeric order:
>
> 1. Run **Sub-agent A — Task review** for task `N`.
> 2. If A returns `STATUS: needs-decision`, escalate the question to
>    the user and **wait** for an answer. After the user responds,
>    re-run A so it records the decision into the sub-doc, then
>    continue.
> 3. If A returns `STATUS: needs-update`, the sub-doc has been edited
>    in place. Re-run A once to confirm `STATUS: ready`. If a second
>    run still returns `needs-update`, escalate to the user — there is
>    a loop or A is wrong about itself.
> 4. Run **Sub-agent B — Implementor** for task `N`.
> 5. If B returns `STATUS: failed`, escalate to the user with the
>    failure summary; **do not** attempt a second implementation
>    automatically (avoid runaway).
> 6. Run **Sub-agent C — Change reviewer** for task `N`.
> 7. If C returns `STATUS: failed`, escalate; if `STATUS: fixed`, C has
>    already amended the working tree — proceed.
> 8. Run **Sub-agent D — Committer** for task `N`.
> 9. Run **Sub-agent E — Document updater** for task `N`.
> 10. Print a short orchestrator-level summary to the user (one line:
>     `task NN — committed <sha> — docs updated`) and proceed to
>     `N+1`.
>
> Only ONE sub-agent runs at a time. Never launch sub-agents for
> different tasks in parallel. Within a task, A must finish before B
> starts, etc.
>
> ### Hard rules
>
> - **Never modify the locked design decisions** in
>   `docs/telemetry/08-pipeline-run-status.md` "Design decisions
>   (locked)" without explicit user approval. Sub-agent A may surface
>   that a decision needs revisiting; if so, escalate.
> - **Never `git push`, never amend a commit, never `--no-verify`.**
> - **Never `git reset --hard`, `git clean -f`, `git checkout --` to
>   "fix" a problem.** If sub-agent C cannot reconcile the diff with
>   the task plan, escalate.
> - **Stop the loop on the first unrecoverable error.** Tasks build on
>   each other (01 → 02 → 03 → 04 → 05/06/07 → 08 → 09 → 10);
>   pushing through a broken task corrupts the chain.
> - **Run `scripts/check_all.sh` exactly once per task** (inside
>   sub-agent C). Don't re-run from the orchestrator.
> - **Never make outbound HTTP requests in tests.** Mock or skip when
>   the network is required. The cross-SDK test (task 09) runs against
>   the Dockerised harness only.
> - **Confirm before any action with shared-state effects** beyond the
>   working tree.
> - **Env-mutating tests must be serialised.** Use
>   `#[serial_test::serial]` for any Rust test that touches the global
>   tokio runtime, env vars, or shared DB pool.
>
> ### Scope guard
>
> Gap 08 covers pipeline run status persistence parity only. The
> orchestrator must **resist scope creep**:
>
> - **Do not add new `RunEventKind` variants.** Decision 13 keeps the
>   in-memory event channel unchanged. The `AlreadyCompleted` variant
>   that already exists in `crates/core/src/pipeline_run_registry/types.rs`
>   is reused by task 08-08; no new variant is needed.
> - **Do not refactor the `pipeline_run_payload_fields` sidecar.**
>   Decision 9 keeps it Rust-only. Cross-SDK reads ignore it.
> - **Do not introduce a global pipeline_run_id registry / dedup
>   cache.** Decision 12 reuses ids by derivation, which is
>   intentional. Multiple rows per id is the design.
> - **Do not add `shutdown_*()` companion helpers.** Out of scope.
> - **Do not extend the qualification check to ingestion.** Decision 3
>   limits it to cognify + memify because Python only gates those.
> - **Do not touch Android scripts.** Android consumes the CLI; the
>   CLI changes from task 07 propagate automatically.

## Sub-agent prompt templates

### Sub-agent A — Task review

> You are reviewing
> `docs/telemetry/08/{TASK_FILE}.md` for task `08-{NN}`. Read it
> end-to-end together with the parent
> [`docs/telemetry/08-pipeline-run-status.md`](../08-pipeline-run-status.md)
> and the locked design decisions table inside it.
>
> Your goal:
>
> 1. Confirm the sub-doc's "Pre-conditions" hold against the current
>    working tree (run `git status`, `cargo check --all-targets`).
> 2. Confirm the sub-doc's "Step-by-step" still matches reality. If a
>    referenced file has shifted line numbers since the doc was
>    written, **update the doc in place** and report
>    `STATUS: needs-update`.
> 3. Surface any unresolved decision the doc still defers to a sibling
>    document or to the user. If you find one, report
>    `STATUS: needs-decision` with the question.
> 4. Otherwise report `STATUS: ready`.
>
> Report formats (one short paragraph each):
>
> - `STATUS: ready` — proceed to implementor.
> - `STATUS: needs-update` — the doc has been edited in place; rerun me.
> - `STATUS: needs-decision` — quote the open question, the relevant
>   sub-doc section, and what the locked decisions table currently
>   says (or fails to say).

### Sub-agent B — Implementor

> Implement task `08-{NN}` by following
> `docs/telemetry/08/{TASK_FILE}.md` "Step-by-step" exactly. Constraints:
>
> - Honour the locked decisions table in
>   [`docs/telemetry/08-pipeline-run-status.md`](../08-pipeline-run-status.md).
> - Honour project conventions (no `unwrap()` outside tests; tests run
>   in debug; lock-poison `unwrap` on `Mutex`/`RwLock` is acceptable
>   with a `// lock poison is unrecoverable` comment).
> - Reuse the existing types in `cognee-database` (`PipelineRun`,
>   `PipelineRunStatus`, `PipelineRunRepository`,
>   `SeaOrmPipelineRunRepository`) and `cognee-core`
>   (`PipelineWatcher`, `PipelineRunInfo`, `RunSpec`) rather than
>   defining new parallel types.
> - Do not modify files outside the "Files modified" list in the
>   sub-doc, except trivial fixups required to compile.
> - Do not commit. Sub-agent D commits.
>
> When done, report `STATUS: ok` with a list of files modified and a
> one-paragraph diff summary. If you hit an unrecoverable issue,
> report `STATUS: failed` with the diagnostic.

### Sub-agent C — Change reviewer

> Review the working-tree diff for task `08-{NN}` against
> `docs/telemetry/08/{TASK_FILE}.md`. Run, in this order:
>
> 1. `cargo fmt --check`
> 2. `cargo check --all-targets`
> 3. `cargo check -p cognee-lib --no-default-features` (sanity — the
>    schema/repo additions must compile feature-flag-clean).
> 4. `cargo clippy --all-targets -- -D warnings`
> 5. `scripts/check_all.sh`
> 6. The verification commands listed in the sub-doc itself
>    ("Verification" section).
>
> If any step fails, fix the underlying issue (do **not** silence
> warnings, do **not** disable lints) and report `STATUS: fixed` with
> the fix description. If the failure is unrelated to the task,
> report `STATUS: failed` and escalate.
>
> If everything passes, report `STATUS: ok`.

### Sub-agent D — Committer

> Stage and commit the working-tree changes for task `08-{NN}`.
>
> 1. `git status` to confirm only intended files changed.
> 2. `git add <files>` (explicit list — never `-A`).
> 3. `git commit -m "$(cat <<'EOF'\ntelemetry/pipeline-runs-08-{NN}: <subject from sub-doc>\n\n<one-paragraph body>\n\nCo-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>\nEOF\n)"`.
> 4. `git status` to confirm clean tree.
> 5. Print the new SHA.
>
> Never amend, never push, never use `--no-verify`. If the pre-commit
> hook fails, fix the underlying issue, re-stage, create a NEW commit.

### Sub-agent E — Document updater

> Update `docs/telemetry/08-pipeline-run-status.md` "Action
> items" table:
>
> 1. Flip the `Status` cell for row `{NN}` from `⬜` to `✅ <SHA>`.
> 2. If the implementation diverged from the original sub-doc plan
>    in any user-visible way, append a one-line note to the sub-doc
>    "Status" header (e.g.
>    `**Status**: implemented in commit <SHA> (note: also touched
>    crates/http-server/src/routers/datasets.rs — see commit body)`).
> 3. If this is task 10, also write the "Closure summary" section at
>    the bottom of the parent doc, listing every commit in landing
>    order — same format as the gap-05 / gap-06 / gap-07 closure
>    summaries.
>
> Report `STATUS: ok` with a one-line summary of what changed.

---

## Sequencing notes

- **Task 08-01** (schema migration + nullability) is foundation work
  and unblocks everything else. Every later task assumes
  `dataset_id: Option<Uuid>` already at the domain layer.
- **Task 08-02** (data_info helper + RunSpec carrier) is a pure
  plumbing addition with no behaviour change. Lands isolated so the
  diff is easy to review.
- **Task 08-03** (run_info shape alignment) is the first behavioural
  change — existing `Started`/`Completed`/`Errored` rows start writing
  the Python-shaped JSON. Lands before INITIATED so the existing
  three-state trail is fully Python-faithful before INITIATED is added
  on top.
- **Task 08-04** (INITIATED from executor) adds the fourth state.
  Depends on 03 because the same watcher methods are extended.
- **Task 08-05** (reset helpers) depends on 04 because the reset
  semantics rely on INITIATED actually persisting.
- **Task 08-06** (reader helpers) is independent of 04/05 in code but
  the runbook drives it after 05 so the helpers cover the new INITIATED
  rows from day one.
- **Task 08-07** (library pipeline wiring) depends on 04 because
  library pipelines must produce the four-state trail. Could land
  before 05/06 in principle; runbook keeps numeric order.
- **Task 08-08** (qualification check) depends on 06 (uses
  `get_pipeline_run_by_dataset`) and 07 (library pipelines must
  consult the gate).
- **Task 08-09** (tests) depends on every preceding task — the
  cross-SDK test asserts the full four-state trail.
- **Task 08-10** (docs + CI) lands last.

---

## When the loop ends

After task 10 commits and sub-agent E updates the parent doc, run a
final smoke pass:

```bash
scripts/check_all.sh
cargo test -p cognee-database --test pipeline_run_repository
cargo test -p cognee-core --test pipeline_run_lifecycle
cargo test -p cognee-http-server --test activity_pipeline_runs
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit && cd -
```

If all pass, the gap is closed. Sub-agent E should have written the
"Closure summary" section into
[`docs/telemetry/08-pipeline-run-status.md`](../08-pipeline-run-status.md);
verify the commit list there matches the orchestrator's per-task
commit log.
