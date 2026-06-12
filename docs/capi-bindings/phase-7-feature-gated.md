# Phase 7 — Feature-gated surfaces: visualize, serve/disconnect

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** the two feature-gated TS surfaces reachable from C, with typed errors in
feature-stripped builds. Reference: `js/cognee-neon/src/{sdk_visualization,sdk_cloud}.rs`.

## Prerequisites

Phases 1–2 (facade + error/async conventions). Phases 1–6 are all ✅ Done per STATUS.md;
Phase 7 can be executed immediately.

## Policy (decision D6, inherited from TS Phase 6)

Functions are **always exported** regardless of features; the feature-absent body fires the
callback with `CG_ERR_FEATURE_NOT_BUILT` (SDK code 16) via `spawn_sdk_op`. Callers get a
typed runtime error instead of a link failure, and the header is feature-independent.

## A. Visualization (`visualization` feature)

Async-only (D4), Phase-2 conventions:

| Function | Inputs | Result JSON |
|---|---|---|
| `cg_sdk_visualize` | `opts_json` `{destinationPath?}` | the self-contained HTML document as a quoted JSON string (large — copy out inside the callback or via the waiter) |
| `cg_sdk_visualize_to_file` | `opts_json` `{destinationPath?}` | the written file path as a quoted JSON string (D9) |

### cognee-lib API reference (verified)

Both functions are available in `cognee-lib` under the `visualization` feature:

- `cognee_lib::visualize(graph_db: &dyn GraphDBTrait, dest: Option<&Path>) -> Result<PathBuf, VisualizationError>`
  — writes the HTML to disk, returns the written path. Used by `cg_sdk_visualize_to_file`.
- `cognee_lib::visualization::render(graph_db: &dyn GraphDBTrait) -> Result<String, VisualizationError>`
  — returns the raw HTML string. Used by `cg_sdk_visualize`.

Obtain `graph_db` from `state.services().await?.graph_db` (an `Arc<dyn GraphDBTrait>`).
Pass `&*graph_db` (deref to `dyn GraphDBTrait`). See `js/cognee-neon/src/sdk_visualization.rs`
`inner::run_visualize` / `inner::run_visualize_to_file` for the exact call pattern.

## B. Cloud (`cloud` feature)

**Confirmed from `js/cognee-neon/src/sdk_cloud.rs`**: `cognee_serve` / `cognee_disconnect`
operate on the **process-wide `CloudClient` singleton** — they do NOT accept a `CgSdk*`
handle as a first argument. The C functions `cg_sdk_serve` and `cg_sdk_disconnect` therefore
take `opts_json` as their first (and only data) parameter alongside the callback, with no
`sdk` pointer. The opts derive config from the global env / `ServeConfig` builder, not from
the handle's `HandleState`.

| Function | Inputs | Result JSON |
|---|---|---|
| `cg_sdk_serve` | `opts_json` `{url?, apiKey?, cloudUrl?, auth0Domain?, auth0ClientId?, auth0Audience?}` | `{connected: true, serviceUrl}` |
| `cg_sdk_disconnect` | `opts_json` `{wipeCredentials?}` | `null` (D9) |

### cognee-lib API reference (verified)

```rust
cognee_lib::serve(config: ServeConfig) -> Result<CloudClient, CloudError>
cognee_lib::disconnect(wipe_credentials: bool) -> Result<(), CloudError>
```

`ServeConfig::direct(url)` (with URL) or `ServeConfig::cloud()` (device-code flow) are the
two constructors. All opts fields map to builder methods on `ServeConfig`.

## Tasks

1. Create `capi/cognee-capi/src/sdk_visualization.rs` with `#[cfg(feature = "visualization")]`
   real body and `#[cfg(not(feature = "visualization"))]` `FeatureNotBuilt` body — follow the
   exact neon pattern in `js/cognee-neon/src/sdk_visualization.rs`.
   Create `capi/cognee-capi/src/sdk_cloud.rs` with `#[cfg(feature = "cloud")]` real body and
   `#[cfg(not(feature = "cloud"))]` `FeatureNotBuilt` body — follow
   `js/cognee-neon/src/sdk_cloud.rs`. Note: the C functions `cg_sdk_serve` /
   `cg_sdk_disconnect` do NOT take a `CgSdk*` first argument (they are process-wide singletons;
   no handle pointer in the signature).
2. Register both new modules in `capi/cognee-capi/src/lib.rs`:
   ```rust
   pub mod sdk_visualization;
   pub mod sdk_cloud;
   ```
3. **`cg_json_string_decode` utility** (not feature-gated, R8): strict JSON (D9) means
   `cg_sdk_visualize` delivers megabytes of HTML as a JSON-escaped quoted string. Add
   `CgErrorCode cg_json_string_decode(const char* json_string, char** out_utf8)` to
   `capi/cognee-capi/src/util.rs` — decodes a JSON string literal to raw UTF-8 (freed with
   `cg_string_destroy`); `CG_ERR_SDK_VALIDATION` (14) if the input is not a JSON string.
   Keeps D9's uniform contract while removing the unescaping burden from C callers; document
   `visualize_to_file` as the preferred path for large outputs regardless.
4. Bump `CG_API_VERSION_MINOR` to 6 in both `capi/include/cognee_sdk.h` and the
   `cg_api_version()` return value in `capi/cognee-capi/src/sdk.rs` (one minor increment per
   phase that ships new symbols — established per-phase pattern).
5. Update `capi/include/cognee_sdk.h` (hand-maintained — `build.rs` is a no-op stub; do not
   run cbindgen) to add declarations for `cg_sdk_visualize`, `cg_sdk_visualize_to_file`,
   `cg_sdk_serve`, `cg_sdk_disconnect`, and `cg_json_string_decode` with C doc comments
   matching the style of existing Phase 4–6 declarations. Also update the
   `CG_API_VERSION_MINOR` `#define` comment to include "Phase 7 = 6".
6. Build-matrix verification in `check.sh` and C smoke tests:
   a. **Default build smoke test** (features on): add a `sdk_feature_gated_smoke.c` example
      that calls `cg_sdk_visualize` against a handle with mock embedding and mock graph data,
      verifies the callback fires with `CG_OK`, round-trips through `cg_json_string_decode`,
      and calls `cg_sdk_visualize_to_file`. For `cg_sdk_serve` / `cg_sdk_disconnect`, argument
      validation paths and the `CG_ERR_FEATURE_NOT_BUILT` contract for cloud-absent builds
      suffice (live call NOT required). Register the example in `capi/examples/CMakeLists.txt`
      and add a run block to `capi/scripts/check.sh`.
   b. **Slim build `CG_ERR_FEATURE_NOT_BUILT` test**: the existing `cargo check` slim path in
      `check.sh` (lines 22–27) already verifies the slim configuration compiles. To verify
      the runtime `CG_ERR_FEATURE_NOT_BUILT` return, add a **separate CMake build dir**
      (`build-slim`) following the `build-panic` pattern, passing
      `-DCOGNEE_CAPI_NO_DEFAULT_FEATURES=ON` and `-DCOGNEE_CAPI_CARGO_FEATURES=sqlite,testing`
      to CMake. This requires adding a new `COGNEE_CAPI_NO_DEFAULT_FEATURES` CMake option to
      `capi/CMakeLists.txt` that injects `--no-default-features` into the cargo invocation
      (the existing `COGNEE_CAPI_CARGO_FEATURES` knob only adds `--features`, it does not
      support `--no-default-features`). The slim smoke C binary should call all four ops and
      assert `CG_ERR_FEATURE_NOT_BUILT` (16) via the callback.

## Exit criteria

- [x] `cg_sdk_visualize` returns HTML for a small mock graph in a default build;
      `cg_json_string_decode` round-trips it to raw UTF-8
- [x] `cg_sdk_visualize_to_file` writes a file and returns the path as a quoted JSON string
- [x] `cg_sdk_serve` / `cg_sdk_disconnect` callable in a `cloud` build (live call NOT
      required; argument validation + typed error paths suffice, matching the TS test tier)
- [x] stripped build (`--no-default-features --features sqlite,testing`) returns
      `CG_ERR_FEATURE_NOT_BUILT` (16, via callback) from all four ops
- [x] `cg_json_string_decode` returns `CG_ERR_SDK_VALIDATION` on non-string JSON input
- [x] `CG_API_VERSION_MINOR` bumped to 6 in header and `cg_api_version()` return value
- [x] `cognee_sdk.h` updated with declarations for the 5 new symbols (hand-edited, not
      cbindgen-generated — `build.rs` is a no-op stub; the headers are maintained manually)
