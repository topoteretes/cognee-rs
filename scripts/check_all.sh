#!/usr/bin/env bash
# check_all.sh — Run all checks: formatting, compilation, clippy, and wrapper binding tests.
# Run this before completing any set of changes to ensure nothing is broken.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$REPO_ROOT"

# Route Rust compilation through sccache when it is installed. Set here rather
# than as `build.rustc-wrapper` in .cargo/config.toml, and pointed at the
# sccache binary rather than at a wrapper script, because both alternatives are
# broken in ways that only show up off this developer's machine:
#
#   * A `#!/bin/sh` wrapper drops every environment variable whose name is not
#     a valid shell identifier. On Linux /bin/sh is dash, which discards
#     CARGO_BIN_EXE_cognee-cli before exec'ing rustc, so crates/cli/tests
#     fails to compile. macOS /bin/sh is bash and keeps it, so the breakage is
#     invisible locally and red on every Linux runner.
#   * A committed config.toml entry applies to Windows too, where CreateProcess
#     cannot launch a .sh at all — that would break every Windows release leg.
#     (capi-release.yml already resets the CMake launcher vars on Windows for
#     exactly this reason.)
#
# Exec'ing the sccache binary directly has neither problem, and a caller-set
# RUSTC_WRAPPER still wins.
if command -v sccache > /dev/null 2>&1; then
    export RUSTC_WRAPPER="${RUSTC_WRAPPER:-sccache}"
fi

echo "================================================================"
echo "=== Rust: Checking formatting ==="
echo "================================================================"
cargo fmt --all -- --check

echo ""
echo "================================================================"
echo "=== bindings/: prebuild-workflow path contract ==="
echo "================================================================"
# Sub-second. ts-prebuild.yml and java-prebuild.yml run only on tags and
# workflow_dispatch, so their paths are otherwise first exercised during a
# release; this asserts the contract instead of building it.
bash "$REPO_ROOT/ci/assert-bindings-layout.sh"

echo ""
echo "================================================================"
echo "=== Rust: Checking compilation (all targets) ==="
echo "================================================================"
cargo check --all-targets

echo ""
echo "================================================================"
echo "=== Rust: Running Clippy (all targets) ==="
echo "================================================================"
cargo clippy --all-targets -- -D warnings

echo ""
echo "================================================================"
echo "=== Rust: Compilation check (telemetry feature) ==="
echo "================================================================"
cargo check --all-targets --features telemetry

echo ""
echo "================================================================"
echo "=== Rust: Compilation check (no default features, cognee) ==="
echo "================================================================"
cargo check -p cognee --no-default-features

echo ""
echo "================================================================"
echo "=== Rust: Test (cognee-vector lancedb lane) ==="
echo "================================================================"
# crates/vector declares no default features, so a per-crate `cargo test -p
# cognee-vector` runs ZERO of the inline LanceDB adapter tests. Only the
# workspace-wide run covers them today (via feature unification from `cognee`),
# which hides a break behind an unrelated dependency edge. Spell the lane out.
cargo test -p cognee-vector --features lancedb

echo ""
echo "================================================================"
echo "=== Rust: Compilation check (Postgres-only server: no onnx/ladybug/lancedb) ==="
echo "================================================================"
# Guards the seam a downstream consumer uses to drop the embedded backends —
# ort (onnx), bundled ladybug C++, and the Arrow + lance stack — and run every
# store on Postgres instead. Without a lane like this the `#[cfg(feature = ...)]`
# paths behind those features rot and the seam silently stops building.
# Scoped to cognee-http-server to stay cheap.
cargo check -p cognee-http-server --no-default-features \
  --features telemetry,html-loader,pgvector,pggraph

echo ""
echo "================================================================"
echo "=== Rust: wasm32 Config-1 (logic crates + wasm test drift guard) ==="
echo "================================================================"
# The wasm smoke-test files are #![cfg(target_arch = "wasm32")], so the native
# `cargo check --all-targets` above compiles them to empty crates and never
# type-checks them. Type-check the wasm *test* build of every crate whose wasm
# test layer this repo gates: utils/models (the tokio dev-dep split + the
# cfg(not(wasm32)) gates on their retry/data_input test modules) and chunking's
# smoke tests (DocumentChunk/chunk_text drift, incl. the shared wasm_smoke
# module). Run chunking under both feature configs so the default build of
# tests/wasm.rs is covered, not just the tiktoken one. Build-only (--no-run):
# running the tests needs Node + wasm-bindgen-cli (see ci.yml's wasm job and
# docs/spike-wasm-config1.md). The target install is a no-op once present.
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
cargo test -p cognee-utils -p cognee-models --target wasm32-unknown-unknown --no-run
# Spell chunking's wasm feature set explicitly (--no-default-features
# [--features tiktoken]) — identical to ci.yml's wasm job — so local and CI
# checks compile the same set even if cognee-chunking's `default` ever grows.
cargo test -p cognee-chunking --no-default-features --target wasm32-unknown-unknown --no-run
cargo test -p cognee-chunking --no-default-features --features tiktoken --target wasm32-unknown-unknown --no-run

echo ""
echo "================================================================"
echo "=== Rust: Test (telemetry crate noop fallback) ==="
echo "================================================================"
# Mirrors the no-default-features test lane in .github/workflows/ci.yml.
# Exercises crates/telemetry/tests/noop_fallback.rs at runtime so the
# cfg(not(feature = "telemetry")) path catches regressions locally before
# they reach CI. Separate CARGO_TARGET_DIR keeps the noop build's rustc
# fingerprint distinct from the workspace's default-features build.
CARGO_TARGET_DIR=target/check-noop \
    cargo test -p cognee-telemetry --no-default-features --tests

echo ""
echo "================================================================"
echo "=== C API: Building and running examples ==="
echo "================================================================"
"$REPO_ROOT/capi/scripts/check.sh"

echo ""
echo "================================================================"
echo "=== Python: Building and running tests ==="
echo "================================================================"
"$REPO_ROOT/python/scripts/check.sh"

echo ""
echo "================================================================"
echo "=== JS/TS: Building and running tests ==="
echo "================================================================"
"$REPO_ROOT/ts/scripts/check.sh"

echo ""
echo "================================================================"
echo "=== Java: Building bindings and running tests ==="
echo "================================================================"
"$REPO_ROOT/java/scripts/check.sh"

echo ""
echo "================================================================"
echo "=== All checks passed! ==="
echo "================================================================"
