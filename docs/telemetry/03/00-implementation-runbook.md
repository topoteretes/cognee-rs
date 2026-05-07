# Telemetry Gap 03 — Pipeline / Task / Search Lifecycle Events Implementation Runbook

## Purpose

This document is a self-contained **prompt** for executing the nine
gap-03 implementation tasks defined in [`docs/telemetry/03/`](.)
sequentially with a fixed five-sub-agent workflow per task. Pasting
this document (or pointing a fresh Claude Code session at it) drives
the gap to completion without burning the main session's context
window on per-task investigation, code reading, or test output.

It mirrors the gap-02 runbook
([`docs/telemetry/02/00-implementation-runbook.md`](../02/00-implementation-runbook.md))
verbatim in shape — only the source-of-truth references and the
binding decisions table change.

## How to use

Open a fresh Claude Code session in `/home/dmytro/dev/cognee/cognee-rust`
and ask: *"Follow `docs/telemetry/03/00-implementation-runbook.md`."*
The orchestrator instructions begin at [Orchestrator
prompt](#orchestrator-prompt).

---

## Source-of-truth documents

The orchestrator MUST treat these as the binding contract:

- **Design decisions (locked)**:
  [`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md)
  → "Design decisions (locked)" table. Nine numbered decisions
  pre-approved by the project owner on 2026-05-07. **Do not
  re-litigate them.** The most load-bearing entries:
  - **Decision 1** — `tenant_id` is threaded through `PipelineContext`
    and `PipelineRunInfo`; lifecycle emitters fall back to the
    literal `"Single User Tenant"` when `None`. Backfilling existing
    API events is **out of scope**.
  - **Decision 3** — `cognee.search EXECUTION STARTED` is **implemented**
    (paired with the existing `EXECUTION COMPLETED` from gap 02-07).
  - **Decision 4** — `cognee.api.improve` OTEL span is **bundled** into
    this gap.
  - **Decision 5** — settings snapshot is a **hand-curated allowlist**
    of provider/model fields only. Never serialize the full `Config`.
  - **Decision 6** — `dataset_id` and `pipeline_run_id` are **omitted**
    from analytics payloads (still on OTEL spans).
  - **Decision 7** — task lifecycle events fire **once per task**, not
    per attempt.

- **Per-task implementation plans**:
  [`01-tenant-id-plumbing.md`](01-tenant-id-plumbing.md),
  [`02-task-type-mapping.md`](02-task-type-mapping.md),
  [`03-settings-snapshot.md`](03-settings-snapshot.md),
  [`04-pipeline-lifecycle-events.md`](04-pipeline-lifecycle-events.md),
  [`05-task-lifecycle-events.md`](05-task-lifecycle-events.md),
  [`06-search-execution-events.md`](06-search-execution-events.md),
  [`07-improve-otel-span.md`](07-improve-otel-span.md),
  [`08-tests.md`](08-tests.md),
  [`09-docs-and-ci.md`](09-docs-and-ci.md).

- **Gap parent**:
  [`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md)
  → "Action items" table.

- **Root gap analysis**:
  [`docs/telemetry/gap-analysis.md`](../gap-analysis.md).

- **Prior art**:
  [`docs/telemetry/02-send-telemetry-analytics.md`](../02-send-telemetry-analytics.md)
  + [`docs/telemetry/02/`](../02/) — closed gap. The transport
  (`cognee_telemetry::send_telemetry`) is already wired, default-on for
  `cognee-lib`/`cognee-cli`, default-off for `android-default`. Gap 03
  consumes that transport — do **not** re-design it.

## Project conventions to honour

From [`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) and
[`/home/dmytro/.claude/CLAUDE.md`](file:///home/dmytro/.claude/CLAUDE.md):

- Compilation check: `cargo check --all-targets`.
- Tests: debug mode (no `--release`).
- Full check before committing: `scripts/check_all.sh`.
- No `unwrap()` in non-test code. Use `expect("reason")` or `?`.
- Commit message convention: `<scope>: <subject>` — for this gap, scope
  is `telemetry/events` (e.g.
  `telemetry/events-03-04: emit Pipeline Run Started/Completed/Errored`).
  Always include the
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
  trailer via heredoc.
- Never `git push`, never amend, never use `--no-verify`.
- Ask the user before any destructive or shared-state action.
- Network calls in tests must hit `127.0.0.1` only — the
  `https://test.prometh.ai` proxy is **never** to be exercised from
  CI or unit tests. Use `mockito` (already a workspace dev-dep, per
  gap-02 decision 10).

---

## Orchestrator prompt

> You are the orchestrator for telemetry gap 03 (pipeline / task /
> search lifecycle events). Your job is to drive the nine tasks
> `03-01` … `03-09` through to a clean, committed, documented state,
> **one at a time, in order, with no parallelism between tasks**.
>
> ### Top-level loop
>
> For each task `N` from 01 to 09, in strict numeric order:
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
>   `docs/telemetry/03-pipeline-task-api-events.md` "Design decisions
>   (locked)" without explicit user approval. Sub-agent A may surface
>   that a decision needs revisiting; if so, escalate.
> - **Never `git push`, never amend a commit, never `--no-verify`.**
> - **Never `git reset --hard`, `git clean -f`, `git checkout --` to
>   "fix" a problem.** If sub-agent C cannot reconcile the diff with
>   the task plan, escalate.
> - **Stop the loop on the first unrecoverable error.** Tasks build on
>   each other; pushing through a broken task corrupts the chain.
> - **Run `scripts/check_all.sh` exactly once per task** (inside
>   sub-agent C). Don't re-run from the orchestrator.
> - **Never make outbound HTTP requests to `test.prometh.ai`.** All
>   tests must bind to `127.0.0.1` via `mockito`.
> - **Confirm before any action with shared-state effects** beyond the
>   working tree (network calls beyond `cargo`/test deps, anything
>   touching real proxies, etc.).
>
> ### Scope guard
>
> Gap 03 is narrowly scoped after gap 02 absorbed the SDK API events.
> The orchestrator must **resist scope creep**:
>
> - The `cognee.recall` / `cognee.improve` / `cognee.forget` /
>   `cognee.remember` payloads are already wired by gap 02-07. Do not
>   modify them in this gap (except `cognee.search EXECUTION
>   COMPLETED`, which is back-filled with `tenant_id` by task 03-01,
>   and the `EXECUTION STARTED` paired event added by task 03-06).
> - HTTP-router-level `... API Endpoint Invoked` events stay out of
>   scope (decision 3 in the gap-02 doc).
> - Production SDK paths (cognify, ingestion, memify) currently
>   bypass `cognee_core::execute()` — see source comments at
>   `crates/cognify/src/tasks.rs:1719`. Gap 03 emits events when
>   `execute()` *is* invoked; the larger refactor to route SDK paths
>   through `execute()` is a follow-up gap, not part of 03.

## Sub-agent prompt templates

### Sub-agent A — Task review

> You are reviewing
> `docs/telemetry/03/{TASK_FILE}.md` for task `03-{NN}`. Read it
> end-to-end together with the parent
> [`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md)
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

> Implement task `03-{NN}` by following
> `docs/telemetry/03/{TASK_FILE}.md` "Step-by-step" exactly. Constraints:
>
> - Honour the locked decisions table in
>   [`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md).
> - Honour project conventions (no `unwrap()` outside tests; tests run
>   in debug; `LLM_API_KEY` and identity helpers come from
>   `cognee-telemetry` — do not re-derive them; mockito only for HTTP
>   tests).
> - Reuse the gap-02 transport (`cognee_telemetry::send_telemetry`).
>   Do not add new dependencies on `pbkdf2`, `hmac`, etc. — those
>   live in the telemetry crate already.
> - Do not modify files outside the "Files modified" list in the
>   sub-doc, except trivial fixups required to compile.
> - Do not commit. Sub-agent D commits.
>
> When done, report `STATUS: ok` with a list of files modified and a
> one-paragraph diff summary. If you hit an unrecoverable issue,
> report `STATUS: failed` with the diagnostic.

### Sub-agent C — Change reviewer

> Review the working-tree diff for task `03-{NN}` against
> `docs/telemetry/03/{TASK_FILE}.md`. Run, in this order:
>
> 1. `cargo fmt --check`
> 2. `cargo check --all-targets` (default features — `telemetry` is
>    on for `cognee-lib`/`cognee-cli` per gap-02 decision 1)
> 3. `cargo check --all-targets --no-default-features` (the OFF path
>    must keep building — `cognee-telemetry::send_telemetry` is a
>    noop in this state)
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

> Stage and commit the working-tree changes for task `03-{NN}`.
>
> 1. `git status` to confirm only intended files changed.
> 2. `git add <files>` (explicit list — never `-A`).
> 3. `git commit -m "$(cat <<'EOF'\ntelemetry/events-03-{NN}: <subject from sub-doc>\n\n<one-paragraph body>\n\nCo-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>\nEOF\n)"`.
> 4. `git status` to confirm clean tree.
> 5. Print the new SHA.
>
> Never amend, never push, never use `--no-verify`. If the pre-commit
> hook fails, fix the underlying issue, re-stage, create a NEW commit.

### Sub-agent E — Document updater

> Update `docs/telemetry/03-pipeline-task-api-events.md` "Action
> items" table:
>
> 1. Flip the `Status` cell for row `{NN}` from `⬜` to `✅ <SHA>`.
> 2. If the implementation diverged from the original sub-doc plan
>    in any user-visible way, append a one-line note to the sub-doc
>    "Status" header (e.g.
>    `**Status**: implemented in commit <SHA> (note: settings allowlist
>    extended with chunk_strategy_kind — see commit body)`).
> 3. If this is task 09, also write the "Closure summary" section at
>    the bottom of the parent doc, listing every commit in landing
>    order — same format as the gap-02 closure summary.
>
> Report `STATUS: ok` with a one-line summary of what changed.

---

## Sequencing notes

- **Tasks 03-01, 03-02, 03-03** are independent and can be implemented
  in any order. The runbook drives them sequentially in numeric order
  for determinism, but they form a logical "PR 1" (foundation).
- **Task 03-04 (pipeline lifecycle)** depends on 03-01 (`tenant_id`
  field) and 03-03 (settings snapshot helper).
- **Task 03-05 (task lifecycle)** depends on 03-01 (`tenant_id`) and
  03-02 (`Task::python_task_type`).
- **Task 03-06 (search EXECUTION STARTED)** depends on 03-01 only
  (`tenant_id` backfill on `EXECUTION COMPLETED` is part of the same
  edit).
- **Task 03-07 (improve OTEL span)** is independent and can land in
  any PR.
- **Task 03-08 (tests)** depends on 04, 05, 06 — the events must exist
  before the integration test can assert on them.
- **Task 03-09 (docs + CI)** lands last.

---

## When the loop ends

After task 09 commits and sub-agent E updates the parent doc, run a
final smoke pass:

```bash
scripts/check_all.sh
cargo test -p cognee-core --features telemetry
cargo test -p cognee-telemetry --features telemetry
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit
```

If all four pass, the gap is closed. Sub-agent E should have written
the "Closure summary" section into
[`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md);
verify the commit list there matches the orchestrator's per-task
commit log.
