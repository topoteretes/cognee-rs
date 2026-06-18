# Mock-LLM Benchmark — Execution Prompt (5-step, sub-agent driven)

This document is an **operating prompt**. Paste its "Orchestrator instructions"
section (at the bottom) into a Claude Code session running at the repo root
(`/home/dmytro/dev/cognee/cognee-rust`). It drives the implementation of every
task in [README.md](README.md) using a fixed **5-step scheme per task**, with a
dedicated sub-agent for each step.

The orchestrator does **not** write task code itself. It launches sub-agents (via
the **Agent tool**), reads each one's report, and makes go/stop decisions.
Keeping the orchestrator thin is what makes this reliable.

---

## 0. Operating rules (read first, follow exactly)

1. **One task at a time, strictly sequential.** Fully complete all 5 steps for a
   task — including the commit — before starting the next. Tasks share files
   (`crates/llm`, `Settings`/`config.rs`, the index), so never run two in parallel.
2. **Follow the task order in §2.** It respects dependencies. Do not reorder.
3. **Resume by reading the index.** At startup, open [README.md](README.md) and
   skip any task whose **Status** is already `Implemented`. Start at the first
   non-`Implemented` task in §2 order.
4. **Sub-agents do the work; the orchestrator only coordinates.** For each step,
   launch the sub-agent with the template in §3, wait for its report, then decide.
5. **Never commit broken code.** Step 5 (commit) happens only if Step 3 returned
   `APPROVED` **and** Step 4 returned `VALIDATED`. Otherwise **stop and report to
   the user** — do not mark the task `Implemented`, do not commit.
6. **One commit per task.** Each commit bundles everything for that task: code,
   any plan-doc fixes from Step 1, new fixtures/scripts, and the index status
   update.
7. **Compilation/test policy (from CLAUDE.md):** `cargo check --all-targets` for
   compilation; run tests in **debug** mode (no `--release`); after changes run
   `scripts/check_all.sh`.
8. **No `unwrap()` in non-test code** (CLAUDE.md). Sub-agents must honor this; the
   review step (3) must reject violations. `Mutex::lock().unwrap()` with a
   `// lock poison is unrecoverable` comment is allowed.
9. **Branch once at the start.** Before the first task, if on `main`, create and
   switch to `git switch -c perf-mock-bench`. All per-task commits land there.
   (If the branch already exists, switch to it.)
10. **Retry budget:** if a sub-agent fails terminally (tool/API error, empty
    result), retry it **once**. If it fails again, stop and report.
11. **Report progress** to the user after each committed task: task ID, one-line
    summary, and the commit hash.
12. **Feature flags.** Mock/recorder code is behind `cognee-llm/mock`; the bench
    driver behind `cognee-cli/bench`. When validating, build/test with the
    relevant feature enabled (e.g. `--features mock`, `--features bench`) as the
    task doc specifies.

---

## 1. The 5-step scheme (overview)

For each task `T` (subdocument `DOC`, e.g. `tasks/task-01-cassette-format.md`):

1. **Check & fix the description** — sub-agent verifies `DOC` is accurate (no
   stale `file:line` refs, matches the task goal, the task is still actual). It
   **edits `DOC`** to fix drift, or reports the task obsolete.
2. **Implement** — sub-agent implements `T` per the (validated) plan. No commit.
3. **Review & fix** — sub-agent reviews the diff for goal-match, correctness,
   cleanliness, and consistency with existing code; fixes issues directly.
   Returns `APPROVED` or `BLOCKED`.
4. **Validate** — sub-agent runs the project checks; if the task adds runnable
   functionality to an executable, it also **runs that binary** to confirm it
   works. Returns `VALIDATED` or `FAILED`.
5. **Commit** — sub-agent marks `T` `Implemented` in the index and commits
   everything for the task in one commit.

If Step 1 declares the task **obsolete** (already implemented / no longer
applicable): skip Steps 2–4, have the commit sub-agent (Step 5) mark the task
`Implemented` in the index with an "obsolete: <reason>" note appended to its row,
commit just the doc/index change, and report it to the user.

---

## 2. Task order (sequential; respects dependencies)

Process in exactly this order. The order already satisfies the dependencies in
[README.md](README.md).

| # | Task | Subdocument | Depends on | Special handling |
|---|------|-------------|------------|------------------|
| 1 | T1 | [tasks/task-01-cassette-format.md](tasks/task-01-cassette-format.md) | — | — |
| 2 | T2 | [tasks/task-02-recording-llm.md](tasks/task-02-recording-llm.md) | T1 | — |
| 3 | T3 | [tasks/task-03-replay-llm.md](tasks/task-03-replay-llm.md) | T1, T2 | — |
| 4 | T4 | [tasks/task-04-factory-wiring.md](tasks/task-04-factory-wiring.md) | T2, T3 | — |
| 5 | T5 | [tasks/task-05-deterministic-embedding.md](tasks/task-05-deterministic-embedding.md) | — | — |
| 6 | T6 | [tasks/task-06-cli-bench-subcommand.md](tasks/task-06-cli-bench-subcommand.md) | T4, T5 | Step 4 runs the `cognee-cli bench` binary |
| 7 | T7 | [tasks/task-07-python-orchestrator.md](tasks/task-07-python-orchestrator.md) | T6 | Edits the **external** `../cognee` repo — see below |
| 8 | T8 | [tasks/task-08-cassette-fixture.md](tasks/task-08-cassette-fixture.md) | T2, T4, T6 | Recording needs **real LLM credentials** — see below |
| 9 | T9 | [tasks/task-09-docs-verification.md](tasks/task-09-docs-verification.md) | T1–T8 | — |

**Special-handling tasks — stop and involve the user as noted:**

- **T7** edits `../cognee/cognee/tests/performance/statistics_percentile_report.py`
  (a *separate* git repo, not part of this repo's commit). The Step-5 commit for
  T7 covers **only this repo's** files (`scripts/perf/run_mock_bench.sh`). Leave
  the `../cognee` edit as a working change there and report it to the user
  (it is meant to be upstreamed). Do not `git add`/commit inside `../cognee`.
- **T8** records a cassette against a **real LLM** and so needs
  `LLM_API_KEY`/`OPENAI_URL` (and quota). Before Step 2 of T8, the implement
  sub-agent must check those are set; if not, **stop the run and ask the user**
  to provide credentials or to skip T8. Never fabricate a cassette by hand.

---

## 3. Step-by-step procedure with sub-agent prompt templates

For the current task, fill `<DOC>`, `<TASK_ID>`, `<TASK_TITLE>` (from §2 and the
subdocument heading) into the templates. Launch each via the Agent tool with
`subagent_type: general-purpose` unless noted.

### Step 1 — Description-check sub-agent

```
You are validating an implementation-plan document before it is implemented.
Repo root: /home/dmytro/dev/cognee/cognee-rust. Read the root CLAUDE.md and
.claude/CLAUDE.md for conventions first. The Python reference repo is the sibling
checkout at ../cognee (do NOT clone anything to /tmp).

Read docs/performance/<DOC> in full, plus docs/performance/python-approach.md for
context. This is the plan for task <TASK_ID>: "<TASK_TITLE>".

Verify the plan is ACTUAL and CORRECT against the current code:
1. For every `file:line` or symbol the doc references, open it and confirm it
   still exists and contains what the doc claims. Line numbers may have shifted —
   match on content, not the number, and fix the number if it drifted.
2. Check for stale references: files moved/renamed, code already changed, APIs
   that no longer exist, claims the current code contradicts.
3. Confirm the steps still achieve the stated goal and the goal is still relevant
   (the task isn't already implemented by something that landed since).
4. Confirm any cross-task assumptions still hold (e.g. a type/feature an earlier
   task was supposed to add actually exists now).

Then do ONE of:
- Accurate: reply starting "PLAN OK" + a 3-5 line confirmation of key facts you
  verified (cite file:line).
- Stale but still needed: EDIT docs/performance/<DOC> directly to fix it (paths,
  line refs, step details — keep structure and intent). Reply "PLAN FIXED" + a
  bullet list of exactly what you changed and why.
- Already done / not applicable: reply "OBSOLETE" + the evidence (what already
  exists), citing file:line.

Do NOT implement the task. Do NOT edit code outside the doc. Do NOT commit.
Your reply IS the deliverable — be concrete and cite file:line.
```

**Decision:** `PLAN OK`/`PLAN FIXED` → Step 2. `OBSOLETE` → obsolete path in §1
(Step 5 marks it done with a note; report to user).

### Step 2 — Implementation sub-agent

```
You are implementing a task in the cognee-rust repo. Root:
/home/dmytro/dev/cognee/cognee-rust. Read the root CLAUDE.md and .claude/CLAUDE.md
FIRST — especially: no unwrap()/expect() in non-test code (use `?` or
`expect("reason it cannot fail")` with a justifying message); thiserror for
library errors; async-first; per-crate patterns; feature strategy. The Python
reference is the sibling checkout ../cognee.

Implement task <TASK_ID> ("<TASK_TITLE>") EXACTLY per the validated plan in
docs/performance/<DOC>. Follow each step. Where the plan offers a recommended
option among alternatives, take it unless the code makes it impossible (then note
why and take the next-best — do not invent a different design).

Constraints:
- Make all code/config/fixture/doc changes the task needs.
- Match surrounding code's style, naming, and idioms.
- Put new mock/recorder code behind the `mock` feature of cognee-llm; the bench
  driver behind the `bench` feature of cognee-cli, per the doc.
- Do NOT update docs/performance/README.md status. Do NOT git add/commit. Leave
  changes in the working tree.

Verify it compiles before returning: `cargo check --all-targets` (and with the
relevant feature enabled, e.g. `cargo check -p cognee-llm --features mock`). Fix
errors.

Reply with: (a) files changed + a one-line description each, (b) the
`cargo check` results, (c) anything the reviewer should focus on. Your reply IS
the deliverable.
```

**Decision:** proceed to Step 3. If the agent reports it could not implement at
all, retry once (rule 10), else stop and report.

### Step 3 — Review-and-fix sub-agent

```
You are the reviewer for an uncommitted implementation in cognee-rust. Root:
/home/dmytro/dev/cognee/cognee-rust. Read the root CLAUDE.md and .claude/CLAUDE.md.

The working tree contains an uncommitted implementation of task <TASK_ID>
("<TASK_TITLE>") per docs/performance/<DOC>. Run `git diff` and `git status` to
see all changes.

Review against:
1. GOAL MATCH — does it do what <DOC>'s "Expected output" and acceptance criteria
   require? Any missed steps or silently-wrong behavior?
2. CORRECTNESS — logic, error handling, async/concurrency (the recorder uses a
   Mutex across async tasks; the hashing must be order-stable), edge cases.
3. CLEANLINESS — idiomatic Rust, consistent with the crate; no dead code, no
   stray debug prints, no leftover TODOs; no unjustified unwrap()/expect() in
   non-test code.
4. CONSISTENCY — naming, error types (thiserror), feature-gating, and module
   layout match the rest of the workspace.

If you find problems, FIX them directly (you have edit tools). Do NOT git
add/commit. Do NOT touch docs/performance/README.md status.

Reply with EITHER:
- "APPROVED" + a summary of what you verified and any fixes you made, OR
- "BLOCKED" + a numbered list of unresolved problems with file:line and what you
  tried. Your reply IS the deliverable.
```

**Decision:** `APPROVED` → Step 4. `BLOCKED` → stop the run, report blockers
verbatim, do not commit.

### Step 4 — Validation sub-agent

```
You are the validator for an uncommitted, already-reviewed implementation of task
<TASK_ID> ("<TASK_TITLE>") in cognee-rust. Root:
/home/dmytro/dev/cognee/cognee-rust. Read the root CLAUDE.md and .claude/CLAUDE.md.

Run the project checks the task requires (debug mode, no --release):
1. The commands in the "Acceptance / verification" section of docs/performance/<DOC>
   (e.g. `cargo test -p <crate> --features <feat>`, feature on/off checks).
2. `scripts/check_all.sh` — fmt, `cargo check --all-targets`, `clippy -D
   warnings`, and the C API / Python / JS binding checks. This MUST pass.

THEN, if this task adds runnable functionality to an executable (the doc's
"Expected output" mentions a CLI subcommand or binary behavior — true for T6, and
T7's wrapper script), actually RUN it end-to-end and confirm it works:
- T6: build with `--features bench` and run
  `cognee-cli bench --mock-llm` against an empty/minimal cassette with
  `MOCK_EMBEDDING=deterministic` (NO API key); confirm exit 0 and a schema-valid
  --output JSON with all six metric keys and success=true.
- T7: run `RUNS=3 scripts/perf/run_mock_bench.sh` (offline, mock mode) and confirm
  it produces the percentile table + report.html/json.
- Library-only tasks (T1–T5) have no binary to run — rely on the test suite.

Fix anything that fails and re-run until clean. If a failure is environmental
(e.g. a binding toolchain or LLM credentials genuinely unavailable), do NOT paper
over it — report it as a blocker.

Do NOT git add/commit. Do NOT touch docs/performance/README.md status.

Reply with EITHER:
- "VALIDATED" + the tail of the passing `scripts/check_all.sh` output and, if a
  binary was run, its result, OR
- "FAILED" + a numbered list of what failed with output excerpts and what you
  tried. Your reply IS the deliverable.
```

**Decision:** `VALIDATED` → Step 5. `FAILED` → stop the run, report verbatim, do
not commit.

### Step 5 — Commit sub-agent

```
You are committing a completed, reviewed, and validated task in cognee-rust.
Root: /home/dmytro/dev/cognee/cognee-rust.

Task <TASK_ID> ("<TASK_TITLE>") has passed review (APPROVED) and validation
(VALIDATED). Do this:

1. Mark it done in the index: in docs/performance/README.md, change the Status
   cell of the <TASK_ID> row from "Not implemented" to "Implemented".
2. Sanity-check the tree: `git status` and `git diff --stat`. Confirm the changes
   are the expected ones for this task (code + any plan-doc fix + fixtures/scripts
   + the index update) and nothing stray. Do NOT add files from the external
   ../cognee repo. If something looks unexpected, STOP and report instead of
   committing.
3. Commit everything in ONE commit:
     git add -A
     git commit -m "<type>(<scope>): <summary> [<TASK_ID>]" -m "<body>"
   - <type>: `feat` for new surface (T2/T3/T4/T6), `chore`/`refactor` for support
     (T1/T5), `docs` for T9, `test`/`chore` for T7/T8 — pick the dominant change.
   - <scope>: `llm`, `embedding`, `cli`, `bench`, or `perf`.
   - <body>: 1-3 lines on what changed and why, then this trailer EXACTLY:
       Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
4. Reply with: the task ID, a one-line summary, and the short commit hash
   (`git rev-parse --short HEAD`). Your reply IS the deliverable.

Do NOT push. Do NOT open a PR.
```

**Decision:** on success, report to the user and move to the next task in §2. If
the commit sub-agent reports an unexpected tree or a git failure, stop and report.

---

## 4. Validation reference

- **Compilation:** `cargo check --all-targets` (add `--features mock`/`bench` for
  feature-gated code).
- **Tests (debug):** `cargo test -p <crate> [--features <feat>]`. Tests needing an
  LLM/embeddings: `bash scripts/run_tests_with_openai.sh <name>`.
- **Full suite (required before each commit):** `scripts/check_all.sh` — fmt →
  `cargo check --all-targets` → `clippy -D warnings` → C API check → Python check
  → JS check.
- Each subdocument's **Acceptance / verification** section lists task-specific
  commands; the validate step must run those too.

If `scripts/check_all.sh` cannot complete for an environmental reason (a binding
toolchain unavailable on this machine), do NOT mark the task `Implemented` on a
partial pass — report the environmental blocker to the user.

---

## 5. Stop conditions (hand back to the user)

Stop the run and report immediately if any of these occur:
- Step 1 returns `OBSOLETE` for a task you expected to implement and the reason is
  surprising — confirm with the user before skipping.
- Step 3 returns `BLOCKED`.
- Step 4 returns `FAILED` and the validator cannot fix it.
- T8's implement step finds no real LLM credentials (see §2 special handling).
- A sub-agent fails terminally twice (after one retry).
- Git refuses to commit, or the tree contains unexpected changes.

When stopping, report: which task, which step, the sub-agent's verdict/output,
and the current `git status`. Leave the working tree as-is for the user.

---

## 6. Completion

The run is complete when every task in §2 is `Implemented` in
[README.md](README.md). Then:
1. Report a summary to the user: tasks completed, commit hashes, any tasks skipped
   as obsolete (with reasons), and the `../cognee` working change from T7 (if any)
   that still needs upstreaming.
2. Do **not** push or open a PR unless the user asks.

---

## Orchestrator instructions (paste this into the session)

> Execute the mock-LLM benchmark plan in `docs/performance/` following
> `docs/performance/EXECUTION-PROMPT.md` exactly. Read that file and the index
> `docs/performance/README.md` first. Work through the tasks in the §2 order, one
> at a time, running the 5-step scheme (check & fix description → implement →
> review & fix → validate → commit) with a separate general-purpose sub-agent for
> each step. Honor every rule in §0: sequential only; commit (step 5) only if
> step 3 returned APPROVED and step 4 returned VALIDATED; one commit per task
> including the doc/index updates; branch `perf-mock-bench` first. Treat T7
> (edits the external ../cognee repo) and T8 (needs real LLM credentials) per the
> §2 special-handling notes — stop and ask me if T8 has no credentials. Stop and
> report on any BLOCKED, FAILED, surprising OBSOLETE, or repeated sub-agent
> failure. Report progress with the commit hash after each completed task.
