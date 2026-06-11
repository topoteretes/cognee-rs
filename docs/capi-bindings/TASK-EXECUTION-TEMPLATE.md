# Task Execution Template — C API bindings (Sonnet orchestrator)

← [Index](README.md) · [Status](STATUS.md)

A self-contained prompt for a **Sonnet** session to implement the entire C-API-bindings plan
(phases 0–8) with minimal main-session context usage. The orchestrator itself writes no code
and reads no source files — all heavy work happens in four sub-agents per step, each gating
the next:

1. **Plan-correction** — verify the phase doc against the current codebase; fix stale
   references; confirm the task is still needed; escalate genuine decision forks.
2. **Implementation** — implement the task exactly to the (corrected) plan; make all checks
   pass.
3. **Review & validation** — review the diff for correctness, cleanliness, security,
   goal-fit, and consistency with the existing code; re-run the checks.
4. **Commit & mark done** — update STATUS.md, commit with a message matching the task.

## How to use

1. Start a fresh session (Sonnet), working directory = the repo root
   (`cognee-rust/`).
2. Paste the **Master orchestrator prompt** below verbatim. No placeholders to fill — the
   phase list and all sub-agent prompts are in this file, and the orchestrator reads them
   from here.
3. The run is resumable: the orchestrator always derives the next step from
   `docs/capi-bindings/STATUS.md`, so a new session can pick up where the last one stopped.

---

## Master orchestrator prompt (copy verbatim into a fresh session)

```
You are the ORCHESTRATOR for implementing the cognee-rust C API bindings plan. You coordinate
sub-agents; you do NOT write code, edit files (except docs/capi-bindings/STATUS.md status
flips), or read source files yourself. Keep your own context small: read ONLY
docs/capi-bindings/TASK-EXECUTION-TEMPLATE.md (this file, once), docs/capi-bindings/STATUS.md,
and the final reports your sub-agents return. Never read crate sources, diffs, or headers
yourself — sub-agents do that.

THE PLAN: docs/capi-bindings/README.md (index, decisions D1–D12 + R1–R9 — all LOCKED),
docs/capi-bindings/STATUS.md (live progress), and one phase doc per step:

| Step | Phase doc | Branch slug |
|---|---|---|
| 0  | docs/capi-bindings/phase-0-scaffolding.md | capi-bindings/phase-0-scaffolding |
| 1a (Part A only: bindings-common crate + neon refactor) | docs/capi-bindings/phase-1-shared-facade-and-handle.md | capi-bindings/phase-1a-facade-hoist |
| 1b (Parts B+C: CgSdk handle + async plumbing + header) | docs/capi-bindings/phase-1-shared-facade-and-handle.md | capi-bindings/phase-1b-sdk-handle |
| 2  | docs/capi-bindings/phase-2-errors-async-json-conventions.md | capi-bindings/phase-2-conventions |
| 3  | docs/capi-bindings/phase-3-config.md | capi-bindings/phase-3-config |
| 4  | docs/capi-bindings/phase-4-core-ops.md | capi-bindings/phase-4-core-ops |
| 5  | docs/capi-bindings/phase-5-retrieval.md | capi-bindings/phase-5-retrieval |
| 6  | docs/capi-bindings/phase-6-remaining-sdk.md | capi-bindings/phase-6-remaining-sdk |
| 7  | docs/capi-bindings/phase-7-feature-gated.md | capi-bindings/phase-7-feature-gated |
| 8  | docs/capi-bindings/phase-8-header-examples-tests-ci.md | capi-bindings/phase-8-tests-ci |

EXECUTION ORDER: strictly the table order, one step at a time (run step 7 sequentially too,
even though the plan allows overlap — simplicity beats parallelism here). Phase 1 is split
into 1a and 1b on purpose (locked decision R9) — run the full 4-sub-agent cycle for each.

PROCEDURE — repeat until all steps are ✅ Done in STATUS.md:

A. Read docs/capi-bindings/STATUS.md. Find the first step in the table above whose STATUS row
   is not ✅ Done. (For phase 1, treat 1a as done when STATUS notes say "PR-1/1a done".) If a
   row is ⛔ Blocked, stop and report the blocker to the user instead of proceeding.
B. Set that step's STATUS row to 🟡 In progress (edit only the status cell + "Last updated").
C. Run Sub-agent 1 (PLAN-CORRECTION) using the prompt template from
   docs/capi-bindings/TASK-EXECUTION-TEMPLATE.md, substituting {{STEP}}, {{PHASE_DOC}},
   {{BRANCH}} from the table.
   - If it returns "BLOCKING QUESTIONS:", ask the user with AskUserQuestion (one question per
     fork, with the agent's options), then re-run Sub-agent 1 with "User answers: ..."
     appended. Loop until it returns "PLAN READY".
   - If it returns "TASK OBSOLETE" with justification, set the row to ⛔ Blocked with a
     one-line note and STOP — report to the user.
D. Run Sub-agent 2 (IMPLEMENTATION). Gate: its report must state that every required check
   command passed. If it reports failure, re-run it ONCE with the failure report appended as
   context. If it fails again, set ⛔ Blocked + note, STOP, report to the user.
E. Set the row to 🔵 In review. Run Sub-agent 3 (REVIEW & VALIDATION). Gate: verdict line must
   be "REVIEW: approved". If "REVIEW: changes-needed" with unresolved findings, send the
   findings back to a fresh Sub-agent 2 run (one retry), then re-run Sub-agent 3. If still not
   approved, set ⛔ Blocked + note, STOP, report to the user.
F. Run Sub-agent 4 (COMMIT & MARK DONE). Gate: it must report the commit hash. Then verify (by
   reading STATUS.md) that the row is ✅ Done; fix the row yourself if the agent missed it.
G. Report one line to the user ("Step N done: <commit subject> @ <hash>") and continue with
   the next step.

RULES:
- Sub-agents: use the Agent tool, subagent_type "general-purpose", no worktree isolation,
  sequential (never two sub-agents at once — each builds on the previous tree state).
- Always pass each sub-agent: the SHARED CONTEXT block from the template file + its own
  numbered instructions + the step substitutions. Pass them the text itself, not a pointer.
- Never commit red code; never skip a gate; never reorder steps.
- If anything unexpected happens that the procedure does not cover, stop and ask the user.
```

---

## Shared context (inject verbatim into every sub-agent, after substituting `{{…}}`)

```
Project: cognee-rust — Rust port of the Python cognee AI-memory pipeline (knowledge-graph
add → cognify → search). You are working on ONE step of the C-API-bindings plan.

Step: {{STEP}}
Phase doc (the task spec): {{PHASE_DOC}}
Plan index (architecture, locked decisions D1–D12 and R1–R9, parity table):
docs/capi-bindings/README.md
Progress + decision log: docs/capi-bindings/STATUS.md
Reference implementation (the parity target): js/cognee-neon/src/ (Rust) + js/src/ (TS types).
Conventions: .claude/CLAUDE.md (project) — read its "Coding Conventions" section.

LOCKED DECISIONS — do not reopen or deviate (full text in README.md §4 + STATUS decision log):
- D1: shared facade lives in crates/bindings-common (cognee-bindings-common).
- D3+D9: JSON payloads camelCase, byte-identical to js/src/types.ts; results are ALWAYS valid
  JSON documents (true/false, "quoted-string", null for void ops).
- D4+R1: SDK ops are async-only via CgSdkResultCallback; callback fires exactly once, ALWAYS
  deferred (never synchronously from the initiating call), from a runtime thread. Sync bridge =
  single-use CgSdkWaiter (R6).
- D5+R2: SDK error codes 11–18 map 1:1 to TS kinds; cg_sdk_* functions return ONLY SDK codes +
  CG_OK/NULL_POINTER/RUNTIME/UTF8 — never engine codes 2,4–9. Enum values are append-only.
- D6: default features mirror cognee-neon (full); slim build must keep compiling.
- D8: two public headers — capi/include/cognee.h (engine, do not touch its surface) and
  capi/include/cognee_sdk.h (SDK tier), both cbindgen-generated and committed; CG_API_VERSION_*
  defines + cg_api_version().
- D10: capi/ is (becoming) its own cargo workspace; crates/bindings-common stays in the root
  workspace, consumed by path.
- D11: new symbols use the cg_sdk_ prefix.
- R4: no cancellation for SDK ops in v1 (documented non-goal).

HARD RULES:
- No .unwrap() in non-test code — use expect("reason why this cannot fail at runtime") or
  ?/proper propagation. Mutex/RwLock .lock().unwrap() allowed with a
  "// lock poison is unrecoverable" comment.
- thiserror for library error enums; anyhow only in binaries/examples.
- All public traits Send + Sync; Arc<dyn Trait> for shared ownership; async-first (tokio).
- Match the surrounding code style; keep changes scoped to THIS step; never weaken or delete
  unrelated tests to make checks pass.
- Debug mode only: never add --release to cargo commands.

VERIFICATION COMMANDS (which to run depends on what the step touches — the phase doc says):
- Root workspace compile:   cargo check --all-targets        (run at repo root)
- Root workspace tests:     cargo test                       (only when root crates changed)
- capi build + examples:    bash capi/scripts/check.sh       (CMake build + run all examples
                            and smoke tests; this is the primary capi gate)
- capi workspace compile:   cargo check --all-targets        (run inside the capi workspace
                            once phase 0 extracted it; also with
                            --no-default-features --features sqlite,testing for the slim build)
- JS bindings (ONLY when js/cognee-neon or js/src changed, i.e. steps 1a and anything touching
  bindings-common APIs the neon crate uses): bash js/scripts/check.sh
- Full repo gate (run before any commit): scripts/check_all.sh
  (fmt --check → check → clippy -D warnings → capi check → python check → js check)
- Headers: cbindgen output for cognee.h / cognee_sdk.h is checked in — if your change adds or
  alters exported symbols, regenerate and commit the header(s); never hand-edit generated parts.
- Tier-B (live LLM) tests/examples must SKIP cleanly (exit 0 + "SKIP" message) when
  OPENAI_URL/OPENAI_TOKEN are absent. Do not attempt live LLM verification yourself; note it
  as "deferred to CI/manual" in your report if the phase doc lists it.
```

---

## Sub-agent 1 — Plan-correction

`subagent_type: general-purpose` · no worktree.

```
{{SHARED_CONTEXT}}

You are the PLAN-CORRECTION agent for step {{STEP}}. You do NOT write implementation code.

1. Read {{PHASE_DOC}} fully, plus README.md §3–§5 and the STATUS.md decision log.
2. Verify the phase doc against the CURRENT codebase (read the actual sources it references):
   a. Stale references — do every named file, module, type, function, feature flag, and
      js/cognee-neon source path still exist with the shapes the doc assumes? (Earlier steps
      may have moved code, e.g. into crates/bindings-common.)
   b. Goal fit — if the tasks are executed as written, are the phase's exit criteria actually
      met? List any missing step or wrong ordering (e.g. something that cannot compile in the
      given sequence).
   c. Actuality — is the task still needed, or was part of it already implemented (check git
      log for the capi-bindings/* branches and the current tree)? Mark already-done items.
   d. Prerequisites — are the declared prerequisite phases ✅ Done in STATUS.md?
3. EDIT {{PHASE_DOC}} to fix what you found (stale paths, renamed symbols, wrong assumptions,
   missing micro-steps). Keep its structure and the locked decisions intact. Record every
   edit you made.
4. Decisions D1–D12 and R1–R9 are LOCKED — never reopen them. Only if you hit a genuinely NEW
   fork (something the plan does not decide and that changes what gets built), do NOT guess:
   output it as a blocking question with 2–4 concrete options and a recommendation.

Output EXACTLY one of:
- "PLAN READY" + bullet list of corrections applied (or "no corrections needed");
- "BLOCKING QUESTIONS:" + numbered questions with options;
- "TASK OBSOLETE" + justification (only if the entire step is already implemented or no
  longer applicable).
Do not begin implementation under any circumstances.
```

---

## Sub-agent 2 — Implementation

`subagent_type: general-purpose` · no worktree.

```
{{SHARED_CONTEXT}}

You are the IMPLEMENTATION agent for step {{STEP}}. The phase doc has just been validated —
implement it exactly.

1. If the current branch is main, create and switch to {{BRANCH}} first. If {{BRANCH}} exists
   from a previous attempt, continue on it.
2. Implement the step's full scope per {{PHASE_DOC}} — no more, no less:
   - Mirror the reference neon module named in the doc (js/cognee-neon/src/sdk_*.rs) for call
     sequences and JSON shapes; the wire format must stay byte-identical to js/src/types.ts.
   - Follow the canonical call pattern from README.md §3 (parse JSON → facade →
     cognee-lib api → strict-JSON result via the deferred callback).
   - New extern "C" functions: null-check every pointer argument (use the existing null_check!
     pattern), document ownership in the doc comment, and add them to the cbindgen-generated
     header by regenerating it.
3. Add the tests/examples/smoke binaries the phase doc lists (Tier-A deterministic, using
   MOCK_EMBEDDING=true + tempdirs; Tier-B with the SKIP-without-credentials pattern) and wire
   them into capi/scripts/check.sh if the doc says so.
4. Make everything green, in this order: cargo fmt, then the verification commands relevant
   to what you touched (see SHARED CONTEXT), finishing with scripts/check_all.sh. Fix every
   failure you introduced. Do not weaken/delete unrelated tests, do not loosen clippy.
5. Update the phase doc's task checkboxes if it has any; do NOT touch STATUS.md (the commit
   agent does that), do NOT commit.

Report back: (a) files created/changed with one line each on why; (b) the exact check
commands you ran and their pass/fail results; (c) anything from the phase doc you could NOT
do, stated explicitly with the reason (never silently skip). Keep the report under ~40 lines.
```

---

## Sub-agent 3 — Review & validation

`subagent_type: general-purpose` · no worktree.

```
{{SHARED_CONTEXT}}

You are the REVIEW & VALIDATION agent for step {{STEP}}. Review the UNCOMMITTED working-tree
changes: git status + git diff (and read new untracked files in full).

Check, in this order:
1. Task completeness — open {{PHASE_DOC}} and verify every task and exit criterion this step
   claims is actually present in the diff (functions exported, tests added, header
   regenerated, docs updated). List anything claimed-but-missing.
2. Correctness — logic vs the reference neon implementation (same call sequence into the
   facade/cognee-lib, same JSON keys/shapes); FFI safety: every pointer null-checked, no
   use-after-free across the callback boundary, user_data passed with the established
   send-pointer pattern, callback fired exactly once and never synchronously (R1), error-code
   tiering respected (R2).
3. Consistency — naming (cg_sdk_*), _new/_destroy pairs, cg_string_destroy for returned
   strings, doc-comment style and ownership notes matching the existing capi modules; no
   logic in capi that belongs in cognee-bindings-common.
4. Cleanliness — no dead code, debug prints, commented-out blocks, or stray TODOs; no
   .unwrap() in non-test code; idiomatic error propagation; comments only where the code
   cannot speak.
5. Security — inputs from C validated before use (UTF-8, JSON shape), no secrets in logs or
   error strings (respect the redact util), paths handled safely, any unsafe blocks minimal
   and individually justified by a comment.
6. Checks — re-run the verification commands relevant to the diff, ALWAYS including
   scripts/check_all.sh at the end. All must pass.

Apply fixes directly for clear-cut issues and re-run the affected checks. For anything
ambiguous or out-of-scope, list it as a follow-up instead of changing code.

Output: first line EXACTLY "REVIEW: approved" or "REVIEW: changes-needed". Then: findings
(numbered, with file:line), fixes you applied, follow-ups deferred, and the final check
results. Approve ONLY if the task is fully implemented, the code is correct/clean/secure/
consistent, and every check passes.
```

---

## Sub-agent 4 — Commit & mark done

`subagent_type: general-purpose` · no worktree.

```
{{SHARED_CONTEXT}}

You are the COMMIT agent for step {{STEP}}. The changes are reviewed and green.

1. Confirm you are on {{BRANCH}} (create/switch if the implementor failed to — never commit
   to main).
2. Update docs/capi-bindings/STATUS.md:
   - set this step's row to ✅ Done with {{BRANCH}} and the short commit hash (fill the hash
     after committing via a quick amend, or commit STATUS.md in the same commit by writing
     "pending" then amending — your choice, but the final state must show the real hash);
   - for step 1a, do not flip the phase-1 row to ✅; instead add the note "PR-1/1a done:
     facade hoisted, neon green" — step 1b completes the row;
   - tick every exit-criteria checkbox this step satisfied;
   - add a one-line decision-log entry for any cross-cutting choice made during
     implementation/review (the sub-agent reports are passed to you below);
   - bump "Last updated".
3. Stage the step's changes plus STATUS.md and the phase doc edits (git add the specific
   paths; do not sweep in unrelated files — check git status first).
4. Commit with a message matching the task:
   - subject: "capi: <what landed> (C API bindings step {{STEP}})" — e.g.
     "capi: add CgSdk handle + CgSdkWaiter (C API bindings step 1b)";
   - body: 2–6 lines on what was built and why, referencing {{PHASE_DOC}};
   - end with the exact Co-Authored-By trailer your environment instructions specify for git
     commits.
5. Do NOT push and do NOT open a PR unless the user has asked.

Report: branch, short hash, subject line, and the STATUS.md row as it now reads.
```

---

## Phase-specific notes for the orchestrator (pass the relevant note to all 4 sub-agents of that step)

| Step | Note |
|---|---|
| 0 | After extraction, run `cargo check --all-targets` in BOTH workspaces. The root `Cargo.toml` TODO about capi must end up resolved. Record size/build-time baselines in the STATUS notes column. |
| 1a | Touches js/cognee-neon: `bash js/scripts/check.sh` must be fully green — this is the hard gate. No capi changes in this step beyond Cargo plumbing. |
| 1b | First exported `cg_sdk_*` symbols: the second cbindgen config for `cognee_sdk.h` and the version symbols land here. |
| 2 | Conventions step: deliverables are mostly helpers + header docs + negative-path smoke tests. The deferred-delivery guarantee (R1) needs an explicit smoke assertion. |
| 4–6 | Wire shapes: when in doubt, diff against the neon module named in the phase doc — it is the single source of truth, not memory. |
| 7 | Two build configurations must both be exercised (default + `--no-default-features --features sqlite,testing`). |
| 8 | CI edits (.github/workflows/capi-check.yml) cannot be verified locally — validate YAML syntactically, state "CI run pending" in the report, and note it in STATUS. Tier-B examples must build and SKIP locally without credentials. |

## Notes

- **Why sequential, no worktree:** each step builds on the previous tree state; the four
  sub-agents within a step share the working tree by design.
- **Context discipline:** the orchestrator's only memory between steps is STATUS.md plus
  one-line step summaries. If the session nears its limits, stop after the current step
  completes — a fresh session resumes from STATUS.md.
- **Interactivity lives in the orchestrator:** sub-agents never ask the user; blocking
  questions bubble up through Sub-agent 1's output and the orchestrator's AskUserQuestion.
- **Stop-on-failure:** one retry per gate (implementation, review), then ⛔ Blocked + report.
  Red code is never committed.
