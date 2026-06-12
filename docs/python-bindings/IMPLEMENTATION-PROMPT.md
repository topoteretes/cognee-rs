# Orchestration Prompt: Implement Python SDK-Tier Bindings

> **How to use:** start a fresh Claude Code session in the repository root
> (`cognee-rust/`) and paste this entire document as the prompt, or say:
> *"Follow docs/python-bindings/IMPLEMENTATION-PROMPT.md"*.
> The session model orchestrates; sub-agents do all heavy work.

---

## Mission

Implement the Python SDK-tier bindings according to the plan documents in
`docs/python-bindings/`. Work through the task list below **one task at a time, in order**,
using the four-phase sub-agent scheme described in "Per-task workflow". Track progress in
`docs/python-bindings/STATUS.md`.

You are the **orchestrator**. Your job is to dispatch sub-agents, relay context between them,
enforce the rules, and keep your own context small. You do **not** read source files, write
code, or run builds yourself — sub-agents do that.

## Hard rules (non-negotiable)

1. **One task at a time.** Never start a task before the previous one is committed and marked
   done in STATUS.md. Never implement two tasks in one sub-agent call.
2. **Keep the main session lean.** In the main session you may only: read
   `docs/python-bindings/STATUS.md`, read sub-agent reports, and launch sub-agents. Do not
   `Read` source files, do not run `cargo`/`pytest` yourself, do not browse the codebase.
3. **All four phases run for every task**, even if a phase seems unnecessary. Phase order is
   fixed: check-plan → implement → review → finalize.
4. **A task is done only when** `scripts/check_all.sh` passes (verified by the Phase-3 agent)
   AND the Phase-3 agent's verdict is APPROVE.
5. **Retry budget:** if Phase 3 returns REJECT, relay its findings verbatim to a new Phase-2
   agent (fix round). After **2 consecutive REJECTs** on the same task, stop and ask the user.
   If any sub-agent reports it is blocked (missing API, contradictory plan, environment
   failure), stop and ask the user — do not improvise.
6. **Committing is pre-authorized** for completed tasks (this document is the authorization).
   **Pushing is not** — never push.
7. Project coding rules apply to all generated code (sub-agents must be told):
   - `unwrap()` forbidden in non-test code — use `expect("reason why this cannot fail")` or
     `?` propagation; `Mutex/RwLock` poison-unwraps allowed with a
     `// lock poison is unrecoverable` comment.
   - Tests in debug mode (never pass `--release`).
   - `thiserror` in library crates; match surrounding code style; all public traits
     `Send + Sync`.
8. **Test environment:** Python op tests should run with `MOCK_EMBEDDING=true` and an
   in-repo SQLite/temp-dir setup so no network or real LLM is needed. Tests requiring a real
   LLM must skip gracefully when `OPENAI_URL`/`OPENAI_TOKEN` are unset (existing repo
   convention).

## Task list

Execute strictly in this order (each task's plan document is in `docs/python-bindings/`):

| ID | Title | Plan document | Depends on | Commit scope prefix |
|----|-------|---------------|------------|---------------------|
| T1 | Python SDK handle + error hierarchy | `sdk-handle.md` | — | `python:` |
| T2 | Config surface | `config-surface.md` | T1 | `python:` |
| T3 | Hoist pipeline ops to bindings-common + Python add/cognify/add_and_cognify | `core-pipeline-ops.md` (incl. Step 0) | T1, T2 | `bindings-common:` then `python:` (one commit is fine, prefix `python:`) |
| T4 | Hoist + Python search/recall | `retrieval-ops.md` | T3 | `python:` |
| T5 | Hoist + Python forget/update/prune | `data-ops.md` | T3 | `python:` |
| T6 | Hoist + Python dataset management | `dataset-management.md` | T3 | `python:` |
| T7 | Hoist + Python remember/memify/improve | `memory-ops.md` | T3 | `python:` |
| T8 | Hoist + Python sessions/admin/notebooks | `session-admin-ops.md` | T3 | `python:` |
| T9 | Python visualization ops | `visualization-ops.md` | T3 | `python:` |
| T10 | Python cloud serve/disconnect | `cloud-ops.md` | T3 | `python:` |
| T11 | Minor engine-tier gaps | `minor-engine-gaps.md` | — | `python:` |

Notes:
- T3 is the largest task: it includes the one-time refactor of moving op bodies from
  `capi/cognee-capi/src/sdk_ops.rs` and `js/cognee-neon/src/sdk_ops.rs` into a new
  `cognee_bindings_common::ops` module, rewiring capi and neon to call it, **plus** the Python
  surface. If the Phase-1 agent judges this too large for one implementor pass, it may split
  T3 into T3a (hoist + rewire, capi/js checks green) and T3b (Python surface) — record the
  split in STATUS.md.
- T4–T10 each begin by hoisting their domain's op bodies the same way (their plan docs say so
  in the "Prerequisite" note). The hoist pattern will already exist from T3 — implementors
  must follow it exactly.
- T11 is independent and touches only the existing engine-tier files
  (`python/src/cancellation.rs`, `python/src/progress.rs`, `python/cognee_pipeline/__init__.py`).

## Per-task workflow

For each task `{ID}` run these four phases sequentially. Use the `Agent` tool with
`subagent_type: "general-purpose"` for every phase. Pass each agent the **full prompt template**
below with placeholders filled in. Relay the previous phase's report into the next phase's
prompt where the template says so.

---

### Phase 1 — Plan checker (verify & fix the plan document)

Launch with this prompt (fill `{ID}`, `{TITLE}`, `{PLAN_DOC}`):

```
You are a plan auditor for the cognee-rust repository (working dir: repo root).
Task {ID}: "{TITLE}". The plan is docs/python-bindings/{PLAN_DOC}.

Read the plan document, then verify EVERY claim in it against the current code:
1. Every file path, function, struct, enum variant, and method referenced in the plan must
   exist where the plan says (use Grep/Read). Flag and FIX stale references by editing the
   plan document.
2. Check the task is still actual: has any part already been implemented (look in python/src/,
   crates/bindings-common/src/)? If partially implemented, edit the plan to mark the completed
   parts and narrow the remaining scope.
3. Check the implementation steps actually achieve the task goal stated at the top of the
   document, and that prerequisites mentioned in the plan are satisfied (e.g. the
   cognee_bindings_common::ops module from core-pipeline-ops.md Step 0, if this task depends
   on it). If a prerequisite is missing, say so as a BLOCKER.
4. Verify the acceptance criteria are testable with the repo's tooling
   (python/scripts/check.sh = maturin develop + pytest tests/ -v).

Make only minimal, surgical edits to the plan document — do not rewrite it.
Also read docs/python-bindings/STATUS.md for context on completed tasks.

Return a report in EXACTLY this format:
VERDICT: READY | READY-WITH-EDITS | BLOCKER
EDITS-MADE: <bullet list of plan-doc edits, or "none">
BLOCKERS: <bullet list, or "none">
KEY-FACTS-FOR-IMPLEMENTOR: <up to 10 bullets: verified file paths, function signatures,
  and gotchas the implementor must know>
```

- VERDICT `BLOCKER` → stop, ask the user.
- Otherwise proceed to Phase 2, passing along `KEY-FACTS-FOR-IMPLEMENTOR`.

---

### Phase 2 — Implementor

Launch with this prompt (fill `{ID}`, `{TITLE}`, `{PLAN_DOC}`, `{KEY_FACTS}`; on a fix round
also fill `{REVIEW_FINDINGS}`, otherwise write "none — first attempt"):

```
You are implementing task {ID}: "{TITLE}" in the cognee-rust repository (working dir: repo
root). The implementation plan is docs/python-bindings/{PLAN_DOC} — read it fully and follow
its steps. Verified facts from the plan auditor:
{KEY_FACTS}

Review findings to address from the previous attempt (if any):
{REVIEW_FINDINGS}

Rules:
- Implement ONLY this task. Do not refactor unrelated code. Do not start other tasks.
- unwrap() is forbidden in non-test code: use expect("reason why this cannot fail at
  runtime") or proper ? propagation. Mutex/RwLock lock().unwrap() is allowed with a
  "// lock poison is unrecoverable" comment.
- Match the existing code style of python/src/ (PyO3 0.23, pyo3_async_runtimes::tokio::
  future_into_py for async methods) and of capi/neon for any bindings-common changes.
- When hoisting code from capi/cognee-capi/src/ or js/cognee-neon/src/ into
  crates/bindings-common/src/ops/, move logic verbatim where possible and rewire BOTH
  existing bindings to call the shared function; delete the duplicated bodies.
- Write the tests the plan's "Tests" step specifies, under python/tests/. Tests must pass
  with MOCK_EMBEDDING=true and skip gracefully if they need a real LLM and OPENAI_URL is
  unset.
- Update python/cognee_pipeline/__init__.py re-exports as the plan says.
- Verify as you go: run `cargo check --all-targets` after Rust changes, and
  `bash python/scripts/check.sh` after Python-facing changes (it runs maturin develop +
  pytest). Run `cargo fmt` before finishing. Do NOT run scripts/check_all.sh (the reviewer
  does) and do NOT commit.

Return a report in EXACTLY this format:
STATUS: COMPLETE | BLOCKED
FILES-CHANGED: <list of file paths with one-line description each>
CHECKS-RUN: <commands you ran and their outcomes>
DEVIATIONS-FROM-PLAN: <bullet list with justification, or "none">
NOTES-FOR-REVIEWER: <anything the reviewer should pay attention to>
```

- STATUS `BLOCKED` → stop, ask the user.
- Otherwise proceed to Phase 3, passing along `FILES-CHANGED` and `DEVIATIONS-FROM-PLAN`.

---

### Phase 3 — Reviewer & validator

Launch with this prompt (fill `{ID}`, `{TITLE}`, `{PLAN_DOC}`, `{FILES_CHANGED}`,
`{DEVIATIONS}`):

```
You are a strict code reviewer for the cognee-rust repository (working dir: repo root).
Task {ID}: "{TITLE}" was just implemented per docs/python-bindings/{PLAN_DOC}.
Files changed (per implementor): {FILES_CHANGED}
Implementor's claimed deviations from the plan: {DEVIATIONS}

Do all of the following:
1. Read the plan document's goal and acceptance criteria. Read the diff (git diff + git
   status for untracked files) and verify the task is ACTUALLY implemented — every acceptance
   criterion satisfiable, every missing symbol from the plan's tables now present.
2. Correctness review: error handling, async usage (no blocking calls inside
   future_into_py futures, no GIL deadlocks — acquiring the GIL inside a tokio worker via
   Python::with_gil is OK, holding it across .await is NOT), memory/lifetime issues at the
   PyO3 boundary, panics reachable from Python.
3. Convention review: no unwrap() in non-test code (expect() messages must explain WHY they
   cannot fail), thiserror usage, naming consistent with existing python/src/ modules, no
   dead code, no stray debug prints.
4. Security review: no secrets logged or committed, config secret-redaction preserved, no
   command/path injection from user-supplied strings, file writes only to caller-specified
   or documented default paths.
5. Cross-binding integrity: if code was hoisted into crates/bindings-common, confirm
   capi and js bindings were rewired (not left duplicated) and their behaviour is unchanged.
6. Run the checks, in this order, and report each result:
   a. cargo fmt --check
   b. cargo check --all-targets
   c. cargo clippy --all-targets -- -D warnings
   d. bash python/scripts/check.sh        (maturin develop + pytest, needs MOCK_EMBEDDING=true)
   e. bash scripts/check_all.sh           (full suite incl. capi and js binding checks)
   Use debug mode only (no --release).
7. You may fix TRIVIAL issues yourself (typos, missing fmt, doc comments). Anything
   substantive goes in the findings list instead.

Return a report in EXACTLY this format:
VERDICT: APPROVE | REJECT
CHECKS: <one line per check: command — PASS/FAIL (+ key error lines on FAIL)>
FINDINGS: <numbered list, each tagged [blocker]/[minor], or "none">
TRIVIAL-FIXES-APPLIED: <list or "none">
SUMMARY: <2-3 sentences: what the change does and whether it fulfils the task>
```

- VERDICT `REJECT` → go back to Phase 2 (fix round) with `FINDINGS` as `{REVIEW_FINDINGS}`.
  Count it against the 2-REJECT budget.
- VERDICT `APPROVE` → proceed to Phase 4.

---

### Phase 4 — Finalizer (status + commit)

Launch with this prompt (fill `{ID}`, `{TITLE}`, `{PLAN_DOC}`, `{SCOPE_PREFIX}`,
`{REVIEW_SUMMARY}`):

```
You are finalizing task {ID}: "{TITLE}" in the cognee-rust repository (working dir: repo
root). The reviewer approved the implementation. Reviewer summary: {REVIEW_SUMMARY}

Do exactly this:
1. Edit docs/python-bindings/STATUS.md: set task {ID}'s Status to "done", fill in today's
   date, and after committing (step 3) record the commit hash in the table.
2. Edit docs/python-bindings/{PLAN_DOC}: change the "## Status" line near the top to
   "## Status: ✅ Implemented" and, if a feature-matrix row in
   docs/python-bindings/README.md covers this task's operations, update those rows from ❌
   to ✅ in the Python column.
3. Stage ALL files belonging to this task (git status to find them — implementation, tests,
   plan-doc edits, STATUS.md, README.md) and create ONE commit. Message format:
   "{SCOPE_PREFIX} <concise imperative summary of the task> (python bindings {ID})".
   Follow any commit-trailer conventions your harness instructions specify. Do NOT push.
4. Amend STATUS.md with the commit hash from step 3 and amend the commit so the hash file is
   included — OR simpler: put the hash of the implementation commit into STATUS.md and
   include STATUS.md in a tiny follow-up commit "docs: record {ID} commit hash". Choose the
   follow-up-commit approach if amending confuses you.

Return: the final commit hash(es) and one line confirming STATUS.md and the plan doc were
updated.
```

After Phase 4 returns, verify (by reading only `docs/python-bindings/STATUS.md`) that the
task row is marked done, then move to the next task.

---

## Completion

After T11 is finalized:
1. Launch one last general-purpose agent to do a holistic pass: read
   `docs/python-bindings/README.md`, confirm every feature-matrix row in the Python column is
   ✅ (or document why not), run `bash scripts/check_all.sh` one final time, and write a short
   completion summary into STATUS.md.
2. Report to the user: tasks completed, commits made, anything skipped or deferred.

## Quick reference (for sub-agents; orchestrator: do not act on these yourself)

- Plan docs: `docs/python-bindings/*.md` — index in `README.md`
- Python binding source: `python/src/*.rs`, wrapper `python/cognee_pipeline/__init__.py`,
  tests `python/tests/`
- Shared facade crate: `crates/bindings-common/src/{handle,error,services,wire}.rs`
  (new `ops/` module created by T3)
- Reference implementations to hoist from: `capi/cognee-capi/src/sdk*.rs`,
  `js/cognee-neon/src/sdk*.rs`
- Checks: `cargo check --all-targets` · `cargo clippy --all-targets -- -D warnings` ·
  `bash python/scripts/check.sh` · `bash scripts/check_all.sh`
- Test env: `MOCK_EMBEDDING=true`; LLM-dependent tests skip without `OPENAI_URL`/`OPENAI_TOKEN`
