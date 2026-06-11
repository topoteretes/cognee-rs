# Phase 0 — Scaffolding & build

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** `cognee-capi` lives in its own `[workspace]` under `capi/` (decision D10),
compiles and links against `cognee-lib` with real backends (qdrant, ladybug, onnx,
tokenizers, sqlite — decision D6), the existing engine examples still pass, and the
size/build-time cost is known. No new public API yet.

## Prerequisites

None (first phase).

## Current state

- `capi/cognee-capi/Cargo.toml` depends on `cognee-core`, `cognee-database` (`sqlite`),
  `cognee-graph` + `cognee-vector` (**`testing` mocks only**), `cognee-logging`,
  `cognee-observability`, `cognee-telemetry`. No `cognee-lib`.
- `cognee-capi` IS a workspace member (root `Cargo.toml:38`), with a standing TODO to extract
  it into its own workspace.
- Build: CMake drives cargo (`capi/scripts/check.sh`); cbindgen runs from `build.rs` and the
  generated header is committed at `capi/include/cognee.h`.
- `cognee-neon` (the reference) declares: `cognee-lib = { path, default-features = false }`
  plus features `visualization`, `cloud`, `qdrant`, `ladybug`, `onnx`, `hf-tokenizer`,
  `tiktoken`, `sqlite`, `testing` as its defaults (`js/cognee-neon/Cargo.toml`).

## Tasks

1. **Extract the workspace (D10)**: add a `[workspace]` root under `capi/` (either in
   `capi/cognee-capi/Cargo.toml` itself or a thin `capi/Cargo.toml`), remove
   `"capi/cognee-capi"` from the root workspace `members`, and resolve the root `Cargo.toml`
   TODO comment. Mirror the root `[patch.crates-io]` table exactly the way
   `js/cognee-neon/Cargo.toml` does (qdrant `tar`/`tonic`/`hyper` forks), and add a
   "keep in sync with the root workspace" comment in **all three** patch tables (root, js,
   capi). Inherited `*.workspace = true` keys (version/edition/deps) must be replaced with
   explicit values or a local `[workspace.package]`. Verify `scripts/check_all.sh`'s capi
   stage still works (it invokes `capi/scripts/check.sh`, which builds via CMake → cargo, so
   the separate workspace is transparent to it).

2. **Add the `cognee-lib` dependency** to `capi/cognee-capi/Cargo.toml`:
   ```toml
   cognee-lib = { path = "../../crates/lib", default-features = false }
   ```
   Keep the existing direct deps (`cognee-core` etc.) — they must resolve to the same versions
   `cognee-lib` pulls in (same note as in the neon Cargo.toml).

3. **Define the feature matrix**, mirroring `cognee-neon`:
   ```toml
   [features]
   default = ["visualization", "cloud", "qdrant", "ladybug", "onnx",
              "hf-tokenizer", "tiktoken", "sqlite", "testing"]
   visualization = ["cognee-lib/visualization"]
   cloud         = ["cognee-lib/cloud"]
   qdrant        = ["cognee-lib/qdrant"]
   ladybug       = ["cognee-lib/ladybug"]
   onnx          = ["cognee-lib/onnx"]
   hf-tokenizer  = ["cognee-lib/hf-tokenizer"]
   tiktoken      = ["cognee-lib/tiktoken"]
   sqlite        = ["cognee-lib/sqlite"]
   testing       = ["cognee-lib/testing"]
   testing-panic = []   # existing, unchanged
   ```
   Rationale for `testing` in defaults: `cg_task_context_mock` (existing public API) requires
   the mock graph/vector backends; same reason cognee-neon keeps it.
   Note for embedded/Android consumers: a slim build is
   `--no-default-features --features sqlite` (+ picks); verify it still compiles (the SDK
   functions land in later phases with `FeatureNotBuilt` bodies where applicable).

4. **CMake/check.sh plumbing**: ensure `capi/scripts/check.sh` still builds (the cargo
   invocation may need `--features`/`--no-default-features` knobs threaded through CMake the
   way `COGNEE_CAPI_CARGO_FEATURES` already is for `testing-panic`). Add a slim-build job
   (`--no-default-features --features sqlite,testing`) to `capi-check.yml`.

5. **Restore the compile gate**: after extraction, the root-workspace
   `cargo check --all-targets` (the standard dev workflow) no longer covers capi at all, and
   the CMake-driven check.sh only builds the default configuration. Add an explicit
   `cargo check --all-targets` (run inside the capi workspace, default + slim feature sets)
   to `scripts/check_all.sh`'s capi stage (or to `capi/scripts/check.sh` itself, which
   check_all invokes), so a plain `scripts/check_all.sh` still catches capi compile breaks.

6. **Record baselines** in the STATUS notes: `libcognee_capi.{a,so}` size and clean-build wall
   time, before (engine-only) and after (full lib). This informs the embedded story.

## Exit criteria

- [ ] capi extracted into its own workspace; root workspace no longer lists it; root TODO
      resolved; patch tables mirrored + cross-referenced in comments
- [ ] `cargo check --all-targets` green in both workspaces; the capi check wired into
      `scripts/check_all.sh`'s capi stage (default + slim feature sets)
- [ ] `capi/scripts/check.sh` fully green (6 examples + 3 smoke tests unchanged)
- [ ] slim build (`--no-default-features --features sqlite,testing`) compiles (CI job added)
- [ ] size/time baseline recorded in [STATUS.md](STATUS.md)

## Risks

- **Patch drift**: the qdrant fork patches now live in three places (root, js, capi); a
  version bump in one must be propagated. Mitigation: cross-referencing comments now, a
  drift-check script later if it bites.
- **Separate target dir**: the capi workspace no longer shares the root build cache — cold
  `capi-check` CI builds get slower; configure cargo caching for the new path.
- **CMake + heavy deps**: ONNX runtime download/build under the CMake-driven cargo build may
  need network or cache configuration in CI (`capi-check.yml`).
