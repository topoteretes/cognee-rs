# API Gaps Implementation Prompt

> **Status (2026-06): COMPLETED.** All 8 gaps this runbook targets have been
> implemented in the Rust codebase (see the resolved status notes in
> `README.md` and the individual gap docs). This prompt is retained as a
> historical record of the process that was followed.

Use this prompt to implement all 8 API gaps from `docs/api-gaps/README.md` sequentially.

---

## Execution instructions

You are implementing API v1 parity gaps between the Python cognee SDK and the Rust `cognee-rust` codebase. The work is split into 8 gaps documented in `docs/api-gaps/`. Each gap has a description file (`NN-*.md`) and an implementation plan (`impl/NN-*-plan.md`).

**Implementation order** (from `README.md` — follow exactly):

| Phase | Gap # | File Prefix | Title |
|-------|-------|-------------|-------|
| 1 | 8 | `08-env-variables` | Environment Variable Coverage |
| 2 | 6 | `06-session-management` | Session Management |
| 3 | 3 | `03-configuration-api` | Configuration API |
| 4 | 1 | `01-missing-parameters` | Missing Parameters on Existing Functions |
| 5 | 5 | `05-dataset-management` | Dataset Management |
| 6 | 2 | `02-missing-functions` | Missing Functions (`forget` → `update` → `prune` → `recall` → `remember` → `improve`) |
| 7 | 7 | `07-ontology-management` | Ontology Management |
| 8 | 4 | `04-user-auth-tenancy` | User / Auth / Multi-Tenancy |

Process each gap **one at a time**, completing the full cycle before moving to the next. After each gap, stop and await the user's go-ahead before proceeding to the next gap.

---

## Per-gap cycle

For each gap N, execute these stages in strict order:

### Stage 1 — Verify (sub-agent, type: `Explore`)

> **Purpose:** Confirm the gap description and implementation plan are still accurate against the current codebase. Code changes from prior gaps may have already closed parts of this gap or shifted file locations/line numbers.

Launch a sub-agent with this brief:

```
You are verifying gap docs for the cognee-rust project.

Codebase: /home/dmytro/dev/cognee/cognee-rust
Python reference: /tmp/cognee-python (clone it if missing: git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python)

Read these two files completely:
  - docs/api-gaps/{NN}-{name}.md          (gap description)
  - docs/api-gaps/impl/{NN}-{name}-plan.md (implementation plan)

For EVERY gap item and EVERY code reference in both files:
  1. Read the referenced Rust source file at the cited line numbers. Confirm the symbol/signature/struct still exists as described. If it has moved or changed, note the new location.
  2. For any Python reference, read the Python source at the cited path and verify the claim.
  3. Check if prior gap implementations have already partially or fully resolved this item.
  4. Verify that proposed new files/modules don't already exist.

Output a structured report:
  - For each item: STILL VALID | STALE (with correction) | ALREADY DONE (with evidence)
  - Updated file paths and line numbers where anything shifted.
  - A summary: which items remain to implement, which can be skipped.

If you find stale content, EDIT the doc files in-place to correct them (update line numbers, fix signatures, mark items already done, remove completed rows from tables). Do NOT remove items that are still pending — only correct their descriptions.
```

Wait for the verify agent to complete. Read its report. If it says some items are already done, note them — the implementor should skip those.

### Stage 2 — Implement (sub-agent in isolated worktree)

> **Purpose:** Implement all remaining items for this gap in an isolated git worktree, producing a single clean commit.

Launch a sub-agent with `isolation: "worktree"`:

```
You are implementing API gap {N} for the cognee-rust project.

## Context

The gap description is at: docs/api-gaps/{NN}-{name}.md
The implementation plan is at: docs/api-gaps/impl/{NN}-{name}-plan.md

Read BOTH files completely before writing any code. They contain:
  - Exact Rust files to create/modify (with line numbers)
  - Proposed struct/trait/function signatures
  - Dependencies between sub-items
  - Suggested implementation order within the gap

## Rules

1. Follow the implementation plan step by step. Do not skip steps or reorder unless a dependency requires it.
2. Read every file you intend to modify BEFORE modifying it. Understand the surrounding code.
3. Do NOT modify files outside the scope of this gap unless strictly necessary for compilation (e.g. re-exports in lib.rs).
4. Preserve all existing tests. Do not delete or weaken test assertions.
5. Add tests for new public API surface where the plan calls for it.
6. `.unwrap()` is forbidden in non-test code. Use `expect("reason")` or proper error propagation.
7. All public traits must be `Send + Sync`.
8. Use `Arc<dyn Trait>` for shared ownership, not generics, unless performance-critical.
9. After all code changes, run:
   - `cargo check --all-targets` — must pass
   - `cargo clippy --all-targets -- -D warnings` — must pass
   - `cargo fmt --all` — apply formatting
   - `cargo test --workspace -- --test-threads=1` — run tests (if tests fail due to missing env vars or models, that is acceptable — compilation failures are not)
10. If `scripts/check_all.sh` is available and the environment supports it, run it. Fix any issues it reports. If C API / Python / JS binding checks fail due to environment issues (missing toolchains), that is acceptable — Rust-side checks must pass.

## Commit

Create a single commit with message:

```
gap-{N}: {short title of the gap}

Implements docs/api-gaps/{NN}-{name}.md

{2-3 sentence summary of what was added/changed}
```

Do NOT push. The commit stays local in the worktree.
```

Wait for the implementor agent to complete. It will return the worktree path and branch name.

### Stage 3 — Review (sub-agent)

> **Purpose:** Review the implementation for correctness, security, and adherence to the plan. Fix issues and amend to the commit if needed.

Launch a sub-agent. Pass it the worktree path from Stage 2:

```
You are reviewing the implementation of API gap {N} for cognee-rust.

Worktree path: {worktree_path}
Branch: {branch_name}

## What to review

1. Read the gap description: docs/api-gaps/{NN}-{name}.md
2. Read the implementation plan: docs/api-gaps/impl/{NN}-{name}-plan.md
3. Run `git diff main...HEAD` in the worktree to see all changes.
4. For each changed file:
   a. Verify the change matches the implementation plan's intent.
   b. Check for security issues: SQL injection, command injection, path traversal, unbounded allocations.
   c. Check for `.unwrap()` in non-test code (forbidden).
   d. Check that existing tests still compile and are not weakened.
   e. Check that new public API has doc comments.
   f. Verify no unrelated files were modified.
   g. Verify no previous changes were overwritten (compare with main branch for files that shouldn't have changed).
5. Run `cargo check --all-targets` and `cargo clippy --all-targets -- -D warnings` in the worktree.
6. Run `cargo test --workspace -- --test-threads=1`.

## Fixing issues

If you find problems:
  - Fix them directly in the worktree files.
  - Run cargo check + clippy again to confirm the fix.
  - Amend the existing commit: `git add -A && git commit --amend --no-edit`

## Report

Output a structured report:
  - PASS or FAIL for each review criterion.
  - List of fixes applied (if any).
  - Final verdict: APPROVED or BLOCKED (with reason).

If BLOCKED, explain what's wrong and what the user should do. Do NOT merge a blocked implementation.
```

If the review says APPROVED, proceed to Stage 4. If BLOCKED, inform the user and stop.

### Stage 4 — Merge and mark done

After review approval:

1. **Merge the worktree branch** into main:
   ```bash
   cd /home/dmytro/dev/cognee/cognee-rust
   git merge --no-ff {branch_name} -m "Merge gap-{N}: {title}"
   ```

2. **Mark the gap as done** in `docs/api-gaps/{NN}-{name}.md`:
   - Change status from "Not Started" to "Implemented"
   - Update the README.md overview if needed

3. **Clean up old worktrees:**
   ```bash
   git worktree prune
   git worktree list   # verify no stale worktrees remain
   ```

4. **Announce completion** to the user: which gap was finished, how many remain.

---

## Additional guidelines

### Python reference codebase

If `/tmp/cognee-python` does not exist, clone it:
```bash
git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python
```
This clone should happen once at the start, not per-gap.

### Build conventions (from CLAUDE.md)

- Check compilation: `cargo check --all-targets`
- Run tests in debug mode (no `--release` unless explicitly asked)
- After all changes: `scripts/check_all.sh` for full verification
- The `.env` file is loaded automatically by `dotenv::dotenv()` in tests and the CLI — no manual sourcing needed

### Error handling conventions

- `.unwrap()` forbidden in non-test code
- Use `thiserror` for custom error enums in library crates
- `Mutex::lock().unwrap()` / `RwLock::read().unwrap()` are acceptable (lock poison is unrecoverable)

### What NOT to do

- Do not implement HTTP endpoints. This is library-level API only.
- Do not add new search types, file format loaders, or LLM providers. Focus on method signatures and orchestration.
- Do not refactor code unrelated to the gap being implemented.
- Do not remove features or weaken existing functionality.
- Do not push to remote. All work stays local.

### Handling large gaps

Gap 2 (Missing Functions) contains 6 sub-functions. Implement them in the order specified in the implementation plan: `forget` → `update` → `prune` → `recall` → `remember` → `improve`. Each sub-function should be a separate logical change within the single gap commit.

Gap 4 (User/Auth/Tenancy) is the largest. The implementation plan has multiple phases. Follow them in order — do not try to implement everything at once.

### If a gap turns out to be already done

If the verify agent finds that all items in a gap are already implemented (e.g., by a prior gap's implementation), skip Stages 2-3 and just mark it done in Stage 4. Announce to the user that the gap was already covered.

### Communication

After completing each gap's full cycle (all 4 stages), report:
- Gap number and title
- What was implemented (brief summary)
- Any items skipped (with reason)
- Remaining gaps count
- Ask the user for go-ahead before proceeding to the next gap
