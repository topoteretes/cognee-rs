# Telemetry Gap 02 — `send_telemetry` Implementation Runbook

## Purpose

This document is a self-contained **prompt** for executing the twelve
`send_telemetry` implementation tasks defined in
[`docs/telemetry/02/`](.) sequentially with a fixed five-sub-agent
workflow per task. Pasting this document (or pointing a fresh Claude
Code session at it) drives the gap to completion without burning the
main session's context window on per-task investigation, code reading,
or test output.

It mirrors the gap-01 runbook
([`docs/telemetry/01/00-implementation-runbook.md`](../01/00-implementation-runbook.md))
verbatim in shape — only the source-of-truth references and the
binding decisions table change.

## How to use

Open a fresh Claude Code session in `/home/dmytro/dev/cognee/cognee-rust`
and ask: *"Follow `docs/telemetry/02/00-implementation-runbook.md`."*
The orchestrator instructions begin at [Orchestrator
prompt](#orchestrator-prompt).

---

## Source-of-truth documents

The orchestrator MUST treat these as the binding contract:

- **Design decisions (locked)**:
  [`docs/telemetry/02-send-telemetry-analytics.md`](../02-send-telemetry-analytics.md)
  → "Design decisions (locked)" table. Twelve numbered decisions
  pre-approved by the project owner on 2026-05-06. **Do not
  re-litigate them.** The most load-bearing entries:
  - **Decision 1** — `telemetry` is **ON** by default in `cognee-lib`
    and `cognee-cli`, **OFF** in `android-default`. Differs from gap 01,
    where the feature was off by default.
  - **Decision 6** — code lives in a new `cognee-telemetry` workspace
    crate, sibling of `cognee-utils`/`cognee-observability`. Do **not**
    place it inside `cognee-utils`.
  - **Decision 9** — pipeline + task lifecycle events are **out of
    scope** for this gap; tracked in
    [`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md).
    This gap covers the *transport* (`send_telemetry` itself + identity
    layers + opt-out + the existing `forget.rs` placeholder + a slice
    of SDK and router callsites).
  - **Decision 11** — `LLM_API_KEY` is read at *event-emission time*,
    not startup. Tests set the env in-test.

- **Per-task implementation plans**:
  [`01-workspace-deps.md`](01-workspace-deps.md),
  [`02-telemetry-crate-scaffold.md`](02-telemetry-crate-scaffold.md),
  [`03-id-derivation.md`](03-id-derivation.md),
  [`04-payload-and-sanitize.md`](04-payload-and-sanitize.md),
  [`05-client-dispatch-and-optout.md`](05-client-dispatch-and-optout.md),
  [`06-public-api-and-noop.md`](06-public-api-and-noop.md),
  [`07-callsite-migration.md`](07-callsite-migration.md),
  [`08-unit-tests.md`](08-unit-tests.md),
  [`09-integration-tests.md`](09-integration-tests.md),
  [`10-cross-sdk-parity.md`](10-cross-sdk-parity.md),
  [`11-user-docs.md`](11-user-docs.md),
  [`12-ci-updates.md`](12-ci-updates.md).

- **Gap parent**:
  [`docs/telemetry/02-send-telemetry-analytics.md`](../02-send-telemetry-analytics.md)
  → "Action items" table.

- **Root gap analysis**:
  [`docs/telemetry/gap-analysis.md`](../gap-analysis.md).

- **Prior art**:
  [`docs/telemetry/01-otel-otlp-export.md`](../01-otel-otlp-export.md)
  + [`docs/telemetry/01/`](../01/) — closed gap. Worth skimming for
  feature-wiring patterns and CI-lane shape; do **not** copy the
  default-off feature stance, which is reversed for gap 02
  (decision 1).

## Project conventions to honour

From [`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) and
[`/home/dmytro/.claude/CLAUDE.md`](file:///home/dmytro/.claude/CLAUDE.md):

- Compilation check: `cargo check --all-targets`.
- Tests: debug mode (no `--release`).
- Full check before committing: `scripts/check_all.sh`.
- No `unwrap()` in non-test code. Use `expect("reason")` or `?`.
- Commit message convention (from `git log`): `<scope>: <subject>` —
  for this gap, scope is `telemetry/send` (e.g.
  `telemetry/send-02-03: implement PBKDF2 api-key tracking id`).
  Always include the
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
  trailer via heredoc.
- Never `git push`, never amend, never use `--no-verify`.
- Ask the user before any destructive or shared-state action.
- Network calls in tests must hit `127.0.0.1` only — the
  `https://test.prometh.ai` proxy is **never** to be exercised from
  CI or unit tests. Use `mockito` (already a workspace dev-dep, see
  decision 10).

---

## Orchestrator prompt

> You are the orchestrator for telemetry gap 02 (`send_telemetry`
> product-analytics client). Your job is to drive the twelve tasks
> `02-01` … `02-12` through to a clean, committed, documented state,
> **one at a time, in order, with no parallelism between tasks**.
>
> ### Top-level loop
>
> For each task `N` from 01 to 12, in strict numeric order:
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
>   `docs/telemetry/02-send-telemetry-analytics.md` "Design decisions
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
>   tests must bind to `127.0.0.1` via `mockito`. The integration
>   tests in tasks 09/10 use a loopback fake — that's fine.
> - **Confirm before any action with shared-state effects** beyond the
>   working tree (network calls beyond `cargo`/test deps, anything
>   touching real proxies, etc.).

## Sub-agent prompt templates

### Sub-agent A — Task review

> You are reviewing
> `docs/telemetry/02/{TASK_FILE}.md` for task `02-{NN}`. Read it
> end-to-end together with the parent
> [`docs/telemetry/02-send-telemetry-analytics.md`](../02-send-telemetry-analytics.md)
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

> Implement task `02-{NN}` by following
> `docs/telemetry/02/{TASK_FILE}.md` "Step-by-step" exactly. Constraints:
>
> - Honour the locked decisions table in
>   [`docs/telemetry/02-send-telemetry-analytics.md`](../02-send-telemetry-analytics.md).
> - Honour project conventions (no `unwrap()` outside tests; tests run
>   in debug; tokio handle fallback per decision 5; `LLM_API_KEY` read
>   at emission time per decision 11; mockito only per decision 10).
> - Do not modify files outside the "Files modified" list in the
>   sub-doc, except trivial fixups required to compile.
> - Do not commit. Sub-agent D commits.
>
> When done, report `STATUS: ok` with a list of files modified and a
> one-paragraph diff summary. If you hit an unrecoverable issue,
> report `STATUS: failed` with the diagnostic.

### Sub-agent C — Change reviewer

> Review the working-tree diff for task `02-{NN}` against
> `docs/telemetry/02/{TASK_FILE}.md`. Run, in this order:
>
> 1. `cargo fmt --check`
> 2. `cargo check --all-targets --features telemetry`
> 3. `cargo check --all-targets --no-default-features` (decision 1
>    requires the OFF path to keep building)
> 4. `cargo clippy --all-targets --features telemetry -- -D warnings`
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

> Stage and commit the working-tree changes for task `02-{NN}`.
>
> 1. `git status` to confirm only intended files changed.
> 2. `git add <files>` (explicit list — never `-A`).
> 3. `git commit -m "$(cat <<'EOF'\ntelemetry/send-02-{NN}: <subject from sub-doc>\n\n<one-paragraph body>\n\nCo-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>\nEOF\n)"`.
> 4. `git status` to confirm clean tree.
> 5. Print the new SHA.
>
> Never amend, never push, never use `--no-verify`. If the pre-commit
> hook fails, fix the underlying issue, re-stage, create a NEW commit.

### Sub-agent E — Document updater

> Update `docs/telemetry/02-send-telemetry-analytics.md` "Action
> items" table:
>
> 1. Flip the `Status` cell for row `{NN}` from `⬜` to `✅ <SHA>`.
> 2. If the implementation diverged from the original sub-doc plan
>    in any user-visible way, append a one-line note to the sub-doc
>    "Status" header (e.g.
>    `**Status**: implemented in commit <SHA> (note: opt-out env
>    parsing tightened — see commit body)`).
> 3. If this is task 12, also write the "Closure summary" section at
>    the bottom of the parent doc, listing every commit in landing
>    order — same format as the gap-01 closure summary.
>
> Report `STATUS: ok` with a one-line summary of what changed.

---

## Sequencing notes

- **Task 02-01 (workspace deps)** is the only task with no dependency.
  Land it first.
- **Tasks 02-03 and 02-04** can be implemented in *either* order after
  the crate scaffold lands; the runbook drives them sequentially in
  numeric order to keep determinism.
- **Task 02-06 (public API + noop)** must land before task 02-07
  (callsite migration) because callsites need a stable public surface
  to compile against.
- **Task 02-09 (integration tests)** depends on task 02-07 because
  some end-to-end assertions exercise the real callsite paths
  (`forget.rs` is the canonical one).
- **Task 02-10 (cross-SDK parity)** runs in Docker and is the only
  task that needs the Python image — keep it last among the test
  tasks.
- **Task 02-11 (user docs)** lands after the public surface is final
  so env-var names and recipes are stable.
- **Task 02-12 (CI updates)** lands last so the new lanes exercise the
  fully-wired tree.

---

## When the loop ends

After task 12 commits and sub-agent E updates the parent doc, run a
final smoke pass:

```bash
scripts/check_all.sh
cargo test -p cognee-telemetry --features telemetry
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit
```

If all three pass, the gap is closed. Sub-agent E should have written
the "Closure summary" section into
[`docs/telemetry/02-send-telemetry-analytics.md`](../02-send-telemetry-analytics.md);
verify the commit list there matches the orchestrator's per-task
commit log.
