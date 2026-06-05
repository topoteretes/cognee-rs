# TypeScript Bindings — Implementation Status

← [Index](../typescript-bindings-plan.md)

Single source of truth for per-phase progress. Update this when a phase starts, blocks, or
completes (the [Task Execution Template](TASK-EXECUTION-TEMPLATE.md) instructs the orchestrator
and commit agent to keep it current).

**Legend:** ⬜ Not started · 🟡 In progress · 🔵 In review · ⛔ Blocked · ✅ Done

Last updated: 2026-06-05 (Phase 9)

## Status table

| Phase | Task | Status | Branch | Commit | Notes |
|---|---|---|---|---|---|
| 0 | [Scaffolding & build](phase-0-scaffolding.md) | ✅ | ts-bindings/phase-0-scaffolding | 3cdffa7 | done |
| 1 | [Handle & service facade](phase-1-handle-and-services.md) | ✅ | ts-bindings/phase-1-handle-facade | e87fa44 | done (keystone) |
| 2 | [Config surface](phase-2-config.md) | ✅ | ts-bindings/phase-2-config | 6988d0d | done |
| 3 | [Pipeline ops (add/cognify)](phase-3-pipeline-ops.md) | ✅ | ts-bindings/phase-3-pipeline-ops | ffeea54 | done |
| 4 | [Retrieval (search/recall)](phase-4-retrieval.md) | ✅ | ts-bindings/phase-4-retrieval | 33efc35 | done |
| 5 | [Remaining SDK](phase-5-remaining-sdk.md) | ✅ | ts-bindings/phase-5-remaining-sdk | 5330435 | done |
| 6 | [Feature-gated surfaces](phase-6-feature-gated.md) | ✅ | ts-bindings/phase-6-feature-gated | 335dca5 | done |
| 7 | [TS layer & actualization](phase-7-typescript-layer.md) | ✅ | ts-bindings/phase-7-ts-layer | 587cac6 | done |
| 8 | [Errors & marshalling](phase-8-errors-marshalling.md) | ✅ | ts-bindings/phase-8-errors-marshalling | bf462a1 | done |
| 9 | [Tests & CI](phase-9-tests-ci.md) | ✅ | ts-bindings/phase-9-tests-ci | 8c75eb3 | done |

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
- [x] `search` over all `SearchType`
- [x] `recall` with scopes + session routing
- [x] `SearchType` ↔ string mapping locked (Tier-A)
- [x] live `add → cognify → search` / `recall` round-trip

### Phase 5 — Remaining SDK
- [x] remember / remember_entry / memify / improve
- [x] forget / delete / update / prune
- [x] DatasetManager (list/has/status/empty/delete)
- [x] sessions / pipeline-run resets / default user
- [x] notebooks scope decided (in scope — api::notebooks is SDK-level)
- [x] Tier-A (datasets/forget/prune/sessions) + Tier-B (memory ops) tests

### Phase 6 — Feature-gated surfaces
- [x] `visualize` returns HTML in a `visualization` build
- [x] `serve` / `disconnect` callable in a `cloud` build
- [x] non-feature builds throw a typed "feature not built" error
- [x] default feature set decided + documented

### Phase 7 — TS layer & actualization
- [x] `Cognee` class with typed methods + `types.ts`
- [x] legacy engine re-homed under `cognee.pipeline.*`
- [x] package identity decided; `package.json`/`index.ts`/`native.ts` updated
- [x] existing `js/` files updated or intentionally re-exported
- [x] `README.md` rewritten around the SDK

### Phase 8 — Errors & marshalling
- [x] `json.rs` single conversion path; no `JSON.parse` shortcuts remain
- [x] error enums → typed JS error subclasses with stable `kind`
- [x] `errors.test.ts` (Tier-A) green

### Phase 9 — Tests & CI
- [x] Phase-8 regression fixed: `js/src/cognee.ts` + `types.ts` restored; `package.json` `"name"` = `"cognee"`; `tsc` produces consistent `lib/`
- [x] Tier-A suite green in the `js-check` CI job (12 suites pass, 129 tests pass)
- [x] Tier-B skips cleanly in `js-check` without credentials (1 suite skipped, 8 tests skipped, 0 failures)
- [x] Runnable `js/examples/add-cognify-search.ts` example committed
- [x] `js/README.md` updated with example pointer (Phase-7 already rewrote the main content)
- [x] CI wiring decision recorded in the decision log (Tier-B stays out of `js-check`)

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
| 2026-06-04 | `SearchType` parsed via `serde_json::from_value(Value::String(s))` using `SCREAMING_SNAKE_CASE` serde attribute — same path as the HTTP server, guaranteed to stay in sync. | 4 |
| 2026-06-04 | `RecallResult` hand-built JSON: `items`/`search_type_used`/`search_response` are each `Serialize` and serialized individually; `auto_routed` bool copied directly; camelCase keys used in the output JSON. | 4 |
| 2026-06-04 | `ScopeInput` constructed directly from opts strings (no serde): `ScopeInput::Single(s)` or `ScopeInput::Many(vec)` then passed to `normalize_scope`; empty `Many` yields `None` so `recall()` applies its own Auto default. | 4 |
| 2026-06-04 | Phase 5: notebooks included in scope — `api::notebooks` is an SDK-level facade (not feature-gated, not HTTP-only); exposed as `cogneeListNotebooks`, `cogneeCreateNotebook`, `cogneeUpdateNotebook`, `cogneeDeleteNotebook`. | 5 |
| 2026-06-04 | `MemifyResult` hand-built JSON: `{ tripletCount, indexedCount, batchCount, alreadyCompleted, priorPipelineRunId }` — `MemifyResult` derives `Debug, Clone` only (not `Serialize`). | 5 |
| 2026-06-04 | `ImproveResult` hand-built JSON: `{ stagesRun, memifyResult, feedbackEntriesProcessed, feedbackEntriesApplied, sessionsPersisted, edgesSynced }` — `ImproveResult` derives `Debug, Clone, Default` only. | 5 |
| 2026-06-04 | `ForgetResult` hand-built JSON: `{ target, deleteResult }` — `ForgetResult` derives `Debug, Clone` only; nested `DeleteResult` IS `Serialize` and passes through serde directly. | 5 |
| 2026-06-04 | `UpdateResult` hand-built JSON: `{ deletedDataId, deleteResult, newData, cognifyResult }` — reuses `cognify_result_json` helper for the last field (local copy per-module, Phase-8 consolidation will factor out). | 5 |
| 2026-06-04 | `PruneResult` hand-built JSON: `{ dataPruned, graphPruned, vectorPruned, metadataPruned, cachePruned }` — `PruneResult` derives `Debug, Clone, Default` only; `cogneePruneData` and `cogneePruneSystem` split into two exports matching the two Rust API functions. | 5 |
| 2026-06-04 | `visualization` and `cloud` features default ON in `cognee-neon`, mirroring `cognee-lib` and `cognee-cli` defaults; a `--no-default-features` build strips both. Functions are always registered in `lib.rs`; the feature-absent body rejects with `FEATURE_NOT_BUILT` so callers get a typed error rather than a cryptic "not a function". | 6 |
| 2026-06-04 | `FeatureNotBuilt(String)` variant added to `SdkError`; annotated `#[allow(dead_code)]` because it is only constructed in `#[cfg(not(feature = "..."))]` branches that are compiled out when defaults are active. `code()` returns `"FEATURE_NOT_BUILT"`. | 6 |
| 2026-06-04 | Package renamed to `cognee` in Phase 7 commit 587cac6; `js/src/cognee.ts` + `types.ts` added; `index.ts` re-exports `Cognee` class. Phase-8 inadvertently reverted these — to be restored in Phase 9. | 7 |
| 2026-06-04 | Phase 8: single `json.rs` conversion path — `js_to_serde`, `serde_to_js`, `read_opts` are the canonical helpers; all private per-module copies removed; `cognify_result_json` and `marshal_inputs`/`marshal_one` extracted to `json.rs` as well. | 8 |
| 2026-06-04 | Phase 8: `throw_sdk_error` and `throw_config_error` now attach both `code` and `kind` (same string value) to thrown errors; `kind` is the stable API identifier, `code` is the backwards-compatible alias. | 8 |
| 2026-06-04 | Phase 8: Neon 1.1 cannot call JS constructors, so typed subclasses live in the TS layer; Rust throws a plain `Error` with `code`+`kind`; `wrapNativeError` in `errors.ts` re-wraps to the correct `CogneeError` subclass by reading `kind`. Tier-A `errors.test.ts` asserts `code`/`kind` on raw native errors (no Phase-7 class needed). | 8 |
| 2026-06-05 | Phase 8 regression: `js/src/cognee.ts` and `js/src/types.ts` were deleted in Phase-8, and `package.json` `"name"` was reverted from `"cognee"` to `"@cognee/pipeline"`. Phase-9 must restore these as its first task before adding the example. | 9 |
| 2026-06-05 | Tier-B CI decision: Tier-B tests stay out of `js-check` — they already skip cleanly; adding model downloads + OPENAI_KEY to `js-check` doubles cost for coverage already provided by the Rust `test` lane. Future home is the cross-SDK Docker harness or a dedicated JS e2e job. | 9 |
