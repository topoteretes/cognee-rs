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

# Native build tools. Unlike the sccache probe above -- which degrades
# gracefully -- these are hard requirements for the default feature set this
# script checks: `cmake` drives Ladybug's and AWS-LC's bundled C++ builds, and
# `protoc` compiles LanceDB's Protobuf schemas. Without them the run dies
# minutes later inside a dependency's build script with an error that names
# neither this repo nor the missing package, so fail here instead.
# Docs: docs/build/prerequisites.md
missing_tools=()
for tool in cmake protoc; do
    command -v "$tool" > /dev/null 2>&1 || missing_tools+=("$tool")
done
if [ ${#missing_tools[@]} -gt 0 ]; then
    echo "ERROR: missing native build tools: ${missing_tools[*]}" >&2
    echo "" >&2
    echo "  Debian/Ubuntu: sudo apt-get install -y build-essential cmake protobuf-compiler" >&2
    echo "  Fedora/RHEL:   sudo dnf install -y gcc-c++ cmake protobuf-compiler" >&2
    echo "  macOS:         brew install cmake protobuf" >&2
    echo "  Windows:       choco install cmake protoc --no-progress" >&2
    echo "" >&2
    echo "  See docs/build/prerequisites.md (incl. the GCC 11 workaround)." >&2
    exit 1
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
echo "=== Rust: Compilation check (slim CLI facade) ==="
echo "================================================================"
# crates/cli reaches `cognee` with `default-features = false`, so its own
# feature list can actually subtract. Guard both ends of that seam:
#   1. the bare slim build (no backend at all), and
#   2. `android-default`, the composite that deliberately drops pdfium,
#      postgres, tiktoken and lancedb — and which silently built the full
#      desktop stack for as long as the CLI manifest was missing the flag.
# A compile check alone cannot tell a slim tree from a fat one -- it passes just
# as happily with either, which is how the original break went unnoticed -- so
# the contract check below asserts the android profile's exclusions directly.
cargo check -p cognee-cli --no-default-features
cargo check -p cognee-cli --no-default-features --features cognee-cli/android-default

# Assert the profile's actual contract: android-default excludes pdfium,
# tiktoken, lancedb and the AWS stack. It also excludes `postgres`, but that one
# is NOT asserted here and no marker can assert it: `sqlx-postgres` is in the
# android tree unconditionally, pulled by sea-orm via cognee-search, so its
# presence says nothing about whether cognee/postgres is on.
# Marker crates beat a package count -- a count is
# host-dependent: the pre-fix fat tree measured 690 packages on this host but
# only 523 for aarch64-linux-android, where lancedb is already target-gated
# away. A ceiling loose enough for one is blind for the other -- 520 would have
# missed the real Android regression by 3 packages. `lbug` and `ort` are
# sentinels: android-default genuinely pulls both, so their absence means the
# measurement itself broke rather than the profile being clean.
android_tree=$(cargo tree -p cognee-cli --no-default-features \
    --features cognee-cli/android-default -e normal --prefix none --format '{p}' \
    | sed 's/ (\*)//' | awk '{print $1}' | sort -u)

for sentinel in lbug ort; do
    if ! grep -qx "${sentinel}" <<<"${android_tree}"; then
        echo "ERROR: '${sentinel}' missing from the android-default tree. Expected it to" >&2
        echo "       be present, so this measurement is broken -- not a clean profile." >&2
        exit 1
    fi
done

for forbidden in lancedb pdfium-render tiktoken-rs aws-config; do
    if grep -qx "${forbidden}" <<<"${android_tree}"; then
        echo "ERROR: '${forbidden}' leaked into cognee-cli/android-default, which is" >&2
        echo "       defined to exclude it. A feature forward has stopped subtracting --" >&2
        echo "       check that crates/cli/Cargo.toml still declares" >&2
        echo "       'default-features = false' on the cognee dependency." >&2
        exit 1
    fi
done
echo "android-default excludes lancedb/pdfium/tiktoken/aws (sentinels lbug, ort present)"

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
