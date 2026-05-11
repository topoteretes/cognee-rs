# Telemetry Gap 07 — Bindings auto-init Implementation Runbook

## Purpose

This document is a self-contained **prompt** for executing the eight
gap-07 implementation tasks defined in [`docs/telemetry/07/`](.)
sequentially with a fixed five-sub-agent workflow per task. Pasting
this document (or pointing a fresh Claude Code session at it) drives
the gap to completion without burning the main session's context
window on per-task investigation, code reading, or test output.

It mirrors the gap-06 runbook
([`docs/telemetry/06/00-implementation-runbook.md`](../06/00-implementation-runbook.md))
verbatim in shape — only the source-of-truth references and the
binding decisions table change.

## How to use

Open a fresh Claude Code session in `/home/dmytro/dev/cognee/cognee-rust`
and ask: *"Follow `docs/telemetry/07/00-implementation-runbook.md`."*
The orchestrator instructions begin at [Orchestrator
prompt](#orchestrator-prompt).

---

## Source-of-truth documents

The orchestrator MUST treat these as the binding contract:

- **Design decisions (locked)**:
  [`docs/telemetry/07-bindings-auto-init.md`](../07-bindings-auto-init.md)
  → "Design decisions (locked)" table. Thirteen numbered decisions
  pre-approved by the project owner on 2026-05-11. **Do not
  re-litigate them.** The most load-bearing entries:
  - **Decision 1** — Hybrid auto-init: minimal default subscriber
    (PyO3 → `pyo3-log`, Neon → stderr fmt) plus explicit
    `setup_logging()` / `setup_telemetry()` / `setup_telemetry_analytics()`
    for heavyweight work.
  - **Decision 2** — OTLP gets its own `setup_telemetry()` entrypoint
    per binding. `setup_logging` is not extended.
  - **Decision 3** — All three binding crates enable the `telemetry`
    cargo feature by default.
  - **Decision 6** — `cg_init` installs a one-shot panic hook.
  - **Decision 8** — Binding-specific `OTEL_SERVICE_NAME` defaults
    (`cognee.python-binding`, `cognee.node-binding`,
    `cognee.capi-binding`).
  - **Decision 10** — `COGNEE_HOST_SDK` sentinel is scoped to
    binding-armed emissions via a `BINDING_ARMED` guard in
    `cognee_telemetry::env`.
  - **Decision 11** — Per-binding analytics defaults: PyO3 OFF, Neon
    ON, C explicit-only.
  - **Decision 12** — Idempotent singleton pattern for all new
    entrypoints.
  - **Decision 13** — Cross-SDK no-double-emit test is skipped until
    a binding emits.

- **Per-task implementation plans**:
  [`01-workspace-deps.md`](01-workspace-deps.md),
  [`02-pyo3-bridge.md`](02-pyo3-bridge.md),
  [`03-neon-default-subscriber.md`](03-neon-default-subscriber.md),
  [`04-capi-panic-hook.md`](04-capi-panic-hook.md),
  [`05-binding-otlp-setup.md`](05-binding-otlp-setup.md),
  [`06-host-sdk-sentinel.md`](06-host-sdk-sentinel.md),
  [`07-tests.md`](07-tests.md),
  [`08-docs-and-ci.md`](08-docs-and-ci.md).

- **Gap parent**:
  [`docs/telemetry/07-bindings-auto-init.md`](../07-bindings-auto-init.md)
  → "Action items" + "Design decisions (locked)" tables.

- **Root gap analysis**:
  [`docs/telemetry/gap-analysis.md`](../gap-analysis.md).

- **Prior art**:
  - [`docs/telemetry/06-file-logging-rotation.md`](../06-file-logging-rotation.md)
    + [`docs/telemetry/06/`](../06/) — closed gap; this runbook's
    five-sub-agent shape and locked-decisions discipline are lifted
    verbatim from there. Gap 06 task 08 also already landed
    `setup_logging()` in all three bindings — sub-doc 05 sits next to
    that file rather than replacing it.
  - [`docs/telemetry/01-otel-otlp-export.md`](../01-otel-otlp-export.md)
    + [`docs/telemetry/01/`](../01/) — closed gap; defines
    `cognee_observability::init_telemetry` + `EnvSettingsView` which
    sub-doc 05 composes from each binding.
  - [`docs/telemetry/02-send-telemetry-analytics.md`](../02-send-telemetry-analytics.md)
    + [`docs/telemetry/02/`](../02/) — closed gap; defines
    `cognee_telemetry::env::is_disabled()` which sub-doc 06 extends.
  - The current binary subscribers at
    [`crates/cli/src/main.rs`](../../../crates/cli/src/main.rs) and
    [`crates/http-server/src/main.rs`](../../../crates/http-server/src/main.rs)
    define the integration shape gap 07 must preserve (telemetry
    layer composition alongside the existing `init_logging` /
    `SpanBufferLayer` stack).

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
  scope is `telemetry/bindings` (e.g.
  `telemetry/bindings-07-02: add pyo3-log bridge to _native module`).
  Always include the
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
  trailer via heredoc.
- Never `git push`, never amend, never use `--no-verify`.
- Ask the user before any destructive or shared-state action.
- Env-mutating tests must be marked `#[serial_test::serial]` (or
  equivalent). Python tests that mutate env should use pytest's
  `monkeypatch` to scope mutation.

---

## Orchestrator prompt

> You are the orchestrator for telemetry gap 07 (bindings auto-init
> for tracing & telemetry). Your job is to drive the eight tasks
> `07-01` … `07-08` through to a clean, committed, documented state,
> **one at a time, in order, with no parallelism between tasks**.
>
> ### Top-level loop
>
> For each task `N` from 01 to 08, in strict numeric order:
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
>   `docs/telemetry/07-bindings-auto-init.md` "Design decisions
>   (locked)" without explicit user approval. Sub-agent A may surface
>   that a decision needs revisiting; if so, escalate.
> - **Never `git push`, never amend a commit, never `--no-verify`.**
> - **Never `git reset --hard`, `git clean -f`, `git checkout --` to
>   "fix" a problem.** If sub-agent C cannot reconcile the diff with
>   the task plan, escalate.
> - **Stop the loop on the first unrecoverable error.** Tasks build on
>   each other (especially 01 → 02/03 → 05; 01 → 06); pushing
>   through a broken task corrupts the chain.
> - **Run `scripts/check_all.sh` exactly once per task** (inside
>   sub-agent C). Don't re-run from the orchestrator.
> - **Never make outbound HTTP requests in tests.** Mock or skip when
>   the network is required.
> - **Confirm before any action with shared-state effects** beyond the
>   working tree.
> - **Env-mutating tests must be serialized.** Use
>   `#[serial_test::serial]` for any Rust test that touches
>   `COGNEE_BINDING_SUPPRESS_LOGS`, `COGNEE_HOST_SDK`,
>   `COGNEE_RUST_TELEMETRY`, `OTEL_*`, `TELEMETRY_DISABLED`, or `ENV`.
>   Two parallel env-mutating tests in the same process will flake.
>
> ### Scope guard
>
> Gap 07 covers binding init for tracing + OTLP + analytics policy
> only. The orchestrator must **resist scope creep**:
>
> - **Do not extend `setup_logging()` to also wire OTLP.** Decision 2
>   keeps the two seams separate.
> - **Do not implement the JS `setLogger(callback)` bridge.**
>   Decision 7 deferred this.
> - **Do not add `shutdown_*()` companion functions.** Decision 12
>   keeps the singleton pattern install-only; drop happens at process
>   exit.
> - **Do not expose new pipeline/API surface from bindings.** Gap 07
>   is about init plumbing; surface expansion is a future gap. This
>   is why the cross-SDK no-double-emit test (decision 13) is
>   skipped — there is nothing in the binding that calls
>   `send_telemetry` yet.
> - **Do not touch Android scripts.** Decision 9 explicitly excluded
>   Android.
> - **Do not refactor the existing `setup_logging()` implementations**
>   from gap 06 task 08 unless the task plan calls it out (e.g. task
>   05 may need to reference the same `OnceLock` pattern but should
>   not modify the logging file).

## Sub-agent prompt templates

### Sub-agent A — Task review

> You are reviewing
> `docs/telemetry/07/{TASK_FILE}.md` for task `07-{NN}`. Read it
> end-to-end together with the parent
> [`docs/telemetry/07-bindings-auto-init.md`](../07-bindings-auto-init.md)
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

> Implement task `07-{NN}` by following
> `docs/telemetry/07/{TASK_FILE}.md` "Step-by-step" exactly. Constraints:
>
> - Honour the locked decisions table in
>   [`docs/telemetry/07-bindings-auto-init.md`](../07-bindings-auto-init.md).
> - Honour project conventions (no `unwrap()` outside tests; tests run
>   in debug; lock-poison `unwrap` on `Mutex`/`RwLock` is acceptable
>   with a `// lock poison is unrecoverable` comment).
> - Reuse `cognee_observability::*`, `cognee_telemetry::*`, and
>   `cognee_logging::*` rather than re-implementing init plumbing in
>   the bindings.
> - Do not modify files outside the "Files modified" list in the
>   sub-doc, except trivial fixups required to compile.
> - Do not commit. Sub-agent D commits.
>
> When done, report `STATUS: ok` with a list of files modified and a
> one-paragraph diff summary. If you hit an unrecoverable issue,
> report `STATUS: failed` with the diagnostic.

### Sub-agent C — Change reviewer

> Review the working-tree diff for task `07-{NN}` against
> `docs/telemetry/07/{TASK_FILE}.md`. Run, in this order:
>
> 1. `cargo fmt --check`
> 2. `cargo check --all-targets`
> 3. `cargo check --all-targets --no-default-features` for the binding
>    crates touched by the task (sanity — the binding's `telemetry`
>    feature must be optional, not load-bearing on compile).
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

> Stage and commit the working-tree changes for task `07-{NN}`.
>
> 1. `git status` to confirm only intended files changed.
> 2. `git add <files>` (explicit list — never `-A`).
> 3. `git commit -m "$(cat <<'EOF'\ntelemetry/bindings-07-{NN}: <subject from sub-doc>\n\n<one-paragraph body>\n\nCo-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>\nEOF\n)"`.
> 4. `git status` to confirm clean tree.
> 5. Print the new SHA.
>
> Never amend, never push, never use `--no-verify`. If the pre-commit
> hook fails, fix the underlying issue, re-stage, create a NEW commit.

### Sub-agent E — Document updater

> Update `docs/telemetry/07-bindings-auto-init.md` "Action
> items" table:
>
> 1. Flip the `Status` cell for row `{NN}` from `⬜` to `✅ <SHA>`.
> 2. If the implementation diverged from the original sub-doc plan
>    in any user-visible way, append a one-line note to the sub-doc
>    "Status" header (e.g.
>    `**Status**: implemented in commit <SHA> (note: also touched
>    python/scripts/check.sh — see commit body)`).
> 3. If this is task 08, also write the "Closure summary" section at
>    the bottom of the parent doc, listing every commit in landing
>    order — same format as the gap-05 / gap-06 closure summaries.
>
> Report `STATUS: ok` with a one-line summary of what changed.

---

## Sequencing notes

- **Task 07-01** (workspace + binding manifests) is foundation work
  and unblocks 07-02 and 07-05. It must land first so subsequent
  tasks can `use pyo3_log` / `cognee_observability::init_telemetry`
  from the binding crates without breaking the build.
- **Task 07-04** (C API panic hook) is independent — has no
  dependency on 07-01 and could in principle land first. The runbook
  keeps numeric order; a real PR may bundle it with 07-01.
- **Tasks 07-02, 07-03** are per-binding subscriber installs and can
  run in parallel in principle. The runbook drives them in numeric
  order to keep one binding's changes contained per commit.
- **Task 07-05** (OTLP per binding) depends on 07-01 (cargo feature
  enabled) and conceptually slots after 07-02/07-03 so the default
  subscribers are already in place when the OTLP layer composes on
  top.
- **Task 07-06** (analytics + sentinel) depends on 07-01 only — it
  edits `cognee-telemetry` directly. Keeping it after 07-05 in the
  runbook just preserves the "plumbing first, sentinel after" mental
  ordering.
- **Task 07-07** (tests) depends on every preceding implementation
  task. The cross-SDK no-double-emit harness lands skipped per
  decision 13.
- **Task 07-08** (docs + CI) lands last.

---

## When the loop ends

After task 08 commits and sub-agent E updates the parent doc, run a
final smoke pass:

```bash
scripts/check_all.sh
cargo test -p cognee-telemetry
bash capi/scripts/check.sh
bash python/scripts/check.sh
bash js/scripts/check.sh
```

If all pass, the gap is closed. Sub-agent E should have written the
"Closure summary" section into
[`docs/telemetry/07-bindings-auto-init.md`](../07-bindings-auto-init.md);
verify the commit list there matches the orchestrator's per-task
commit log.
