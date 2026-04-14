#!/usr/bin/env bash
# Shared utilities for cognee-rust test scripts. Source this file; do not execute directly.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# download_if_missing <local_path> <url>
# Creates parent directories and downloads the file only if it is absent.
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

# setup_embedding_models <model_dir>
# Sets and exports COGNEE_E2E_EMBED_MODEL_PATH and COGNEE_E2E_TOKENIZER_PATH,
# then downloads the BGE-Small ONNX model and tokenizer if not already present.
setup_embedding_models() {
  local model_dir="$1"

  export COGNEE_E2E_EMBED_MODEL_PATH="${COGNEE_E2E_EMBED_MODEL_PATH:-$model_dir/BGE-Small-v1.5-model_quantized.onnx}"
  export COGNEE_E2E_TOKENIZER_PATH="${COGNEE_E2E_TOKENIZER_PATH:-$model_dir/bge-small-tokenizer.json}"

  download_if_missing \
    "$COGNEE_E2E_EMBED_MODEL_PATH" \
    "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx"

  download_if_missing \
    "$COGNEE_E2E_TOKENIZER_PATH" \
    "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/tokenizer.json"
}

# print_env — prints the 5 key test environment variables
print_env() {
  echo -e "${BLUE}📝 Environment:${NC}"
  echo -e "   OPENAI_URL=${OPENAI_URL}"
  echo -e "   OPENAI_TOKEN=${OPENAI_TOKEN}"
  echo -e "   OPENAI_MODEL=${OPENAI_MODEL}"
  echo -e "   COGNEE_E2E_EMBED_MODEL_PATH=${COGNEE_E2E_EMBED_MODEL_PATH}"
  echo -e "   COGNEE_E2E_TOKENIZER_PATH=${COGNEE_E2E_TOKENIZER_PATH}"
  echo
}

# run_cargo_tests [test_name]
# Runs the workspace test suite. Uses `cargo nextest run` when available
# (faster compile scheduling, cleaner output) and falls back to `cargo test`
# otherwise. `--no-capture` forces serial execution, which matches the
# prior `--test-threads=1` requirement that many LLM tests rely on.
# Doctests are run separately because nextest does not execute them yet.
# Caller must cd to PROJECT_ROOT first.
run_cargo_tests() {
  local test_name="${1:-}"

  echo -e "${BLUE}🧪 Running workspace tests (including LLM/model tests)...${NC}"
  echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
  echo

  if command -v cargo-nextest >/dev/null 2>&1; then
    if [[ -n "$test_name" ]]; then
      cargo nextest run --workspace --no-capture "$test_name"
    else
      cargo nextest run --workspace --no-capture
    fi
    # nextest does not run doctests; exercise them separately.
    cargo test --workspace --doc -- --nocapture
  else
    if [[ -n "$test_name" ]]; then
      cargo test --workspace "$test_name" -- --nocapture --test-threads=1
    else
      cargo test --workspace -- --nocapture --test-threads=1
    fi
  fi

  echo
  echo -e "${GREEN}✅ All tests passed.${NC}"
}
