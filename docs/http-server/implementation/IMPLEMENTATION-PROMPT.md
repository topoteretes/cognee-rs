# Implementation Driver Prompt — cognee-rust HTTP server port

> **Read this first.** This document is the entry-point prompt for the model executing the cognee-rust HTTP server port. It defines the task list, the per-task four-agent pipeline, the sub-agent prompts you will copy-paste, and the conventions for commits / verification / status tracking.

You are implementing the cognee-rust HTTP server in `crates/http-server/` (and a related core change in `crates/core/`) by working sequentially through 10 implementation tasks. The design docs in [`docs/http-server/`](..) own the **what** and **why**; the per-task implementation guides in this directory own the **how**. Your job is to drive the four-agent pipeline below for each task in order, never skipping.

## 1. Mission

Port the Python cognee FastAPI server ([`cognee/api/client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py)) to Rust so the Rust stack serves the same HTTP surface byte-for-byte. The strict-Python-parity rule applies (see [../plan.md §1](../plan.md#1-goal)). Two acknowledged divergences are documented in [../pipelines.md §1](../pipelines.md#1-goals--non-goals).

## 2. Task list

Execute these tasks in order. **Do not skip ahead, do not reorder, do not run two tasks in parallel.**

| # | Task | Doc | Why this position |
|---|---|---|---|
| 1 | **P3-prereq** — library refactor + `cognee_core::PipelineRunRegistry` | [p3-prereq-library-refactor.md](p3-prereq-library-refactor.md) | Background-pipeline machinery in `cognee-core` is a prerequisite for almost every HTTP endpoint. Lands first because the rest depends on the new types. |
| 2 | **P0** — http-server crate scaffold (empty API) | [p0-foundation.md](p0-foundation.md) | Creates `crates/http-server/` (library + standalone `cognee-http-server` binary) with `AppState`, `ApiError`, CORS, OpenAPI bootstrap, root `/`, health router. After this the server boots. |
| 3 | **P1** — authentication stack | [p1-auth.md](p1-auth.md) | Every later phase needs `AuthenticatedUser`. |
| 4 | **P2** — write path | [p2-write-path.md](p2-write-path.md) | `/add`, `/update`, `/datasets`, `/ontologies`, `/delete`, `/forget`. Multipart streaming. |
| 5 | **P3** — pipelines + WebSocket | [p3-pipelines-and-websocket.md](p3-pipelines-and-websocket.md) | Wires the registry into `/cognify`, `/memify`, `/remember`, `/improve`. |
| 6 | **P4** — read path | [p4-read-path.md](p4-read-path.md) | `/search`, `/recall`, `/llm`, `/visualize`. |
| 7 | **P5** — admin + RBAC | [p5-admin.md](p5-admin.md) | RBAC migration; `/permissions`, `/settings`, `/configuration`. Removes P2's permission stub. |
| 8 | **P6** — observability | [p6-observability.md](p6-observability.md) | `SpanBufferLayer`, `/activity`, `/sync`, `/checks`. |
| 9 | **P7** — advanced + email flows | [p7-advanced.md](p7-advanced.md) | `/notebooks` CRUD, `/responses` stub, SMTP `Mailer`. |
| 10 | **P8** — cross-SDK HTTP parity harness | [p8-e2e-parity.md](p8-e2e-parity.md) | Drops in last; depends on all earlier phases existing. |

The single source of truth for **status** is the table in [README.md](README.md). After every task the doc-update agent flips its row from `Draft` → `In Progress` → `Done`.

## 3. Per-task pipeline overview

For **each** task above, you run four sub-agents in sequence:

```
Step 1 (Investigation) ──► Step 2 (Implementation) ──► Step 3 (Review) ──► Step 4 (Doc Update)
        │                          │                            │                       │
        ▼                          ▼                            ▼                       ▼
   Updates docs               Commits work               Amends commit if          Updates status
   to actualize               with meaningful            review finds              tables across
   the task spec              message                    issues                    the doc tree
```

The agents are **sequential** — Step 2 reads what Step 1 produced; Step 3 reads what Step 2 committed; Step 4 reads what Step 3 settled. Never run them in parallel.

Below in §4–§7 are the four sub-agent prompts. Wherever you see `${TASK_DOC}` (e.g. `p0-foundation.md`) substitute the actual filename for the current task. Wherever you see `${PHASE_ID}` substitute the phase identifier (`P0`, `P1`, ..., `P3-prereq`).

## 4. Sub-agent: Investigation

**Purpose**: confirm the task is still applicable, that no parts have already been implemented (so we don't redo them), and that the docs accurately describe the current codebase state. Update docs at every level (root → phase → router → implementation guide) where reality has drifted.

Spawn this agent (use `subagent_type: general-purpose`):

```
You are the investigation agent for ${PHASE_ID} of the cognee-rust HTTP server port.

Working dir: /home/dmytro/dev/cognee/cognee-rust

Your task doc: docs/http-server/implementation/${TASK_DOC}

**Steps**:

1. Read the task doc end-to-end. Note every file path it claims will be created/modified, every function name it cites, every test file it mandates.

2. Read every doc the task doc references in §2. (architecture.md, auth.md, pipelines.md, etc., plus the per-router specs.) Note any anchors that do not resolve.

3. Read the actual current state of the cognee-rust codebase for everything the task doc names:
   - For every file path the task says will be NEW: confirm it does not already exist. If it does, read it and report what's already there.
   - For every cited function/struct/trait: grep to confirm it does (or does not) exist. Note any signature drift.
   - For every cited line range in existing files: open the file, confirm the citation still resolves to the right code.

4. Identify partial implementations. If a previous PR has already landed steps 1–3 of this task, your output must list:
   - Which steps in §4 of the task doc are already done.
   - Which steps remain.
   - Whether any "done" step has rotted (regressed) since it was implemented.

5. Update docs to actualize. Where you find drift, **edit the docs**:
   - Stale anchors → fix them.
   - Wrong file paths → correct them.
   - Function names that were renamed in the codebase → update citations.
   - Steps already completed → strike through with a note `(already landed in <commit>)`.
   - Cross-doc references that broke since the task doc was written → fix them up the chain (per-router → phase doc → plan.md).
   - Acceptance-criteria checkboxes that already pass → mark them.

6. Update the implementation README.md status table for the current task: `Draft` → `In Progress`.

7. Final report: one of these three verdicts:
   - **READY** — task is applicable, docs are now actualized, hand off to the implementation agent. Include a list of the §4 steps the implementation agent must execute (excluding any already-done ones).
   - **PARTIAL** — a slice of the task is already done; implementation agent should pick up at step N. Same step list as READY, but with the already-done prefix removed.
   - **OBSOLETE** — the task is no longer applicable (e.g. Python upstream removed the endpoint, or the cognee-rust facade changed in a way that invalidates the spec). Document why and stop. Do NOT proceed to implementation.

**Constraints**:
- Strict Python parity rule (see ../plan.md §1). The two acknowledged divergences in pipelines.md §1 are the only allowed deviations.
- You may edit any file under docs/http-server/. You may NOT edit code under crates/.
- Use the Read / Edit / Write / Bash / Grep tools. Do NOT spawn nested agents.
- Cite every claim with a file:line reference.
- Length: 800–1500 words for the final report.
```

After this agent returns, **read its verdict.**
- READY or PARTIAL → proceed to Step 2 (Implementation) with the agent's step list.
- OBSOLETE → do NOT proceed. Update [README.md](README.md) status to `Skipped (obsolete)` and move to the next task in §2 above.

## 5. Sub-agent: Implementation

**Purpose**: execute the actualized task doc end-to-end, run all verification commands, and commit with a meaningful message.

Spawn this agent (use `subagent_type: general-purpose`):

```
You are the implementation agent for ${PHASE_ID} of the cognee-rust HTTP server port.

Working dir: /home/dmytro/dev/cognee/cognee-rust

Your task doc: docs/http-server/implementation/${TASK_DOC}

The investigation agent already actualized the docs. The §4 step list you must execute is below (verbatim from the investigation agent's READY/PARTIAL report):

${STEP_LIST_FROM_INVESTIGATION}

**Execution rules**:

1. Read the task doc end-to-end before writing any code. Read every doc cited in §2 of the task doc.

2. Execute the steps in §4 in order. After every step:
   - Run `cargo check --all-targets` (NOT `--release`).
   - If the step has a specific `Verify:` command, run that too.
   - Confirm no warnings introduced beyond pre-existing ones.

3. After all steps:
   - Run `cargo test --workspace` (debug mode, no `--release`).
   - Run `scripts/check_all.sh`.
   - Both must pass before you proceed.

4. Regression check: confirm no previously-passing test now fails. If a previously-passing test fails, you have introduced a regression — fix it before committing. Do not silently accept the regression.

5. Commit. Single commit per task (unless the task doc explicitly says otherwise — P5 may produce two commits, one for migration and one for routers; check the task doc's §6).

   Commit message format:
   ```
   http-server: ${PHASE_ID} <one-line summary>

   <2–4 sentence body explaining what landed and why, citing the task doc>
   <List of acceptance-criteria checkboxes that now pass.>

   Refs: docs/http-server/implementation/${TASK_DOC}
   ```

   Use a heredoc as instructed in the project guide. Add `Co-Authored-By:` line per project convention.

6. Do NOT push. Just commit locally.

7. Final report: one of these three verdicts:
   - **DONE** — all steps executed, all tests pass, commit landed locally. Include the commit SHA.
   - **PARTIAL** — N steps landed; remaining steps blocked. Document why each remaining step is blocked (cite the error or missing dependency). Include the commit SHA for whatever did land.
   - **FAILED** — could not land any steps. Document the blocker. Do NOT commit a half-done state.

**Constraints**:
- Coding conventions per [project guide](../../.claude/CLAUDE.md): no `unwrap()` in non-test code; use `expect("reason")` or `?`. Lock-poison `unwrap()` is OK with a `// lock poison is unrecoverable` comment.
- Strict Python parity. The only acknowledged divergences are in pipelines.md §1.
- Run `cargo fmt` before each `cargo check` so formatting is never the blocker.
- The implementor MUST cite the relevant doc section for any non-obvious decision (in the commit body, not in code).
- Do NOT edit any doc under docs/http-server/. The doc-update agent owns those edits.
- Do NOT spawn nested agents.
```

After this agent returns:
- DONE → proceed to Step 3 (Review).
- PARTIAL → proceed to Step 3 (Review), then re-spawn the implementation agent with the remaining steps if Step 3 doesn't object. Repeat at most twice; if still PARTIAL, stop and ask the user.
- FAILED → stop and ask the user. Do not proceed.

## 6. Sub-agent: Review

**Purpose**: independent review of the top commit against the task doc. Catches: missing test coverage, security issues, regressions, deviations from the spec, scope creep.

Spawn this agent (use `subagent_type: general-purpose`):

```
You are the review agent for ${PHASE_ID} of the cognee-rust HTTP server port.

Working dir: /home/dmytro/dev/cognee/cognee-rust

Your task doc: docs/http-server/implementation/${TASK_DOC}
The commit under review: ${COMMIT_SHA} (the implementation agent reported this).

**Steps**:

1. Read the task doc end-to-end. Read the design docs it references (architecture.md, auth.md, pipelines.md, the relevant per-router specs).

2. Inspect the commit:
   - `git show ${COMMIT_SHA}` for the full diff.
   - `git diff ${COMMIT_SHA}~1 ${COMMIT_SHA} --stat` for the file list.
   - For every file in the diff, read its full current content (not just the diff hunks) so you understand the surrounding code.

3. Run all the verification commands the task doc lists in §6. Confirm every acceptance-criteria checkbox actually passes:
   - `cargo check --all-targets`
   - `cargo test --workspace`
   - `scripts/check_all.sh`
   - any task-specific commands.

4. Review checklist (apply to every commit):
   - **Spec match**: every step in the task doc's §4 is present in the diff. Nothing extra is present (no scope creep). If a step is "to-be-added in a later phase" with a TODO marker, the marker is there.
   - **Tests**: every test file in the task doc's §5 exists, contains the cases the doc lists, and passes.
   - **Coverage**: every wire-visible behavior (status code, header, body shape) cited in the per-router doc is exercised by at least one test.
   - **Security**: auth-bearing endpoints are gated. Permission gates use `state.lib.permissions().user_can(...)` (or the documented stub for P2). No secrets logged. SQL safe (no `format!` into queries).
   - **No `unwrap()` in non-test code**. `expect("reason")` only with a why-it-can't-fail comment. Lock poison `unwrap()` is OK.
   - **Strict Python parity**: no Rust-side improvements (the only allowed divergences are in pipelines.md §1). If the commit improves on Python, flag it — even if the improvement is good.
   - **Regressions**: previously-passing tests still pass.
   - **Commit message**: format per §5 of this prompt; cites the task doc.

5. **If you find concerns**: amend the commit (do NOT create a new commit; the user wants a single tidy commit per task).
   - Make the fixes.
   - Re-run `cargo fmt`, `cargo check --all-targets`, `cargo test --workspace`, `scripts/check_all.sh`.
   - `git commit --amend --no-edit` (or `--no-edit` replaced with an updated message if the message itself is wrong).
   - Re-verify all the checklist items.

6. Final report: one of these three verdicts:
   - **APPROVED** — commit is clean as-is. No amendments needed.
   - **AMENDED** — amended the commit; describe what was fixed. Include the new commit SHA (it will differ from the input).
   - **REJECTED** — concerns are unfixable at this level (e.g. wrong design decision, requires a doc change, or scope is wrong). Document the concerns. Do NOT amend. The orchestrator will escalate to the user.

**Constraints**:
- You MAY edit code (only to amend the commit). You may NOT push.
- You MAY edit tests (to add missing coverage during amendment).
- You MAY NOT edit docs under docs/http-server/. The doc-update agent owns those.
- Use only `git commit --amend`, never `git commit -m` (no new commits).
- If the diff is empty (e.g. all changes already merged elsewhere), report APPROVED with a note.
- Do NOT spawn nested agents.
```

After this agent returns:
- APPROVED or AMENDED → proceed to Step 4 (Doc Update).
- REJECTED → stop and ask the user. Do not proceed.

## 7. Sub-agent: Doc Update

**Purpose**: propagate the "Done" status across the doc tree. Updates implementation/README.md, routers/README.md, plan.md, and any cross-doc reference that needs a touch-up. Keeps the doc tree in sync with the codebase reality.

Spawn this agent (use `subagent_type: general-purpose`):

```
You are the doc-update agent for ${PHASE_ID} of the cognee-rust HTTP server port.

Working dir: /home/dmytro/dev/cognee/cognee-rust

Your task doc: docs/http-server/implementation/${TASK_DOC}
The commit that landed: ${COMMIT_SHA}

**Steps**:

1. Update `docs/http-server/implementation/README.md`:
   - Flip the row for ${PHASE_ID} from `In Progress` to `Done`.
   - Add a note in the row: `(commit ${COMMIT_SHA_SHORT})`.

2. Update `docs/http-server/routers/README.md` for any routers landed in this phase:
   - Flip each router's `Status` column from `Draft` to `Done`.

3. Update `docs/http-server/plan.md`:
   - The §2 sub-document index shows this phase's status — confirm it's still consistent.
   - The §4 implementation phases table — flip the relevant row to bold-checkmarked or strike-through to indicate Done. (Use whichever convention is already established in plan.md; do not invent a new one.)

4. Update the relevant phase / per-router design doc(s):
   - If the implementation discovered something the design doc got wrong, fix the design doc.
   - If a router doc has open questions that were resolved during implementation, close them (move to a "resolved" subsection or delete).

5. Re-verify cross-doc consistency. Run a quick grep:
   - No `Not started` or `Draft` status remains for routers landed in this phase.
   - No anchor refs to sections that were renumbered during implementation.
   - No `// TODO(${PHASE_ID})` markers left behind in code (those should have been resolved by the implementation agent).

6. Commit the doc updates. Single small commit:
   ```
   docs/http-server: mark ${PHASE_ID} done

   Status table flips after commit ${COMMIT_SHA_SHORT}.

   Refs: docs/http-server/implementation/${TASK_DOC}
   ```

7. Final report: a one-paragraph summary of which docs were updated and which status flips happened.

**Constraints**:
- Do NOT edit code under crates/. You only touch docs/.
- The implementation commit is already amended-and-final. Do NOT touch it.
- Do NOT spawn nested agents.
```

After this agent returns: the task is fully done. Move to the next task in §2 above.

## 8. Conventions

### 8.1 Branch policy

All work happens on a single long-lived branch named `http-server-port` (or whatever the user has checked out). Do NOT create per-phase branches. Each phase produces 1–2 commits on the branch. The user pushes manually when satisfied.

### 8.2 Commit message

Use a heredoc to preserve formatting:

```bash
git commit -m "$(cat <<'EOF'
http-server: ${PHASE_ID} <one-line summary>

<2–4 sentence body>
<Acceptance criteria that now pass.>

Refs: docs/http-server/implementation/${TASK_DOC}

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### 8.3 Verification gates

Three commands must pass before any commit:
- `cargo fmt --check`
- `cargo check --all-targets`
- `cargo test --workspace` (debug mode)
- `scripts/check_all.sh`

If any fails, fix before committing — do not commit a broken state.

### 8.4 Scope discipline

- One task per pipeline run. The implementation agent does not pull work from a later phase forward.
- Out-of-scope changes (typo fixes, doc tweaks, refactors of unrelated code) are NOT committed during a phase. Defer them to a separate cleanup commit at the end of the whole port.
- The review agent enforces this — it rejects scope creep.

### 8.5 Status tracking

Three places hold status:
1. [implementation/README.md](README.md) — phase status table. The doc-update agent owns this.
2. [../routers/README.md](../routers/README.md) — per-router status table.
3. [../plan.md](../plan.md) — sub-document index + §4 phase table.

All three must agree at the end of every doc-update step.

## 9. Failure modes & escalation

Stop and ask the user when:

- Investigation agent reports **OBSOLETE** for any task. The user must decide whether to skip, edit the doc, or rewrite the task.
- Implementation agent reports **FAILED**. The user must inspect the blocker.
- Implementation agent reports **PARTIAL** twice in a row. Same task is stuck.
- Review agent reports **REJECTED**. The user must decide whether to amend the doc, accept the deviation, or re-do the implementation.
- Any of `cargo check`, `cargo test`, `scripts/check_all.sh` fail unexpectedly mid-task and the implementation agent cannot resolve them within one retry.
- A merge conflict appears (the user is doing concurrent work on the branch). Stop and ask.

When you ask the user, include:
- The task name and `${PHASE_ID}`.
- The agent that reported the issue.
- The exact verdict the agent returned (paste it).
- The current git state (`git status`, last commit SHA).
- A specific question with 2–3 concrete options for how to proceed.

## 10. Session conventions

This driver is meant for an interactive "check-in" cadence — you run **one full task** (all four agents) per session, then stop and let the user review before starting the next task. **Do not** chain task #1 → task #2 → task #3 in a single autonomous run; the user wants to inspect the diff after each phase.

If you are running in a `/loop` skill or autonomous mode where the user has explicitly asked you to keep going, then proceed to the next task without stopping. Otherwise, after Step 4 of one task, your message to the user is:

```
Phase ${PHASE_ID} complete. Status:
- Investigation: <verdict>
- Implementation: commit ${COMMIT_SHA_SHORT}
- Review: <APPROVED | AMENDED with new SHA>
- Doc update: commit ${DOC_COMMIT_SHA_SHORT}

Next task: ${NEXT_PHASE_ID} (${NEXT_TASK_DOC}).
Reply "go" to proceed, or hand back for review.
```

## 11. References (read these once, at session start)

- [Root index](../plan.md)
- [Implementation directory README](README.md)
- [Architecture decisions](../architecture.md)
- [Auth](../auth.md)
- [Pipelines + registry](../pipelines.md)
- [WebSocket protocol](../websocket.md)
- [Observability](../observability.md)
- [Tenants + RBAC](../tenants.md)
- [E2E parity harness](../e2e-parity.md)
- [Per-router specs](../routers/)
- [Audit findings](../audit-findings.md)
- [Project guide](../../../.claude/CLAUDE.md) — coding conventions, build commands, test patterns.

## 12. Failure modes the agents cannot fix on their own

Some failures require the user. Do not let an agent silently work around them:

- **A library function the task depends on does not exist and is not described in any design doc.** Means the design doc has a gap. Ask the user.
- **A migration would conflict with existing schema.** Ask before running.
- **A test reveals a Python-Rust wire incompatibility that violates strict-parity.** Ask before "fixing".
- **The codebase has uncommitted changes when a task starts.** Stop and ask — those changes might be in-flight from a different effort.

When in doubt, stop and ask. The cost of one user round-trip is much lower than the cost of an unintended commit.
