# LIB-06 — Executor-Routed Convenience Pipelines Implementation Runbook

## Purpose

This document is a self-contained **prompt** for executing the six LIB-06
implementation tasks defined in [`docs/telemetry/lib-06/`](.) sequentially
with a fixed five-sub-agent workflow per task. Pasting this document (or
pointing a fresh Claude Code session at it) drives the gap to completion
without burning the main session's context window on per-task investigation,
code reading, or test output.

It mirrors the gap-08 runbook
([`docs/telemetry/08/00-implementation-runbook.md`](../08/00-implementation-runbook.md))
verbatim in shape — only the source-of-truth references and the per-task
decisions change.

## How to use

Open a fresh Claude Code session in `/home/dmytro/dev/cognee/cognee-rust`
and ask: *"Follow `docs/telemetry/lib-06/00-implementation-runbook.md`."*
The orchestrator instructions begin at
[Orchestrator prompt](#orchestrator-prompt).

---

## Source-of-truth documents

The orchestrator MUST treat these as the binding contract:

- **Design decisions (locked)**:
  [`docs/telemetry/lib-06-executor-routed-convenience.md`](../lib-06-executor-routed-convenience.md)
  → "Design decisions (locked)" table. Fifteen numbered decisions
  pre-approved by the project owner on 2026-05-13. **Do not re-litigate
  them.** The most load-bearing entries:
  - **Decision 1** — Convenience functions grow new required parameters;
    every call site updates in the same sub-task.
  - **Decision 3** — Provenance equivalence is *verified per task* by the
    full cognify E2E suite + cross-SDK structural test.
  - **Decision 5** — `extract_dlt_fk_edges` stays as post-pipeline
    teardown, after `execute()` returns.
  - **Decision 7** — `AddParams` injection happens inside the persist task
    closure, not via `RunSpec` or `TaskContext`.
  - **Decision 8** (locked 2026-05-13) — Memify passes `Vec<Triplet>`
    directly to the executor; no placeholder ZST. Triplet extraction
    and the empty-triplets short-circuit are pre-flight.
  - **Decision 11** — Watchers stay `NoopWatcher` for the duration of
    LIB-06. Hooking a real `DbPipelineWatcher` belongs to gap-08 task 07
    (which unblocks once LIB-06 closes).
  - **Decision 14** (locked 2026-05-13) — Pipeline names align on the
    builder strings: `build_cognify_pipeline` → `"cognify"`, the new
    memify builder → `"memify"`, `build_add_pipeline` → `"ingestion"`.
    Legacy inline `stamp_provenance(..., "cognify_pipeline", ...)`
    literals are rewritten to `"cognify"` as part of LIB-06-03.
  - **Decision 15** — Cross-SDK harness is the gap-closure equivalence gate.

- **Per-task implementation plans**:
  [`01-add-pipeline-executor-route.md`](01-add-pipeline-executor-route.md),
  [`02-memify-executor-route.md`](02-memify-executor-route.md),
  [`03-cognify-standard-executor-route.md`](03-cognify-standard-executor-route.md),
  [`04-cognify-temporal-executor-route.md`](04-cognify-temporal-executor-route.md),
  [`05-cleanup-todos.md`](05-cleanup-todos.md),
  [`06-tests-and-closure-summary.md`](06-tests-and-closure-summary.md).

- **Gap parent**:
  [`docs/telemetry/lib-06-executor-routed-convenience.md`](../lib-06-executor-routed-convenience.md)
  → "Action items" + "Design decisions (locked)" tables.

- **Downstream consumer**:
  [`docs/telemetry/08-pipeline-run-status.md`](../08-pipeline-run-status.md)
  task 07 — wires `DbPipelineWatcher` through the convenience functions
  this gap rebuilds. Do not start gap 08-07 until LIB-06 closes.

- **Prior art**:
  - [`docs/telemetry/08/`](../08/) — five-sub-agent workflow lifted from
    here.
  - [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
    + [`docs/telemetry/05/`](../05/) — closed gap; documents
    `stamp_tree_dyn` and how the executor stamps DataPoints. LIB-06's
    provenance-equivalence claim (Decision 3) relies on the gap-05
    invariants holding.
  - Python references the Rust port must remain compatible with:
    [`cognee/modules/pipelines/operations/run_tasks.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks.py)
    (clone via `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`).

## Project conventions to honour

From [`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) and
[`/home/dmytro/.claude/CLAUDE.md`](file:///home/dmytro/.claude/CLAUDE.md):

- Compilation check: `cargo check --all-targets`.
- **Tests: debug mode (no `--release` flag)** unless the user has explicitly
  asked.
- Full check before committing: `scripts/check_all.sh`.
- No `unwrap()` in non-test code. Use `expect("reason")` or `?`. Lock-poison
  `unwrap` on `Mutex` / `RwLock` is acceptable with a
  `// lock poison is unrecoverable` comment.
- Commit message convention: `<scope>: <subject>` — for this gap, scope is
  `telemetry/lib-06` (e.g.
  `telemetry/lib-06-01: route AddPipeline::add through pipeline::execute`).
  Always include the
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
  trailer via heredoc.
- Never `git push`, never amend, never use `--no-verify`.
- Ask the user before any destructive or shared-state action.
- **Provenance equivalence (Decision 3) is verified per task before
  commit**, not at gap closure. Each sub-task that touches a cognify code
  path runs the full cognify E2E suite + the cross-SDK structural test as
  part of sub-agent C's verification.

---

## Orchestrator prompt

> You are the orchestrator for telemetry gap LIB-06 (executor-routed
> convenience pipelines). Your job is to drive the six tasks `lib-06-01` …
> `lib-06-06` through to a clean, committed, documented state, **one at a
> time, in order, with no parallelism between tasks**.
>
> ### Top-level loop
>
> For each task `N` from 01 to 06, in strict numeric order:
>
> 1. Run **Sub-agent A — Task review** for task `N`.
> 2. If A returns `STATUS: needs-decision`, escalate the question to the
>    user and **wait** for an answer. After the user responds, re-run A so
>    it records the decision into the sub-doc, then continue.
> 3. If A returns `STATUS: needs-update`, the sub-doc has been edited in
>    place. Re-run A once to confirm `STATUS: ready`. If a second run still
>    returns `needs-update`, escalate — there is a loop or A is wrong about
>    itself.
> 4. Run **Sub-agent B — Implementor** for task `N`.
> 5. If B returns `STATUS: failed`, escalate to the user with the failure
>    summary; **do not** attempt a second implementation automatically
>    (avoid runaway).
> 6. Run **Sub-agent C — Change reviewer** for task `N`.
> 7. If C returns `STATUS: failed`, escalate; if `STATUS: fixed`, C has
>    already amended the working tree — proceed.
> 8. Run **Sub-agent D — Committer** for task `N`.
> 9. Run **Sub-agent E — Document updater** for task `N`.
> 10. Print a short orchestrator-level summary to the user (one line:
>     `task lib-06-NN — committed <sha> — docs updated`) and proceed to
>     `N+1`.
>
> Only ONE sub-agent runs at a time. Never launch sub-agents for different
> tasks in parallel. Within a task, A must finish before B starts, etc.
>
> ### Hard rules
>
> - **Never modify the locked design decisions** in
>   `docs/telemetry/lib-06-executor-routed-convenience.md` "Design
>   decisions (locked)" without explicit user approval. Sub-agent A may
>   surface that a decision needs revisiting; if so, escalate.
> - **Never `git push`, never amend a commit, never `--no-verify`.**
> - **Never `git reset --hard`, `git clean -f`, `git checkout --` to "fix"
>   a problem.** If sub-agent C cannot reconcile the diff with the task
>   plan, escalate.
> - **Stop the loop on the first unrecoverable error.** Tasks build on
>   each other (01 → 03 → 04 → 05 → 06; 02 is independent of 01 but
>   precedes the cognify changes for narrative continuity); pushing
>   through a broken task corrupts the chain.
> - **Run `scripts/check_all.sh` exactly once per task** (inside sub-agent
>   C). Don't re-run from the orchestrator.
> - **Tests run in debug mode** — never pass `--release` unless the user
>   has explicitly asked.
> - **Provenance equivalence is a per-task gate.** Sub-tasks 03 and 04
>   (cognify) **must** run the full cognify E2E suite + the cross-SDK
>   structural test as part of sub-agent C's verification before commit.
>   A green `cargo check` is not sufficient.
> - **Never make outbound HTTP requests in tests.** The cross-SDK test
>   runs against the Dockerised harness only.
> - **Confirm before any action with shared-state effects** beyond the
>   working tree.
> - **Env-mutating tests must be serialised.** Use
>   `#[serial_test::serial]` for any Rust test that touches the global
>   tokio runtime, env vars, or shared DB pool.
>
> ### Scope guard
>
> LIB-06 covers the executor-route refactor only. The orchestrator must
> **resist scope creep**:
>
> - **Do not wire a real `DbPipelineWatcher` through the convenience
>   functions.** Decision 11 keeps the watcher as `NoopWatcher` for this
>   gap. Gap-08 task 07 picks up the watcher wiring after LIB-06 closes.
> - **Do not introduce a new `Pipeline` / `TaskContext` /
>   `PipelineWatcher` trait.** Decision 12 forbids it. Reuse existing
>   types.
> - **Do not convert `extract_dlt_fk_edges` to a typed task.** Decision 5
>   keeps it as post-pipeline teardown for this gap.
> - **Do not add new fields to `RunSpec` for `AddParams`.** Decision 7
>   keeps the injection inside the task closure.
> - **Do not refactor `MemifyConfig.custom_data` away.** Decision 8 keeps
>   it; sub-task 02 handles the custom-data branch as a pre-flight check.
> - **Align pipeline-name strings on the builder names, not the legacy
>   `_pipeline` suffix.** Decision 14 (locked 2026-05-13) sets the
>   canonical names to `"cognify"` / `"memify"` / `"ingestion"`. Inline
>   `stamp_provenance(..., "cognify_pipeline", ...)` literals in
>   `crates/cognify/src/tasks.rs` are rewritten to `"cognify"` as part
>   of LIB-06-03. Do not pick new strings; do not preserve the legacy
>   `_pipeline` suffix.
> - **Do not add binding-side wrappers.** Bindings consume the existing
>   convenience signatures; if a signature grows a required parameter,
>   bindings pass through the appropriate value (their existing
>   `Arc<dyn GraphDBTrait>` / `Arc<dyn VectorDB>` instances).
> - **Do not touch Android scripts.** Android consumes the CLI; the CLI
>   changes from sub-tasks 01-04 propagate automatically.

## Sub-agent prompt templates

### Sub-agent A — Task review

> You are reviewing
> `docs/telemetry/lib-06/{TASK_FILE}.md` for task `lib-06-{NN}`. Read it
> end-to-end together with the parent
> [`docs/telemetry/lib-06-executor-routed-convenience.md`](../lib-06-executor-routed-convenience.md)
> and the locked design decisions table inside it.
>
> Your goal:
>
> 1. Confirm the sub-doc's "Pre-conditions" hold against the current
>    working tree (run `git status`, `cargo check --all-targets`).
> 2. Confirm the sub-doc's "Step-by-step" still matches reality. If a
>    referenced file has shifted line numbers since the doc was written
>    (which it will, especially after a preceding LIB-06 task commits),
>    **update the doc in place** and report `STATUS: needs-update`. Also
>    refresh the "as of `<sha>`" markers if present.
> 3. Run `rg "LIB-06 follow-up\|cognify_pipeline\|memify_pipeline\|ingestion_pipeline"
>    crates/` and confirm that, post-refactor, every inline
>    `stamp_provenance` call uses the builder string (`"cognify"` /
>    `"memify"` / `"ingestion"`) — not the legacy `_pipeline` suffix.
>    Decision 14 (locked 2026-05-13) requires the alignment.
> 4. Surface any unresolved decision the doc still defers. If you find
>    one, report `STATUS: needs-decision` with the question.
> 5. Otherwise report `STATUS: ready`.
>
> Report formats (one short paragraph each):
>
> - `STATUS: ready` — proceed to implementor.
> - `STATUS: needs-update` — the doc has been edited in place; rerun me.
> - `STATUS: needs-decision` — quote the open question, the relevant
>   sub-doc section, and what the locked decisions table currently says
>   (or fails to say).

### Sub-agent B — Implementor

> Implement task `lib-06-{NN}` by following
> `docs/telemetry/lib-06/{TASK_FILE}.md` "Step-by-step" exactly.
> Constraints:
>
> - Honour the locked decisions table in
>   [`docs/telemetry/lib-06-executor-routed-convenience.md`](../lib-06-executor-routed-convenience.md).
> - Honour project conventions (no `unwrap()` outside tests; tests run in
>   debug; lock-poison `unwrap` on `Mutex` / `RwLock` is acceptable with a
>   `// lock poison is unrecoverable` comment).
> - Reuse existing types: `Pipeline`, `PipelineBuilder`, `TypedTask`,
>   `TaskContext`, `TaskContextBuilder`, `PipelineWatcher`, `NoopWatcher`.
>   Do **not** invent parallel trait machinery.
> - Use existing mocks (`MockGraphDB`, `MockVectorDB`,
>   `cognee_database::connect("sqlite::memory:")`) for tests where a real
>   backend is unavailable.
> - Pipeline-name strings must end up as the builder names (`"cognify"` /
>   `"memify"` / `"ingestion"`) per Decision 14 (locked 2026-05-13). The
>   legacy inline `stamp_provenance(..., "cognify_pipeline", ...)`
>   literals get rewritten to `"cognify"` as part of LIB-06-03; same
>   for any `_pipeline`-suffixed literals in memify and ingestion.
> - For cognify sub-tasks, do **not** drop the `extract_dlt_fk_edges`
>   teardown call. Decision 5 keeps it after `execute()`.
> - Do not modify files outside the "Files modified" list in the sub-doc,
>   except trivial fixups required to compile.
> - Do not commit. Sub-agent D commits.
>
> When done, report `STATUS: ok` with a list of files modified and a
> one-paragraph diff summary. If you hit an unrecoverable issue, report
> `STATUS: failed` with the diagnostic.

### Sub-agent C — Change reviewer

> Review the working-tree diff for task `lib-06-{NN}` against
> `docs/telemetry/lib-06/{TASK_FILE}.md`. Run, in this order:
>
> 1. `cargo fmt --check`
> 2. `cargo check --all-targets`
> 3. `cargo clippy --all-targets -- -D warnings`
> 4. `scripts/check_all.sh`
> 5. The verification commands listed in the sub-doc itself ("Verification"
>    section).
> 6. **For sub-tasks 03 and 04 (cognify)**: run
>    `bash scripts/run_tests_with_openai.sh test_fact_extraction` (or the
>    equivalent cognify-suite command listed in the sub-doc) and confirm
>    the existing baseline outputs hold. Then run
>    `cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit`
>    and confirm `test_cognify_structural.py` passes within the existing
>    50% / 0.3-Jaccard tolerances. This is the Decision 3 gate.
>
> If any step fails, fix the underlying issue (do **not** silence
> warnings, do **not** disable lints, do **not** loosen the cross-SDK
> tolerances) and report `STATUS: fixed` with the fix description. If the
> failure is unrelated to the task, report `STATUS: failed` and escalate.
>
> If everything passes, report `STATUS: ok`.

### Sub-agent D — Committer

> Stage and commit the working-tree changes for task `lib-06-{NN}`.
>
> 1. `git status` to confirm only intended files changed.
> 2. `git add <files>` (explicit list — never `-A`).
> 3. `git commit -m "$(cat <<'EOF'\ntelemetry/lib-06-{NN}: <subject from sub-doc>\n\n<one-paragraph body>\n\nCo-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>\nEOF\n)"`.
> 4. `git status` to confirm clean tree.
> 5. Print the new SHA.
>
> Never amend, never push, never use `--no-verify`. If the pre-commit
> hook fails, fix the underlying issue, re-stage, create a NEW commit.

### Sub-agent E — Document updater

> Update `docs/telemetry/lib-06-executor-routed-convenience.md` "Action
> items" table:
>
> 1. Flip the `Status` cell for row `{NN}` from `⬜` to `✅ <SHA>`.
> 2. If the implementation diverged from the original sub-doc plan in any
>    user-visible way, append a one-line note to the sub-doc "Status"
>    header (e.g. `**Status**: implemented in commit <SHA> (note: also
>    touched crates/X — see commit body)`).
> 3. If this is task 06, write the "Closure summary" section at the
>    bottom of the parent doc, listing every commit in landing order —
>    same format as the gap-05 / gap-07 / gap-08 closure summaries. Also
>    add a note to `docs/telemetry/gap-analysis.md` "Future work" section
>    pointing at LIB-06 as the closure for the
>    `Pipeline::telemetry_settings` follow-up.
>
> Report `STATUS: ok` with a one-line summary of what changed.

---

## Sequencing notes

- **Task lib-06-01** (`AddPipeline::add`) is the simplest convenience
  function — two tasks, no provenance stamping divergence, the cleanest
  prove-out of the executor-route pattern. Lands first so subsequent
  tasks have a working template.
- **Task lib-06-02** (memify) is independent of 01 in code but runs after
  01 in the runbook so the implementor has a fresh executor-route example
  to mirror. Memify is also relatively simple — no provenance stamping
  divergence to verify.
- **Task lib-06-03** (cognify standard) is the riskiest: it has all six
  divergences listed in the parent doc. Lands after 01 + 02 so two
  simpler refactors have demonstrated the pattern works.
- **Task lib-06-04** (cognify temporal) is a smaller variant of 03 and
  reuses the same plumbing.
- **Task lib-06-05** (TODO cleanup) is a single-commit cleanup pass.
- **Task lib-06-06** (tests + closure summary) is the final gate: runs
  the full E2E suite, cross-SDK harness, and writes the closure summary.

---

## When the loop ends

After task 06 commits and sub-agent E updates the parent doc, run a final
smoke pass:

```bash
scripts/check_all.sh
bash scripts/run_tests_with_openai.sh
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit && cd -
```

If all pass, the gap is closed. Sub-agent E should have written the
"Closure summary" section into
[`docs/telemetry/lib-06-executor-routed-convenience.md`](../lib-06-executor-routed-convenience.md);
verify the commit list there matches the orchestrator's per-task commit
log.

After closure, gap-08 task 07
([`docs/telemetry/08/07-library-pipeline-wiring.md`](../08/07-library-pipeline-wiring.md))
is unblocked — its "Pre-conditions" reference LIB-06 closing.
