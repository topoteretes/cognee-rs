# Prompt: generate the Java-bindings implementation plan

> Usage: paste everything below the `---` line as the prompt for an Opus
> planning session run inside this repository (working directory = repo root).

---

You are an expert Rust + Java + JNI engineer producing an **implementation
plan**, not an implementation. Another Opus model (the "executor") will later
execute the plan task by task, in separate sessions, possibly with no memory
of previous sessions other than the plan documents themselves and the state of
the repository. Every word you write is for that executor. Optimize for its
success, not for human readability: be explicit, deterministic, and
repo-grounded; never say "appropriately", "as needed", or "etc." where a
concrete instruction is possible.

## Source of truth

1. Read `docs/design/java-bindings.md` in full. It is the approved design for
   adding a Java SDK (jni-rs JNI shim over `cognee-bindings-common`, idiomatic
   Java layer on top, `CompletableFuture` async, Maven classifier-jar
   packaging). Do not re-litigate decisions recorded there (§1 rationale, §2
   architecture, §4 surface, §5 errors, §6 threading, §7 layout/packaging).
2. The design doc's §9 table lists open decisions with defaults. Adopt every
   default as-is, and record each adopted decision in the plan's root document
   under a "Resolved decisions" section. Exceptions: decisions #4 (Maven
   namespace `ai.cognee`) and #8 (Sonatype credentials) are infra/maintainer
   items — plan all code against the defaults but mark the publishing task
   **Blocked: infra** rather than assuming they are done.
3. **The repository overrides the design doc.** Before planning against any
   file, symbol, signature, feature list, or CI job the design doc mentions,
   open it and verify. Where reality differs, follow reality and note the
   divergence in the root document. In particular, re-derive from source:
   - the exact list and signatures of op functions in
     `crates/bindings-common/src/ops/*.rs` (this defines the JNI surface —
     enumerate every function you will wrap, with its Rust signature);
   - the `SdkError` variants and `code()` strings in
     `crates/bindings-common/src/error.rs`;
   - how `ts/cognee-ts-neon/src/{runtime,errors,config,sdk_ops}.rs` structure
     runtime init, error conversion, config setters, and op wrapping — this
     crate is the structural blueprint for the shim;
   - the option-object shapes (`opts`) accepted by the TS SDK in
     `ts/src/cognee.ts` and how neon's `read_opts`/JSON marshalling encodes
     them — Java must send byte-identical JSON option keys;
   - the default feature list in `ts/cognee-ts-neon/Cargo.toml`;
   - the stage order and style of `scripts/check_all.sh`,
     `ts/scripts/check.sh`, `python/scripts/check.sh`, `capi/scripts/check.sh`;
   - the build matrix in `.github/workflows/ts-prebuild.yml` and job layout in
     `.github/workflows/ci.yml`;
   - the dependency versions available in the workspace `Cargo.toml`
     (`jni` is NOT currently a dependency anywhere — pick the latest published
     `jni` crate version and pin it, stating the version you chose).

## Deliverables

Create these files and nothing else (no source code files, no edits outside
this directory):

```
docs/plans/java-bindings/
├── README.md          # root document
├── T01-<slug>.md
├── T02-<slug>.md
└── ...                # one file per task, zero-padded, kebab-case slugs
```

### Root document (`README.md`) must contain, in order

1. **Introduction** — 3–6 paragraphs: what is being built, the three-layer
   architecture (condensed from the design doc), and how the executor should
   use this plan (see "Executor protocol" below — write that protocol into
   the README so the plan is self-contained even if the executor never sees
   this prompt).
2. **Resolved decisions** — the §9 defaults you adopted + the `jni` crate
   version you pinned + any divergences you found between design doc and repo.
3. **Task index table** with columns: ID | Title | Status | Depends on |
   Exit criterion (one line). Status values: `not-started` | `in-progress` |
   `blocked` | `done`. Initial statuses: everything `not-started` except
   infra-gated tasks which start `blocked`.
4. **Dependency graph** — a small Mermaid `graph TD` of task ordering, and an
   explicit note of which tasks are parallelizable.
5. **Executor protocol** — instructions to the executor: work on exactly one
   task per session unless told otherwise; before starting, re-read the task
   file AND re-verify its "Preconditions" section against the repo; set the
   task's status to `in-progress` in this README at start and `done` at
   completion (statuses live ONLY in the README table, never in task files);
   run the task's verification commands before marking done; if a
   precondition fails or reality diverges from the task file, STOP and record
   the divergence in the README under a "Deviations log" section instead of
   improvising; append one line to the Deviations log for every intentional
   departure from a task file.
6. **Deviations log** — empty section, table header only (Date | Task |
   Deviation | Reason).

### Each task file must contain, in order

1. **Objective** — one paragraph; what exists after this task that didn't
   before.
2. **Dependencies & preconditions** — task IDs that must be `done`, plus
   concrete checkable facts ("`java/cognee-java-jni/Cargo.toml` exists",
   "`cargo check` passes in `java/cognee-java-jni`"), each with the command
   or file-read that verifies it.
3. **Context for this task** — the minimal background the executor needs if
   it has read nothing else: relevant design-doc excerpts inlined (do not
   just cite section numbers — copy the binding rules in), relevant existing
   code to imitate (name the file and the pattern, e.g. "mirror the error
   conversion in `ts/cognee-ts-neon/src/errors.rs`"), and the exact JSON
   shapes involved. Each task file must be executable standalone.
4. **Steps** — numbered, fine-grained, in execution order. Each step names
   the exact file to create/modify and describes the change at the level of:
   function signatures, struct definitions, JNI export names
   (`Java_ai_cognee_internal_Native_<method>` or the `RegisterNatives` table),
   Cargo.toml/pom.xml fragments, Java class skeletons with method signatures.
   Include verbatim code for boilerplate that must be exact (JNI name
   mangling, `catch_unwind` wrapper shape, `NativeLibLoader` resource path
   scheme, version-handshake check). For repetitive families (e.g. wrapping
   30 ops), fully specify the pattern once with one complete worked example
   (both Rust and Java sides), then provide a table of the remaining
   ops: Java method name | native fn name | bindings-common fn | args JSON
   shape | result type. Do not leave signature design to the executor.
5. **Verification** — exact commands (and expected outcomes) the executor
   runs before marking the task done: `cargo fmt`/`check`/`clippy` scoped to
   the shim crate, `mvn -q verify` or equivalent, specific JUnit test names,
   `scripts/check_all.sh` where applicable. Where a step is only verifiable
   on CI (matrix builds), say so and give the local approximation.
6. **Out of scope** — explicit list of things adjacent to this task that must
   NOT be done in it (to stop executor scope-creep), including which later
   task covers them.

## Task decomposition requirements

- Follow the phasing skeleton in design-doc §11 but decompose further: target
  **8–14 tasks**, each completable by one Opus session (~one focused PR of
  roughly ≤600 lines of new code, excluding generated/boilerplate tables).
  Phase 3 (core ops + async upcall machinery) and phase 4 (remaining op
  groups) in particular must each be split into multiple tasks; the async
  upcall machinery (design §6) deserves a task of its own with the most
  detailed spec in the whole plan, including the global-ref lifecycle on
  success/failure/panic paths and the `-Xcheck:jni` test invocation.
- Every task ends with the repo in a green state: `scripts/check_all.sh`
  passes (extend it only in the task that introduces `java/scripts/check.sh`,
  and design that script to no-op gracefully when no JDK is present, mirroring
  the design doc §7).
- Include tasks for: CI wiring (`ci.yml` java-check job), the prebuild
  workflow (`java-prebuild.yml`, cloned from `ts-prebuild.yml`'s matrix),
  documentation updates (design-doc §10 lists the exact files:
  `docs/architecture.md`, `docs/tools/bindings.md`, `docs/tools/README.md`,
  root `README.md`, `.claude/CLAUDE.md`), and the (Blocked: infra) Maven
  Central publishing task.
- Testing is not a single trailing task: each functional task carries its own
  tests in its Steps, per the strategy in design-doc §8 (JUnit for L3 logic
  and lifecycle; LLM-gated integration tests skipped without
  `OPENAI_URL`/`OPENAI_TOKEN`, following the graceful-skip convention used by
  the Rust workspace tests).
- Honor repo coding conventions in every code spec you write: `unwrap()`
  forbidden outside tests (use `expect("invariant …")` or propagation),
  `thiserror` in library code, all JNI entry points panic-guarded, JSON
  across the boundary (never field-by-field JNI object construction).

## Working method

Before writing any plan file: explore the repository until you can enumerate
the complete op surface and have read the neon blueprint files end-to-end.
Budget the majority of your effort on Steps sections — they are the product.
Where you are uncertain about a detail after inspecting the repo (e.g. an
opts key, a feature flag), resolve it by reading more code, not by hedging in
the plan; if it is genuinely unresolvable (external infra), park it in the
root README's blocked items. Do not modify any file outside
`docs/plans/java-bindings/`. When finished, output a one-paragraph summary of
the plan structure and the task count — the plan files themselves are the
deliverable.
