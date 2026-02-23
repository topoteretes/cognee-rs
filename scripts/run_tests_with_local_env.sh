#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Cognee Workspace Tests (Local Env)${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo

model_available_in_endpoint() {
  local base_url="$1"
  local model_name="$2"
  local models_response

  models_response="$(curl -sS --max-time 10 "${base_url%/}/models" || true)"
  [[ -z "$models_response" ]] && return 1

  MODELS_JSON="$models_response" python3 - "$model_name" <<'PY'
import json
import os
import sys

model = sys.argv[1]
raw = os.environ.get('MODELS_JSON', '')
try:
    payload = json.loads(raw)
except Exception:
    raise SystemExit(1)

ids = [item.get('id') for item in payload.get('data', []) if item.get('id')]
raise SystemExit(0 if model in ids else 1)
PY
}

ensure_openai_model_available() {
  local base_url="$1"
  local model_name="$2"

  if model_available_in_endpoint "$base_url" "$model_name"; then
    return 0
  fi

  echo -e "${RED}❌ Model '$model_name' is still unavailable at ${base_url}.${NC}"
  echo -e "${YELLOW}   Set OPENAI_URL to an endpoint where OPENAI_MODEL is available.${NC}"
  return 1
}

download_if_missing() {
  local path="$1"
  local url="$2"
  if [[ -f "$path" ]]; then
    return 0
  fi

  mkdir -p "$(dirname "$path")"
  echo -e "${YELLOW}⬇ Downloading missing artifact:${NC} $(basename "$path")"
  curl -fL "$url" -o "$path"
}

if [[ -z "${OPENAI_URL:-}" ]]; then
  echo -e "${RED}❌ OPENAI_URL must be set explicitly.${NC}"
  exit 1
fi
if [[ -z "${OPENAI_TOKEN:-}" ]]; then
  echo -e "${RED}❌ OPENAI_TOKEN must be set explicitly.${NC}"
  exit 1
fi
if [[ -z "${OPENAI_MODEL:-}" ]]; then
  echo -e "${RED}❌ OPENAI_MODEL must be set explicitly.${NC}"
  exit 1
fi

export OPENAI_URL
export OPENAI_TOKEN
export OPENAI_MODEL

ensure_openai_model_available "$OPENAI_URL" "$OPENAI_MODEL"

MODEL_DIR="${COGNEE_TEST_MODEL_DIR:-$PROJECT_ROOT/target/models}"
export COGNEE_E2E_EMBED_MODEL_PATH="${COGNEE_E2E_EMBED_MODEL_PATH:-$MODEL_DIR/BGE-Small-v1.5-model_quantized.onnx}"
export COGNEE_E2E_TOKENIZER_PATH="${COGNEE_E2E_TOKENIZER_PATH:-$MODEL_DIR/bge-small-tokenizer.json}"

download_if_missing \
  "$COGNEE_E2E_EMBED_MODEL_PATH" \
  "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx"

download_if_missing \
  "$COGNEE_E2E_TOKENIZER_PATH" \
  "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/tokenizer.json"

echo -e "${BLUE}📝 Environment:${NC}"
echo -e "   OPENAI_URL=${OPENAI_URL}"
echo -e "   OPENAI_TOKEN=${OPENAI_TOKEN}"
echo -e "   OPENAI_MODEL=${OPENAI_MODEL}"
echo -e "   COGNEE_E2E_EMBED_MODEL_PATH=${COGNEE_E2E_EMBED_MODEL_PATH}"
echo -e "   COGNEE_E2E_TOKENIZER_PATH=${COGNEE_E2E_TOKENIZER_PATH}"
echo

TEST_NAME="${1:-}"
cd "$PROJECT_ROOT"

echo -e "${BLUE}🧪 Running workspace tests (including LLM/model tests)...${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo

if [[ -n "$TEST_NAME" ]]; then
  cargo test --workspace "$TEST_NAME" -- --nocapture --test-threads=1
else
  cargo test --workspace -- --nocapture --test-threads=1
fi

echo
echo -e "${GREEN}✅ All tests passed.${NC}"
