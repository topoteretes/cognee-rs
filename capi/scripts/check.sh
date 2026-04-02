#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAPI_DIR="$(dirname "$SCRIPT_DIR")"

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

echo ""
echo "================================================================"
echo "=== C API check passed ==="
echo "================================================================"
