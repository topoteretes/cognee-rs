# API v2 Implementation Prompt

Use this prompt to sequentially implement the six API v2 tasks documented in [`README.md`](README.md). Each task has a gap description (`<name>.md`) and a step-by-step plan (`impl/<name>-plan.md`).

---

## Execution principles

1. **Strictly sequential** — only one task in flight at a time. No parallel sub-agents. This avoids merge conflicts and keeps the review surface small.
2. **Main session stays clean** — every non-trivial action (research, code, review, doc updates) is delegated to a sub-agent via the `Agent` tool. The main session only orchestrates and keeps user-facing status. Do NOT perform the research/implementation/review/doc work directly in the main session.
3. **One commit per task** on a single branch (default: current checked-out branch; create `api-v2/impl` if user prefers). Each task's four sub-agents contribute to that one commit. The implementor creates it; the reviewer may amend it; the doc-updater amends it again with doc-status changes.
4. **Pause and ask the user whenever an important question is unresolved** at any stage — ambiguity in scope, a blocking dependency discovered mid-task, an unexpected semantic divergence between Python and Rust, or a review finding that the plan cannot resolve on its own. Do NOT silently invent answers or silently defer.

---

## Prerequisites

Run these once before the first task:

```bash
# Python reference (if missing)
test -d /tmp/cognee-python || git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python

# Verify working tree is clean
cd /home/dmytro/dev/cognee/cognee-rust && git status
```

If the working tree is dirty, ask the user whether to stash, commit, or abort.

---

## Implementation order

Chosen to minimize rework and dependency conflicts. Complete each task fully (all four stages) before starting the next.

| # | Task | Why this position | Effort |
|---|---|---|---|
| 1 | **`forget()`** polish | Smallest change; already 90% done. Low-risk first win that validates the pipeline. | ~5 h |
| 2 | **`recall()`** | Self-contained — rule-parity + override counter + tracing spans. No shared trait changes. | ~11 h |
| 3 | **`visualize()`** | New isolated crate (`cognee-visualization`). Zero coupling with existing code; safe to land independently. | ~19 h |
| 4 | **`improve()`** | Largest internal feature. Adds new `GraphDBTrait` batch methods + `CheckpointStore`. Must land before `remember()` so we don't duplicate Stage 2/4 work. | ~108 h |
| 5 | **`remember()`** | Depends on `improve()` Stages 2 & 4 (the remember plan embeds them). With improve done, this task becomes pure integration + `RememberResult` polish. | ~28 h (reduced from 27.5 h in plan because Stages 2/4 are already done) |
| 6 | **`serve()` / `disconnect()`** | Completely separate (new `cognee-cloud` crate, optional feature). Heaviest external scope. Doing it last means the core SDK is fully usable even if serve/disconnect is deferred or split into its own PR series. | ~200 h |

The task list lives on the current branch. After task 6 (or any subset the user stops at), the branch can be merged to `main` via a separate PR.

---

## Per-task cycle

For each task T in the table above, execute stages 1 → 2 → 3 → 4 in strict order. Between stages, the main session re-reads the updated state of the docs/commit before spawning the next sub-agent.

### Stage 1 — Research (actualize the docs)

**Purpose.** Before touching code, verify the task's gap doc and implementation plan are still accurate. Prior tasks on this branch may have closed parts of this task, shifted file locations, or invalidated line numbers cited in the plan. The research sub-agent edits the docs in-place to correct staleness.

**Launch via `Agent` tool with `subagent_type: "Explore"`.** Prompt template:

```
You are the research sub-agent for API v2 task {T}.

Goal: verify the gap description and implementation plan are still accurate
against the CURRENT state of the Rust codebase, and edit them in place if not.

Codebase: /home/dmytro/dev/cognee/cognee-rust
Python reference: /tmp/cognee-python

Files to read completely first:
  - docs/api-v2/{T}.md                 (gap description)
  - docs/api-v2/impl/{T}-plan.md       (step-by-step plan)
  - docs/api-v2/README.md              (status overview)
  - .claude/CLAUDE.md                  (project conventions)

Verification checklist — apply to EVERY code reference in both task files:

  1. For every cited Rust file path and line number: open the file, confirm
     the symbol / signature / struct is still at that location. If it has
     moved or changed, note the new location.
  2. For every cited Python path: confirm it still exists in /tmp/cognee-python.
  3. For every "create new file" instruction: confirm the file does NOT yet
     exist. If it does (prior task already created it), note that.
  4. Check recent commits (`git log --oneline -20`) for changes that touch any
     file in the plan. Flag any overlap.
  5. Confirm the task's "prerequisites" section (if any) is still accurate —
     e.g. improve's plan depends on Gap-6 session fields; verify they exist.
  6. Check if prior API v2 tasks on this branch have already closed part of
     this one.

Edit the doc files in place to correct anything stale:
  - Update line numbers that have shifted.
  - Strike through or remove items already done (use "~~text~~ — done in
    commit {hash}").
  - Add a note at the top of the gap doc summarizing the research findings
    (one short paragraph).
  - Do NOT remove items that are still pending — only correct their
    descriptions.

Output a short structured report (~200 words):
  - STILL VALID items
  - STALE items (with corrections you applied)
  - ALREADY DONE items (with commit reference)
  - BLOCKING QUESTIONS — anything you found that the plan does not answer
    and that the implementor cannot decide alone

If you found BLOCKING QUESTIONS, list them clearly. Do NOT edit them away;
the main session will surface them to the user.
```

**After the sub-agent returns.** Read its report. If it contains BLOCKING QUESTIONS, **stop and ask the user**. Do not proceed to Stage 2 until every blocking question is resolved. If the user resolves a question, re-spawn the research agent briefly to fold the answer into the docs, then continue.

### Stage 2 — Implementor

**Purpose.** Implement all remaining work for task T in a single commit. The implementor must pull the latest revision first, so it builds on every prior task's work.

**Launch via `Agent` tool with `subagent_type: "general-purpose"`.** Prompt template:

```
You are the implementor sub-agent for API v2 task {T}.

Codebase: /home/dmytro/dev/cognee/cognee-rust

## Pre-flight

1. `git status` — working tree MUST be clean. If not, stop and report.
2. `git log --oneline -5` — confirm the last commit is the previous API v2
   task's commit (or the branch base for task 1). If something unexpected
   is there, stop and report.
3. Pull latest if a remote branch exists: `git pull --ff-only` (skip if
   on a purely local branch).

## Read before coding

  - docs/api-v2/{T}.md                 (gap — freshly updated by Stage 1)
  - docs/api-v2/impl/{T}-plan.md       (plan — freshly updated by Stage 1)
  - .claude/CLAUDE.md                  (Rust coding conventions)
  - Any Rust files the plan cites — read them fully before modifying.

## Implementation rules

1. Follow the plan step by step. Do not skip or reorder unless a dependency
   inside the plan forces it. If you must deviate, document why in the
   commit message.
2. Do NOT modify files outside the task scope unless strictly required for
   compilation (e.g. re-exports in `crates/lib/src/lib.rs`).
3. Preserve every existing test. Do not delete or weaken assertions.
4. Add tests for new public API surface where the plan calls for it. Tests
   must compile and pass.
5. `.unwrap()` is forbidden in non-test code — use `expect("reason why this
   cannot fail")` or proper `?`/`map_err`/`ok_or` propagation. Exception:
   `Mutex::lock().unwrap()` is allowed with a `// lock poison is
   unrecoverable` comment.
6. All public traits must be `Send + Sync`. Prefer `Arc<dyn Trait>` at call
   sites over generics unless performance-critical.
7. If the plan depends on prerequisite work from another gap that is NOT
   yet done on this branch (and the Stage-1 research didn't flag it),
   STOP and report — do not invent workarounds.

## Verification

After code changes, run in order:

   cargo fmt --all
   cargo check --all-targets          # must pass
   cargo clippy --all-targets -- -D warnings   # must pass
   cargo test --workspace -- --test-threads=1 --nocapture

Tests that fail because of missing env vars / models / external services
(see `scripts/run_tests_with_openai.sh`) are acceptable — compilation
failures and unit-test failures are not.

If `scripts/check_all.sh` exists and runs in this environment, execute it
and fix what it reports. Binding checks (C API / Python / JS) may fail
due to missing toolchains — that is acceptable.

## Commit

Stage all changes and create ONE commit:

   git add <explicit files>          # prefer explicit paths over `-A`
   git commit -m "api-v2: implement {T}

   Closes docs/api-v2/{T}.md per docs/api-v2/impl/{T}-plan.md.

   {2-4 sentence summary: which files were added/modified, which stages/steps
   of the plan are covered, any deliberate deviations with one-line reasons.}"

Do NOT push. The commit stays local.

## Report

Return (~150 words):
  - Short summary of what was implemented
  - List of files created / modified (absolute paths)
  - Commit SHA (`git rev-parse HEAD`)
  - `cargo check` / `cargo clippy` / `cargo test` results
  - Any deviations from the plan, with reason
  - Any BLOCKING QUESTIONS you hit that need user input before the
    reviewer runs

If you hit an unrecoverable blocker that the plan does not address, do NOT
commit. Leave the working tree dirty and report the blocker — the main
session will surface it to the user.
```

**After the sub-agent returns.** Read its report. If it reports BLOCKING QUESTIONS or that no commit was created, **stop and ask the user**. Otherwise extract the commit SHA and proceed to Stage 3.

### Stage 3 — Reviewer

**Purpose.** Independently check the latest commit against the task docs. The reviewer is allowed to fix issues in-place and amend the commit.

**Launch via `Agent` tool with `subagent_type: "general-purpose"`.** Prompt template:

```
You are the reviewer sub-agent for API v2 task {T}.

Codebase: /home/dmytro/dev/cognee/cognee-rust

## Context

Commit under review: HEAD ({commit_sha_from_stage_2})
Task docs:
  - docs/api-v2/{T}.md
  - docs/api-v2/impl/{T}-plan.md
  - .claude/CLAUDE.md (coding conventions)

## What to review

1. `git show --stat HEAD` — list of changed files.
2. `git diff HEAD^..HEAD` — full diff of the task's commit.
3. For each changed file:
   a. Does the change match the plan's intent for that file?
   b. Security: SQL injection, command injection, path traversal,
      unbounded allocations, log injection, unchecked deserialization.
   c. `.unwrap()` in non-test code (forbidden except for `Mutex::lock`).
   d. Preserved behavior: existing tests still compile and are not weakened.
   e. New public API has at least a one-line doc comment.
   f. No unrelated files touched.
   g. Imports and re-exports are correct (nothing dangling).
4. Run in the repo root:
      cargo fmt --check
      cargo check --all-targets
      cargo clippy --all-targets -- -D warnings
      cargo test --workspace -- --test-threads=1
   Every step must pass (same env caveats as the implementor).
5. Spot-check the gap-doc claim: does the commit actually close the items
   the gap says it should? If the gap says "Stages 1, 2, 4 are stubs and
   this task replaces them with real impls", open the relevant files and
   confirm they are no longer stubs.

## Fixing issues

If you find problems:
  - Fix them in place in the repo working tree.
  - Re-run cargo check + clippy + tests to confirm the fix.
  - Amend the existing commit (preserve the original message):
        git add <explicit files>
        git commit --amend --no-edit
  - Note each fix in your report.

## Report

Return a structured report:
  - One line per review criterion: PASS or FAIL (with reason if FAIL).
  - List of fixes applied, each with file:line.
  - Final verdict: APPROVED | BLOCKED.
  - If BLOCKED: explain what's wrong and what the user / implementor
    should do. Do NOT amend if blocked — the original commit should stay
    so the user can inspect it.

If you find a design-level issue the plan did not anticipate, list it as
a BLOCKING QUESTION rather than silently redesigning the solution.
```

**After the sub-agent returns.** If the verdict is APPROVED, proceed to Stage 4. If BLOCKED, **stop and ask the user** with the reviewer's findings summarized. Do NOT run Stage 4 until the block is resolved (either by a follow-up implementor run or user-directed waiver).

### Stage 4 — Doc update

**Purpose.** Reflect the newly-landed work in the task docs and the overview, and fold those edits into the same commit so there's a single atomic commit per task.

**Launch via `Agent` tool with `subagent_type: "general-purpose"`.** Prompt template:

```
You are the doc-update sub-agent for API v2 task {T}.

The implementation of task {T} has just been reviewed and APPROVED at
commit HEAD ({commit_sha}).

## Your job

Update the following docs to reflect that task {T} is now Implemented:

1. docs/api-v2/{T}.md
   - Change the `**Rust status:** ...` line at the top to `**Rust status:**
     **Implemented**` (keep any parenthetical notes that are still accurate,
     e.g. "with session cleanup for `everything` mode").
   - Add a "## Implementation notes" section at the bottom (or extend it if
     already present) with:
       - Commit SHA of the implementation
       - One-paragraph summary of what was actually done (pull from the
         commit message)
       - Any deliberate deviations from the plan, each explained briefly

2. docs/api-v2/impl/{T}-plan.md
   - For each step, mark it `- [x]` if done (add a leading checkbox if it's
     not already there). Leave `- [ ]` for anything deferred with a note.
   - Add a short "## Status" section at the top:
       Implemented: yes | partial (with list of deferred items)
       Commit: {sha}
       Date: {ISO date}

3. docs/api-v2/README.md
   - Update the row for task {T} in the "Functions at a glance" table:
     change Rust status to **Implemented** (or **Partial** if some items
     were deferred).
   - If all 6 tasks are now Implemented, update the "Summary of findings"
     section accordingly.

## Rules

  - Do NOT modify any code files. Docs only.
  - Do NOT retcon or erase previously-true content. Mark it done; don't
    hide that it was once pending.
  - Preserve all existing references, links, and formatting.

## Commit

Amend the current commit (task {T}'s single commit) with these doc changes:

   git add docs/api-v2/{T}.md docs/api-v2/impl/{T}-plan.md docs/api-v2/README.md
   git commit --amend --no-edit

## Report

Return (~60 words): which files were touched and the final commit SHA
(`git rev-parse HEAD`). If something in the docs couldn't be updated
without user input (e.g. ambiguous status between Implemented and
Partial), list it as a BLOCKING QUESTION.
```

**After the sub-agent returns.** Announce to the user that task T is complete: the new commit SHA, the one-paragraph summary, and the remaining task count. Then await the user's go-ahead before starting Stage 1 of the next task.

---

## When to pause and ask the user

Pause the sequence and surface the question to the user whenever:

1. **Stage 1** returns BLOCKING QUESTIONS from the research agent.
2. **Stage 2** reports an unrecoverable implementation blocker or no commit was created.
3. **Stage 3** returns a BLOCKED verdict.
4. **Stage 4** reports an ambiguous doc status.
5. Any sub-agent discovers a semantic divergence between the Python SDK and what the plan says Rust should do.
6. The working tree is unexpectedly dirty at the start of a stage.
7. A stage would require editing files well outside the task's scope (e.g. workspace-level `Cargo.toml` changes that weren't in the plan).
8. The user has not yet confirmed to proceed to the next task after the previous one completed.

When pausing, state clearly:
- Which task / stage triggered the pause.
- The specific question(s) to answer.
- The suggested default action if the user just says "proceed" (with its risks).

Never invent answers to unresolved questions. Never skip a pause condition.

---

## What NOT to do

- Do NOT run sub-agents in parallel for the same task, or across tasks.
- Do NOT use isolated worktrees — commits land directly on the current branch so each task builds on the previous one's state.
- Do NOT squash, rebase, or reorder commits. Each task = one commit; amends during Stages 3 and 4 are permitted but the overall count stays at one.
- Do NOT push to any remote.
- Do NOT skip the doc-update stage "because the code already works" — the docs are load-bearing for the next task's research agent.
- Do NOT add HTTP endpoints, new LLM providers, or unrelated refactors. Stay inside the task's scope.

---

## If a task turns out to be already done

If the Stage-1 research agent concludes that all items for task T are already implemented on the current branch (e.g. a prior task closed them as a side-effect), skip Stages 2 and 3 and run Stage 4 directly to mark the task done — amending the previous task's commit is not allowed, so in that case create a standalone docs-only commit titled `api-v2: mark {T} as implemented`. Announce clearly to the user that task T was already covered.

---

## Conventions reminders (from `.claude/CLAUDE.md`)

- `cargo check --all-targets` for compilation checks.
- Run tests in debug mode (no `--release` unless the user explicitly asks).
- `scripts/check_all.sh` is the final verification gate.
- `.env` is auto-loaded via `dotenv::dotenv()`; do not `source` it manually.
- Error handling: `thiserror` in library crates, `anyhow` in binaries/examples.
- Prefer streaming / zero-copy / `Arc<dyn Trait>` patterns where applicable.
- New feature-gated capabilities must be propagated to `cognee-lib` and `cognee-cli` default feature lists unless platform-specific or test-only.
