# Implementation Prompts — COG-4457 Cognify Compatibility

Parent: [../cognify-compatibility-implementation-plan.md](../cognify-compatibility-implementation-plan.md)

This document holds a ready-to-run prompt per work item. Each prompt drives the
same **3-step scheme**:

1. **Implement** — a sub-agent re-validates the task doc against current code
   (the plan may be stale), fixes the doc if needed, then implements per the plan.
2. **Review** — a fresh sub-agent reviews the diff for correctness, security,
   consistency, and fidelity to the task doc; runs formatting/build/clippy/tests;
   fixes anything it finds.
3. **Land** — flip the task status to ✅ Implemented (in the sub-doc header **and**
   the root index table) and commit with a meaningful, task-matching message.

## How to use

Run the tasks in the dependency order from the root plan
([§4 Recommended sequencing](../cognify-compatibility-implementation-plan.md)):
**1 → 2 → 4 → 5 → 3**. Items 1 and 2 are best landed together. Paste the chosen
task's prompt to the orchestrator; it will spawn the sub-agents for steps 1–2 and
perform step 3 itself.

### Conventions every step must honor

- **Build/test policy** (per repo CLAUDE.md): `cargo check --all-targets` for
  compilation; run tests in **debug** mode (no `--release`); finish with
  `scripts/check_all.sh` (fmt, check, clippy `-D warnings`, and the C/Python/JS
  binding checks).
- **No `unwrap()` in non-test code** — use `expect("why it cannot fail")` or
  proper `?`/error propagation. `Mutex/RwLock` lock `unwrap()` is allowed with a
  `// lock poison is unrecoverable` comment.
- **Python parity** — the Python reference is the source of truth. If absent,
  clone it: `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`.
- **Feature propagation** — a new non-platform feature must be added to the
  `default` lists of both `cognee-lib` and `cognee-cli`.
- **Commit message footer** — end the commit body with:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Branch** — work on `feature/cog-4457-fully-compatible-cognify-operation`
  (the ticket branch); if on `main`, branch first. Do not push unless asked.
- **Status flip (step 3)** — set `Status: ✅ Implemented (<short-sha>)` in the
  sub-doc header line and change the matching row's Status cell in the root index
  table from `📋 Planned` to `✅ Implemented`.

---

## Reusable step templates

The per-task sections below fill in `{{TASK_DOC}}`, `{{TASK_TITLE}}`, and
`{{COMMIT_SCOPE}}`. The three steps are identical in shape:

### Step 1 prompt (Implementation sub-agent — general-purpose)

> You are implementing **{{TASK_TITLE}}** in the cognee-rust repo
> (`/home/dmytro/dev/cognee/cognee-rust`). The task spec is `{{TASK_DOC}}`.
>
> 1. **Re-validate the spec against the current code first.** The plan was written
>    earlier and may be stale. For every file/line/symbol the doc references,
>    open it and confirm it still exists and says what the doc claims. Verify the
>    Python reference too (clone to `/tmp/cognee-python` if missing). If anything
>    is wrong (moved code, changed signatures, an already-done sub-step, a
>    mismatched Python reference), **fix the task doc** to match reality before
>    coding, and note what you changed.
> 2. **Implement the task** exactly per the (corrected) spec. Follow the repo
>    conventions: no `unwrap()` in non-test code, `thiserror` in libs, async via
>    tokio, `dyn Trait` at call sites, feature propagation to `cognee-lib` /
>    `cognee-cli` defaults for any new non-platform feature. Add/extend unit and
>    integration tests as the spec's acceptance criteria require.
> 3. **Verify locally**: `cargo check --all-targets` must pass; run the relevant
>    tests in debug mode (no `--release`). Do **not** commit — leave the working
>    tree dirty for review.
> 4. Report: what you changed in the doc (if anything), the files you touched, the
>    commands you ran with their results, and any acceptance criterion you could
>    not satisfy (e.g. needs a live Postgres / LLM / model) and why.

### Step 2 prompt (Review sub-agent — general-purpose, fresh context)

> You are reviewing an **uncommitted** change implementing **{{TASK_TITLE}}** in
> `/home/dmytro/dev/cognee/cognee-rust`. The spec is `{{TASK_DOC}}`; the diff is in
> the working tree (`git diff` / `git status`).
>
> Review for:
> - **Correctness** — does the code do what the spec describes? Trace the logic;
>   check edge cases, error paths, and that it matches the Python reference where
>   parity is claimed.
> - **Spec fidelity** — does the implemented state match the task doc, including
>   its acceptance criteria? If the code is right but the doc is now inaccurate,
>   fix the doc.
> - **Security** — input validation, SQL/identifier injection (esp. Postgres
>   table/collection names), credential handling/redaction, no secrets logged.
> - **Consistency** — naming, error types (`thiserror`), `dyn Trait` usage, the
>   `no unwrap() in non-test code` rule, feature-flag propagation.
> - **Checks** — run `scripts/check_all.sh` (fmt, `cargo check --all-targets`,
>   clippy `-D warnings`, binding checks) and the task's tests in debug mode.
>   Capture the actual output.
>
> **Fix** anything you find (code, tests, or doc) directly. Do not commit. Report
> a verdict (approve / changes-made), the issues found, the fixes applied, and the
> final `scripts/check_all.sh` result. If a check genuinely cannot run in this
> environment (no live Postgres/LLM/model), say so explicitly rather than
> claiming it passed.

### Step 3 (orchestrator — status flip + commit)

After the review sub-agent approves and `scripts/check_all.sh` is green:

1. Set `Status: ✅ Implemented (<short-sha-after-commit>)` in `{{TASK_DOC}}` and
   flip the matching Status cell in
   [../cognify-compatibility-implementation-plan.md](../cognify-compatibility-implementation-plan.md) to `✅ Implemented`.
   (Stage the doc edits in the same commit; backfill the SHA via `--amend` or note
   it in the row — pick one and be consistent across tasks.)
2. Commit on the ticket branch with a message matching the task, e.g.
   `{{COMMIT_SCOPE}}`. Include a short body summarizing what changed and any
   deferred acceptance criteria. End with the `Co-Authored-By` footer. Do **not**
   push unless asked.

---

## Task 1 — Wire `PgGraphAdapter` into `ComponentManager`

- `{{TASK_DOC}}` = [01-wire-pggraph-component-manager.md](01-wire-pggraph-component-manager.md)
- `{{TASK_TITLE}}` = "wire PgGraphAdapter into ComponentManager (enable the postgres graph provider at runtime)"
- `{{COMMIT_SCOPE}}` = `feat(lib): wire PgGraphAdapter into ComponentManager for postgres graph provider`

Run **Step 1** (implementation prompt above with these fills) → **Step 2** (review
prompt) → **Step 3** (status flip + commit). Blocking item — do this first.

## Task 2 — Graph → relational credential fallback (+ `graph_database_host`)

- `{{TASK_DOC}}` = [02-postgres-graph-credential-fallback.md](02-postgres-graph-credential-fallback.md)
- `{{TASK_TITLE}}` = "add graph_database_host + resolved_graph_db_url() fallback to relational DB creds (Python get_graph_engine parity)"
- `{{COMMIT_SCOPE}}` = `feat(lib): fall back to relational DB creds for postgres graph; add graph_database_host`

Best landed together with Task 1 (or immediately after). Step 1 → Step 2 → Step 3.

## Task 4 — Custom summarization output schema

- `{{TASK_DOC}}` = [04-custom-summarization-schema.md](04-custom-summarization-schema.md)
- `{{TASK_TITLE}}` = "add configurable summarization output schema (summary_schema + set_summarization_model), Python summarization_model parity — NOT a per-stage LLM"
- `{{COMMIT_SCOPE}}` = `feat(cognify): configurable summarization output schema (set_summarization_model parity)`

Independent of the Postgres work. Step 1's re-validation is especially important
here: confirm the `Llm::create_structured_output_raw` dynamic path still exists and
that `summary_schema` is wired into `SummaryExtractor` (not left dead like
`graph_schema`). Step 1 → Step 2 → Step 3.

## Task 5 — Full PostgreSQL-stack E2E test

- `{{TASK_DOC}}` = [05-postgres-full-stack-e2e-test.md](05-postgres-full-stack-e2e-test.md)
- `{{TASK_TITLE}}` = "add gated full-Postgres-stack E2E test (relational + PgGraph + PgVector) via ComponentManager with real BGE embeddings"
- `{{COMMIT_SCOPE}}` = `test(cognify): full PostgreSQL-stack add→cognify E2E gated on TEST_POSTGRES_URL`

Depends on Tasks 1+2. The test must **skip cleanly** without `TEST_POSTGRES_URL` /
LLM / model. Note in step 1/step 2 reports whether the test was actually executed
against a live Postgres or only compiled + skipped. Step 1 → Step 2 → Step 3.

## Task 3 — Full `PgHybridAdapter` + unified-engine wiring (milestone)

- `{{TASK_DOC}}` = [03-pghybrid-full-adapter.md](03-pghybrid-full-adapter.md)
- `{{TASK_TITLE}}` = "implement PgHybridAdapter sharing one Postgres connection for graph+vector, with USE_UNIFIED_PROVIDER=pghybrid wiring"
- `{{COMMIT_SCOPE}}` = per-PR, e.g. `feat(hybrid): PgHybridAdapter skeleton (PR1)`, `feat(lib): USE_UNIFIED_PROVIDER=pghybrid wiring (PR2)`, …

**This task is a multi-PR milestone** — do **not** run it as a single Step
1/2/3 pass. Instead apply the 3-step scheme **once per PR** defined in the spec
(PR1 skeleton → PR2 wiring → PR3 combined write → PR4 combined search [optional] →
PR5 tests). For each PR: Step 1 (re-validate that PR's section + implement), Step 2
(review that PR's diff), Step 3 (commit the PR; flip the task Status to
✅ Implemented only after PR2–3 land, leaving PR4 as a tracked follow-up if
deferred). Resolve the **crate-dependency-cycle** question in PR1 before writing
code, as the spec calls out.

---

## Notes for the orchestrator

- Spawn step-1 and step-2 agents as **separate** sub-agents so the reviewer has
  fresh context and does not rubber-stamp its own work.
- If a step-1 agent reports it changed the task doc, make sure the step-2 reviewer
  is told to validate against the **updated** doc.
- Tasks 1, 2, 4, 5 are each one Step-1/2/3 cycle. Task 3 is five cycles.
- If `scripts/check_all.sh` cannot fully run (e.g. binding toolchains absent),
  record exactly which sub-checks ran and which were skipped — never report a
  skipped check as passed.
