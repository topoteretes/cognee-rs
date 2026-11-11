#!/usr/bin/env bash
# Run the capi workspace's Rust tests (the inline #[cfg(test)] modules in
# cognee-capi) and fail unless they really executed.
#
#   * capi/ is its own Cargo workspace, so the root `cargo test --workspace`
#     never reaches these tests, and check.sh only does CMake `cargo build`s.
#     CI runs this as a step of `capi-check` (ci.yml) and as the `capi-rust-tests`
#     lane of community.yml (fork PRs, which skip ci.yml).
#   * Slim feature set (`--no-default-features --features sqlite,testing`): none
#     of the tests touch lancedb, ladybug (lbug's bundled C++), ort or bedrock,
#     so building them would only add ~14 min of cold compile. Slim is ~1 min
#     of wall clock on a dev box. No secrets, network or models are needed.
#   * A test build cannot reuse any of the CMake build's artifacts: capi's dev
#     profile is `panic = "abort"` (the FFI safety floor) while the libtest
#     harness needs unwinding, so cargo rebuilds the dependency graph. It
#     therefore uses its own CARGO_TARGET_DIR (default capi/target/rust-tests,
#     so clean_all.sh still covers it). Sharing capi/target/debug would let this
#     build overwrite the libcognee_capi.{a,so,dylib} that check.sh's examples
#     link against (built with `testing-panic`). CI points it outside the
#     rust-cache'd dirs (COGNEE_CAPI_TEST_TARGET_DIR) so it is rebuilt cold
#     rather than bloating the cache.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAPI_DIR="$(dirname "$SCRIPT_DIR")"
REPO_ROOT="$(dirname "$CAPI_DIR")"

export CARGO_TARGET_DIR="${COGNEE_CAPI_TEST_TARGET_DIR:-$CAPI_DIR/target/rust-tests}"
LOG="$CARGO_TARGET_DIR/capi-rust-tests.log"
mkdir -p "$CARGO_TARGET_DIR"

echo ""
echo "================================================================"
echo "=== C API: Rust unit tests (cargo test, capi workspace, slim) ==="
echo "================================================================"

cargo test --manifest-path "$CAPI_DIR/Cargo.toml" --workspace \
    --no-default-features --features sqlite,testing \
    -- --nocapture 2>&1 | tee "$LOG"

# A green `cargo test` proves nothing about *how many* tests ran: a filter,
# a feature/cfg change or a crate-type change can compile the suite down to
# `running 0 tests` and still exit 0. Floor the count (8 cases today) and name
# every case, so each module (error, exec_status, task, sdk waiter) must have
# run; the two waiter cases are the #361 use-after-free regressions.
bash "$REPO_ROOT/scripts/ci/assert_pg_suite_ran.sh" --label CapiRustTests \
    --log "$LOG" --min-tests 8 \
    --require-test set_last_error_from_stores_and_returns \
    --require-test from_sdk_error_maps_to_sdk_tier_codes \
    --require-test stamp_provenance_nul_node_set_does_not_panic \
    --require-test interior_nul_does_not_panic \
    --require-test callback_stream_yields_items_then_ends_on_ok_completion \
    --require-test callback_stream_surfaces_error_completion_after_items \
    --require-test waiter_callback_reference_outlives_destroy \
    --require-test waiter_destroy_may_race_with_callback_notify
