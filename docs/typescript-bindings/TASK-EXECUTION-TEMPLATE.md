# Task Execution Template — TypeScript bindings

← [Index](../typescript-bindings-plan.md)

A reusable prompt template for implementing **one phase/task** of the TypeScript-bindings plan
via four sub-agents that run **one by one** (each gates the next):

1. **Plan-correction** — validate/repair the plan; ask the user when blocked.
2. **Implementation** — implement to the plan; make it compile, format, and pass tests.
3. **Code-review** — review uncommitted changes for correctness, cleanliness, security, and
   goal-fit; apply fixes; re-run checks.
4. **Commit** — commit the changes with a meaningful message.

## How to use

1. Pick the task (a phase doc, e.g. `phase-1-handle-and-services.md`).
2. Copy the **Orchestrator prompt** below, replace the `{{...}}` placeholders, and send it.
3. The orchestrator runs the four sub-agents sequentially with the `Agent` tool, stopping if any
   step fails, and escalating plan-correction questions to you interactively.

Placeholders:
- `{{TASK_NAME}}` — e.g. `Phase 1 — Handle & service facade`
- `{{PHASE_DOC}}` — e.g. `docs/typescript-bindings/phase-1-handle-and-services.md`

---

## Orchestrator prompt (copy, fill placeholders, send)

> Implement **{{TASK_NAME}}** for the cognee-rust TypeScript bindings by running the four
> sub-agents below **in sequence** with the `Agent` tool (`subagent_type: general-purpose`, no
> worktree isolation). Treat each as a gate: **do not start a step until the previous one
> reports success.** If any step fails and cannot self-recover, stop and report to me with the
> failure detail. Pass each sub-agent the **Shared context** block verbatim plus its own
> instructions.
>
> Before starting, set this phase's row in `docs/typescript-bindings/STATUS.md` to 🟡 In progress
> (and to 🔵 In review while Sub-agent 3 runs). If you stop on failure, set it to ⛔ Blocked with a
> one-line note. Sub-agent 4 sets it to ✅ Done.
>
> - After **Sub-agent 1 (Plan-correction)**: if it returns blocking questions, ask me with
>   `AskUserQuestion`, then re-run Sub-agent 1 with my answers appended until it reports the plan
>   is ready. Only then proceed.
> - After **Sub-agent 2 (Implementation)**: proceed only if compilation, formatting, and tests
>   pass.
> - After **Sub-agent 3 (Code-review)**: proceed only if the review verdict is "approved" and all
>   checks still pass after any applied fixes.
> - **Sub-agent 4 (Commit)** runs last.
>
> Use the prompts defined in `docs/typescript-bindings/TASK-EXECUTION-TEMPLATE.md` for each
> sub-agent, with `{{TASK_NAME}}` and `{{PHASE_DOC}}` substituted.

---

## Shared context (inject into every sub-agent)

```
Project: cognee-rust — Rust port of the Python cognee AI-memory pipeline.
Task: {{TASK_NAME}}
Plan doc (implementation detail): {{PHASE_DOC}}
Rationale / overall plan + sequence: docs/typescript-bindings-plan.md
Conventions: .claude/CLAUDE.md (project) and ~/.claude/CLAUDE.md (global).

Hard rules:
- No .unwrap() in non-test code — use expect("reason it cannot fail") or ? / proper error
  propagation. (Mutex/RwLock lock().unwrap() is allowed with a "// lock poison is unrecoverable"
  note.)
- thiserror for library error enums; anyhow in binaries/examples.
- Prefer dyn Trait (&dyn / Arc<dyn>) at call sites; async-first; Arc for shared ownership.
- Match surrounding code style; keep changes scoped to the task.

The TS bindings crate (js/cognee-neon) is currently a STANDALONE crate (its own [workspace]),
so workspace-wide `cargo` commands do NOT cover it. Build/check it from its own directory.

Verification commands (debug mode — never add --release unless asked):
- Rust workspace compile:        cargo check --all-targets
- Rust workspace tests:          cargo test
- TS-binding build + tests:      bash js/scripts/check.sh   (node check → npm install → npm run
                                 build [Neon + tsc] → npm test [jest])
- Full repo check suite:         scripts/check_all.sh  (fmt --check → check → clippy -D warnings
                                 → capi/python/js binding checks)
- CI equivalent for JS:          the `js-check` job in .github/workflows/ci.yml runs
                                 js/scripts/check.sh; it has NO LLM/embedding setup, so only
                                 deterministic (Tier-A) tests run there. LLM-gated (Tier-B)
                                 tests must skip cleanly when OPENAI_*/model env is absent.
```

---

## Sub-agent 1 — Plan-correction

`subagent_type: general-purpose` · no worktree.

```
{{SHARED_CONTEXT}}

You are the PLAN-CORRECTION agent. Do NOT write implementation code.

1. Read the plan doc ({{PHASE_DOC}}) and the rationale (docs/typescript-bindings-plan.md:
   sections on the service facade, parity checklist, and sequence plan).
2. Verify the plan against the CURRENT codebase: do the referenced types, traits, modules,
   builders, and cognee-lib API functions still exist with the shapes the plan assumes? Does the
   plan, if followed, actually achieve the task's stated goal and exit criteria? Are the listed
   dependencies/prerequisites still accurate?
3. If the plan needs corrections (stale references, missing steps, wrong assumptions, an order
   that won't compile), EDIT {{PHASE_DOC}} to fix it. Keep the doc's structure
   (Scope/Structures/Functionalities/Dependencies/Risks/Done-when). Note what you changed.
4. If there are decisions only the user can make (genuine forks that change what gets built —
   e.g. package rename, a backend default, an API shape with no obvious default), do NOT guess.
   Return them as a clearly delimited "BLOCKING QUESTIONS" list with 2–4 concrete options each.

Output exactly one of:
- "PLAN READY" + a short summary of any corrections you applied; or
- "BLOCKING QUESTIONS:" followed by the numbered questions (with options) that must be answered
  before implementation can start.
Do not begin implementation under any circumstances.
```

The orchestrator, on "BLOCKING QUESTIONS", asks the user via `AskUserQuestion`, then re-invokes
this agent with `User answers: ...` appended, looping until it returns "PLAN READY".

---

## Sub-agent 2 — Implementation

`subagent_type: general-purpose` · no worktree.

```
{{SHARED_CONTEXT}}

You are the IMPLEMENTATION agent. The plan ({{PHASE_DOC}}) has been validated — implement the
task to it.

1. Implement exactly the task's scope — no more, no less. Follow the plan's structures
   (modules, types, functions) and the hard rules above. Stay consistent with the canonical
   call pattern and marshalling approach described in the plan/index.
2. Make it build and pass checks (debug mode):
   - For Rust workspace changes: `cargo check --all-targets`, then `cargo test`.
   - For js/cognee-neon (standalone) changes: `bash js/scripts/check.sh` (builds the Neon addon
     + TS and runs jest). Add any new deterministic (Tier-A) tests the plan calls for; gate
     LLM-dependent assertions so they skip without OPENAI_*/model env.
   - Run `cargo fmt` (and `tsc`/the jest run via check.sh) so formatting is clean.
3. Fix compile errors, failing tests, and clippy/format issues you introduced. Do NOT weaken or
   delete unrelated tests to make things pass.

Report: a concise summary of files changed and why, plus the exact commands you ran and their
pass/fail results. If something genuinely cannot pass (e.g. an external dependency/env gap),
state it explicitly rather than masking it.
```

---

## Sub-agent 3 — Code-review

`subagent_type: general-purpose` · no worktree.

```
{{SHARED_CONTEXT}}

You are the CODE-REVIEW agent. Review the UNCOMMITTED working-tree changes (`git diff` and
`git status`; include untracked files).

Check:
- Correctness — does the implementation do what the task goal states, and match the plan?
- Cleanliness — naming, structure, no dead code, no leftover debugging, matches surrounding
  style; no .unwrap() in non-test code; errors via thiserror/?; no needless allocations.
- Security — input validation at the JS↔Rust boundary, no secrets logged/echoed (respect the
  `redact` util), safe handling of buffers/paths, no unsound `unsafe`.
- Tests & checks — adequate Tier-A coverage of the main use-cases; LLM-gated tests skip cleanly;
  re-run `cargo check --all-targets` + `cargo test` and/or `bash js/scripts/check.sh` as relevant
  and confirm green.

Apply fixes directly for clear issues (no worktree isolation needed); re-run the checks after
fixing. For anything ambiguous or out-of-scope, list it as a follow-up rather than changing it.

Output: a verdict line — "REVIEW: approved" or "REVIEW: changes-needed" — then the findings, the
fixes you applied, and the final check results. Approve only when the code is correct, clean,
secure, matches the goal, and all checks pass.
```

---

## Sub-agent 4 — Commit

`subagent_type: general-purpose` · no worktree.

```
{{SHARED_CONTEXT}}

You are the COMMIT agent. Commit the reviewed, passing changes.

1. If the current branch is the default branch (main), create and switch to a feature branch
   first: `ts-bindings/<short-task-slug>` (e.g. ts-bindings/phase-1-handle-facade). Do NOT
   commit directly to main.
2. Stage the task's changes (`git add` the relevant files; do not sweep in unrelated edits).
3. Commit with a meaningful message: a concise summary line scoped to the task
   (e.g. "js: add CogneeHandle + CogneeServices facade (TS bindings phase 1)"), a short body
   explaining what and why, and ending with exactly:
   Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
4. Update `docs/typescript-bindings/STATUS.md`: set this phase's row to ✅ Done with the branch
   and short commit hash, tick the exit-criteria checkboxes that are now satisfied, bump
   "Last updated", and add any cross-cutting decision to the decision log. Include this STATUS.md
   edit in the same commit.
5. Do NOT push unless explicitly asked. Report the branch name and the commit hash + message.
```

---

## Notes

- **Why sequential, no worktree:** each step builds on the previous tree state; isolation would
  fragment it. Only parallelize across *independent* tasks, not within one.
- **Interactivity lives in the orchestrator.** Sub-agents run autonomously and return a final
  message; the plan-correction agent surfaces questions, and the orchestrator (main loop) asks
  the user with `AskUserQuestion` and re-invokes it with the answers.
- **Stop-on-failure.** If implementation or review cannot reach green, the orchestrator halts and
  reports — it never commits red code.
- **Scope discipline.** Every agent is told to stay within the task's scope; cross-phase changes
  belong to their own task run.
