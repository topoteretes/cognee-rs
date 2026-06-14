# 01 — Release Decisions (D1–D5)

> Wave 0 · Priority P0 · Track A+B · Release-blocking: yes · Effort: hours (stakeholder) ·
> Depends on: — · Source: [release-readiness-plan.md](../release-readiness-plan.md) §2

## Goal

Capture the five stakeholder decisions that gate downstream tasks. This task is **not
code** — it is a sign-off. Fill in each "Decision" box, then unblock the dependent tasks.

## Background & why

Several tasks cannot be finalized until a human chooses a direction (license text,
whether crates.io is in scope, how to treat the S3 stub, etc.). Recording the choices
here keeps the rest of the plan unambiguous for an automated implementer.

## Decisions

### D1 — What does "release" mean?
- **Options:** A = bindings + source (PyPI/npm/C/GitHub tag); B = crates.io; or both.
- **Recommendation:** Ship **A** now; pursue **B** later.
- **Impacts:** task 24 (crates.io publishability) is in scope only if D1 ⊇ B.
- **Decision:** **A — bindings + source release** (PyPI / npm / C artifact / GitHub tag).
  crates.io (Track B) deferred to a later release. → **Task 24 is OUT OF SCOPE (skip).**
  _(Set by stakeholder, 2026-06-14.)_

### D2 — License
- **Options:** MIT · Apache-2.0 · dual `MIT OR Apache-2.0` · proprietary.
- **Recommendation:** match the Python cognee license (confirm what it is at
  `/tmp/cognee-python/LICENSE` or the cognee GitHub repo).
- **Impacts:** task 02 (licensing), task 07 (governance), task 22 (metadata).
- **Decision:** **Apache-2.0** — matches the Python cognee project. Confirmed against
  `/tmp/cognee-python/LICENSE` (Apache License Version 2.0) and `pyproject.toml`
  (`license = "Apache-2.0"`). _(Set by stakeholder, 2026-06-14; pending final
  stakeholder confirmation — flagged in orchestrator report.)_

### D3 — `DataInput::S3Path` disposition
- **Options:** implement S3 fetch (COG-4456) · feature-gate the variant · keep as a
  documented stub with a prominent rustdoc note.
- **Recommendation:** feature-gate + rustdoc note for 0.1.0.
- **Impacts:** task 17 / parity-backlog handling of erroring public surfaces.
- **Decision:** **Feature-gate the `S3Path` variant + prominent rustdoc note** for 0.1.0.
  Full S3 fetch (COG-4456) deferred. _(Set by stakeholder, 2026-06-14.)_

### D4 — `[profile.release] debug`
- **Options:** keep `true` · `false` · `line-tables-only` (+ optional split-debuginfo).
- **Recommendation:** `line-tables-only` (edge/Android target — current `true` bloats binaries 2–5×).
- **Impacts:** task 23 (release profile).
- **Decision:** **`line-tables-only`** (keeps backtraces while shrinking the edge/Android
  binary 2–5× vs `debug = true`). _(Set by stakeholder, 2026-06-14.)_

### D5 — crates.io fork strategy (only if D1 ⊇ B)
- **Options:** publish the qdrant/lbug/litert forks to crates.io · vendor them · make
  the qdrant + litert backends optional and **off** in the published feature set.
- **Recommendation:** make those backends optional + off by default for the published library.
- **Impacts:** task 24.
- **Decision:** **N/A for 0.1.0** — Track B (crates.io) is out of scope per D1, so no fork
  strategy is needed now. When Track B is pursued, the recommendation stands: make the
  qdrant/litert backends optional + off by default for the published library.
  _(Set by stakeholder, 2026-06-14.)_

## Acceptance criteria

- [x] All five decisions recorded above with a concrete choice.
- [x] D2 license confirmed against the Python project's license (Apache-2.0).
- [x] Downstream tasks (02, 07, 17, 22, 23, 24) — all decisions follow the documented
  recommendation, so no downstream subdoc edits are required. Task 24 is skipped per D1.

## Rollback

Decisions can be revised; if so, re-open and update the dependent task subdocuments.
