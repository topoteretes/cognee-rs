# Telemetry Gap 05 — DataPoint Provenance Stamping Implementation Runbook

## Purpose

This document is a self-contained **prompt** for executing the eleven
gap-05 implementation tasks defined in [`docs/telemetry/05/`](.)
sequentially with a fixed five-sub-agent workflow per task. Pasting
this document (or pointing a fresh Claude Code session at it) drives
the gap to completion without burning the main session's context
window on per-task investigation, code reading, or test output.

It mirrors the gap-04 runbook
([`docs/telemetry/04/00-implementation-runbook.md`](../04/00-implementation-runbook.md))
verbatim in shape — only the source-of-truth references and the
binding decisions table change.

## How to use

Open a fresh Claude Code session in `/home/dmytro/dev/cognee/cognee-rust`
and ask: *"Follow `docs/telemetry/05/00-implementation-runbook.md`."*
The orchestrator instructions begin at [Orchestrator
prompt](#orchestrator-prompt).

---

## Source-of-truth documents

The orchestrator MUST treat these as the binding contract:

- **Design decisions (locked)**:
  [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
  → "Design decisions (locked)" table. Ten numbered decisions
  pre-approved by the project owner on 2026-05-08. **Do not
  re-litigate them.** The most load-bearing entries:
  - **Decision 1** — Walk via the `HasDataPoint` trait, not
    serde-JSON reflection.
  - **Decision 2** — Visited-set keyed on `DataPoint.id: Uuid`
    (not pointer identity).
  - **Decision 3** — Keep `ExecStatusManager::stamp_provenance`
    (audit-log hook) as-is; do not rename it.
  - **Decision 4** — Add `user_email: Option<String>` to
    `PipelineContext` plus a `user_label()` helper.
  - **Decision 5** — Vector payloads carry the full DataPoint dump
    (Python parity), not just the five `source_*` keys.
  - **Decision 6** — Keep cognify's local `stamp_provenance` helper
    alongside executor-driven stamping.
  - **Decision 7** — `Data.content_hash` propagation gets its own
    audit task (05-02) before the core machinery lands.
  - **Decision 8** — Stream / iterator items are stamped eagerly at
    consumption.
  - **Decision 9** — Pre-stamping inside `extract_graph_from_data`
    is in scope.
  - **Decision 10** — Cross-SDK parity test ships in
    [`05-10`](10-tests.md).

- **Per-task implementation plans**:
  [`01-source-content-hash-field.md`](01-source-content-hash-field.md),
  [`02-data-content-hash-audit.md`](02-data-content-hash-audit.md),
  [`03-provenance-core.md`](03-provenance-core.md),
  [`04-has-datapoint-impls.md`](04-has-datapoint-impls.md),
  [`05-pipeline-context-fields.md`](05-pipeline-context-fields.md),
  [`06-pipeline-executor-integration.md`](06-pipeline-executor-integration.md),
  [`07-user-label-plumbing.md`](07-user-label-plumbing.md),
  [`08-vector-payload-full-dump.md`](08-vector-payload-full-dump.md),
  [`09-cognify-prestamp.md`](09-cognify-prestamp.md),
  [`10-tests.md`](10-tests.md),
  [`11-docs-and-ci.md`](11-docs-and-ci.md).

- **Gap parent**:
  [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
  → "Action items" + "Design decisions (locked)" tables.

- **Root gap analysis**:
  [`docs/telemetry/gap-analysis.md`](../gap-analysis.md).

- **Prior art**:
  [`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md)
  + [`docs/telemetry/03/`](../03/) — closed gap. Lifecycle event
  emitters from gap 03 are unrelated to provenance, but gap 05's
  pipeline-executor edits live in the same file
  (`crates/core/src/pipeline.rs`) and must be additive.
  [`docs/telemetry/04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md)
  — closed gap. Gap 05 does not consume any of its outputs.

## Project conventions to honour

From [`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) and
[`/home/dmytro/.claude/CLAUDE.md`](file:///home/dmytro/.claude/CLAUDE.md):

- Compilation check: `cargo check --all-targets`.
- Tests: debug mode (no `--release`).
- Full check before committing: `scripts/check_all.sh`.
- No `unwrap()` in non-test code. Use `expect("reason")` or `?`.
- Commit message convention: `<scope>: <subject>` — for this gap, scope
  is `telemetry/provenance` (e.g.
  `telemetry/provenance-05-03: add stamp_tree and HasDataPoint trait`).
  Always include the
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
  trailer via heredoc.
- Never `git push`, never amend, never use `--no-verify`.
- Ask the user before any destructive or shared-state action.
- Tests must not depend on a specific `RUST_LOG` value — install any
  test subscriber explicitly per the relevant test helper.

---

## Orchestrator prompt

> You are the orchestrator for telemetry gap 05 (DataPoint provenance
> stamping). Your job is to drive the eleven tasks `05-01` … `05-11`
> through to a clean, committed, documented state, **one at a time, in
> order, with no parallelism between tasks**.
>
> ### Top-level loop
>
> For each task `N` from 01 to 11, in strict numeric order:
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
>   `docs/telemetry/05-datapoint-provenance.md` "Design decisions
>   (locked)" without explicit user approval. Sub-agent A may surface
>   that a decision needs revisiting; if so, escalate.
> - **Never `git push`, never amend a commit, never `--no-verify`.**
> - **Never `git reset --hard`, `git clean -f`, `git checkout --` to
>   "fix" a problem.** If sub-agent C cannot reconcile the diff with
>   the task plan, escalate.
> - **Stop the loop on the first unrecoverable error.** Tasks build on
>   each other (especially 03 → 04 → 06); pushing through a broken
>   task corrupts the chain.
> - **Run `scripts/check_all.sh` exactly once per task** (inside
>   sub-agent C). Don't re-run from the orchestrator.
> - **Never make outbound HTTP requests in tests.** Mock or skip when
>   the network is required.
> - **Confirm before any action with shared-state effects** beyond the
>   working tree.
>
> ### Scope guard
>
> Gap 05 covers DataPoint provenance stamping only. The orchestrator
> must **resist scope creep**:
>
> - **Do not redesign `Data.content_hash`.** Task 05-02 is an audit;
>   if it finds a missing-write path, fix that path narrowly and stop.
>   Switching hash algorithms or column types belongs in a different
>   gap.
> - **Do not refactor `cognify()` to route through
>   `cognee_core::execute()`.** That convergence is explicitly a
>   follow-up tracked under decision 6.
> - **Do not extend the `HasDataPoint` trait** beyond what 05-03
>   specifies (the `for_each_child_mut` recursion hook). Adding
>   visitors for non-provenance use cases belongs in a separate gap.
> - **Do not add new vector-payload keys** beyond the canonical
>   DataPoint dump. The per-call `with_metadata("field", …)` /
>   `with_metadata("dataset_id", …)` extras stay as they are today.
> - **Do not change the `ExecStatusManager::stamp_provenance` audit
>   hook.** Decision 3 locked this in place; the new
>   `cognee_core::provenance::stamp_tree` is a parallel mechanism.

## Sub-agent prompt templates

### Sub-agent A — Task review

> You are reviewing
> `docs/telemetry/05/{TASK_FILE}.md` for task `05-{NN}`. Read it
> end-to-end together with the parent
> [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
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

> Implement task `05-{NN}` by following
> `docs/telemetry/05/{TASK_FILE}.md` "Step-by-step" exactly. Constraints:
>
> - Honour the locked decisions table in
>   [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md).
> - Honour project conventions (no `unwrap()` outside tests; tests run
>   in debug; lock-poison `unwrap` on `Mutex`/`RwLock` is acceptable
>   with a `// lock poison is unrecoverable` comment).
> - Reuse `cognee_core::provenance::*` once task 05-03 lands. Do not
>   re-derive the recursion logic in adapter or task crates.
> - Do not modify files outside the "Files modified" list in the
>   sub-doc, except trivial fixups required to compile.
> - Do not commit. Sub-agent D commits.
>
> When done, report `STATUS: ok` with a list of files modified and a
> one-paragraph diff summary. If you hit an unrecoverable issue,
> report `STATUS: failed` with the diagnostic.

### Sub-agent C — Change reviewer

> Review the working-tree diff for task `05-{NN}` against
> `docs/telemetry/05/{TASK_FILE}.md`. Run, in this order:
>
> 1. `cargo fmt --check`
> 2. `cargo check --all-targets`
> 3. `cargo check --all-targets --no-default-features` (sanity — gap
>    05 is feature-flag-agnostic, but the OFF path must keep building.)
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

> Stage and commit the working-tree changes for task `05-{NN}`.
>
> 1. `git status` to confirm only intended files changed.
> 2. `git add <files>` (explicit list — never `-A`).
> 3. `git commit -m "$(cat <<'EOF'\ntelemetry/provenance-05-{NN}: <subject from sub-doc>\n\n<one-paragraph body>\n\nCo-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>\nEOF\n)"`.
> 4. `git status` to confirm clean tree.
> 5. Print the new SHA.
>
> Never amend, never push, never use `--no-verify`. If the pre-commit
> hook fails, fix the underlying issue, re-stage, create a NEW commit.

### Sub-agent E — Document updater

> Update `docs/telemetry/05-datapoint-provenance.md` "Action
> items" table:
>
> 1. Flip the `Status` cell for row `{NN}` from `⬜` to `✅ <SHA>`.
> 2. If the implementation diverged from the original sub-doc plan
>    in any user-visible way, append a one-line note to the sub-doc
>    "Status" header (e.g.
>    `**Status**: implemented in commit <SHA> (note: also touched
>    crates/foo/Cargo.toml — see commit body)`).
> 3. If this is task 11, also write the "Closure summary" section at
>    the bottom of the parent doc, listing every commit in landing
>    order — same format as the gap-04 closure summary.
>
> Report `STATUS: ok` with a one-line summary of what changed.

---

## Sequencing notes

- **Tasks 05-01, 05-02, 05-05** are foundation work that does not
  introduce stamping behaviour. They can land in any order between
  themselves; the runbook drives them sequentially. They unblock the
  rest of the chain.
- **Task 05-03** (provenance core) depends on 05-01 because
  `stamp_tree` writes to `source_content_hash`.
- **Task 05-04** (model impls) depends on 05-03 (the trait must
  exist before models can implement it).
- **Task 05-06** (executor wiring) depends on 03 + 04 + 05. This is
  the integration point where stamping starts being end-to-end
  visible.
- **Task 05-07** (user-label plumbing) depends only on 05. It can run
  in parallel with 06 in principle, but the runbook keeps strict
  numeric order.
- **Task 05-08** (vector payload) depends on 05-01 (needs
  `source_content_hash` field).
- **Task 05-09** (cognify pre-stamp) depends on 05-03 (uses
  `ProvenanceContext`).
- **Task 05-10** (tests) depends on every preceding implementation
  task. Cross-SDK parity test additionally depends on 05-08.
- **Task 05-11** (docs + CI) lands last.

---

## When the loop ends

After task 11 commits and sub-agent E updates the parent doc, run a
final smoke pass:

```bash
scripts/check_all.sh
cargo test -p cognee-core provenance
cargo test -p cognee-cognify provenance
cargo test -p cognee-vector provenance_payload
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit
```

If all four pass, the gap is closed. Sub-agent E should have written
the "Closure summary" section into
[`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md);
verify the commit list there matches the orchestrator's per-task
commit log.
