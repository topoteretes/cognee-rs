#!/usr/bin/env bash
# Run the Criterion batch-add-cognify benchmark against the Rust HTTP server.
#
# Ports cognee/tests/performance/batch_add_cognify_test.py.
# Mirrors scripts/run_locust_http_server_bench.sh in structure.
#
# Usage:
#   LLM_API_KEY=sk-... OPENAI_URL=https://api.openai.com/v1 \
#       scripts/run_batch_add_cognify_bench.sh
#
#   # Python-equivalent 200-file run:
#   COGNEE_BENCH_NUM_FILES=200 LLM_API_KEY=sk-... \
#       scripts/run_batch_add_cognify_bench.sh
#
#   # Pass extra Criterion arguments after --:
#   scripts/run_batch_add_cognify_bench.sh -- batch_add_cognify/add
#
# Set COGNEE_HTTP_SERVER_BIN to a pre-built release binary to avoid the
# cargo build cost incurred on each benchmark iteration.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$PROJECT_ROOT"

if [[ -z "${COGNEE_HTTP_SERVER_BIN:-}" ]]; then
    echo "Building cognee-http-server (set COGNEE_HTTP_SERVER_BIN to skip)..." >&2
    cargo build -p cognee-http-server --features bin --release
    export COGNEE_HTTP_SERVER_BIN="$PROJECT_ROOT/target/release/cognee-http-server"
fi

echo "Server binary : $COGNEE_HTTP_SERVER_BIN" >&2
echo "NUM_FILES     : ${COGNEE_BENCH_NUM_FILES:-10 (default)}" >&2

cargo bench -p cognee-bench --bench batch_add_cognify "$@"
