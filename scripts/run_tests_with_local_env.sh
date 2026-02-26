#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Cognee Workspace Tests (Local Env)${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo

detect_openai_url() {
  local candidates=(
    "${OPENAI_URL:-}"
    "http://localhost:11435/v1"
    "http://localhost:11434/v1"
  )

  for candidate in "${candidates[@]}"; do
    [[ -z "$candidate" ]] && continue
    if curl -sS --max-time 5 "${candidate%/}/models" >/dev/null 2>&1; then
      echo "$candidate"
      return 0
    fi
  done

  return 1
}

detect_openai_model() {
  local base_url="$1"
  local models_response
  models_response="$(curl -sS --max-time 10 "${base_url%/}/models" || true)"

  if [[ -z "$models_response" ]]; then
    return 1
  fi

  local available_models
  available_models="$(MODELS_JSON="$models_response" python3 - <<'PY'
import json, os

raw = os.environ.get('MODELS_JSON', '')
try:
    payload = json.loads(raw)
except Exception:
    raise SystemExit(1)

for item in payload.get('data', []):
    model_id = item.get('id')
    if model_id:
        print(model_id)
PY
  )"

  if [[ -z "$available_models" ]]; then
    return 1
  fi

  local preferred_models=(
    "llama3.2:3b"
    "llama3.1:8b"
    "llama3:8b"
    "mistral:7b"
    "qwen2.5:7b"
    "qwen2.5:3b"
    "qwen3:0.6b"
    "qwen3:4b"
  )

  local model
  for model in "${preferred_models[@]}"; do
    if printf '%s\n' "$available_models" | grep -Fxq "$model"; then
      echo "$model"
      return 0
    fi
  done

  local first_non_embedding
  first_non_embedding="$(printf '%s\n' "$available_models" | grep -Evi 'embed|embedding|bge|nomic' | head -n 1 || true)"
  if [[ -n "$first_non_embedding" ]]; then
    echo "$first_non_embedding"
    return 0
  fi

  printf '%s\n' "$available_models" | head -n 1
}

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

  echo -e "${YELLOW}⚠ Model '$model_name' not available at ${base_url}.${NC}"

  if [[ "$base_url" =~ ^http://(localhost|127\.0\.0\.1):([0-9]+)/v1/?$ ]]; then
    local host="${BASH_REMATCH[1]}"
    local port="${BASH_REMATCH[2]}"
    local startup_port="$port"

    if ! command -v docker >/dev/null 2>&1; then
      echo -e "${RED}❌ Docker is not installed/running, cannot auto-pull model '$model_name'.${NC}"
      return 1
    fi

    echo -e "${BLUE}🐳 Attempting to start/update local Ollama Docker with model '$model_name'...${NC}"
    if ! (
      cd "$PROJECT_ROOT/docker/ollama"
      MODEL_NAME="$model_name" \
      MODEL_NAMES="${MODEL_NAMES:-${model_name},llama3.2:3b,llama3.1:8b}" \
      MODEL="$model_name" \
      PORT="$startup_port" \
      ./start.sh
    ); then
      local alternate_port="11435"
      if [[ "$startup_port" == "11435" ]]; then
        alternate_port="11434"
      fi

      echo -e "${YELLOW}⚠ Ollama startup failed on port ${startup_port}, retrying on ${alternate_port}.${NC}"

      (
        cd "$PROJECT_ROOT/docker/ollama"
        MODEL_NAME="$model_name" \
        MODEL_NAMES="${MODEL_NAMES:-${model_name},llama3.2:3b,llama3.1:8b}" \
        MODEL="$model_name" \
        PORT="$alternate_port" \
        ./start.sh
      )

      startup_port="$alternate_port"
    fi

    OPENAI_URL="http://${host}:${startup_port}/v1"
    export OPENAI_URL
    echo -e "${BLUE}ℹ Using OPENAI_URL=${OPENAI_URL}${NC}"

    local i
    for i in {1..30}; do
      if model_available_in_endpoint "$OPENAI_URL" "$model_name"; then
        echo -e "${GREEN}✓ Model '$model_name' is now available.${NC}"
        return 0
      fi
      sleep 2
    done
  fi

  echo -e "${RED}❌ Model '$model_name' is still unavailable at ${base_url}.${NC}"
  echo -e "${YELLOW}   Set OPENAI_MODEL to an available model or start docker/ollama with that model.${NC}"
  return 1
}

OPENAI_URL_DETECTED="$(detect_openai_url || true)"
if [[ -z "$OPENAI_URL_DETECTED" ]]; then
  echo -e "${RED}❌ Could not reach a local OpenAI-compatible endpoint.${NC}"
  echo -e "${YELLOW}   Tried: ${OPENAI_URL:-<unset>}, http://localhost:11435/v1, http://localhost:11434/v1${NC}"
  exit 1
fi

export OPENAI_URL="$OPENAI_URL_DETECTED"
export OPENAI_TOKEN="${OPENAI_TOKEN:-not-needed}"

if [[ -n "${OPENAI_MODEL:-}" ]]; then
  export OPENAI_MODEL
else
  DETECTED_MODEL="$(detect_openai_model "$OPENAI_URL" || true)"
  export OPENAI_MODEL="${DETECTED_MODEL:-llama3.1:8b}"
fi

ensure_openai_model_available "$OPENAI_URL" "$OPENAI_MODEL"

MODEL_DIR="${COGNEE_TEST_MODEL_DIR:-$PROJECT_ROOT/target/models}"
setup_embedding_models "$MODEL_DIR"

print_env

TEST_NAME="${1:-}"
cd "$PROJECT_ROOT"

run_cargo_tests "$TEST_NAME"
