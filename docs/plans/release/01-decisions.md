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
- **Decision:** _______________________________________________

### D2 — License
- **Options:** MIT · Apache-2.0 · dual `MIT OR Apache-2.0` · proprietary.
- **Recommendation:** match the Python cognee license (confirm what it is at
  `/tmp/cognee-python/LICENSE` or the cognee GitHub repo).
- **Impacts:** task 02 (licensing), task 07 (governance), task 22 (metadata).
- **Decision:** _______________________________________________

### D3 — `DataInput::S3Path` disposition
- **Options:** implement S3 fetch (COG-4456) · feature-gate the variant · keep as a
  documented stub with a prominent rustdoc note.
- **Recommendation:** feature-gate + rustdoc note for 0.1.0.
- **Impacts:** task 17 / parity-backlog handling of erroring public surfaces.
- **Decision:** _______________________________________________

### D4 — `[profile.release] debug`
- **Options:** keep `true` · `false` · `line-tables-only` (+ optional split-debuginfo).
- **Recommendation:** `line-tables-only` (edge/Android target — current `true` bloats binaries 2–5×).
- **Impacts:** task 23 (release profile).
- **Decision:** _______________________________________________

### D5 — crates.io fork strategy (only if D1 ⊇ B)
- **Options:** publish the qdrant/lbug/litert forks to crates.io · vendor them · make
  the qdrant + litert backends optional and **off** in the published feature set.
- **Recommendation:** make those backends optional + off by default for the published library.
- **Impacts:** task 24.
- **Decision:** _______________________________________________

## Acceptance criteria

- [ ] All five decisions recorded above with a concrete choice.
- [ ] D2 license confirmed against the Python project's license.
- [ ] Downstream tasks (02, 07, 17, 22, 23, 24) updated if a decision deviates from the recommendation.

## Rollback

Decisions can be revised; if so, re-open and update the dependent task subdocuments.
