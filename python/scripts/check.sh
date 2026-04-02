#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PYTHON_DIR"

echo "================================================================"
echo "=== Python: Building bindings with maturin ==="
echo "================================================================"

if ! command -v maturin &> /dev/null; then
    echo "ERROR: maturin not found. Install it with: pip install maturin"
    exit 1
fi

maturin develop

echo ""
echo "================================================================"
echo "=== Python: Running tests ==="
echo "================================================================"

if ! command -v pytest &> /dev/null; then
    echo "ERROR: pytest not found. Install it with: pip install pytest"
    exit 1
fi

pytest tests/ -v

echo ""
echo "================================================================"
echo "=== Python check passed ==="
echo "================================================================"
