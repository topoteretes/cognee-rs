# Phase 9 — Tests, examples, docs, CI

← [Index](../typescript-bindings-plan.md)

**Goal:** prove the bindings work on the main use-cases, run those tests in CI, and ship an
example + docs. Tests are **two-tier** because the existing `js-check` CI job has **no LLM or
embedding-model setup** — only deterministic tests can run there unconditionally.

## Scope

- **In:** Tier-A deterministic tests, Tier-B LLM-gated e2e, the CI wiring, a runnable example,
  doc updates, Cognee-class + types.ts restoration (Phase-8 regression), README rewrite.
- **Out:** new functionality, cross-SDK Node ↔ Python parity (optional, deferred).

## Pre-condition: Phase-8 regression to fix first

Phase-8 deleted `js/src/cognee.ts` and `js/src/types.ts` from `js/src/` and reverted
`package.json` `"name"` from `"cognee"` back to `"@cognee/pipeline"`. The `lib/` compiled output
retains stale pre-phase-8 copies of both files (built during phase-7) but the source files no
longer exist. This means `tsc` produces a `lib/` that is inconsistent with `src/`: re-running
`npm run build:ts` from scratch would **delete** `lib/cognee.js` and `lib/types.js`.

**Fix (must land before or as part of Phase 9):**
1. Restore `js/src/cognee.ts` (the `Cognee` class) and `js/src/types.ts` (shared TS types) from
   the Phase-7 commit (`587cac6`).
2. Restore the Phase-7 `js/src/index.ts` re-export of `Cognee`, `serve`, `disconnect` and the
   `types.ts` barrel.
3. Update `js/package.json` `"name"` back to `"cognee"` (Phase-7 decision, reverted in Phase-8).
4. Run `npm run build:ts` to verify `lib/` is consistent, then confirm `npm test` is still green.

These changes are non-functional (the compiled runtime behaviour is unchanged). They are a
prerequisite for the example (which imports `Cognee`) and the README rewrite.

## Tier A — deterministic, always runs in CI

No LLM, no model download: use `MOCK_EMBEDDING=true` + a `tmp` dir for SQLite/graph/vector.
Files under `js/__tests__/` (jest auto-discovers `**/*.test.ts`):

**Already green (do not recreate):**

| Test file | Coverage |
|---|---|
| `sdk_handle.test.ts` | `cogneeNew` construction, `cogneeWarm`, owner bootstrap (#15) |
| `config.test.ts` | every setter, generic `set` (incl. `UnknownKey`), Settings-from-object (#17) |
| `add.test.ts` | `add` text + file, dedup, dataset creation (#1) |
| `datasets.test.ts` | `DatasetManager` list/has/status/forget/prune/sessions/notebooks (#12, #7, #10) |
| `search.test.ts` | `SearchType` wire-name lock for all 15 variants + rejection of invalid type (#3) |
| `errors.test.ts` | error → `kind`/`code` on raw native errors (Phase 8) |
| existing engine tests | `pipeline.test.ts`, `smoke.test.ts`, `logging.test.ts`, etc. |

**Current run:** 12 suites pass, 129 tests pass, 8 tests skip (4 search Tier-B + 4 session/memory
Tier-B), 1 suite skips (`cognify.test.ts` — Tier-B). All Tier-A tests are already green.

**No new Tier-A test files are needed.** The plan's originally-listed `forget-prune.test.ts` and
`searchtype-mapping.test.ts` are subsumed by `datasets.test.ts` and `search.test.ts` respectively.

## Tier B — LLM-gated e2e, skips when env absent

Existing Tier-B tests (already skip cleanly in CI without credentials):

- `cognify.test.ts` — `add → cognify` round-trip (full suite skips).
- `search.test.ts` — 4 `cogneeSearch`/`cogneeRecall` live tests skip individually.
- `datasets.test.ts` — 4 memory-op tests skip individually.

**No new `e2e.test.ts` is needed** as the coverage across the three existing Tier-B guarded
sections already exercises `add → cognify → search → recall → memify → improve`. If a single
consolidated e2e file is desired for readability, it is optional and can be extracted later.

The gating pattern used throughout is `const describeMaybe = haveCreds ? describe : describe.skip`
(matching `scripts/run_tests_with_openai.sh`). Do not change it.

### Test helpers already in place

- `MOCK_EMBEDDING=true` set inline per test in Tier-A tests.
- `fs.mkdtempSync` + `afterAll` cleanup for isolated temp workspaces.
- Per-test unique `default_user_email` to avoid cross-test state leakage.

No new shared helper modules are needed.

## CI wiring — current state and decision

The **`js-check` job in `.github/workflows/ci.yml`** runs `bash js/scripts/check.sh` →
`npm run build` → `npm test`. The job has **no LLM/embedding env vars**.

**Current state:** Tier-A is already green in this job. Tier-B already skips cleanly (verified
locally: 1 suite skipped, 8 tests skipped, 0 failures).

**Decision (record in the decision log):** Keep Tier-B **out of `js-check`** for now. Rationale:
the Rust `test` job already runs the equivalent Rust pipeline tests with credentials; adding model
downloads and `OPENAI_KEY` to `js-check` roughly doubles its duration for coverage already
provided by the Rust lane. If/when a dedicated JS e2e CI lane is added, it should follow the same
structure as the Rust `test` job (model cache + `OPENAI_KEY` secret). The cross-SDK Docker harness
(`e2e-cross-sdk/`) is the appropriate long-term home for Node ↔ Python ↔ Rust round-trip tests.

The `js-check` job requires **no changes** to `ci.yml`.

## Example

A runnable example must be created at `js/examples/add-cognify-search.ts` (or `.js`). It should:

- Import `Cognee` from `"../src"` (or `"cognee"` once the package is published).
- Construct a `Cognee` instance with env-var config (`OPENAI_URL`, `OPENAI_TOKEN`, etc.).
- Run `add → cognify → search` and print the result.
- Include a `README`-style comment block at the top so readers can understand it standalone.
- Be executable with `npx ts-node js/examples/add-cognify-search.ts` (or plain `node` if compiled).

The example does **not** run in CI (it requires live credentials). Add a note to `js/README.md`
pointing to it.

## README & docs

- Rewrite `js/README.md` around the `Cognee` SDK quick start (currently documents only the legacy
  pipeline engine). Keep the legacy `Pipeline` section as an appendix.
- The package name in the README should reflect the restored `"cognee"` name.

## Cross-SDK (optional, deferred)

Adding a Node ↔ Python ↔ Rust parity case under `e2e-cross-sdk/` is out of scope for this phase.
Defer to a follow-up task.

## Dependencies & ordering

1. Fix the Phase-8 regression (restore `cognee.ts`, `types.ts`, package name) — prerequisite.
2. Verify Tier-A is still green after the fix (`bash js/scripts/check.sh`).
3. Write the example.
4. Rewrite `js/README.md`.
5. Record the Tier-B CI decision in the decision log.

## Done when

- Phase-8 regression fixed: `js/src/cognee.ts`, `js/src/types.ts`, and `package.json` `"name"`
  restored; `tsc` produces a consistent `lib/`; `npm test` is still green.
- Tier-A suite green in the `js-check` CI job on every PR (already the case; must remain so after
  the regression fix).
- Tier-B skips cleanly in `js-check` without credentials (already the case).
- Runnable `js/examples/add-cognify-search.ts` example committed.
- `js/README.md` rewritten around the `Cognee` SDK class.
- Tier-B CI decision recorded in the decision log.
