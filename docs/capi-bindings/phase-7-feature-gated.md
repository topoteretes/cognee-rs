# Phase 7 — Feature-gated surfaces: visualize, serve/disconnect

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** the two feature-gated TS surfaces reachable from C, with typed errors in
feature-stripped builds. Reference: `js/cognee-neon/src/{sdk_visualization,sdk_cloud}.rs`.

## Prerequisites

Phases 1–2 (facade + error/async conventions). Independent of Phases 4–6 — can run in
parallel with them.

## Policy (decision D6, inherited from TS Phase 6)

Functions are **always exported** regardless of features; the feature-absent body sets the
last error and returns `CG_ERR_FEATURE_NOT_BUILT`. Callers get a typed runtime error instead
of a link failure, and the header is feature-independent.

## A. Visualization (`visualization` feature)

Async-only (D4), Phase-2 conventions:

| Function | Inputs | Result JSON |
|---|---|---|
| `cg_sdk_visualize` | `opts_json` `{destinationPath?}` | the self-contained HTML document as a quoted JSON string (large — copy out inside the callback or via the waiter) |
| `cg_sdk_visualize_to_file` | `opts_json` `{destinationPath?}` | the written file path as a quoted JSON string (D9) |

## B. Cloud (`cloud` feature)

These are **module-level** in TS (not methods on `Cognee`) but still take the handle's
config; mirror the neon signatures:

| Function | Inputs | Result JSON |
|---|---|---|
| `cg_sdk_serve` | `opts_json` `{url?, apiKey?, cloudUrl?, auth0Domain?, auth0ClientId?, auth0Audience?}` | `{connected: true, serviceUrl}` |
| `cg_sdk_disconnect` | `opts_json` `{wipeCredentials?}` | `null` (D9) |

Check at implementation time whether neon's `cognee_serve`/`cognee_disconnect` take the
handle or construct their own context — match exactly (`js/cognee-neon/src/sdk_cloud.rs`).

## Tasks

1. `sdk_visualization.rs` + `sdk_cloud.rs` in capi with `#[cfg(feature)]` real bodies and
   `#[cfg(not(feature))]` `FeatureNotBuilt` bodies (the exact neon pattern).
2. **`cg_json_string_decode` utility** (not feature-gated, R8): strict JSON (D9) means
   `cg_sdk_visualize` delivers megabytes of HTML as a JSON-escaped quoted string. Ship
   `CgErrorCode cg_json_string_decode(const char* json_string, char** out_utf8)` — decodes a
   JSON string literal to raw UTF-8 (freed with `cg_string_destroy`); `CG_ERR_VALIDATION` if
   the input is not a JSON string. Keeps D9's uniform contract while removing the unescaping
   burden from C callers; document `visualize_to_file` as the preferred path for large
   outputs regardless.
3. Build-matrix verification in `check.sh`: one default build (features on, smoke calls
   succeed against mock data) and one `--no-default-features --features sqlite,testing`
   build (calls return `CG_ERR_FEATURE_NOT_BUILT`) — reuse the existing
   `COGNEE_CAPI_CARGO_FEATURES` CMake knob.

## Exit criteria

- [ ] visualize returns HTML for a small mock graph in a default build;
      `cg_json_string_decode` round-trips it to raw UTF-8
- [ ] serve/disconnect callable in a `cloud` build (live call NOT required; argument
      validation + typed error paths suffice, matching the TS test tier)
- [ ] stripped build returns `CG_ERR_FEATURE_NOT_BUILT` (via callback) from all four ops
- [ ] `cognee_sdk.h` regenerated
