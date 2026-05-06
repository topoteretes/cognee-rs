# Telemetry Gap 01 — OTEL/OTLP Implementation Runbook

## Purpose

This document is a self-contained **prompt** for executing the twelve
OpenTelemetry implementation tasks defined in
[`docs/telemetry/01/`](.) sequentially with a fixed five-sub-agent
workflow per task. Pasting this document (or pointing a fresh Claude
Code session at it) drives the gap to completion without burning the
main session's context window on per-task investigation, code reading,
or test output.

## How to use

Open a fresh Claude Code session in `/home/dmytro/dev/cognee/cognee-rust`
and ask: *"Follow `docs/telemetry/01/00-implementation-runbook.md`."*
The orchestrator instructions begin at [Orchestrator
prompt](#orchestrator-prompt).

---

## Source-of-truth documents

The orchestrator MUST treat these as the binding contract:

- **Design decisions (locked)**:
  [`docs/telemetry/01-otel-otlp-export.md`](../01-otel-otlp-export.md)
  → "Design decisions (locked)" table. Twelve numbered decisions
  pre-approved by the project owner. **Do not re-litigate them.**
- **Per-task implementation plans**: [`01-workspace-otel-deps.md`](01-workspace-otel-deps.md),
  [`02-observability-crate-scaffold.md`](02-observability-crate-scaffold.md),
  [`03-cognee-lib-feature-wiring.md`](03-cognee-lib-feature-wiring.md),
  [`04-init-telemetry-implementation.md`](04-init-telemetry-implementation.md),
  [`05-cognee-lib-reexports.md`](05-cognee-lib-reexports.md),
  [`06-cli-subscriber-refactor.md`](06-cli-subscriber-refactor.md),
  [`07-http-server-subscriber-refactor.md`](07-http-server-subscriber-refactor.md),
  [`08-noop-fallback.md`](08-noop-fallback.md),
  [`09-observability-unit-tests.md`](09-observability-unit-tests.md),
  [`10-otel-export-integration-test.md`](10-otel-export-integration-test.md),
  [`11-user-facing-documentation.md`](11-user-facing-documentation.md),
  [`12-ci-updates.md`](12-ci-updates.md).
- **Gap parent**: [`docs/telemetry/01-otel-otlp-export.md`](../01-otel-otlp-export.md)
  → "Action items" table.
- **Root gap analysis**: [`docs/telemetry/gap-analysis.md`](../gap-analysis.md).

## Project conventions to honour

From [`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) and
[`/home/dmytro/.claude/CLAUDE.md`](file:///home/dmytro/.claude/CLAUDE.md):

- Compilation check: `cargo check --all-targets`.
- Tests: debug mode (no `--release`).
- Full check before committing: `scripts/check_all.sh`.
- No `unwrap()` in non-test code. Use `expect("reason")` or `?`.
- Commit message convention (from `git log`): `<scope>: <subject>` — e.g.
  `telemetry/otel: <description>`. Always include the
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
  trailer via heredoc.
- Never `git push`, never amend, never use `--no-verify`.
- Ask the user before any destructive or shared-state action.

---

## Orchestrator prompt

> You are the orchestrator for telemetry gap 01 (OpenTelemetry / OTLP
> export). Your job is to drive the twelve tasks `01-01` … `01-12`
> through to a clean, committed, documented state, **one at a time, in
> order, with no parallelism between tasks**.
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
>   `docs/telemetry/01-otel-otlp-export.md` "Design decisions (locked)"
>   without explicit user approval. Sub-agent A may surface that a
>   decision needs revisiting; if so, escalate.
> - **Never `git push`, never amend a commit, never `--no-verify`.**
> - **Never `git reset --hard`, `git clean -f`, `git checkout --` to
>   "fix" a problem.** If sub-agent C cannot reconcile the diff with
>   the task plan, escalate.
> - **Stop the loop on the first unrecoverable error.** Tasks build on
>   each other; pushing through a broken task corrupts the chain.
> - **Run `scripts/check_all.sh` exactly once per task** (inside
>   sub-agent C). Don't re-run from the orchestrator.
> - **Confirm before any action with shared-state effects** beyond the
>   working tree (network calls beyond `cargo`/test deps, anything
>   touching real OTLP collectors, etc.). The integration test in
>   task 10 uses a loopback fake — that's fine.
>
> ### Per-sub-agent prompts
>
> Use the templates below verbatim, substituting `<N>` (zero-padded,
> e.g. `04`), `<SUBDOC_PATH>` (e.g.
> `docs/telemetry/01/04-init-telemetry-implementation.md`), and other
> placeholders as noted. Each sub-agent should be invoked with
> `subagent_type: general-purpose` unless otherwise specified.

---

## Sub-agent A — Task review

```
You are reviewing task 01-<N> of the OTEL/OTLP implementation runbook.

## Inputs
- Sub-doc you must verify: <SUBDOC_PATH>
- Parent gap doc (read for context, do NOT modify): docs/telemetry/01-otel-otlp-export.md
- Design decisions (locked): the table in the parent gap doc. Treat as
  immutable contract.
- Sibling sub-docs in docs/telemetry/01/. Reference only.

## Your job

1. Read <SUBDOC_PATH> end to end.
2. Verify every code reference (file:line) still exists and the
   surrounding context still matches what the sub-doc claims. Use the
   Read tool — do NOT trust the doc blindly.
3. Verify dependency versions cited in the sub-doc still match the
   current state of `Cargo.toml` (or, for task 01-01, still match what
   crates.io currently considers stable).
4. Check whether prior tasks (01-01 … 01-<N-1>) introduced changes
   that invalidate any reference in <SUBDOC_PATH>. (For example, after
   task 01-04 lands, a `Settings` struct field rename would ripple.)
5. Identify any unresolved design questions that genuinely block
   implementation. **Only escalate questions that the locked design
   decisions don't already answer.**

## Allowed actions

- Edit <SUBDOC_PATH> in place to fix stale file:line refs, version
  pins, or factual errors. Do NOT change the substance of the plan.
- If a substantive change is required (different approach, new
  dependency, new design choice), surface it as a decision question
  rather than editing.
- Do NOT modify any source code.
- Do NOT modify other sub-docs or the parent gap doc.

## Output

Print exactly one of:

  STATUS: ready
  <one-paragraph confirmation>

  STATUS: needs-update
  <bullet list of what you changed in the sub-doc and why>

  STATUS: needs-decision
  <numbered list of blocker questions, each <= 3 sentences,
   each with your recommendation>

Return only the status block — no preamble, no trailing remarks.
```

---

## Sub-agent B — Implementor

```
You are implementing task 01-<N> of the OTEL/OTLP work.

## Inputs
- Authoritative plan: <SUBDOC_PATH>
- Locked design decisions: docs/telemetry/01-otel-otlp-export.md
  → "Design decisions (locked)" table.
- Project rules:
  - cargo check --all-targets after every meaningful change.
  - Run tests in DEBUG mode (no --release).
  - No .unwrap() in non-test code (use expect("reason") or ? — see
    .claude/CLAUDE.md for the rule).
  - Format with `cargo fmt`.
  - Don't add dependencies, abstractions, or features the sub-doc
    doesn't call for.
  - Don't write comments that explain WHAT the code does; only WHY,
    and only when WHY is non-obvious.

## Your job

1. Read <SUBDOC_PATH> in full. Treat its "Step-by-step" section as the
   binding work order.
2. Apply each step in order. Use Read/Edit/Write — do NOT shell out
   for file edits.
3. After every step that compiles code, run `cargo check --all-targets`
   and verify success. If a step's purpose is to add tests, run those
   tests too: `cargo test -p <crate> --features <if-needed>` in debug
   mode.
4. If a step fails:
   - First diagnose by reading error output. Do NOT retry blindly.
   - If the failure is due to a stale reference in the sub-doc, fix
     the implementation to match current reality and note the
     discrepancy in your output.
   - If the failure is due to a missing prerequisite from a prior
     task, STOP and report.
   - Do NOT bypass with `--no-verify`, `cargo check --offline`, or
     similar workarounds.
5. Do NOT commit. Sub-agent D handles commits.
6. Do NOT update documentation. Sub-agent E handles docs.

## Output

Print exactly one of:

  STATUS: success
  Files changed: <list>
  Verification:
    - cargo check --all-targets: PASS
    - cargo test (scope: <...>): PASS / N/A
  Notes: <anything sub-agent C should know about>

  STATUS: failed
  Failed step: <step number / description>
  Failure summary: <2-3 sentences>
  Last command output (last 30 lines): <fenced block>
  Suggested next move: <one option>

Return only the status block.
```

---

## Sub-agent C — Change reviewer

```
You are reviewing the unstaged changes produced by the task 01-<N>
implementor.

## Inputs
- The plan: <SUBDOC_PATH>
- The locked design decisions in docs/telemetry/01-otel-otlp-export.md
- The current working tree (use `git status` and `git diff`).

## Your job — review the diff against four lenses

1. **Plan match.** Does every change correspond to a step in
   <SUBDOC_PATH>? Are there extraneous edits the sub-doc didn't ask
   for? Are any required steps missing?
2. **Code quality.**
   - No `.unwrap()` outside test code.
   - No commented-out code, no debug prints, no `dbg!` left over.
   - Functions and modules carry rustdoc only where it adds value
     (per project conventions).
   - No comments that re-state what the code does.
   - No new dependencies that weren't in the sub-doc.
3. **Security.**
   - No secrets, tokens, or credentials introduced (env-var names are
     fine; literal values are not).
   - Network calls only to the documented OTLP endpoint or to
     127.0.0.1 in tests.
   - No `unsafe` blocks not justified by the sub-doc.
4. **Verification.** Run `scripts/check_all.sh` once. It must pass.
   If it fails, diagnose and fix the root cause — do NOT bypass.

## Allowed actions

- Edit source files to address review findings (formatting, missing
  rustdoc on a public type, swapping `unwrap()` for `expect()`, etc.).
- Run cargo / scripts. No git mutations.
- Do NOT undo the implementor's work without escalating; you are
  refining, not rewriting.

## Output

Print exactly one of:

  STATUS: clean
  scripts/check_all.sh: PASS
  Findings: none

  STATUS: fixed
  scripts/check_all.sh: PASS
  Findings addressed:
    - <bullet list>
  Files changed by reviewer: <list>

  STATUS: failed
  scripts/check_all.sh: FAIL (or review found a blocker)
  Blocker: <2-3 sentences>
  Suggested next move: <one option>

Return only the status block.
```

---

## Sub-agent D — Committer

```
You are committing the staged + unstaged changes for task 01-<N>.

## Pre-flight checks

1. Run `git status -s` and `git diff --stat`. Confirm there are
   changes to commit.
2. Confirm `scripts/check_all.sh` was run by sub-agent C and passed.
   If you cannot confirm, run it yourself.
3. Confirm no file in the diff matches `**/.env*`, `**/*credentials*`,
   or `**/*secret*`. If any does, STOP and escalate.

## Commit

Use the project's commit-message convention from `git log --oneline`:
`<scope>: <subject>`. For these tasks use the scope
`telemetry/otel-01-<N>`.

Subject = a concise description of what shipped (under 70 chars). Use
imperative mood ("add", "wire", "refactor", not "added").

Body (optional, only if needed):
- 1–3 short bullets on what changed at a high level.
- A "Closes" line referencing the sub-doc:
  `Implements docs/telemetry/01/<NN>-...md`.

Always include the Co-Authored-By trailer.

Stage with explicit paths (`git add <paths>`), NOT `git add -A` or
`git add .`.

Use a heredoc for the message so formatting survives:

  git commit -m "$(cat <<'EOF'
  telemetry/otel-01-<N>: <subject>

  - <bullet>
  - <bullet>

  Implements docs/telemetry/01/<NN>-...md

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"

After committing, run `git status` and confirm a clean tree.

## Output

  STATUS: committed
  SHA: <short-sha>
  Subject: <commit subject>
  Tree: clean | dirty (<list>)

If commit failed (pre-commit hook, signing failure, anything):

  STATUS: failed
  Reason: <2-3 sentences>
  Next move: <one option, never --no-verify>
```

---

## Sub-agent E — Document updater

```
You are updating the documentation for completed task 01-<N>.

## Inputs

- The sub-doc just implemented: <SUBDOC_PATH>
- The parent gap doc:
  docs/telemetry/01-otel-otlp-export.md
- The root gap analysis:
  docs/telemetry/gap-analysis.md
- The just-created commit SHA from sub-agent D.

## Your job (bottom-up)

1. **Update <SUBDOC_PATH>**:
   - At the top, set `**Status**: Implemented in commit <sha>` (replace
     any prior `Status:` line, or insert one immediately after the
     title).
   - If the implementor or reviewer noted any deviation from the plan,
     append a short "## Implementation notes" section with bullets
     describing what differed and why.

2. **Update parent doc
   `docs/telemetry/01-otel-otlp-export.md`**:
   - In the "Action items" table, append a "Status" column if not
     present. Mark task <N>'s row as `✅ <sha>`.
   - If task <N> was the LAST one (12), additionally:
     - Replace the doc-level intro phrasing of "this gap is open" with
       "this gap is closed".
     - Add a closing summary section with the list of all 12 commit
       SHAs.

3. **Update root `docs/telemetry/gap-analysis.md`**:
   - In the "Prioritized Gap List", mark item 1 progress: e.g.
     `(N/12 sub-tasks complete)`.
   - If task <N> = 12, change item 1 to `(complete — see commits
     <range>)` and move OTEL out of the active gap list into a brief
     "Completed work" subsection (or strike-through, your choice —
     match existing styling if any).

4. Do NOT modify sibling sub-docs (other tasks).
5. Do NOT modify source code.

## Output

  STATUS: docs-updated
  Files updated:
    - <SUBDOC_PATH>
    - docs/telemetry/01-otel-otlp-export.md
    - docs/telemetry/gap-analysis.md (if applicable)

If a doc edit fails, escalate via:

  STATUS: failed
  Reason: <2-3 sentences>
```

---

## Stop / escalation matrix

| Trigger | Action |
|---|---|
| Sub-agent A returns `needs-decision` | Print the questions to the user. Wait. |
| Sub-agent A returns `needs-update`, second run still `needs-update` | Escalate — possible doc loop or A is malfunctioning. |
| Sub-agent B returns `failed` | Escalate. Do not retry. Suggest the user inspect, then resume the runbook from the same task. |
| Sub-agent C returns `failed` | Same. |
| `scripts/check_all.sh` fails after C | Same. |
| Sub-agent D pre-flight finds secrets / .env files | Stop, escalate, don't commit. |
| Any sub-agent attempts a destructive git op | Treat as bug, escalate. |
| User asks to skip a task | Confirm intent, mark the sub-doc `**Status**: Skipped — reason: <X>` (sub-agent E), proceed. |
| User asks to abort | Stop the loop. Don't unstage or revert. Report current state. |

## Resuming after interruption

If the loop is interrupted (Ctrl-C, context switch, machine restart):

1. Run `git log --oneline | head -20` and look for the most recent
   `telemetry/otel-01-NN: …` commit. The next task is `NN+1`.
2. Run `git status -s`. If the tree is dirty, the previous task did
   not finish — escalate with the dirty file list rather than
   silently committing or discarding.
3. Resume the loop from `NN+1`.

## Verification at the end

After task 12 completes, run as a final check:

```bash
cargo check --all-targets
cargo check --all-targets --features cognee-lib/telemetry
cargo check --workspace --no-default-features
cargo test -p cognee-observability --features telemetry
scripts/check_all.sh
```

All must pass. If any fail, escalate; do not declare the gap closed.
