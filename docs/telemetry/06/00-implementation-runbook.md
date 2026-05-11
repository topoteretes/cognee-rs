# Telemetry Gap 06 — File-Based Logging with Rotation Implementation Runbook

## Purpose

This document is a self-contained **prompt** for executing the eleven
gap-06 implementation tasks defined in [`docs/telemetry/06/`](.)
sequentially with a fixed five-sub-agent workflow per task. Pasting
this document (or pointing a fresh Claude Code session at it) drives
the gap to completion without burning the main session's context
window on per-task investigation, code reading, or test output.

It mirrors the gap-05 runbook
([`docs/telemetry/05/00-implementation-runbook.md`](../05/00-implementation-runbook.md))
verbatim in shape — only the source-of-truth references and the
binding decisions table change.

## How to use

Open a fresh Claude Code session in `/home/dmytro/dev/cognee/cognee-rust`
and ask: *"Follow `docs/telemetry/06/00-implementation-runbook.md`."*
The orchestrator instructions begin at [Orchestrator
prompt](#orchestrator-prompt).

---

## Source-of-truth documents

The orchestrator MUST treat these as the binding contract:

- **Design decisions (locked)**:
  [`docs/telemetry/06-file-logging-rotation.md`](../06-file-logging-rotation.md)
  → "Design decisions (locked)" table. Fourteen numbered decisions
  pre-approved by the project owner on 2026-05-11. **Do not
  re-litigate them.** The most load-bearing entries:
  - **Decision 1** — Time-based rotation (`Rotation::DAILY`) for v1
    via `tracing-appender`. Size-based parity deferred.
  - **Decision 2** — New `crates/logging/` workspace crate; library
    crates must not depend on it.
  - **Decision 4** — Custom Python-byte-exact `FormatEvent` for the
    `plain` format.
  - **Decision 5** — Replicate Python's `LOG_FILE_NAME` multi-process
    inheritance; document the rotation race.
  - **Decision 6** — Broad library-noise suppression as the default
    filter.
  - **Decision 7** — `LOG_LEVEL` is a fallback for `RUST_LOG`.
  - **Decision 8** — Env-var-only surface; no new CLI flags.
  - **Decision 9** — Python + JS + C bindings all expose
    `setup_logging()`.
  - **Decision 11** — Cleanup is startup-only.
  - **Decision 12** — Cross-SDK parity is per-message-strict,
    per-filename-loose.
  - **Decision 13** — `SpanBufferLayer` stays independent.

- **Per-task implementation plans**:
  [`01-workspace-deps.md`](01-workspace-deps.md),
  [`02-logging-config.md`](02-logging-config.md),
  [`03-paths-and-cleanup.md`](03-paths-and-cleanup.md),
  [`04-python-plain-formatter.md`](04-python-plain-formatter.md),
  [`05-init-logging.md`](05-init-logging.md),
  [`06-cli-refactor.md`](06-cli-refactor.md),
  [`07-http-server-refactor.md`](07-http-server-refactor.md),
  [`08-binding-entrypoints.md`](08-binding-entrypoints.md),
  [`09-android-wiring.md`](09-android-wiring.md),
  [`10-tests.md`](10-tests.md),
  [`11-docs-and-ci.md`](11-docs-and-ci.md).

- **Gap parent**:
  [`docs/telemetry/06-file-logging-rotation.md`](../06-file-logging-rotation.md)
  → "Action items" + "Design decisions (locked)" tables.

- **Root gap analysis**:
  [`docs/telemetry/gap-analysis.md`](../gap-analysis.md).

- **Prior art**:
  - [`docs/telemetry/05-datapoint-provenance.md`](../05-datapoint-provenance.md)
    + [`docs/telemetry/05/`](../05/) — closed gap; this runbook's
    five-sub-agent shape and locked-decisions discipline are lifted
    verbatim from there.
  - The current binary subscribers at
    [`crates/cli/src/main.rs`](../../../crates/cli/src/main.rs) and
    [`crates/http-server/src/main.rs`](../../../crates/http-server/src/main.rs)
    define the integration shape gap 06 must preserve (telemetry
    layer composition, `SpanBufferLayer` composition).

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
  scope is `telemetry/logging` (e.g.
  `telemetry/logging-06-03: add resolve_logs_dir and cleanup_old_logs`).
  Always include the
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
  trailer via heredoc.
- Never `git push`, never amend, never use `--no-verify`.
- Ask the user before any destructive or shared-state action.
- Tests must not depend on a specific `RUST_LOG` value — install any
  test subscriber explicitly per the relevant test helper. Tests that
  mutate process env vars must be marked `#[serial_test::serial]` (or
  equivalent) because env is process-global state and parallel tests
  will interfere.

---

## Orchestrator prompt

> You are the orchestrator for telemetry gap 06 (file-based logging
> with rotation). Your job is to drive the eleven tasks `06-01` …
> `06-11` through to a clean, committed, documented state, **one at a
> time, in order, with no parallelism between tasks**.
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
>   `docs/telemetry/06-file-logging-rotation.md` "Design decisions
>   (locked)" without explicit user approval. Sub-agent A may surface
>   that a decision needs revisiting; if so, escalate.
> - **Never `git push`, never amend a commit, never `--no-verify`.**
> - **Never `git reset --hard`, `git clean -f`, `git checkout --` to
>   "fix" a problem.** If sub-agent C cannot reconcile the diff with
>   the task plan, escalate.
> - **Stop the loop on the first unrecoverable error.** Tasks build on
>   each other (especially 02 → 03/04 → 05 → 06/07/08); pushing
>   through a broken task corrupts the chain.
> - **Run `scripts/check_all.sh` exactly once per task** (inside
>   sub-agent C). Don't re-run from the orchestrator.
> - **Never make outbound HTTP requests in tests.** Mock or skip when
>   the network is required.
> - **Confirm before any action with shared-state effects** beyond the
>   working tree.
> - **Env-mutating tests must be serialized.** Use
>   `#[serial_test::serial]` (already a workspace dev-dep) for any
>   test that sets `COGNEE_LOG_*`, `LOG_FILE_NAME`, `LOG_LEVEL`, or
>   `RUST_LOG`. Two parallel env-mutating tests in the same process
>   will flake.
>
> ### Scope guard
>
> Gap 06 covers file-based logging + rotation only. The orchestrator
> must **resist scope creep**:
>
> - **Do not implement size-based rotation.** Decision 1 deferred
>   this; if a sub-doc asks for it, escalate.
> - **Do not add CLI flags** (`--log-level`, `--log-format`,
>   `--log-file`). Decision 8 locked env-var-only.
> - **Do not install a library-level subscriber.** Library crates
>   (anything under `crates/` except `cli`, `http-server`, `logging`
>   itself) must not depend on `cognee-logging`. If task 06-08's
>   binding entrypoints need helpers, they call into `cognee-logging`
>   directly — they do not call through library crates.
> - **Do not refactor OTEL/telemetry composition.** Gap 04
>   instrumentation and gap 03's pipeline events are stable; the file
>   layer must compose alongside them via `extra_layers`, never
>   replace them.
> - **Do not change the `SpanBuffer` ring-buffer semantics.**
>   Decision 13 locked the layer as independent; mirror nothing into
>   the file sink.
> - **Do not modify pipeline executor code** to add per-task log
>   files. Logging stays subscriber-side.

## Sub-agent prompt templates

### Sub-agent A — Task review

> You are reviewing
> `docs/telemetry/06/{TASK_FILE}.md` for task `06-{NN}`. Read it
> end-to-end together with the parent
> [`docs/telemetry/06-file-logging-rotation.md`](../06-file-logging-rotation.md)
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

> Implement task `06-{NN}` by following
> `docs/telemetry/06/{TASK_FILE}.md` "Step-by-step" exactly. Constraints:
>
> - Honour the locked decisions table in
>   [`docs/telemetry/06-file-logging-rotation.md`](../06-file-logging-rotation.md).
> - Honour project conventions (no `unwrap()` outside tests; tests run
>   in debug; lock-poison `unwrap` on `Mutex`/`RwLock` is acceptable
>   with a `// lock poison is unrecoverable` comment).
> - Reuse `cognee_logging::*` once task 06-02 lands. Do not re-derive
>   env-var parsing, path resolution, or formatter logic in binaries or
>   bindings.
> - Do not modify files outside the "Files modified" list in the
>   sub-doc, except trivial fixups required to compile.
> - Do not commit. Sub-agent D commits.
>
> When done, report `STATUS: ok` with a list of files modified and a
> one-paragraph diff summary. If you hit an unrecoverable issue,
> report `STATUS: failed` with the diagnostic.

### Sub-agent C — Change reviewer

> Review the working-tree diff for task `06-{NN}` against
> `docs/telemetry/06/{TASK_FILE}.md`. Run, in this order:
>
> 1. `cargo fmt --check`
> 2. `cargo check --all-targets`
> 3. `cargo check --all-targets --no-default-features` (sanity — gap
>    06's `cognee-logging` crate must build standalone, and the OFF
>    path for `cli` / `http-server` `telemetry` feature must keep
>    compiling.)
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

> Stage and commit the working-tree changes for task `06-{NN}`.
>
> 1. `git status` to confirm only intended files changed.
> 2. `git add <files>` (explicit list — never `-A`).
> 3. `git commit -m "$(cat <<'EOF'\ntelemetry/logging-06-{NN}: <subject from sub-doc>\n\n<one-paragraph body>\n\nCo-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>\nEOF\n)"`.
> 4. `git status` to confirm clean tree.
> 5. Print the new SHA.
>
> Never amend, never push, never use `--no-verify`. If the pre-commit
> hook fails, fix the underlying issue, re-stage, create a NEW commit.

### Sub-agent E — Document updater

> Update `docs/telemetry/06-file-logging-rotation.md` "Action
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
>    order — same format as the gap-05 closure summary.
>
> Report `STATUS: ok` with a one-line summary of what changed.

---

## Sequencing notes

- **Tasks 06-01** (workspace deps) is foundation work and unblocks
  everything else. It must land first so subsequent tasks can `use
  tracing_appender` without breaking the build.
- **Tasks 06-02, 06-03, 06-04** are pure library work inside
  `crates/logging/`. They can be authored in any order between
  themselves once 06-01 lands, but the runbook drives them in
  numeric order (02 first since 03/04 reference its types).
- **Task 06-05** (init helper) depends on 02 + 03 + 04. This is
  the integration point where layers compose.
- **Task 06-06** (CLI refactor) depends on 06-05.
- **Task 06-07** (HTTP server refactor) depends on 06-05. Can run
  in parallel with 06-06 in principle, but the runbook keeps strict
  numeric order to avoid interleaved commits in the CLI/server
  binary trees.
- **Task 06-08** (binding entrypoints) depends on 06-05. Each
  binding is independent; the task lands all three in one commit.
- **Task 06-09** (Android wiring) depends only on the env-var
  surface (06-02) but conceptually slots after the binaries are
  refactored so the end-to-end demo path uses the new helper. The
  runbook keeps numeric order.
- **Task 06-10** (tests) depends on every preceding implementation
  task. Cross-SDK parity test additionally depends on 06-04 (Python
  format parity) and 06-08 (binding entrypoint Python uses to set
  up logging from the harness).
- **Task 06-11** (docs + CI) lands last.

---

## When the loop ends

After task 11 commits and sub-agent E updates the parent doc, run a
final smoke pass:

```bash
scripts/check_all.sh
cargo test -p cognee-logging
cargo test -p cognee-cli --test logging_e2e
cargo test -p cognee-http-server --test logging_e2e
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit
```

If all four pass, the gap is closed. Sub-agent E should have written
the "Closure summary" section into
[`docs/telemetry/06-file-logging-rotation.md`](../06-file-logging-rotation.md);
verify the commit list there matches the orchestrator's per-task
commit log.
