#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAPI_DIR="$(dirname "$SCRIPT_DIR")"

# ── Compile gate (R5) ────────────────────────────────────────────────
# After workspace extraction (D10), the root `cargo check --all-targets`
# no longer covers the capi workspace. We run it here (inside the capi
# workspace) so `scripts/check_all.sh`'s capi stage still catches capi
# compile breaks. Two configurations are checked:
#   1. Default features (full build, mirrors cognee-neon)
#   2. Slim build (--no-default-features --features sqlite,testing) —
#      the embedded/Android baseline (D6)
echo "================================================================"
echo "=== C API: cargo check (default features) ==="
echo "================================================================"
cargo check --all-targets --manifest-path "$CAPI_DIR/Cargo.toml"

echo ""
echo "================================================================"
echo "=== C API: cargo check (slim: --no-default-features --features sqlite,testing) ==="
echo "================================================================"
CARGO_TARGET_DIR="$CAPI_DIR/target/check-slim" \
    cargo check --all-targets \
        --manifest-path "$CAPI_DIR/Cargo.toml" \
        --no-default-features --features sqlite,testing

echo ""
echo "================================================================"
echo "=== C API: Building with CMake ==="
echo "================================================================"

BUILD_DIR="$CAPI_DIR/build"
mkdir -p "$BUILD_DIR"

cmake -S "$CAPI_DIR" -B "$BUILD_DIR" -DCMAKE_BUILD_TYPE=Debug
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
echo "=== Phase 7 slim build: CG_ERR_FEATURE_NOT_BUILT verification ==="
echo "================================================================"

SLIM_BUILD_DIR="$CAPI_DIR/build-slim"
rm -rf "$SLIM_BUILD_DIR"
cmake -S "$CAPI_DIR" -B "$SLIM_BUILD_DIR" \
    -DCMAKE_BUILD_TYPE=Debug \
    -DCOGNEE_CAPI_NO_DEFAULT_FEATURES=ON \
    -DCOGNEE_CAPI_CARGO_FEATURES=sqlite,testing \
    > /dev/null
cmake --build "$SLIM_BUILD_DIR" --target sdk_feature_smoke_slim

echo ""
echo "--- Running: sdk_feature_smoke_slim (slim build — all four ops expect CG_ERR_FEATURE_NOT_BUILT) ---"
MOCK_EMBEDDING=true \
    COGNEE_TRACING_ENABLED="" \
    "$SLIM_BUILD_DIR/examples/sdk_feature_smoke_slim"

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

# Configure a separate CMake build dir that opts the smoke target in
# and passes `--features testing-panic` through to the cargo build
# wrapped by CMake's `cognee_capi_cargo` custom target. The feature is
# purely additive (it only adds an exported symbol), so this rebuild
# only adds `cg_test_force_panic` on top of the existing static lib.
PANIC_BUILD_DIR="$CAPI_DIR/build-panic"
rm -rf "$PANIC_BUILD_DIR"
cmake -S "$CAPI_DIR" -B "$PANIC_BUILD_DIR" \
    -DCMAKE_BUILD_TYPE=Debug \
    -DCOGNEE_BUILD_PANIC_SMOKE=ON \
    -DCOGNEE_CAPI_CARGO_FEATURES=testing-panic \
    > /dev/null
cmake --build "$PANIC_BUILD_DIR" --target panic_hook_smoke

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
echo "================================================================"
echo "=== C API check passed ==="
echo "================================================================"
