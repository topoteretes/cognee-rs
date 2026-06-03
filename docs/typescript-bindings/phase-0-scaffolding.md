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
  `onnx`, `hf-tokenizer`, `tiktoken`. Keep `testing` for the legacy mock `TaskContext`. Treat
  `telemetry`, `cloud`, `visualization` as opt-in build flags surfaced as `cognee-neon` features
  (consumed in Phase 6). Mirror the default set that `cognee-cli` enables, minus platform-only
  ones (`android-litert`).
- Prune now-redundant direct deps: `cognee-lib` re-exports `cognee-database`/`graph`/`vector`,
  so the binding can depend on `cognee-lib` for those and keep only `cognee-core` directly (the
  legacy engine modules use it). Keep `cognee-logging`/`observability`/`telemetry` as already
  wired for the setup functions.

### Standalone vs workspace (decision)
- Today `cognee-neon` is its **own workspace** (`[workspace]` table) with a private
  `[patch.crates-io]` block for qdrant's `tar`/`tonic`/`hyper` forks.
- **Option A — stay standalone:** mirror the root workspace's full patch table here. Pro:
  isolation, independent build. Con: patch table must be kept in sync by hand.
- **Option B — join the workspace:** inherit patches + the shared `rust-cache` key. Pro: one
  source of truth, faster cached CI. Con: couples the cdylib build into the workspace graph.
- **Recommendation:** Option A short-term (smallest blast radius, the crate already builds this
  way), revisit Option B if patch drift becomes painful. Either way the qdrant forks must resolve
  identically to the workspace, or `cognee-lib`'s vector backend won't compile.

### Build pipeline (`js/package.json`)
- Replace the hand-rolled `cargo build && cp ... .node` with a robust copy that handles the
  platform-specific artifact name (`libcognee_neon.so`/`.dylib`/`cognee_neon.dll`). Consider
  `@neon-rs/cli` or `cargo-cp-artifact` for correctness.
- Define a **prebuild matrix** (Linux/macOS/Windows × x64/arm64) for npm distribution; the
  loaded `.node` should be selected per platform at install/runtime.

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
- **System build deps** — the CI `js-check` job already installs `protobuf-compiler` and uses
  `mold`; confirm ONNX runtime's needs are met there too.

## Done when

- `npm run build` produces a `.node` that `require()`s without error.
- The existing `__tests__/*` (engine, logging, telemetry, smoke) pass with `cognee-lib` linked.
- `.node` size and cold-build time recorded as a baseline in this doc.
