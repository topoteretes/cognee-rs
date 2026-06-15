# Cognee-Rust 0.1.0 Release — Master Task Index

> **Created:** 2026-06-14
> **Purpose:** The single source of truth for sequencing and tracking every task
> required to take cognee-rust from its current state to a credible `0.1.0` release.
> Consolidates the [release-readiness-plan.md](../release-readiness-plan.md) and the
> [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) into discrete,
> PR-sized tasks, each with its own detailed implementation subdocument in this folder.

## How to use this folder (read this first)

Each task below has a numbered subdocument (e.g. `08-triplet-distance-penalty.md`).
The subdocuments are written to be **self-contained and executable by an automated
coding agent without prior context** — every file path, code snippet, command, and
acceptance check is spelled out. An implementer should:

1. Pick the lowest-numbered unblocked task (respect the **Depends on** column).
2. Open its subdocument and follow the steps top to bottom.
3. Run the **Verification** commands; confirm every **Acceptance criterion**.
4. Mark the task ✅ in the status table below and commit.

**One task = one branch = one PR.** Do not batch unrelated tasks.

> **Automated execution:** to run the whole task list autonomously with a Sonnet
> orchestrator (validate-doc → implement → Opus-review → commit, per task), use
> [EXECUTION-PROMPT.md](EXECUTION-PROMPT.md).

## Conventions (apply to every task)

- **Branch:** `git checkout -b task/<NN>-<slug>` off `main`. Never commit to `main`.
- **No `unwrap()` in non-test code** — use `expect("why it can't fail")` or propagate. (Project rule; enforced once task 23 lands.)
- **Cross-SDK parity is sacred:** never change on-disk DB schema columns, content-hash inputs, UUID5 namespaces/inputs, vector collection name formats, or stored-file naming **unless the task explicitly says so** — these must stay byte-compatible with Python cognee.
- **Determinism:** IDs are content-addressed (`uuid5`). If you change what is hashed/chunked, you change IDs — call it out.
- **Verify before you trust:** the audit docs cite line numbers from 2026-06-14. Re-grep to confirm current locations before editing.
- **Run the gate before pushing:** `scripts/check_all.sh` (fmt → check → clippy -D warnings → C/Python/JS binding checks). For tests touching the LLM/embedding path use `bash scripts/run_tests_with_openai.sh <test_name>`.

## Subdocument template (every task file follows this)

```
# <NN> — <Title>
> Wave · Priority (P0 blocker / P1 should-fix / P2 nice-to-have) · Track (A/B) ·
> Release-blocking (yes/no) · Effort · Depends on · Source (audit/plan refs)
## Goal                — one-paragraph end state
## Background & why    — context a cold-start implementer needs (+ Python-vs-Rust for parity)
## Prerequisites       — branch cmd, files & Python sources to read first
## Files to change     — table: path | change
## Python reference    — (parity only) exact file:line + behavior to match
## Implementation steps— numbered, imperative, explicit; before/after code per step
## Verification        — exact commands + expected output; tests to add
## Acceptance criteria — checkbox list
## Gotchas / do-not    — parity/determinism/schema traps
## Rollback            — how to revert safely
```

---

## Release tracks

- **Track A — bindings + source release** (PyPI / npm / C artifact / GitHub tag). The
  fastest credible release; does **not** require crates.io publishability.
- **Track B — crates.io** — adds task 24 (git-dep/patch removal), a separate larger effort.

A task's **Track** column says whether it's needed for A, or only for B.

---

## Master task table

Legend — Priority: **P0** release-blocking · **P1** should-fix before release · **P2** nice-to-have/post-release.
Status: ⬜ todo · 🟡 in-progress · ✅ done · ⏸️ blocked.

| # | Task | Wave | Pri | Track | Blocking | Effort | Depends on | Status |
|---|------|------|-----|-------|----------|--------|------------|--------|
| 01 | [Release decisions (D1–D5)](01-decisions.md) | 0 | P0 | A+B | yes | hours | — | ✅ |
| 02 | [Licensing & legal](02-licensing.md) | 1 | P0 | A+B | yes | 0.5d | 01(D2) | ✅ |
| 03 | [FFI & Neon panic safety](03-ffi-neon-panic-safety.md) | 1 | P0 | A | yes | 0.5d | — | ⏭️ |
| 04 | [Rust code cleanup](04-rust-code-cleanup.md) | 1 | P1 | A | no | 1d | — | ✅ |
| 05 | [Documentation cleanup](05-documentation-cleanup.md) | 1 | P1 | A | no | 1d | — | ✅ |
| 06 | [Bindings & examples cleanup](06-bindings-and-examples-cleanup.md) | 1 | P1 | A | no | 1d | — | ✅ |
| 07 | [Governance files](07-governance-files.md) | 1 | P1 | A | no | 0.5d | 01(D2) | ✅ |
| 08 | [Fix triplet distance penalty default](08-triplet-distance-penalty.md) | 2 | P0 | A | yes | 0.25d | — | ✅ |
| 09 | [Fix destructive/silent lifecycle ops](09-lifecycle-destructive-ops.md) | 2 | P0 | A | yes | 0.5d | — | ✅ |
| 10 | [Wire permission revoke endpoints](10-permission-revoke-endpoints.md) | 2 | P0 | A | yes | 0.5–1d¹ | — | ✅ |
| 11 | [Collapse DB migrations to baseline](11-collapse-db-migrations.md) | 3 | P1 | A | no | 0.5d | — | ✅ |
| 12 | [Re-enable cross-SDK parity CI](12-cross-sdk-parity-ci.md) | 3 | P1 | A | strongly rec. | 1–2d | — | ✅ |
| 13 | [Sync LLM prompts to Python + drift guard](13-prompt-parity-sync.md) | 3 | P1 | A | no | 0.5d | — | ⬜ |
| 14 | [Chunking parity (tiktoken default + chunk size)](14-chunking-parity.md) | 3 | P1 | A | no | 0.5d | — | ⬜ |
| 15 | [Vector collection parity](15-vector-collection-parity.md) | 3 | P1 | A | no | 0.5d | — | ⬜ |
| 16 | [Graph extraction parity (Edge.description + Documents)](16-graph-extraction-parity.md) | 3 | P1 | A | no | 1d | 15 | ⬜ |
| 17 | [Run loaders at ADD + raw_content_hash](17-loaders-at-add.md) | 4 | P1 | A | no | 1d | — | ⬜ |
| 18 | [forget memory_only + DatasetManager.create_dataset](18-forget-memoryonly-and-create-dataset.md) | 4 | P1 | A | no | 1d | 09 | ⬜ |
| 19 | [Embedding auto-dimension resolution](19-embedding-dimension-resolution.md) | 4 | P1 | A | no | 0.5d | — | ⬜ |
| 20 | [improve() stages + session integration](20-improve-and-session-integration.md) | 4 | P2 | A | no | 1.5d | — | ⬜ |
| 21 | [Parity backlog (config/datasets/cloud/viz/recall)](21-parity-backlog-misc.md) | 4 | P2 | A | no | 2d | — | ⬜ |
| 22 | [Workspace metadata + MSRV + CHANGELOG](22-workspace-metadata-msrv-changelog.md) | 5 | P0 | A+B | yes | 0.5d | 02, 11 | ⬜ |
| 23 | [Lint enforcement & release profile](23-lint-enforcement-and-profile.md) | 5 | P1 | A | no | 0.5d | 01(D4), 03, 04 | ⬜ |
| 24 | [crates.io publishability](24-cratesio-publishability.md) | 5 | P1 | B | B only | weeks | 01(D5), 22 | ⬜ |
| 25 | [Deferred refactors (post-release)](25-deferred-refactors.md) | 6 | P2 | — | no | multi-day | — | ⬜ |

¹ Task 10: `revoke_acl`/`revoke_role` repo methods already exist (pure wiring), but full Python parity also needs a **new `delete_role` repo method** for `DELETE /roles/{role_id}` — see the subdoc; defer that sub-item if scope-constrained.

### Corrections verified during authoring (the subdocs are authoritative)

The subdocuments were written against the live code and corrected a few audit/plan figures:
- `cognify/tasks.rs` is **~4,507 lines** (plan said 3,438) — see task 25.
- Chunking auto `max_chunk_size` resolves to **≈512** with the default ONNX engine (embedding term dominates), not ≈8191 — see task 14.
- C API paths are under `capi/cognee-capi/src/…` and `capi/` is a **separate workspace** — see tasks 02/03.
- Logging env vars live in `crates/logging/src/config.rs` (not `lib/config.rs`) — see task 05.
- Recommended embedding fallback dim is **384** (Rust default model is BGE-Small), and `graph_database_provider` default should be **`ladybug`** — see tasks 19/21.

---

## Recommended execution sequence

The waves below maximize parallelism while respecting dependencies. Tasks **within a
wave have no ordering dependency on each other** and can be done concurrently (by
different people or agents). Waves are gates: finish a wave's P0s before relying on them.

### Wave 0 — Decide (blocks everything downstream)
- **01** Release decisions. Resolve D1 (release track), D2 (license), D3 (S3 disposition), D4 (release debug), D5 (crates.io fork strategy). A few of these gate later tasks; everything else can start immediately.

### Wave 1 — Foundations & low-risk cleanup (parallel)
- **02** Licensing · **03** FFI panic safety (both P0, release-blocking)
- **04** Rust cleanup · **05** Docs cleanup · **06** Bindings cleanup · **07** Governance
- *Why first:* cleanup reduces churn before the metadata freeze (22) and lint enforcement (23); the two P0s are small and unblock the release floor.

### Wave 2 — Parity Tier-1 correctness (parallel; P0, release-blocking)
- **08** Triplet penalty (≈1 line) · **09** Lifecycle destructiveness · **10** Permission revoke wiring
- *Why:* cheap, high-impact correctness/safety fixes that protect the 90%-parity promise and prevent silent data loss.

### Wave 3 — Structural correctness & parity infra (parallel; P1)
- **11** Migration squash · **12** Parity CI · **13** Prompt sync · **14** Chunking · **15** Vector collections · **16** Graph extraction (16 depends on 15)

### Wave 4 — Deeper parity (parallel; P1/P2)
- **17** Loaders at ADD · **18** forget memory_only + create_dataset (depends on 09) · **19** Embedding dims · **20** improve/session · **21** Parity backlog

### Wave 5 — Release finalization
- **22** Metadata/MSRV/CHANGELOG (after cleanup 04/05/06 and migrations 11 settle; depends on 02) · **23** Lint enforcement (after 03 + 04) · **24** crates.io (Track B only; after 22 + D5)

### Wave 6 — Post-release
- **25** Deferred refactors (split `cognify/tasks.rs`, reduce `too_many_arguments`, pg_graph spans).

```
Wave0:  01
         │
Wave1:  02  03  04  05  06  07          ── foundations + cleanup (parallel)
Wave2:  08  09  10                      ── parity tier-1 (parallel, release-blocking)
Wave3:  11  12  13  14  15 →16          ── structural correctness (16 after 15)
Wave4:  17  18(after 09)  19  20  21    ── deeper parity
Wave5:  22(after 02,11)  23(after 03,04)  24(B; after 22)
Wave6:  25                              ── post-release
```

---

## Minimum release gate (Track A)

A 0.1.0 Track-A release may ship once **all P0s and the strongly-recommended parity CI** are green:

- [ ] 01 decisions made
- [ ] 02 licensing · 03 panic safety
- [ ] 08 triplet penalty · 09 lifecycle ops · 10 permission revoke
- [ ] 12 cross-SDK parity CI green (strongly recommended gate) — suite definitions in [`e2e-cross-sdk/README.md`](../../../e2e-cross-sdk/README.md#ci-gate)
- [ ] 22 metadata/MSRV/CHANGELOG
- [ ] `scripts/check_all.sh` passes; PyPI + npm dry-run publishes succeed

Everything else (P1/P2 parity + cleanup) raises quality and parity % but is not a hard gate — track remaining items as issues at tag time.

---

## Source traceability

Every task subdoc cites its origin in the two audits so nothing is lost:
- **Release mechanics / blockers** → [release-readiness-plan.md](../release-readiness-plan.md) (B1, B2, T2–T9 IDs)
- **Cleanup** → [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) Part A (A1/A2/A3)
- **Parity** → [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) Part B (B1–B8)
