# TypeScript Bindings — Implementation Status

← [Index](../typescript-bindings-plan.md)

Single source of truth for per-phase progress. Update this when a phase starts, blocks, or
completes (the [Task Execution Template](TASK-EXECUTION-TEMPLATE.md) instructs the orchestrator
and commit agent to keep it current).

**Legend:** ⬜ Not started · 🟡 In progress · 🔵 In review · ⛔ Blocked · ✅ Done

Last updated: 2026-06-03

## Status table

| Phase | Task | Status | Branch | Commit | Notes |
|---|---|---|---|---|---|
| 0 | [Scaffolding & build](phase-0-scaffolding.md) | ⬜ | — | — | |
| 1 | [Handle & service facade](phase-1-handle-and-services.md) | ⬜ | — | — | keystone |
| 2 | [Config surface](phase-2-config.md) | ⬜ | — | — | |
| 3 | [Pipeline ops (add/cognify)](phase-3-pipeline-ops.md) | ⬜ | — | — | |
| 4 | [Retrieval (search/recall)](phase-4-retrieval.md) | ⬜ | — | — | |
| 5 | [Remaining SDK](phase-5-remaining-sdk.md) | ⬜ | — | — | |
| 6 | [Feature-gated surfaces](phase-6-feature-gated.md) | ⬜ | — | — | |
| 7 | [TS layer & actualization](phase-7-typescript-layer.md) | ⬜ | — | — | |
| 8 | [Errors & marshalling](phase-8-errors-marshalling.md) | ⬜ | — | — | |
| 9 | [Tests & CI](phase-9-tests-ci.md) | ⬜ | — | — | |

## Per-phase exit criteria

Check off the criteria as they land (the granular view behind the status column).

### Phase 0 — Scaffolding & build
- [ ] `.node` linking `cognee-lib` loads via `require()`
- [ ] existing engine/logging/telemetry/smoke tests pass with `cognee-lib` linked
- [ ] `.node` size + cold-build-time baseline recorded
- [ ] standalone-vs-workspace + `[patch.crates-io]` decision recorded

### Phase 1 — Handle & service facade
- [ ] `CogneeHandle` constructs from TS and survives across calls
- [ ] `CogneeServices` builds all engines + derived services
- [ ] config-version bump triggers a services rebuild
- [ ] Tier-A test constructs + warms a handle (mock embedding, temp dir)

### Phase 2 — Config surface
- [ ] all granular setters exposed; bulk + generic `set(key,value)`
- [ ] Settings construction from object and from env
- [ ] `config.test.ts` (Tier-A) green, incl. `UnknownKey` + rebuild-on-change

### Phase 3 — Pipeline ops
- [ ] `add` (text/file) with dedup + dataset creation
- [ ] `cognify` + `add-and-cognify`
- [ ] `add.test.ts` (Tier-A, no LLM) green
- [ ] live `add → cognify` round-trip verified

### Phase 4 — Retrieval
- [ ] `search` over all `SearchType`
- [ ] `recall` with scopes + session routing
- [ ] `SearchType` ↔ string mapping locked (Tier-A)
- [ ] live `add → cognify → search` / `recall` round-trip

### Phase 5 — Remaining SDK
- [ ] remember / remember_entry / memify / improve
- [ ] forget / delete / update / prune
- [ ] DatasetManager (list/has/status/empty/delete)
- [ ] sessions / pipeline-run resets / default user
- [ ] notebooks scope decided (in or out)
- [ ] Tier-A (datasets/forget/prune/sessions) + Tier-B (memory ops) tests

### Phase 6 — Feature-gated surfaces
- [ ] `visualize` returns HTML in a `visualization` build
- [ ] `serve` / `disconnect` callable in a `cloud` build
- [ ] non-feature builds throw a typed "feature not built" error
- [ ] default feature set decided + documented

### Phase 7 — TS layer & actualization
- [ ] `Cognee` class with typed methods + `types.ts`
- [ ] legacy engine re-homed under `cognee.pipeline.*`
- [ ] package identity decided; `package.json`/`index.ts`/`native.ts` updated
- [ ] existing `js/` files updated or intentionally re-exported
- [ ] `README.md` rewritten around the SDK

### Phase 8 — Errors & marshalling
- [ ] `json.rs` single conversion path; no `JSON.parse` shortcuts remain
- [ ] error enums → typed JS error subclasses with stable `kind`
- [ ] `errors.test.ts` (Tier-A) green

### Phase 9 — Tests & CI
- [ ] Tier-A suite green in the `js-check` CI job
- [ ] Tier-B e2e runs with creds, skips cleanly without
- [ ] runnable `add → cognify → search` example
- [ ] CI wiring decision (Tier-B in `js-check` vs cross-SDK) recorded

## Decision log

Record cross-cutting decisions as they're made (one line each), so later phases inherit them.

| Date | Decision | Phase |
|---|---|---|
| | _(e.g. standalone crate, keep separate patch table)_ | 0 |
| | _(e.g. package renamed to `cognee`)_ | 7 |
