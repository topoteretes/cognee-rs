#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAPI_DIR="$(dirname "$SCRIPT_DIR")"

# ── Compile gate (R5) ────────────────────────────────────────────────
# After workspace extraction (D10), the root `cargo check --all-targets`
# no longer covers the capi workspace, so the capi stage has to cover it.
#
# This used to be two standalone `cargo check --all-targets` passes (default
# features, then slim in its own CARGO_TARGET_DIR). Both were near-pure
# duplicate work: the two CMake builds further down run full `cargo build`s of
# the same crate at the same two feature sets, and a build strictly subsumes a
# check. Measured on CI run 31029376408 the two checks cost 475s + 420s = 15
# min of a 38.7-min job — while every C smoke test in this script combined
# takes 13.5s.
#
# What the checks covered that the CMake builds do not is cargo *test*
# targets — cognee-capi has inline #[cfg(test)] modules (src/error.rs,
# src/exec_status.rs) that only compile under --all-targets. Note they were
# only ever *compiled* here, never run: nothing in CI executes the capi
# workspace's Rust unit tests (the root `cargo test --workspace` does not
# reach into capi/, which is a separate workspace).
#
# Restoring that compile-check via `cargo test --no-run` after the CMake build
# was tried and measured at 819s — as expensive as the checks it replaced,
# because `cargo test` pulls in dev-dependencies and rebuilds the lib as a
# test harness rather than reusing the CMake build's artifacts. It was dropped
# again. If those two test modules are worth covering, the cheap fix is to
# make them *run* somewhere sensible rather than to buy a compile-only check
# here for ~14 min of every CI run.
echo ""
echo "================================================================"
echo "=== C API: Header sync check (exported vs. declared symbols) ==="
echo "================================================================"
bash "$SCRIPT_DIR/check_header_sync.sh"

echo ""
echo "================================================================"
echo "=== C API: Building with CMake ==="
echo "================================================================"

BUILD_DIR="$CAPI_DIR/build"
mkdir -p "$BUILD_DIR"

# `testing-panic` is enabled here rather than in a second build dir. It is
# purely additive — it exports one extra symbol, `cg_test_force_panic` — so it
# does not change what any other smoke test exercises, and building it in the
# main tree lets `panic_hook_smoke` (below) reuse this build instead of
# triggering a whole second cargo build + archive + relink cycle.
#
# It is safe for the header-sync check: check_header_sync.sh greps
# `pub extern "C" fn` out of the *source*, which sees the symbol whether or not
# the feature is on, so enabling it changes nothing there.
cmake -S "$CAPI_DIR" -B "$BUILD_DIR" \
    -DCMAKE_BUILD_TYPE=Debug \
    -DCOGNEE_BUILD_PANIC_SMOKE=ON \
    -DCOGNEE_CAPI_CARGO_FEATURES=testing-panic
cmake --build "$BUILD_DIR"

echo ""
echo "================================================================"
echo "=== C API: Running examples ==="
echo "================================================================"

EXAMPLES=(
    example_sync_task
    example_async_task
    example_iter_task
    example_batch_task
    example_pipeline
    example_cancellation
    example_background_task
)

for example in "${EXAMPLES[@]}"; do
    echo ""
    echo "--- Running: $example ---"
    "$BUILD_DIR/examples/$example"
done

# Redirect SDK runtime artifacts into the build dir so that .cognee_system/,
# .data_storage/, and cognee.db never appear as untracked files in the repo root.
RUNTIME_DIR="$BUILD_DIR/cognee-runtime"
mkdir -p "$RUNTIME_DIR"
export COGNEE_SYSTEM_ROOT_DIRECTORY="$RUNTIME_DIR/.cognee_system"
export COGNEE_DATA_ROOT_DIRECTORY="$RUNTIME_DIR/.data_storage"
export DATABASE_URL="sqlite:$RUNTIME_DIR/cognee.db?mode=rwc"

echo ""
echo "================================================================"
echo "=== Phase 1b SDK handle smoke test (Tier-A, mock embedding) ==="
echo "================================================================"

echo ""
echo "--- Running: sdk_handle_smoke (MOCK_EMBEDDING=true, no network) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$BUILD_DIR/examples/sdk_handle_smoke"

echo ""
echo "================================================================"
echo "=== Phase 2 conventions smoke tests ==="
echo "================================================================"

echo ""
echo "--- Running: sdk_conventions_smoke (R1 deferred-delivery, MOCK_EMBEDDING=true) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$BUILD_DIR/examples/sdk_conventions_smoke"

echo ""
echo "--- Running: sdk_negative_path_smoke (bad-JSON + single-use guard) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$BUILD_DIR/examples/sdk_negative_path_smoke"

echo ""
echo "================================================================"
echo "=== Phase 3 config surface smoke test ==="
echo "================================================================"

echo ""
echo "--- Running: sdk_config_smoke (set/get round-trip, error codes, rebuild-on-change) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$BUILD_DIR/examples/sdk_config_smoke"

echo ""
echo "================================================================"
echo "=== Phase 4 core ops smoke test (Tier-A, mock embedding) ==="
echo "================================================================"

echo ""
echo "--- Running: example_sdk_add (add/dedup counts, MOCK_EMBEDDING=true) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$BUILD_DIR/examples/example_sdk_add"

echo ""
echo "================================================================"
echo "=== Phase 4 live add+cognify (Tier-B, skips without credentials) ==="
echo "================================================================"

echo ""
if [ -n "${OPENAI_URL:-}" ] && [ -n "${OPENAI_TOKEN:-}" ]; then
    echo "--- Running: example_sdk_add_cognify (live, OPENAI_URL set) ---"
    MOCK_EMBEDDING=true \
        COGNEE_TRACING_ENABLED="" \
        "$BUILD_DIR/examples/example_sdk_add_cognify"
else
    echo "--- Skipping: example_sdk_add_cognify (OPENAI_URL/OPENAI_TOKEN not set) ---"
fi

echo ""
echo "================================================================"
echo "=== Phase 5 retrieval smoke test (Tier-A, mock embedding) ==="
echo "================================================================"

echo ""
echo "--- Running: sdk_retrieval_smoke (search/recall, MOCK_EMBEDDING=true) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$BUILD_DIR/examples/sdk_retrieval_smoke"

echo ""
echo "================================================================"
echo "=== Phase 5 live add+cognify+search (Tier-B, skips without credentials) ==="
echo "================================================================"

echo ""
if [ -n "${OPENAI_URL:-}" ] && [ -n "${OPENAI_TOKEN:-}" ]; then
    echo "--- Running: example_sdk_add_cognify_search (live, OPENAI_URL set) ---"
    MOCK_EMBEDDING=true \
        COGNEE_TRACING_ENABLED="" \
        "$BUILD_DIR/examples/example_sdk_add_cognify_search"
else
    echo "--- Skipping: example_sdk_add_cognify_search (OPENAI_URL/OPENAI_TOKEN not set) ---"
fi

echo ""
echo "================================================================"
echo "=== Phase 6 data-ops smoke test (Tier-A) ==="
echo "================================================================"

echo ""
echo "--- Running: sdk_data_smoke (forget/prune/datasets, MOCK_EMBEDDING=true) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$BUILD_DIR/examples/sdk_data_smoke"

echo ""
echo "================================================================"
echo "=== Phase 7 feature-gated smoke test (default build) ==="
echo "================================================================"

echo ""
echo "--- Running: sdk_feature_smoke (MOCK_EMBEDDING=true, default features) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$BUILD_DIR/examples/sdk_feature_smoke"


echo ""
echo "================================================================"
echo "=== Gap 07 smoke tests (OTLP + analytics init) ==="
echo "================================================================"

echo ""
echo "--- Running: init_otlp_smoke (no-config, idempotent) ---"
env -u OTEL_EXPORTER_OTLP_ENDPOINT -u COGNEE_TRACING_ENABLED \
    "$BUILD_DIR/examples/init_otlp_smoke"

echo ""
echo "--- Running: init_telemetry_smoke (default policy) ---"
env -u TELEMETRY_DISABLED -u COGNEE_HOST_SDK -u ENV \
    "$BUILD_DIR/examples/init_telemetry_smoke"

echo ""
echo "--- Running: init_telemetry_smoke (TELEMETRY_DISABLED=1 suppresses) ---"
SUPPRESSED_OUT=$(env -u COGNEE_HOST_SDK -u ENV TELEMETRY_DISABLED=1 \
    "$BUILD_DIR/examples/init_telemetry_smoke")
if [ "$SUPPRESSED_OUT" != "not_armed" ]; then
    echo "FAIL: expected 'not_armed', got '$SUPPRESSED_OUT'" >&2
    exit 1
fi
echo "  policy suppression OK"

echo ""
echo "================================================================"
echo "=== Gap 07 panic-hook smoke (testing-panic feature) ==="
echo "================================================================"

# panic_hook_smoke is built by the main CMake configure above, which enables
# both COGNEE_BUILD_PANIC_SMOKE and the additive `testing-panic` feature. It
# used to get its own build dir, which meant a second full cargo build with a
# different feature set, a re-archive of the ~805 MB .a into the shared
# capi/target/debug, and a relink of every example on the following run.
PANIC_BUILD_DIR="$BUILD_DIR"

echo ""
echo "--- Running: panic_hook_smoke (expect [cognee-capi panic] on stderr, non-zero exit) ---"
PANIC_STDERR=$(mktemp)
set +e
"$PANIC_BUILD_DIR/examples/panic_hook_smoke" 2>"$PANIC_STDERR"
PANIC_EXIT=$?
set -e
if [ $PANIC_EXIT -eq 0 ]; then
    echo "FAIL: panic_hook_smoke exited 0 (panic did not propagate)" >&2
    cat "$PANIC_STDERR" >&2
    rm -f "$PANIC_STDERR"
    exit 1
fi
if ! grep -q "\[cognee-capi panic\]" "$PANIC_STDERR"; then
    echo "FAIL: panic marker '[cognee-capi panic]' not found on stderr" >&2
    cat "$PANIC_STDERR" >&2
    rm -f "$PANIC_STDERR"
    exit 1
fi
echo "  panic hook fired with marker on stderr (exit=$PANIC_EXIT)"
rm -f "$PANIC_STDERR"

echo ""
# Kept last on purpose. This build shares capi/target with the default one,
# so it overwrites libcognee_capi.{a,dylib} with the slim-featured variant —
# and every other example is dynamically linked against that dylib, so any
# $BUILD_DIR binary run after this point would fail at load time with a
# missing symbol (cg_test_force_panic was the one that caught this).
#
# Redirecting it to its own CARGO_TARGET_DIR would also avoid the clobber but
# costs 3.8 GiB — check-slim goes 1.6 -> 5.4 GiB, holding a full build rather
# than just `cargo check` metadata. Ordering is free, so order it is.
echo "================================================================"
echo "=== Phase 7 slim build: CG_ERR_FEATURE_NOT_BUILT verification ==="
echo "================================================================"

SLIM_BUILD_DIR="$CAPI_DIR/build-slim"
rm -rf "$SLIM_BUILD_DIR"
cmake -S "$CAPI_DIR" -B "$SLIM_BUILD_DIR" \
    -DCMAKE_BUILD_TYPE=Debug \
    -DCOGNEE_CAPI_NO_DEFAULT_FEATURES=ON \
    -DCOGNEE_CAPI_CARGO_FEATURES=sqlite,testing,lancedb \
    > /dev/null
cmake --build "$SLIM_BUILD_DIR" --target sdk_feature_smoke_slim

echo ""
echo "--- Running: sdk_feature_smoke_slim (slim build — all four ops expect CG_ERR_FEATURE_NOT_BUILT) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$SLIM_BUILD_DIR/examples/sdk_feature_smoke_slim"

# Restore the default-featured library. The slim build above shares
# capi/target, so it leaves libcognee_capi.{a,dylib} as the
# --no-default-features variant — and every example in build/ is dynamically
# linked against that absolute path. Without this, re-running any smoke by hand
# after a green check (./capi/build/examples/sdk_data_smoke, panic_hook_smoke)
# fails with a missing symbol or a spurious CG_ERR_FEATURE_NOT_BUILT. Cheap:
# one cargo rebuild of cognee-capi plus ~50 KB relinks.
echo ""
echo "--- Restoring the default-featured libcognee_capi for build/ ---"
cmake --build "$BUILD_DIR" > /dev/null
echo ""
echo "================================================================"
echo "=== C API check passed ==="
echo "================================================================"
