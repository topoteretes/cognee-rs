# Phase 8 — Header, examples, tests & CI

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** the SDK surface is shippable: fresh committed header, runnable examples, the
extended check suite green in CI, and documentation rewritten around the SDK. Mirrors TS
Phase 9.

## Prerequisites

Phases 4–7 (consolidates per-phase artifacts).

## Test tiers (same split as TS)

- **Tier-A (CI, deterministic, no credentials):** mock embedding + temp dirs; covers handle
  lifecycle, config, add/dedup, datasets/data ops, error paths, search-type string mapping,
  feature-stripped builds. Implemented as C example binaries run by `check.sh` (the existing
  pattern — no C test framework added).
- **Tier-B (gated, live):** `add → cognify → search`/`recall`/`memify` against a real LLM;
  each binary SKIPs (exit 0 + message) when `OPENAI_URL`/`OPENAI_TOKEN` are absent.
  **Runs inside the existing `capi-check` CI job** (decision D12): the job gets the
  `OPENAI_KEY` secret + embedding-model caching (reuse the `lib-tests.yml` setup); the
  SKIP-clean behavior keeps fork PRs (no secrets) green.

## Tasks

1. **Header freshness check**: `check.sh` regenerates **both** `cognee.h` and
   `cognee_sdk.h` into the build tree and diffs against the committed
   `capi/include/` copies; fail on drift. (Prevents the recurring "implemented but not in
   the public header" gap — `cg_pipeline_set_data_id_fn` is the existing instance; fix or
   explicitly exclude it while here.) Also assert `cg_api_version()` matches the
   `CG_API_VERSION_*` defines.
2. **Consolidate examples**: ensure the per-phase smoke binaries
   (`sdk_handle_smoke`, `sdk_config_smoke`, `example_sdk_add`, `sdk_data_smoke`,
   feature-stripped checks) are wired into `capi/scripts/check.sh` alongside the 9 existing
   engine examples/smokes; flagship Tier-B example
   `example_sdk_add_cognify_search.c` builds in CI but SKIPs.
3. **CI** (`.github/workflows/capi-check.yml`): build-time/caching updates for the heavier
   dep tree (ONNX, qdrant, ladybug) and the separate capi workspace target dir; the
   feature-stripped build job (added in Phase 0) stays; wire the Tier-B stage per D12
   (secret-gated, model cache shared with `lib-tests.yml`); keep total runtime acceptable
   (consider `sccache`/cargo cache reuse).
4. **Docs**:
   - Rewrite `capi/README.md` around the SDK (quick start: config via env → `cg_sdk_new` →
     add → cognify → search; memory-ownership and error-handling contracts; the engine API
     moves to a "low-level pipeline engine" section).
   - Update the index/STATUS of this plan; flip the §5 comparison table rows to ✅.
   - Update root `CLAUDE.md`/`README` capi mentions if the surface description changed.
5. **Final parity audit**: script or manual pass diffing the 136 neon exports against the C
   export list; every SDK-relevant export has a `cg_sdk_*` counterpart or a documented
   exclusion (e.g. TS-only conveniences like the 39 granular config setters → covered by
   `cg_sdk_config_set`).

## Exit criteria

- [ ] header-freshness check (both headers) + version-symbol consistency enforced in CI
- [ ] full `capi/scripts/check.sh` green locally and in `capi-check` CI
- [ ] Tier-B flagship example runs end-to-end in `capi-check` with secrets; SKIPs cleanly
      without (fork PRs)
- [ ] parity audit recorded in STATUS (exclusions listed)
- [ ] `capi/README.md` rewritten; plan docs updated to final state
- [ ] `scripts/check_all.sh` green (the capi check is one of its stages)
