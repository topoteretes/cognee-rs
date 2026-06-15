#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JS_DIR="$(dirname "$SCRIPT_DIR")"

cd "$JS_DIR"

echo "================================================================"
echo "=== JS: Checking version parity with Cargo workspace ==="
echo "================================================================"

# Fail if js/package.json version has drifted from the root Cargo workspace
# version.  This catches bumps that update Cargo.toml but forget package.json.
WS_VERSION=$(grep -m1 '^version' "$JS_DIR/../Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')
PKG_VERSION=$(node -p "require('$JS_DIR/package.json').version")
if [ "$WS_VERSION" != "$PKG_VERSION" ]; then
  echo "error: version drift — workspace Cargo.toml=${WS_VERSION}, js/package.json=${PKG_VERSION}" >&2
  exit 1
fi
echo "version ok (${PKG_VERSION})"

echo ""
echo "================================================================"
echo "=== JS: Checking Node version ==="
echo "================================================================"

REQUIRED_NODE_MAJOR=16
NODE_VERSION_RAW="$(node --version)"
NODE_MAJOR="$(echo "$NODE_VERSION_RAW" | sed -E 's/^v([0-9]+)\..*/\1/')"
if [[ -z "$NODE_MAJOR" || "$NODE_MAJOR" -lt "$REQUIRED_NODE_MAJOR" ]]; then
  echo "error: node ${NODE_VERSION_RAW} is too old; need >= v${REQUIRED_NODE_MAJOR} (ts-jest uses the 'node:' import scheme)" >&2
  exit 1
fi
echo "node ${NODE_VERSION_RAW} (ok)"

echo ""
echo "================================================================"
echo "=== JS: Installing npm dependencies ==="
echo "================================================================"

npm install

echo ""
echo "================================================================"
echo "=== JS: Building Rust (Neon) and TypeScript ==="
echo "================================================================"

npm run build

echo ""
echo "================================================================"
echo "=== JS: Running tests ==="
echo "================================================================"

npm test

echo ""
echo "================================================================"
echo "=== JS: Smoke-testing examples (credential-gated) ==="
echo "================================================================"

# Run the core example only when LLM credentials are present.
# Uses MOCK_EMBEDDING=true to skip the ONNX model download (fast, no GPU).
# When credentials are absent the example script exits 0 with a SKIP message,
# matching the C API examples' skip-guard pattern.
if [[ -n "${OPENAI_URL:-}" && -n "${OPENAI_TOKEN:-}" ]]; then
    echo "Credentials detected — running add-cognify-search.ts with MOCK_EMBEDDING=true"
    MOCK_EMBEDDING=true npm run example
else
    echo "SKIP: OPENAI_URL or OPENAI_TOKEN not set — skipping example smoke test"
fi

echo ""
echo "================================================================"
echo "=== JS check passed ==="
echo "================================================================"
