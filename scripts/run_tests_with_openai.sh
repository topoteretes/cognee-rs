#!/usr/bin/env bash
# Run the full test suite using the OpenAI-compatible API configured in the environment or .env file.
#
# The `.env` file is loaded automatically by the Rust test utilities via
# `dotenv::dotenv()` — no manual sourcing is needed here.
#
# Required (in environment or .env — canonical Python-compatible names):
#   LLM_API_KEY   — API key              (legacy alias: OPENAI_TOKEN)
#   LLM_ENDPOINT  — API base URL         (legacy alias: OPENAI_URL)
#
# Optional:
#   LLM_MODEL                 — model to use (default: gpt-4o-mini; alias: OPENAI_MODEL)
#   COGNEE_TEST_MODEL_DIR     — directory for embedding model cache
#   COGNEE_E2E_EMBED_MODEL_PATH / COGNEE_E2E_TOKENIZER_PATH — full path overrides

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

# Resolve API key: canonical LLM_API_KEY takes precedence; fall back to OPENAI_TOKEN.
LLM_API_KEY="${LLM_API_KEY:-${OPENAI_TOKEN:-}}"
LLM_ENDPOINT="${LLM_ENDPOINT:-${OPENAI_URL:-}}"
LLM_MODEL="${LLM_MODEL:-${OPENAI_MODEL:-gpt-4o-mini}}"
export LLM_API_KEY LLM_ENDPOINT LLM_MODEL

# Also export legacy aliases so tests that still reference them directly still work.
export OPENAI_TOKEN="${LLM_API_KEY}"
export OPENAI_URL="${LLM_ENDPOINT}"
export OPENAI_MODEL="${LLM_MODEL}"

echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Cognee Workspace Tests${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo

if [[ -z "${LLM_API_KEY:-}" ]]; then
  echo -e "${RED}❌ LLM_API_KEY is not set. Set it in the environment or .env file.${NC}"
  exit 1
fi

if [[ -z "${LLM_ENDPOINT:-}" ]]; then
  echo -e "${RED}❌ LLM_ENDPOINT is not set. Set it in the environment or .env file.${NC}"
  exit 1
fi

MODEL_DIR="${COGNEE_TEST_MODEL_DIR:-$PROJECT_ROOT/target/models}"
setup_embedding_models "$MODEL_DIR"

print_env

TEST_NAME="${1:-}"
cd "$PROJECT_ROOT"

run_cargo_tests "$TEST_NAME"
