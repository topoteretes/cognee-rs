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
echo "=== Python: Installing test dependencies ==="
echo "================================================================"

pip install -e ".[test]"

echo ""
echo "================================================================"
echo "=== Python: Running tests ==="
echo "================================================================"

pytest tests/ -v

echo ""
echo "================================================================"
echo "=== Python: Smoke-testing examples (credential-gated) ==="
echo "================================================================"

# Run the core example only when LLM credentials are present.
# Uses MOCK_EMBEDDING=true to skip the ONNX model download (fast, no GPU).
# Prints a SKIP message and exits 0 when OPENAI_URL or OPENAI_TOKEN is absent,
# matching the C API examples' skip-guard pattern.
if [[ -n "${OPENAI_URL:-}" && -n "${OPENAI_TOKEN:-}" ]]; then
    echo "Credentials detected — running add_cognify_search.py with MOCK_EMBEDDING=true"
    MOCK_EMBEDDING=true python examples/add_cognify_search.py
else
    echo "SKIP: OPENAI_URL or OPENAI_TOKEN not set — skipping example smoke test"
fi

echo ""
echo "================================================================"
echo "=== Python check passed ==="
echo "================================================================"
