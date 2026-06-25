#!/usr/bin/env bash
# check_all.sh — Run all checks: formatting, compilation, clippy, and wrapper binding tests.
# Run this before completing any set of changes to ensure nothing is broken.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$REPO_ROOT"

echo "================================================================"
echo "=== Rust: Checking formatting ==="
echo "================================================================"
cargo fmt --all -- --check

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
echo "=== Rust: Compilation check (no default features, cognee-lib) ==="
echo "================================================================"
cargo check -p cognee-lib --no-default-features

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
echo "=== All checks passed! ==="
echo "================================================================"
