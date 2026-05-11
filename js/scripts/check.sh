#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JS_DIR="$(dirname "$SCRIPT_DIR")"

cd "$JS_DIR"

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
echo "=== JS check passed ==="
echo "================================================================"
