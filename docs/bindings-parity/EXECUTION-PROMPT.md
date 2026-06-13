# Bindings Parity — Execution Prompt (4-step, sub-agent driven)

This document is an **operating prompt**. Paste its "Orchestrator instructions"
section into a Claude Code session (Sonnet-class is fine) running at the repo
root (`/home/dmytro/dev/cognee/cognee-rust`). It drives the implementation of
every task in [README.md](README.md) using a fixed 4-step scheme per task, with a
dedicated sub-agent for each step.

The orchestrator does **not** write task code itself. It launches sub-agents (via
the **Task tool**, `subagent_type: general-purpose`), reads each sub-agent's
report, and makes go/stop decisions. Keeping the orchestrator thin is what makes
this reliable on a less-capable model.

---

## 0. Operating rules (read first, follow exactly)

1. **One task at a time, strictly sequential.** Fully complete all 4 steps for a
   task — including the commit — before starting the next. Never run two tasks in
   parallel; tasks can touch shared files (`crates/bindings-common`, the three
   `config.rs`, the index).
2. **Follow the task order in §2.** It respects dependencies. Do not reorder.
3. **Resume by reading the index.** At startup, open [README.md](README.md) and
   skip any task whose **Status** is already `Done`. Start at the first
   non-`Done` task in the §2 order.
4. **Sub-agents do the work; the orchestrator only coordinates.** For each step,
   launch the sub-agent with the exact template in §3, wait for its report, then
   decide.
5. **Never commit broken code.** Step 4 (commit) happens only if Step 3 returns
   `APPROVED` *and* `scripts/check_all.sh` passed. If not, **stop and report to
   the user** — do not mark the task `Done`, do not commit.
6. **One commit per task.** Each commit includes everything for that task: code
   changes, any plan-doc fixes from Step 1, and the index status update.
7. **Compilation/test policy (from CLAUDE.md):** use `cargo check --all-targets`
   to check compilation; run tests in **debug** mode (no `--release`); after
   changes run `scripts/check_all.sh`.
8. **No `unwrap()` in non-test code** (CLAUDE.md). Sub-agents must honor this; the
   review step must reject violations.
9. **Branch once at the start.** Before the first task, if on `main`, create and
   switch to a working branch: `git switch -c bindings-parity`. All per-task
   commits land there. (If the branch already exists, switch to it.)
10. **Retry budget:** if a sub-agent fails terminally (tool/API error, empty
    result), retry it **once**. If it fails again, stop and report.
11. **Report progress** to the user after each committed task: task ID, one-line
    summary, and the commit hash.

---

## 1. The 4-step scheme (overview)

For each task `T` (with subdocument `DOC`, e.g. `01-capi-panic-safety.md`):

1. **Validate the plan** — sub-agent verifies `DOC` is still accurate (no stale
   `file:line` refs, matches the goal). It **fixes the plan in `DOC`** if needed,
   or reports the task is obsolete.
2. **Implement** — sub-agent implements `T` per the (validated) plan. No commit.
3. **Review & validate** — sub-agent reviews the diff for correctness /
   cleanliness / security / consistency, fixes issues, and runs the validation
   checks until they pass. Returns `APPROVED` or blockers.
4. **Record & commit** — orchestrator marks `T` `Done` in the index and commits
   everything in one commit.

If Step 1 declares the task **obsolete** (already implemented / no longer
applicable): skip Steps 2–3, mark the task `Done` in the index with a short
"obsolete: <reason>" note appended to its row, and commit just the doc/index
change (Step 4). Report this to the user.

---

## 2. Task order (sequential; respects dependencies)

Process in exactly this order. Dependencies are noted; the order already
satisfies them.

| # | Task | Subdocument | Depends on |
|---|------|-------------|------------|
| 1 | CR-1 | [01-capi-panic-safety.md](01-capi-panic-safety.md) | — |
| 2 | CR-2 | [02-capi-pipeline-async-tasks.md](02-capi-pipeline-async-tasks.md) | — |
| 3 | CL-1 | [03-capi-header-cbindgen.md](03-capi-header-cbindgen.md) | — (do after CR-1/CR-2 so the header reflects final exports) |
| 4 | ID-1 | [04-python-sdk-parity.md](04-python-sdk-parity.md) | — |
| 5 | ID-2 / DOC-1 | [05-python-typing-stubs.md](05-python-typing-stubs.md) | ID-1 (result-casing & compat layer) |
| 6 | PKG-1 | [06-python-packaging-tests.md](06-python-packaging-tests.md) | — (adds the mypy dev-dep used by ID-2) |
| 7 | PKG-2 | [07-js-distribution.md](07-js-distribution.md) | — |
| 8 | ID-3 / ID-4 / CR-3 | [08-js-types-and-surface.md](08-js-types-and-surface.md) | coordinate ID-4 with ID-2 (shared wire shape) |
| 9 | CL-2 | [10-shared-cleanliness.md](10-shared-cleanliness.md) | — |
| 10 | EX-1 | [09-examples-parity.md](09-examples-parity.md) | ID-1 (Python compat example) |
| 11 | DOC-2 | [11-documentation-parity.md](11-documentation-parity.md) | ID-1, ID-2 (final API shape) |

> Note: `08-js-types-and-surface.md` bundles three task IDs (ID-3, ID-4, CR-3) and
> `05-python-typing-stubs.md` bundles two (ID-2, DOC-1). Treat each *subdocument*
> as one unit of work and one commit, covering all the task IDs it contains.

---

## 3. Step-by-step procedure with sub-agent prompt templates

For the current task, fill the placeholders `<DOC>`, `<TASK_IDS>`, `<TASK_TITLE>`
(from §2 and the subdocument heading) into the templates below. Launch each via
the Task tool with `subagent_type: general-purpose`.

### Step 1 — Plan-validation sub-agent

Launch with this prompt:

```
You are validating an implementation-plan document before it is implemented.
Repo root: /home/dmytro/dev/cognee/cognee-rust. Project conventions are in
.claude/CLAUDE.md and the root CLAUDE.md — read them.

Read docs/bindings-parity/<DOC> in full. This is the plan for task(s) <TASK_IDS>:
"<TASK_TITLE>".

Verify the plan is still ACTUAL and CORRECT against the current code:
1. For every `file:line` or symbol the doc references, open it and confirm it
   still exists and still contains what the doc claims (use grep/read; line
   numbers may have shifted — match on content, not just the number).
2. Check for stale references: files renamed/moved, code already fixed, APIs that
   no longer exist, claims contradicted by the current code.
3. Confirm the plan's steps still achieve the stated goal, and that the goal is
   still relevant (the task isn't already done).

Then do ONE of:
- If the plan is accurate: reply starting with "PLAN OK" and a 3-5 line
  confirmation of the key facts you verified.
- If the plan has stale/incorrect details but the task is still needed: EDIT
  docs/bindings-parity/<DOC> directly to correct it (fix file paths, line refs,
  step details — keep the structure and intent). Reply starting with
  "PLAN FIXED" and a bullet list of exactly what you changed and why.
- If the task is already implemented or no longer applicable: reply starting with
  "OBSOLETE" and explain the evidence (what already exists), citing file:line.

Do NOT implement the task. Do NOT edit any code outside the doc. Do NOT commit.
Your reply IS the deliverable — be concrete and cite file:line.
```

**Orchestrator decision after Step 1:**
- `PLAN OK` or `PLAN FIXED` → proceed to Step 2.
- `OBSOLETE` → skip to the obsolete-handling path in §1 (mark `Done` with note,
  commit doc/index only, move to next task).

### Step 2 — Implementation sub-agent

Launch with this prompt:

```
You are implementing a task in the cognee-rust repo. Root:
/home/dmytro/dev/cognee/cognee-rust. Read the root CLAUDE.md and .claude/CLAUDE.md
for conventions FIRST, especially: no unwrap()/expect() in non-test code (use
`?`/`expect("reason it cannot fail")` only with a justifying message), thiserror
for errors, async-first, the per-crate patterns.

Implement task(s) <TASK_IDS> ("<TASK_TITLE>") EXACTLY per the validated plan in
docs/bindings-parity/<DOC>. Follow each step. Where the plan offers a
"recommended" option among alternatives, take the recommended one unless the
code makes it impossible (if so, note why and take the next-best, do not invent a
different design).

Constraints:
- Make all necessary code/config/doc changes for the task.
- Match the surrounding code's style, naming, and idioms.
- Do NOT update docs/bindings-parity/README.md status. Do NOT git commit or
  git add. Leave changes in the working tree.

When done, verify it compiles: run `cargo check --all-targets` and fix any
errors. If the task touches a single binding and a fast check exists, you may
also run that binding's `*/scripts/check.sh`, but the full validation is the
reviewer's job in the next step.

Reply with: (a) a list of files changed and a one-line description of each
change, (b) the `cargo check --all-targets` result, (c) anything the reviewer
should pay special attention to. Your reply IS the deliverable.
```

**Orchestrator decision after Step 2:** proceed to Step 3 regardless of minor
notes; if the sub-agent reports it could not implement the task at all, retry
once (per rule 10), else stop and report.

### Step 3 — Review-and-validate sub-agent

Launch with this prompt:

```
You are the reviewer/validator for an implemented task in cognee-rust. Root:
/home/dmytro/dev/cognee/cognee-rust. Read the root CLAUDE.md and .claude/CLAUDE.md
for conventions.

The working tree contains an uncommitted implementation of task(s) <TASK_IDS>
("<TASK_TITLE>"), per docs/bindings-parity/<DOC>. Run `git diff` and
`git status` to see all changes.

Review the changes against these criteria:
1. CORRECTNESS — does it do what the task description and the doc's
   "definition of done" require? Any logic errors, missed steps, or
   silently-wrong behavior?
2. CLEANLINESS — idiomatic for the target language and consistent with existing
   code; no dead code, no stray debug output, no leftover TODOs.
3. SOUNDNESS & SECURITY — no new `unwrap()`/`expect()` without justification in
   non-test code; no panics across FFI; no leaked secrets; no memory-safety,
   ownership, or lifetime issues; input validation where the doc requires it.
4. CONSISTENCY — naming, error handling, and patterns match the sibling bindings
   and the rest of the crate.

If you find problems, FIX them directly in the code (you have edit tools). Re-run
checks after fixing.

Then run the validation:
- The specific commands in the "Verification" section of docs/bindings-parity/<DOC>.
- `scripts/check_all.sh` (formatting, compilation, clippy -D warnings, and all
  wrapper binding checks). This MUST pass.
Fix anything that fails and re-run until clean (or until you hit a genuine
blocker).

Do NOT git commit or git add. Do NOT touch docs/bindings-parity/README.md status.

Reply with EITHER:
- "APPROVED" + a summary of what you verified + the tail of the passing
  `scripts/check_all.sh` output, OR
- "BLOCKED" + a numbered list of unresolved problems with file:line and what you
  tried. Your reply IS the deliverable.
```

**Orchestrator decision after Step 3:**
- `APPROVED` → proceed to Step 4.
- `BLOCKED` → **stop the whole run**, report the blockers to the user verbatim,
  and do not commit. (The user decides whether to intervene or adjust the plan.)

### Step 4 — Record status and commit (orchestrator does this directly)

Only on `APPROVED`. The orchestrator performs these actions itself (no sub-agent):

1. **Update the index.** In [README.md](README.md), change the **Status** cell of
   the task's row(s) from `Not started` (or `In progress`) to `Done`. If the
   subdocument bundles multiple task IDs, set all of them to `Done`.
2. **Sanity-check the tree:** run `git status` and `git diff --stat` to confirm
   the changes are the expected ones (code + doc + index, nothing stray).
3. **Commit everything in one commit** with a Conventional-Commits message:

   ```
   git add -A
   git commit -m "<type>(<scope>): <summary> [<TASK_IDS>]" -m "<body>"
   ```

   - `<type>`: `fix` for correctness tasks (CR-*), `feat` for new surface
     (ID-1/ID-3), `refactor`/`chore` for cleanliness (CL-*), `docs` for DOC-*,
     `test`/`chore` for PKG-*/EX-*. Pick the dominant change.
   - `<scope>`: `capi`, `python`, `js`, or `bindings` for shared.
   - Body: 1–3 lines on what changed and why, then the standard Claude Code
     co-author trailer your harness uses
     (`Co-Authored-By: Claude <noreply@anthropic.com>`).

   Example:
   ```
   fix(capi): remove reachable unwrap() on FFI paths and enforce panic safety [CR-1]

   Replace CString::new(...).unwrap() in exec_status.rs with a NUL-safe helper,
   set panic = "abort" for the capi workspace, and add an interior-NUL smoke test.
   Marks CR-1 Done in the bindings-parity index.

   Co-Authored-By: Claude <noreply@anthropic.com>
   ```

4. **Report** to the user: task ID(s), one-line summary, and the new commit hash
   (`git rev-parse --short HEAD`).
5. Move to the next task in §2.

---

## 4. Validation reference

- **Compilation:** `cargo check --all-targets`
- **Tests (debug):** `cargo test` (a specific test: `bash scripts/run_tests_with_openai.sh <name>` when it needs an LLM/embeddings)
- **Full suite (required before each commit):** `scripts/check_all.sh` — runs
  `cargo fmt --check` → `cargo check --all-targets` → `cargo clippy -- -D warnings`
  → C API check (`capi/scripts/check.sh`) → Python check (`python/scripts/check.sh`)
  → JS check (`js/scripts/check.sh`).
- **Per-binding checks** (faster, for the implementer): `capi/scripts/check.sh`,
  `python/scripts/check.sh`, `js/scripts/check.sh`.
- Each subdocument's **Verification** section lists task-specific commands; the
  reviewer must run those too.

If `scripts/check_all.sh` cannot run to completion for an environmental reason
(e.g. a binding toolchain is unavailable on this machine), do NOT mark the task
`Done` on a partial pass — report the environmental blocker to the user.

---

## 5. Stop conditions (when to hand back to the user)

Stop the run and report immediately if any of these occur:
- Step 1 returns `OBSOLETE` for a task you expected to implement and the reason is
  surprising (e.g. it claims a P0 correctness bug is already fixed) — confirm with
  the user before skipping, since a false "obsolete" would silently drop a fix.
- Step 3 returns `BLOCKED`.
- `scripts/check_all.sh` fails and the reviewer cannot fix it.
- A sub-agent fails terminally twice (after one retry).
- Git refuses to commit (conflicts, hooks rejecting).

When stopping, report: which task, which step, the sub-agent's verdict/output,
and the current `git status`. Leave the working tree as-is for the user to
inspect.

---

## 6. Completion

The run is complete when every task in §2 is `Done` in [README.md](README.md).
At that point:
1. Re-score the maturity baseline table in [README.md](README.md) to reflect the
   new state (this can be a final small `docs:` commit).
2. Report a summary to the user: tasks completed, commit hashes, and any tasks
   skipped as obsolete (with reasons).
3. Do **not** push or open a PR unless the user asks.

---

## Orchestrator instructions (paste this into the session)

> Execute the bindings-parity plan in `docs/bindings-parity/` following
> `docs/bindings-parity/EXECUTION-PROMPT.md` exactly. Read that file and the
> index `README.md` first. Work through the tasks in the §2 order, one at a time,
> running the 4-step scheme (validate plan → implement → review & validate →
> record & commit) with a separate general-purpose sub-agent for steps 1–3 and
> doing step 4 yourself. Honor every rule in §0: sequential only, never commit
> unless the reviewer returns APPROVED and `scripts/check_all.sh` passes, one
> commit per task including the doc/index updates, branch `bindings-parity` first.
> Stop and report to me on any BLOCKED, OBSOLETE-surprise, or repeated sub-agent
> failure. Report progress with the commit hash after each completed task.
