# Phase 0 — Scaffolding & build

← [Index](../typescript-bindings-plan.md)

**Goal:** make `cognee-neon` capable of linking `cognee-lib` and producing a loadable `.node`,
with a sustainable build/packaging story. No SDK functions yet — this phase only proves the
foundation compiles and the existing engine surface still works.

## Scope

- **In:** Cargo manifest changes, the standalone-vs-workspace decision, `[patch.crates-io]`
  reconciliation, feature selection, the `.node` build pipeline, packaging/prebuild strategy,
  baseline size/time metrics.
- **Out:** any new exported functions, marshalling, the handle/facade (Phase 1).

## Structures & changes

### `js/cognee-neon/Cargo.toml`
- Add `cognee-lib = { path = "../../crates/lib", features = [...] }`.
- **Feature set:** enable the non-platform features the SDK needs — `qdrant`, `ladybug`,
  `onnx`, `hf-tokenizer`, `tiktoken`. **Also enable `cognee-lib`'s `testing` feature** — it is
  required for the legacy mock `TaskContext` to keep working *through* `cognee-lib` (see the
  pruning note below; the mock re-exports `cognee_lib::graph::MockGraphDB` /
  `cognee_lib::vector::MockVectorDB` are `#[cfg(feature = "testing")]`-gated in
  `crates/lib/src/lib.rs`). Treat `telemetry`, `cloud`, `visualization` as opt-in build flags
  surfaced as `cognee-neon` features (consumed in Phase 6). Mirror the default set that
  `cognee-cli` enables (`crates/cli/Cargo.toml`: `onnx`, `ladybug`, `qdrant`, `pgvector`,
  `pggraph`, `sqlite`, `postgres`, `hf-tokenizer`, `tiktoken`, `pdf-pdfium`, `csv-loader`,
  `unstructured`, `visualization`, `cloud`, `telemetry`), minus platform-only ones
  (`android-litert`). For Phase 0 the minimal compiling set is the five engine features above
  plus `sqlite` (needed by `task_context.rs`, which calls `cognee_database::connect("sqlite::memory:")`)
  and `testing`; heavier loaders (`pdf-pdfium`, `unstructured`) can be deferred and feature-gated.
- Prune now-redundant direct deps: `cognee-lib` re-exports `cognee-database`, `cognee-graph`,
  `cognee-vector`, and **`cognee-core` (via `cognee_lib::core::*`)**, so in principle the binding
  could route all of these through `cognee-lib`. **However**, `task_context.rs` imports
  `cognee_core::{TaskContext, RayonThreadPool, TaskContextBuilder}`, `cognee_graph::MockGraphDB`,
  `cognee_vector::MockVectorDB`, and `cognee_database::{connect, initialize}` by their crate
  paths today. To keep the diff minimal and avoid touching the legacy engine modules in Phase 0,
  **keep `cognee-core` as a direct dep** and keep the direct `cognee-graph` / `cognee-vector`
  (with `features = ["testing"]`) / `cognee-database` (with `features = ["sqlite"]`) deps — they
  must resolve to the *same* versions `cognee-lib` pulls (path deps, so they will). Rewiring the
  legacy modules onto `cognee-lib` re-exports is optional cleanup, not required for this phase.
  Keep `cognee-logging`/`observability`/`telemetry` as already wired for the setup functions.

### Standalone vs workspace (decision)
- Today `cognee-neon` is its **own workspace** (`[workspace]` table) with a private
  `[patch.crates-io]` block for qdrant's `tar`/`tonic`/`hyper` forks.
- **Option A — stay standalone:** mirror the root workspace's full patch table here. Pro:
  isolation, independent build. Con: patch table must be kept in sync by hand.
- **Option B — join the workspace:** inherit patches from the root `[patch.crates-io]` (which is
  identical to the standalone block today: `tar`, `tonic`, `hyper` qdrant forks). Pro: one source
  of truth for the patch table. Con: couples the cdylib build into the workspace graph. Note: the
  CI `rust-cache` key is *already* shared — the `js-check` job uses `shared-key: workspace-v3` and
  caches both `. -> target` and `js/cognee-neon -> target` — so cache-sharing is not a
  differentiator between the options.
- **Recommendation:** Option A short-term (smallest blast radius, the crate already builds this
  way), revisit Option B if patch drift becomes painful. Either way the qdrant forks must resolve
  identically to the workspace, or `cognee-lib`'s vector backend won't compile.

### Build pipeline (`js/package.json`)
- Replace the hand-rolled `cargo build && cp ... .node` with a robust copy that handles the
  platform-specific artifact name (`libcognee_neon.so`/`.dylib`/`cognee_neon.dll`). Consider
  `@neon-rs/cli` or `cargo-cp-artifact` for correctness.
- The current `build:rust` script is `cargo build --release && cp …`. `js/scripts/check.sh` runs
  `npm run build`, so CI builds **release**; this is intentional (packaging) and unaffected by the
  "debug by default" dev convention. Keep `--release` in the packaged build script; for local
  iteration developers can `cargo build` (debug) inside `js/cognee-neon` directly.
- Define a **prebuild matrix** (Linux/macOS/Windows × x64/arm64) for npm distribution; the
  loaded `.node` should be selected per platform at install/runtime.

### CI system build deps (`.github/workflows/ci.yml`, `js-check` job) — REQUIRED EDIT
The `js-check` job currently installs only `protobuf-compiler`. Linking `cognee-lib` with the
`ladybug` feature pulls in the `lbug` crate, whose `build.rs` compiles bundled C++ via
`cmake::Config` (`build_bundled_cmake()` in `lbug-0.14.1/build.rs`). **Without `cmake` on the
runner the `js-check` build will fail.** The `capi` CI job already installs `cmake` for this exact
reason. So this phase must add `cmake` to the `js-check` apt-get step:

```yaml
- run: sudo apt-get update && sudo apt-get install -y cmake protobuf-compiler
```

The ONNX runtime needs nothing extra on Linux x64 — `ort` 2.0.0-rc.11 downloads a prebuilt
runtime at build time (no system package required). Document this in the Risks section below.

## Functionalities

None added. The deliverable is: the crate compiles with `cognee-lib` linked, and the **existing
engine exports and jest tests still pass** unchanged.

## Dependencies & ordering

First phase; nothing precedes it. Everything else depends on this compiling.

## Risks

- **Patch conflicts** — the qdrant forks are the most likely build blocker; resolve before
  anything else.
- **Build time / binary size** — linking ONNX runtime + qdrant + ladybug + tokenizers will grow
  the ~19 MB baseline substantially; record the new numbers and decide which backends to
  feature-gate.
- **System build deps** — the CI `js-check` job installs `protobuf-compiler` and uses `mold` but
  **does not install `cmake`**. The `ladybug` feature's `lbug` build requires CMake (see the "CI
  system build deps" section above) — this is the most likely CI blocker and the required fix is
  adding `cmake` to the `js-check` apt step. ONNX (`ort` 2.0.0-rc.11) needs no extra system package
  on Linux x64; it downloads its runtime at build time.
- **`lbug` rebuild churn** — `lbug-0.14.1` re-runs its CMake build whenever its `rerun-if-changed`
  source paths change; expect cold builds to be slow on CI. The shared `rust-cache` (`workspace-v3`)
  mitigates this on warm runs.

## Done when

- `npm run build` produces a `.node` that `require()`s without error.
- The existing `__tests__/*` pass with `cognee-lib` linked. Current test files (verified):
  `default_subscriber.test.ts`, `logging.test.ts`, `pipeline.test.ts`,
  `setup_telemetry_analytics.test.ts`, `setup_telemetry.test.ts`, `smoke.test.ts`.
- `bash js/scripts/check.sh` passes locally **and** the `js-check` CI job passes (after adding
  `cmake` to its apt step — see "CI system build deps").
- `.node` size and cold-build time recorded as a baseline in this doc. **Pre-link baseline (engine
  only): 19 MB** (`js/cognee_neon.node`). Record the post-link size after Phase 0 lands.

### Post-link baselines (Phase 0, recorded 2026-06-04)

Measured on Linux x64 (debug toolchain `cargo 1.93.0`), feature set
`qdrant, ladybug, onnx, hf-tokenizer, tiktoken, sqlite, testing` (the minimal
compiling set), **release** profile (the packaged build):

| Metric | Pre-link (engine only) | Post-link (`cognee-lib`) |
|---|---|---|
| `.node` size | 19 MB | **26.75 MB** (26,754,440 bytes / 25.5 MiB) |
| Cold build (`cargo clean` → `cargo build --release`) | — | **6m 01s** (`/usr/bin/time -v`, wall clock) |

Notes:
- Size grew ~8 MB from linking the ONNX/qdrant/ladybug/tokenizer backends — well
  under the worst-case feared in the Risks section; no backend needs to be
  feature-gated for Phase 0.
- The crate is still **standalone** (Option A) with its own `[patch.crates-io]`
  mirroring the root workspace (`tar`/`tonic`/`hyper` qdrant forks) — they
  resolve identically, so `cognee-lib`'s vector backend compiles.
- Warm rebuild after a no-op change is sub-second (the `js/scripts/check.sh`
  run reported `Finished in 0.49s`). Cold builds dominate because `lbug`
  recompiles bundled C++ via CMake; the shared `rust-cache` (`workspace-v3`)
  mitigates this on CI warm runs.
