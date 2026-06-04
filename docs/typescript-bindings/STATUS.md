# TypeScript Bindings — Implementation Status

← [Index](../typescript-bindings-plan.md)

Single source of truth for per-phase progress. Update this when a phase starts, blocks, or
completes (the [Task Execution Template](TASK-EXECUTION-TEMPLATE.md) instructs the orchestrator
and commit agent to keep it current).

**Legend:** ⬜ Not started · 🟡 In progress · 🔵 In review · ⛔ Blocked · ✅ Done

Last updated: 2026-06-04 (Phase 3)

## Status table

| Phase | Task | Status | Branch | Commit | Notes |
|---|---|---|---|---|---|
| 0 | [Scaffolding & build](phase-0-scaffolding.md) | ✅ | ts-bindings/phase-0-scaffolding | 3cdffa7 | done |
| 1 | [Handle & service facade](phase-1-handle-and-services.md) | ✅ | ts-bindings/phase-1-handle-facade | e87fa44 | done (keystone) |
| 2 | [Config surface](phase-2-config.md) | ✅ | ts-bindings/phase-2-config | 6988d0d | done |
| 3 | [Pipeline ops (add/cognify)](phase-3-pipeline-ops.md) | ✅ | ts-bindings/phase-3-pipeline-ops | 418ab2d | done |
| 4 | [Retrieval (search/recall)](phase-4-retrieval.md) | ⬜ | — | — | |
| 5 | [Remaining SDK](phase-5-remaining-sdk.md) | ⬜ | — | — | |
| 6 | [Feature-gated surfaces](phase-6-feature-gated.md) | ⬜ | — | — | |
| 7 | [TS layer & actualization](phase-7-typescript-layer.md) | ⬜ | — | — | |
| 8 | [Errors & marshalling](phase-8-errors-marshalling.md) | ⬜ | — | — | |
| 9 | [Tests & CI](phase-9-tests-ci.md) | ⬜ | — | — | |

## Per-phase exit criteria

Check off the criteria as they land (the granular view behind the status column).

### Phase 0 — Scaffolding & build
- [x] `.node` linking `cognee-lib` loads via `require()`
- [x] existing engine/logging/telemetry/smoke tests pass with `cognee-lib` linked
- [x] `.node` size + cold-build-time baseline recorded
- [x] standalone-vs-workspace + `[patch.crates-io]` decision recorded

### Phase 1 — Handle & service facade
- [x] `CogneeHandle` constructs from TS and survives across calls
- [x] `CogneeServices` builds all engines + derived services
- [x] config-version bump triggers a services rebuild
- [x] Tier-A test constructs + warms a handle (mock embedding, temp dir)

### Phase 2 — Config surface
- [x] all granular setters exposed; bulk + generic `set(key,value)`
- [x] Settings construction from object and from env
- [x] `config.test.ts` (Tier-A) green, incl. `UnknownKey` + rebuild-on-change

### Phase 3 — Pipeline ops
- [x] `add` (text/file) with dedup + dataset creation
- [x] `cognify` + `add-and-cognify`
- [x] `add.test.ts` (Tier-A, no LLM) green
- [x] live `add → cognify` round-trip verified (Tier-B, skips cleanly in CI)

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
| 2026-06-04 | `cognee-neon` stays a standalone crate (Option A) with its own `[patch.crates-io]` table mirroring the root workspace (`tar`/`tonic`/`hyper` qdrant forks); revisit joining the workspace if patch drift becomes painful. | 0 |
| 2026-06-04 | `owner_id` is derived via Python default-user semantics: `uuid5` of the configured default-user email (matching the Python SDK's default user), so Rust and Python produce comparable owner-scoped IDs. | 1 |
| 2026-06-04 | `CogneeServices` is cached on the handle and invalidated by config version: a `Settings` version bump triggers a full services rebuild; runtime (tokio) init is idempotent and shared across handles. | 1 |
| 2026-06-04 | Extended the shared `cognee-lib` `ConfigManager` (Option B) with granular setters + widened bulk dispatch rather than mapping config in the binding, so Rust/TS/CLI share one config surface. | 2 |
| 2026-06-04 | `cogneeNew` constructs `Settings` via a `defaults < env < object` overlay (object wins; absent fields fall back to env, then defaults). | 2 |
| 2026-06-04 | `add()` partitions added vs deduplicated via pre-scan since `AddPipeline` returns all items including duplicates (duplicate path returns the pre-existing row); pre-scan counts rows in the dataset before the run and post-scan computes the delta. | 3 |
| 2026-06-04 | `DataInput` discriminated union marshalled explicitly in Rust: `type` field drives a `match`; `binary.bytes` accepts `Buffer`/`number[]`/base64-string via `base64` crate; `url`/`s3` return `Unsupported` until wired end-to-end. | 3 |
| 2026-06-04 | `CognifyResult` is hand-built JSON from pipeline output fields (chunks, entities, edges, summaries, embeddings, alreadyCompleted, priorPipelineRunId) rather than deriving `Serialize` on internal types. | 3 |
| | _(e.g. package renamed to `cognee`)_ | 7 |
