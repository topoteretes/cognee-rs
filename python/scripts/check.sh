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

# Drop any stale prebuilt extension module from a previous Python version or
# architecture so it cannot shadow the freshly-built one.
rm -f cognee_py/_native*.so

# --extras installs the [test] extra's dependencies as part of this same
# dev-profile install, which is why there is no `pip install -e ".[test]"`
# afterwards. That command looked free but was not: it handed the build to the
# maturin PEP-517 backend, which defaults to --release, so the whole ~630-crate
# graph was compiled a second time in a second profile — measured at 9.8 GiB of
# target/release and ~4 minutes — purely to land two pure-Python wheels.
#
# Let maturin/pip resolve the specifiers rather than parsing pyproject.toml
# here: they handle PEP 508 markers, quoting and spaces correctly, and this
# script then needs no TOML parser (python < 3.11 has no stdlib tomllib).
maturin develop --extras test

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
