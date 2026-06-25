#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BENCH_DIR="$PROJECT_ROOT/e2e-cross-sdk/performance/locust"

if [[ ! -d "$BENCH_DIR" ]]; then
  echo "Benchmark directory not found: $BENCH_DIR" >&2
  exit 1
fi

cd "$BENCH_DIR"

if [[ -z "${VIRTUAL_ENV:-}" ]]; then
  echo "warning: no active virtualenv; using system python" >&2
fi

python3 -m pip install -r requirements.txt
python3 locust_performance_analysis.py "$@"
