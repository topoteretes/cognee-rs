# Phase 9 — Tests, examples, docs, CI

← [Index](../typescript-bindings-plan.md)

**Goal:** prove the bindings work on the main use-cases, run those tests in CI, and ship an
example + docs. Tests are **two-tier** because the existing `js-check` CI job has **no LLM or
embedding-model setup** — only deterministic tests can run there unconditionally.

## Scope

- **In:** Tier-A deterministic tests, Tier-B LLM-gated e2e, the CI wiring, a runnable example,
  doc updates, optional cross-SDK parity.
- **Out:** new functionality.

## Tier A — deterministic, always runs in CI

No LLM, no model download: use `MOCK_EMBEDDING=true` + a `tmp` dir for SQLite/graph/vector. New
files under `js/__tests__/` (jest auto-discovers `**/*.test.ts`):

| Test file | Use-case (surfaces) |
|---|---|
| `sdk-handle.test.ts` | `cogneeNew` construction, `cogneeWarm`, survives across calls, owner bootstrap (#15) |
| `config.test.ts` | every setter, generic `set` (incl. `UnknownKey`), Settings-from-object/env, version-bump rebuild (#17) |
| `add.test.ts` | `add` text + file, dedup, dataset creation (#1) — **no LLM** |
| `datasets.test.ts` | `DatasetManager` list/has/status/empty/delete (#12) |
| `forget-prune.test.ts` | `forget`/`delete` (#7/#8), `prune_data`/`prune_system` (#10) |
| `searchtype-mapping.test.ts` | `SearchType` ↔ string lock (#3) — no backend |
| `errors.test.ts` | error → typed JS subclass + `kind` (Phase 8) |
| existing engine tests | kept, imports moved to the `pipeline` namespace |

## Tier B — LLM-gated e2e, skips when env absent

`e2e.test.ts`: full `add → cognify → search → recall → memify → improve` round-trip
(surfaces #2–#6, #11). Guarded by an env check (`OPENAI_URL`/`OPENAI_TOKEN` +
`COGNEE_E2E_EMBED_MODEL_PATH`); `test.skip` with a clear log when unset, so local `check.sh`
stays green without credentials. Mirrors the Rust `scripts/run_tests_with_openai.sh` pattern.

### Test helpers
- `describeIfLlm(...)` — gating wrapper that skips Tier-B without credentials.
- `withTempWorkspace(fn)` — temp dir + teardown for isolated storage/db.
- A shared "mock embedding + temp dirs" setup for Tier-A.

## CI wiring (correction: there is no `js-check.yml`)

The JS bindings are checked by the **`js-check` job in `.github/workflows/ci.yml`**, which runs
`bash js/scripts/check.sh` → `npm run build` → `npm test`.

- **Tier A** runs here as-is once the SDK test files exist.
- **Tier B in CI** requires the `js-check` job to gain what the Rust `test` job already has: the
  `OPENAI_KEY` secret (mapped to `OPENAI_TOKEN`) and the cached BGE embedding model (reuse the
  model-cache step / `scripts/lib/common.sh`). **Decide:** enable Tier-B in `js-check`, or run it
  only in the cross-SDK harness. Either way document it — do not let Tier-B silently never run.

## Examples & docs

- A runnable Node example: `add → cognify → search` (under `examples/` or `js/examples/`).
- Rewrite `js/README.md` around the SDK quick start (Phase 7); keep an engine appendix.
- Update the workspace README / `docs/not-implemented.md` if any binding gaps remain.

## Cross-SDK (optional)

Add a Node ↔ Python ↔ Rust parity case under `e2e-cross-sdk/`, reusing its existing OpenAI-backed
Docker harness for the Tier-B round-trip and on-disk-format parity checks.

## Dependencies & ordering

Tier-A tests land incrementally with each op phase; this phase consolidates them, adds Tier-B,
and finalizes CI.

## Done when

- Tier A is green in the `js-check` CI job on every PR.
- Tier B runs (in CI or the cross-SDK harness) with credentials and skips cleanly without them.
- The example runs; README + docs updated.
