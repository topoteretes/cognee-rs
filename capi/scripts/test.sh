#!/usr/bin/env bash
# Run the capi workspace's Rust tests (the inline #[cfg(test)] modules in
# cognee-capi) and fail unless they really executed.
#
# Kept separate from check.sh on purpose, and run by its own CI job
# (`capi-rust-tests` in ci.yml, mirrored in community.yml) rather than as a
# step of `capi-check`:
#
#   * capi/ is its own Cargo workspace, so the root `cargo test --workspace`
#     never reaches these tests, and check.sh only does CMake `cargo build`s.
#   * A test build cannot reuse any of the CMake build's artifacts.
#     capi/Cargo.toml sets `panic = "abort"` on the dev profile (the FFI
#     safety floor), while the libtest harness needs unwinding — so cargo
#     rebuilds the entire dependency graph with `panic = "unwind"`. That was
#     measured at ~819s inside capi-check, which is already CI's critical
#     path; as a parallel job it adds no wall-clock time.
#
# It also uses its own CARGO_TARGET_DIR (under capi/target, so clean_all.sh
# and the capi rust-cache still cover it). Sharing capi/target/debug would let
# this default-featured build overwrite the libcognee_capi.{a,so,dylib} that
# check.sh's examples are linked against (built with `testing-panic`), and
# since nothing is shared anyway, a separate dir costs nothing extra.
#
# Default features, i.e. the feature set the C API ships with. No secrets,
# network or models are needed, so this is also safe for fork PRs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAPI_DIR="$(dirname "$SCRIPT_DIR")"
REPO_ROOT="$(dirname "$CAPI_DIR")"

export CARGO_TARGET_DIR="${COGNEE_CAPI_TEST_TARGET_DIR:-$CAPI_DIR/target/rust-tests}"
LOG="$CARGO_TARGET_DIR/capi-rust-tests.log"
mkdir -p "$CARGO_TARGET_DIR"

echo ""
echo "================================================================"
echo "=== C API: Rust unit tests (cargo test, capi workspace) ==="
echo "================================================================"

cargo test --manifest-path "$CAPI_DIR/Cargo.toml" --workspace \
    -- --nocapture 2>&1 | tee "$LOG"

# A green `cargo test` proves nothing about *how many* tests ran: a filter,
# a feature/cfg change or a crate-type change can compile the suite down to
# `running 0 tests` and still exit 0. Floor the count (8 cases today: error,
# exec_status, sdk waiter and task stream modules) and name the waiter
# use-after-free regressions from #361 explicitly.
bash "$REPO_ROOT/scripts/ci/assert_pg_suite_ran.sh" --label CapiRustTests \
    --log "$LOG" --min-tests 8 \
    --require-test waiter_callback_reference_outlives_destroy \
    --require-test waiter_destroy_may_race_with_callback_notify
