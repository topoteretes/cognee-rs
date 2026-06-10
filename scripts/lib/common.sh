#!/usr/bin/env bash
# Shared utilities for cognee-rust test scripts. Source this file; do not execute directly.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'


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
