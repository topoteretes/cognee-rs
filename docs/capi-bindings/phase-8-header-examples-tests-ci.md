# Phase 8 — Header, examples, tests & CI

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** the SDK surface is shippable: headers verified, runnable examples, the extended
check suite green in CI, and documentation rewritten around the SDK. Mirrors TS Phase 9.

## Prerequisites

Phases 4–7 (consolidates per-phase artifacts). ✓ All phases 0–7 Done per STATUS.md.

## Test tiers (same split as TS)

- **Tier-A (CI, deterministic, no credentials):** mock embedding + temp dirs; covers handle
  lifecycle, config, add/dedup, datasets/data ops, error paths, search-type string mapping,
  feature-stripped builds. Implemented as C example binaries run by `check.sh` (the existing
  pattern — no C test framework added).
- **Tier-B (gated, live):** `add → cognify → search`/`recall`/`memify` against a real LLM;
  each binary SKIPs (exit 0 + message) when `OPENAI_URL`/`OPENAI_TOKEN` are absent.
  **Runs inside the existing `capi-check` CI job** (decision D12): the job gets the
  `OPENAI_KEY` secret + embedding-model caching; the SKIP-clean behavior keeps fork PRs
  (no secrets) green.

## Actuality notes (verified 2026-06-12 against current tree)

**IMPORTANT:** Both `cognee.h` and `cognee_sdk.h` are **hand-maintained** (not cbindgen-generated).
`capi/cognee-capi/build.rs` is intentionally empty (comment explains the manual-header choice).
The "regenerate + diff" framing in Task 1 below is therefore **not applicable** and has been
corrected. The correct approach for header drift prevention is described in the corrected task.

All Tier-A smoke binaries and Tier-B flagship examples are already wired; see Task 2 notes.

## Tasks

1. **Header integrity check** ✗ GENUINELY REMAINS (but approach differs from original spec):
   - **`cg_pipeline_set_data_id_fn` gap**: symbol is implemented in `pipeline.rs` but absent
     from `capi/include/cognee.h`. Either add the declaration to the header (preferred —
     closes the gap for consumers) or add an explicit exclusion comment in the header.
     *(Original spec said "fix or explicitly exclude it while here" — this still applies.)*
   - **Version-symbol consistency check in a smoke test**: `sdk_handle_smoke.c` currently
     asserts `major=1 && minor>=1` rather than asserting exact equality against the
     `CG_API_VERSION_*` macros from the header. Add or extend a Tier-A smoke test that does:
     `assert(cg_api_version() == ((CG_API_VERSION_MAJOR << 16) | CG_API_VERSION_MINOR))`
     so that a header/Rust source version skew is caught at test time. This is the correct
     substitute for a "cbindgen diff" in a manual-header workflow.
   - ~~`check.sh` regenerates both headers via cbindgen and diffs against committed copies~~
     **DROPPED** — both headers are hand-maintained; cbindgen is not in the build pipeline.

2. **Consolidate examples** ✓ COMPLETED IN PHASES 1–7:
   - All smoke binaries exist and are wired into `check.sh` and `CMakeLists.txt`:
     `sdk_handle_smoke`, `sdk_conventions_smoke`, `sdk_negative_path_smoke`,
     `sdk_config_smoke`, `example_sdk_add`, `example_sdk_add_cognify`,
     `sdk_retrieval_smoke`, `example_sdk_add_cognify_search`, `sdk_data_smoke`,
     `sdk_feature_smoke` / `sdk_feature_smoke_slim`, plus the gap-07 smokes.
   - Tier-B examples (`example_sdk_add_cognify`, `example_sdk_add_cognify_search`) have
     built-in skip guards (`check_credentials()` → `exit(0)` when env vars absent).
   - **Remaining bug in `check.sh`**: Both Tier-B `if/else` blocks in `check.sh` run the
     binary even in the `else` branch (they just print "Skipping" but still invoke the
     binary). The binaries' own skip guard makes this safe, but the shell script is
     misleading. Fix: the `else` branch should not invoke the binary at all, or should rely
     entirely on the binary's built-in skip guard (remove the `if/else` wrapper entirely
     and always run the binary unconditionally, relying on the built-in guard).

3. **CI** (`.github/workflows/ci.yml`, `capi-check` job) ✗ GENUINELY REMAINS:
   - The `capi-check` job currently does **not** have `OPENAI_URL` / `OPENAI_TOKEN` /
     `OPENAI_MODEL` secrets wired in. Without them, Tier-B examples always SKIP in CI,
     even on non-fork pushes where the secrets are available — this violates D12.
   - **Required**: add an `env:` block to the `capi-check` job with:
     ```yaml
     env:
       OPENAI_URL: https://api.openai.com/v1
       OPENAI_TOKEN: ${{ secrets.OPENAI_KEY }}
       OPENAI_MODEL: gpt-4o-mini
     ```
   - The ORT binary cache (`capi/target/ort-cache`) is already wired correctly.
   - Cargo/rust-cache (`shared-key: capi-check-v1`) is already wired correctly.
   - No separate `.github/workflows/capi-check.yml` file exists; the job lives inside
     `.github/workflows/ci.yml` at the `capi-check:` stanza. The phase doc's references to
     a standalone `capi-check.yml` are stale.

4. **Docs** ✗ GENUINELY REMAINS:
   - `capi/README.md` is still engine-only (no `cg_sdk_*` mention, no SDK quick start,
     no memory-ownership or error-handling contract for SDK users). Must be rewritten with:
     - SDK quick-start section: `cg_sdk_new` → configure → `cg_sdk_add` → `cg_sdk_cognify`
       → `cg_sdk_search`; waiter pattern; JSON in/out contract.
     - Memory ownership table (`cg_string_destroy` rules).
     - Error-handling contract (callback code + message; thread-safety note).
     - The existing engine API content moves to a "Low-level pipeline engine" section.
   - Update plan STATUS.md: flip Phase 8 row to ✅ Done after all exit criteria pass;
     update plan README.md §5 parity table row 19 to ✅.
   - Root `CLAUDE.md`/workspace README: capi section already describes the SDK tier; verify
     no stale "engine-only" language remains.

5. **Final parity audit** ✗ GENUINELY REMAINS:
   - No recorded parity audit exists in STATUS.md yet.
   - Manual or scripted pass: diff the 136 neon exports (`js/cognee-neon/src/lib.rs`) against
     the `cg_sdk_*` symbol list in `capi/include/cognee_sdk.h`; document every SDK-relevant
     export that has a C counterpart, and every explicit exclusion (e.g. the 39 granular
     config setters → covered by `cg_sdk_config_set`; TS-only JS-error throw helpers → N/A
     in C).
   - Record the audit result in STATUS.md under a "Parity audit" heading.

## Exit criteria

- [x] `cg_pipeline_set_data_id_fn` either added to `cognee.h` or explicitly excluded with comment
- [x] version-symbol consistency (`cg_api_version() == (MAJOR<<16)|MINOR`) asserted in a Tier-A smoke
- [x] check.sh Tier-B `if/else` wrapper fixed (either remove the else-runs-binary issue or rely on built-in guard)
- [x] `capi-check` CI job has `OPENAI_KEY` secret wired so Tier-B runs with secrets; SKIPs cleanly on fork PRs
- [x] `capi/README.md` rewritten around the SDK surface; engine API in a subordinate section
- [x] parity audit recorded in STATUS.md (exclusions documented)
- [x] plan STATUS.md table Phase 8 row → ✅; README.md §5 row 19 → ✅
- [x] `capi/scripts/check.sh` and `scripts/check_all.sh` fully green locally
