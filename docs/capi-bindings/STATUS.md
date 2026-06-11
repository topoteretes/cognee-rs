# C API Bindings — Implementation Status

← [Index](README.md)

Single source of truth for per-phase progress. Update this when a phase starts, blocks, or
completes (the [Task Execution Template](TASK-EXECUTION-TEMPLATE.md) instructs the
orchestrator and commit agent to keep it current; phase 1 lands as two steps, 1a and 1b —
the 1a completion is recorded in this table's Notes column, the row flips to ✅ only after 1b).

**Legend:** ⬜ Not started · 🟡 In progress · 🔵 In review · ⛔ Blocked · ✅ Done

Last updated: 2026-06-11 (plan authored)

## Status table

| Phase | Task | Status | Branch | Commit | Notes |
|---|---|---|---|---|---|
| 0 | [Scaffolding & build](phase-0-scaffolding.md) | ⬜ | | | |
| 1 | [Shared facade & SDK handle](phase-1-shared-facade-and-handle.md) | ⬜ | | | keystone |
| 2 | [Errors, async & JSON conventions](phase-2-errors-async-json-conventions.md) | ⬜ | | | |
| 3 | [Config surface](phase-3-config.md) | ⬜ | | | |
| 4 | [Core ops (add/cognify)](phase-4-core-ops.md) | ⬜ | | | |
| 5 | [Retrieval (search/recall)](phase-5-retrieval.md) | ⬜ | | | |
| 6 | [Remaining SDK](phase-6-remaining-sdk.md) | ⬜ | | | |
| 7 | [Feature-gated surfaces](phase-7-feature-gated.md) | ⬜ | | | |
| 8 | [Header, examples, tests & CI](phase-8-header-examples-tests-ci.md) | ⬜ | | | |

## Per-phase exit criteria

### Phase 0 — Scaffolding & build
- [ ] `cognee-capi` extracted into its own `[workspace]` under `capi/`, mirroring the root `[patch.crates-io]` table (D10)
- [ ] `cognee-capi` links `cognee-lib` with the neon-equivalent default feature set (D6)
- [ ] existing 6 engine examples + 3 smoke tests still pass via `capi/scripts/check.sh`
- [ ] slim build (`--no-default-features` + picks) compiles; CI job added
- [ ] capi-workspace `cargo check --all-targets` wired into `scripts/check_all.sh` (R5)
- [ ] cdylib/staticlib size + cold-build-time baseline recorded (vs engine-only)
- [ ] root `Cargo.toml` TODO resolved; mirroring rule documented in all three patch tables

### Phase 1 — Shared facade & SDK handle
- [ ] `crates/bindings-common` (`cognee-bindings-common`) exists with `HandleState`, `CogneeServices`, `SdkError`, shared `wire` helpers (D1)
- [ ] `cognee-neon` consumes it; `js/scripts/check.sh` fully green (no behavior change)
- [ ] `CgSdk` handle: `cg_sdk_new` / `cg_sdk_warm` (async) / `cg_sdk_owner_id` (async) / `cg_sdk_clone` / `cg_sdk_destroy`
- [ ] minimal async plumbing: `CgSdkResultCallback` typedef + single-use `CgSdkWaiter` (new/wait/destroy) (D4, R6)
- [ ] `cognee_sdk.h` generated (second cbindgen config) + `CG_API_VERSION_*` defines + `cg_api_version()` (D8); runtime-ordering footgun documented (R7)
- [ ] landed as ≥2 PRs (R9): facade hoist + neon refactor, then capi handle + plumbing
- [ ] C smoke test constructs + warms a handle via the waiter (mock embedding, temp dirs)

### Phase 2 — Errors, async & JSON conventions
- [ ] `CgErrorCode` extended with the 8 SDK codes; mapping table + tiering rule (R2) documented
- [ ] `spawn_sdk_op` helper hardened: callback fires exactly once, always deferred (R1), `error_message` delivery, send-pointer `user_data`
- [ ] `cg_sdk_waiter_wait` fails fast with `CG_ERR_RUNTIME` when called from a runtime/callback thread
- [ ] cancellation non-goal + reserved extension shape documented in `cognee_sdk.h` (R4)
- [ ] JSON contract documented in `cognee_sdk.h` (ownership, UTF-8, camelCase, strict-JSON results incl. `true`/`null` scalars) (D3, D9)
- [ ] negative-path smoke test: bad JSON → `CG_ERR_VALIDATION` + message via the callback

### Phase 3 — Config surface
- [ ] `cg_sdk_config_set` / `cg_sdk_config_set_llm_config` / `…_embedding_config` / `…_vector_db_config` / `…_graph_db_config` / `cg_sdk_config_get`
- [ ] unknown key → `CG_ERR_UNKNOWN_CONFIG_KEY`; type mismatch → `CG_ERR_CONFIG_TYPE_MISMATCH`
- [ ] config change bumps version and triggers services rebuild (asserted in smoke test)

### Phase 4 — Core ops
- [ ] `cg_sdk_add` (async, waiter in examples) with text/file/binary inputs, dedup, dataset creation
- [ ] `cg_sdk_cognify`, `cg_sdk_add_and_cognify`
- [ ] deterministic add example (no LLM) green in check.sh
- [ ] live `add → cognify` round-trip verified (Tier-B, gated in capi-check per D12)

### Phase 5 — Retrieval
- [ ] `cg_sdk_search` over all 15 `SearchType` strings
- [ ] `cg_sdk_recall` with scopes + session routing
- [ ] live `add → cognify → search` round-trip from C (Tier-B)

### Phase 6 — Remaining SDK
- [ ] remember / remember_entry / memify / improve
- [ ] forget / update / prune_data / prune_system
- [ ] 7 dataset ops · 5 session ops
- [ ] pipeline-run resets · default user · 4 notebook ops
- [ ] deterministic smoke coverage (datasets/forget/prune) in check.sh

### Phase 7 — Feature-gated surfaces
- [ ] `cg_sdk_visualize` / `cg_sdk_visualize_to_file` in a `visualization` build
- [ ] `cg_sdk_serve` / `cg_sdk_disconnect` in a `cloud` build
- [ ] `cg_json_string_decode` utility shipped + covered in the smoke test (R8)
- [ ] non-feature builds return `CG_ERR_FEATURE_NOT_BUILT` (symbol always present)

### Phase 8 — Header, examples, tests & CI
- [ ] `cognee.h` + `cognee_sdk.h` regenerated; CI freshness check covers both
- [ ] runnable `capi/examples/example_sdk_add_cognify_search.c` committed
- [ ] `capi/scripts/check.sh` runs all SDK smoke tests; `capi-check` CI green
- [ ] Tier-B examples run in `capi-check` when secrets present, SKIP cleanly otherwise (D12)
- [ ] `capi/README.md` rewritten around the SDK surface

## Decision log

Record cross-cutting decisions as they're made (one line each), so later phases inherit them.

| Date | Decision | Phase |
|---|---|---|
| 2026-06-11 | Plan authored. | — |
| 2026-06-11 | **D1 (owner):** facade (`HandleState`/`CogneeServices`/`SdkError`/wire helpers) hoists into a NEW `crates/bindings-common` crate (`cognee-bindings-common`), not into `cognee-lib`; neon refactored to consume it. | 1 |
| 2026-06-11 | **D3 (owner):** wire JSON is camelCase, byte-identical to the TS shapes (`js/src/types.ts`); result builders shared via `bindings-common` unchanged. | 2+ |
| 2026-06-11 | **D6 (owner):** default features = full, mirroring `cognee-neon` (incl. visualization/cloud/testing); slim embedded build verified in CI. | 0 |
| 2026-06-11 | **D10 (owner):** `cognee-capi` is extracted into its own `[workspace]` under `capi/` (honors the root Cargo.toml TODO); mirrors the root `[patch.crates-io]` table like `cognee-neon`. | 0 |
| 2026-06-11 | **D4 (owner):** SDK ops are async-only (`CgSdkResultCallback`); sync usage via one generic `CgSdkWaiter` (waiter callback + `cg_sdk_waiter_wait`). No per-op blocking variants. | 1–2 |
| 2026-06-11 | **D7 (owner):** config surface = generic `cg_sdk_config_set`/`_set_str` + 4 bulk group setters + `cg_sdk_config_get`; no granular typed setters. | 3 |
| 2026-06-11 | **D11 (owner):** SDK symbol prefix is `cg_sdk_*`; existing `cognee_*` observability entry points stay as legacy. | all |
| 2026-06-11 | **D8 (owner):** split public headers — `cognee.h` (engine) + `cognee_sdk.h` (SDK); add `CG_API_VERSION_{MAJOR,MINOR}` + runtime `cg_api_version()`. | 1, 8 |
| 2026-06-11 | **D9 (owner):** `result_json` is strict JSON always (`true`/`false`, quoted strings, `null` for void ops). | 2+ |
| 2026-06-11 | **D12 (owner):** Tier-B live tests run inside the existing `capi-check` CI job, gated on secret availability (SKIP cleanly without credentials); reuse `lib-tests.yml` model caching. | 8 |
| 2026-06-11 | **R1 (review):** callback delivery is always deferred — never synchronous from the initiating `cg_sdk_*` call (libuv/gRPC/ORT rule); validation errors also arrive via a spawned task. | 2 |
| 2026-06-11 | **R2 (review):** error-code tiering rule — `cg_sdk_*` returns only SDK codes (11–18) + `CG_OK`/`NULL_POINTER`/`RUNTIME`/`UTF8`; engine codes 2, 4–9 never cross tiers. Documented in `cognee_sdk.h`, enforced in the `From<&SdkError>` mapping. | 2 |
| 2026-06-11 | **R3 (review):** corrected inherited claim — custom `MemifyTask`s (JSON-in/JSON-out) ARE expressible in C, unlike Neon closures; excluded from v1 as beyond-TS-parity, reserved as `CgMemifyTaskFn` post-parity extension. | 6 |
| 2026-06-11 | **R4 (review):** SDK-op cancellation is an explicit v1 non-goal (TS parity); extension shape reserved (optional `CgCancellationToken*` or op handle). | 2 |
| 2026-06-11 | **R5 (review):** after workspace extraction, `scripts/check_all.sh`'s capi stage gains an explicit `cargo check --all-targets` for the capi workspace (default + slim) — root cargo check no longer covers it. | 0 |
| 2026-06-11 | **R6 (review):** `CgSdkWaiter` is single-use; reuse → `CG_ERR_VALIDATION`. | 1 |
| 2026-06-11 | **R7 (review):** runtime-ordering footgun documented: `cg_init_with_threads` must precede the first `cg_sdk_new` (OnceLock no-ops afterwards). | 1 |
| 2026-06-11 | **R8 (review):** `cg_json_string_decode(json_string, out_utf8)` utility ships with Phase 7 to unescape large quoted-JSON results (keeps strict-JSON D9 uniform). | 7 |
| 2026-06-11 | **R9 (review):** Phase 1 lands as ≥2 PRs: PR-1 facade hoist + neon refactor (JS-suite gated), PR-2 capi handle + plumbing (C-smoke gated). | 1 |
