# Telemetry Gap 04 — DB-Adapter Span Instrumentation Implementation Runbook

## Purpose

This document is a self-contained **prompt** for executing the eleven
gap-04 implementation tasks defined in [`docs/telemetry/04/`](.)
sequentially with a fixed five-sub-agent workflow per task. Pasting
this document (or pointing a fresh Claude Code session at it) drives
the gap to completion without burning the main session's context
window on per-task investigation, code reading, or test output.

It mirrors the gap-03 runbook
([`docs/telemetry/03/00-implementation-runbook.md`](../03/00-implementation-runbook.md))
verbatim in shape — only the source-of-truth references and the
binding decisions table change.

## How to use

Open a fresh Claude Code session in `/home/dmytro/dev/cognee/cognee-rust`
and ask: *"Follow `docs/telemetry/04/00-implementation-runbook.md`."*
The orchestrator instructions begin at [Orchestrator
prompt](#orchestrator-prompt).

---

## Source-of-truth documents

The orchestrator MUST treat these as the binding contract:

- **Design decisions (locked)**:
  [`docs/telemetry/04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md)
  → "Design decisions (locked)" table. Nine numbered decisions
  pre-approved by the project owner on 2026-05-07. **Do not
  re-litigate them.** The most load-bearing entries:
  - **Decision 1** — SeaORM is **ops-level only** (one span per public
    function in `crates/database/src/ops/*.rs`); per-query
    instrumentation is explicitly out of scope.
  - **Decision 2** — `cognee.db.query` is **omitted** for Qdrant and
    pgvector (matches Python LanceDB).
  - **Decision 3** — pgvector + pg_graph are **in scope** even though
    Python does not instrument them.
  - **Decision 4** — LiteRT lives in **its own task** (04-07).
  - **Decision 6** — tests use a custom `SpanCapture` `tracing::Layer`
    in `cognee-test-utils` (Approach B), not `tracing-test` /
    `logs_contain`.
  - **Decision 8** — adapter instrumentation is **unconditional** (no
    `telemetry` feature gate); the `telemetry` feature only gates
    analytics events from gap 02/03.
  - **Decision 9** — truncation order: `redact(query[..min(len, 500)])`,
    truncate **then** redact.

- **Per-task implementation plans**:
  [`01-redact-relocate.md`](01-redact-relocate.md),
  [`02-tracing-constants-dedupe.md`](02-tracing-constants-dedupe.md),
  [`03-span-capture-test-helper.md`](03-span-capture-test-helper.md),
  [`04-qdrant-instrumentation.md`](04-qdrant-instrumentation.md),
  [`05-ladybug-instrumentation.md`](05-ladybug-instrumentation.md),
  [`06-openai-llm-fields.md`](06-openai-llm-fields.md),
  [`07-litert-llm-fields.md`](07-litert-llm-fields.md),
  [`08-pg-adapters.md`](08-pg-adapters.md),
  [`09-seaorm-ops-instrumentation.md`](09-seaorm-ops-instrumentation.md),
  [`10-tests.md`](10-tests.md),
  [`11-docs-and-ci.md`](11-docs-and-ci.md).

- **Gap parent**:
  [`docs/telemetry/04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md)
  → "Action items" + "Design decisions (locked)" tables.

- **Root gap analysis**:
  [`docs/telemetry/gap-analysis.md`](../gap-analysis.md).

- **Prior art**:
  [`docs/telemetry/03-pipeline-task-api-events.md`](../03-pipeline-task-api-events.md)
  + [`docs/telemetry/03/`](../03/) — closed gap. Lifecycle event
  emitters from gap 03 are unrelated to span instrumentation; gap 04
  consumes the same `tracing` infrastructure but at a different layer.

## Project conventions to honour

From [`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) and
[`/home/dmytro/.claude/CLAUDE.md`](file:///home/dmytro/.claude/CLAUDE.md):

- Compilation check: `cargo check --all-targets`.
- Tests: debug mode (no `--release`).
- Full check before committing: `scripts/check_all.sh`.
- No `unwrap()` in non-test code. Use `expect("reason")` or `?`.
- Commit message convention: `<scope>: <subject>` — for this gap, scope
  is `telemetry/db-spans` (e.g.
  `telemetry/db-spans-04-04: instrument QdrantAdapter search/upsert/delete`).
  Always include the
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
  trailer via heredoc.
- Never `git push`, never amend, never use `--no-verify`.
- Ask the user before any destructive or shared-state action.
- Tests must not depend on a specific `RUST_LOG` value — install the
  test subscriber explicitly via the `SpanCapture` helper from task
  04-03.

---

## Orchestrator prompt

> You are the orchestrator for telemetry gap 04 (DB / vector / graph /
> LLM adapter span instrumentation). Your job is to drive the eleven
> tasks `04-01` … `04-11` through to a clean, committed, documented
> state, **one at a time, in order, with no parallelism between tasks**.
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
>   `docs/telemetry/04-db-adapter-instrumentation.md` "Design decisions
>   (locked)" without explicit user approval. Sub-agent A may surface
>   that a decision needs revisiting; if so, escalate.
> - **Never `git push`, never amend a commit, never `--no-verify`.**
> - **Never `git reset --hard`, `git clean -f`, `git checkout --` to
>   "fix" a problem.** If sub-agent C cannot reconcile the diff with
>   the task plan, escalate.
> - **Stop the loop on the first unrecoverable error.** Tasks build on
>   each other (especially 01 → 02 → 03/04/05); pushing through a
>   broken task corrupts the chain.
> - **Run `scripts/check_all.sh` exactly once per task** (inside
>   sub-agent C). Don't re-run from the orchestrator.
> - **Never make outbound HTTP requests in tests.** All tests must use
>   the `SpanCapture` helper from task 04-03 plus mock adapters.
> - **Confirm before any action with shared-state effects** beyond the
>   working tree.
>
> ### Scope guard
>
> Gap 04 covers vector/graph/relational/LLM adapter span instrumentation
> only. The orchestrator must **resist scope creep**:
>
> - **Do not extend span attribute coverage** beyond the constants
>   already declared in `cognee_utils::tracing_keys`. Adding new
>   semantic conventions belongs in a separate gap.
> - **Do not refactor adapter call paths** to make instrumentation
>   easier. The instrument-wrap pattern is intentionally non-invasive.
> - **Do not migrate existing `cognee_search::observability`
>   call sites** beyond what task 04-02 specifies — that task only
>   re-exports, it does not chase down every consumer.
> - **Do not auto-init tracing in bindings** (capi/python/js/android).
>   That is gap 07 ([`07-bindings-auto-init.md`](../07-bindings-auto-init.md)),
>   not gap 04.
> - **Do not touch the OTEL bridge / OTLP exporters** from gap 01. The
>   adapter spans flow through whichever subscriber is attached;
>   embedders pick OTLP if they want it.

## Sub-agent prompt templates

### Sub-agent A — Task review

> You are reviewing
> `docs/telemetry/04/{TASK_FILE}.md` for task `04-{NN}`. Read it
> end-to-end together with the parent
> [`docs/telemetry/04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md)
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

> Implement task `04-{NN}` by following
> `docs/telemetry/04/{TASK_FILE}.md` "Step-by-step" exactly. Constraints:
>
> - Honour the locked decisions table in
>   [`docs/telemetry/04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md).
> - Honour project conventions (no `unwrap()` outside tests; tests run
>   in debug; the `regex` workspace dep is already pinned at `"1"`).
> - Reuse `cognee_utils::redact::redact` from task 04-01 once it
>   exists. Do not re-derive the regex set in adapter crates.
> - Reuse `cognee_utils::tracing_keys::*` once task 04-02 lands. Do not
>   inline string literals like `"cognee.db.system"` in adapter sites.
> - Do not add a `telemetry` feature gate around span instrumentation
>   (locked decision 8).
> - Do not modify files outside the "Files modified" list in the
>   sub-doc, except trivial fixups required to compile.
> - Do not commit. Sub-agent D commits.
>
> When done, report `STATUS: ok` with a list of files modified and a
> one-paragraph diff summary. If you hit an unrecoverable issue,
> report `STATUS: failed` with the diagnostic.

### Sub-agent C — Change reviewer

> Review the working-tree diff for task `04-{NN}` against
> `docs/telemetry/04/{TASK_FILE}.md`. Run, in this order:
>
> 1. `cargo fmt --check`
> 2. `cargo check --all-targets`
> 3. `cargo check --all-targets --no-default-features` (sanity — gap
>    04 is feature-flag-agnostic per locked decision 8, but the OFF
>    path must keep building.)
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

> Stage and commit the working-tree changes for task `04-{NN}`.
>
> 1. `git status` to confirm only intended files changed.
> 2. `git add <files>` (explicit list — never `-A`).
> 3. `git commit -m "$(cat <<'EOF'\ntelemetry/db-spans-04-{NN}: <subject from sub-doc>\n\n<one-paragraph body>\n\nCo-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>\nEOF\n)"`.
> 4. `git status` to confirm clean tree.
> 5. Print the new SHA.
>
> Never amend, never push, never use `--no-verify`. If the pre-commit
> hook fails, fix the underlying issue, re-stage, create a NEW commit.

### Sub-agent E — Document updater

> Update `docs/telemetry/04-db-adapter-instrumentation.md` "Action
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
>    order — same format as the gap-03 closure summary.
>
> Report `STATUS: ok` with a one-line summary of what changed.

---

## Sequencing notes

- **Tasks 04-01, 04-02, 04-03** are foundation and must land first
  (in any order between themselves, but the runbook drives them
  sequentially). 04-04 / 04-05 / 04-06 / 04-07 / 04-08 / 04-09 all
  consume the relocated `redact()` helper and the deduplicated
  constants.
- **Task 04-10 (tests)** depends on 04-03 (`SpanCapture` helper) and
  on 04-04 / 04-05 / 04-06 / 04-07 / 04-08 / 04-09 — at least one of
  the adapters must be instrumented before the assertions can run, so
  04-10 lands last among instrumentation work.
- **Task 04-11 (docs + CI)** lands last.
- The PG adapters (04-08) and SeaORM ops (04-09) follow the same
  span-shape as 04-04 / 04-05 / 04-09 has different granularity
  (ops-level per locked decision 1).

---

## When the loop ends

After task 11 commits and sub-agent E updates the parent doc, run a
final smoke pass:

```bash
scripts/check_all.sh
cargo test -p cognee-vector --test qdrant_span_instrumentation
cargo test -p cognee-graph --test ladybug_span_instrumentation
cargo test -p cognee-llm --test openai_span_instrumentation
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit
```

If all four pass, the gap is closed. Sub-agent E should have written
the "Closure summary" section into
[`docs/telemetry/04-db-adapter-instrumentation.md`](../04-db-adapter-instrumentation.md);
verify the commit list there matches the orchestrator's per-task
commit log.
