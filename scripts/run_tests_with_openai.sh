#!/usr/bin/env bash
# Run the full test suite using the OpenAI-compatible API configured in the environment or .env file.
#
# Required (from environment or .env):
#   OPENAI_URL   — base URL for the OpenAI-compatible API
#   OPENAI_TOKEN — API token
#
# Optional (from environment or .env):
#   OPENAI_MODEL              — model to use (default: gpt-4o-mini)
#   COGNEE_TEST_MODEL_DIR     — directory for embedding model cache
#   COGNEE_E2E_EMBED_MODEL_PATH / COGNEE_E2E_TOKENIZER_PATH — full path overrides

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

# Load .env if present (only sets variables not already in the environment)
if [[ -f "$PROJECT_ROOT/.env" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$PROJECT_ROOT/.env"
  set +a
fi

echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Cognee Workspace Tests${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo

if [[ -z "${OPENAI_URL:-}" ]]; then
  echo -e "${RED}❌ OPENAI_URL is not set. Set it in the environment or .env file.${NC}"
  exit 1
fi

if [[ -z "${OPENAI_TOKEN:-}" ]]; then
  echo -e "${RED}❌ OPENAI_TOKEN is not set. Set it in the environment or .env file.${NC}"
  exit 1
fi

export OPENAI_URL
export OPENAI_TOKEN
export OPENAI_MODEL="${OPENAI_MODEL:-gpt-4o-mini}"

MODEL_DIR="${COGNEE_TEST_MODEL_DIR:-$PROJECT_ROOT/target/models}"
setup_embedding_models "$MODEL_DIR"

print_env

TEST_NAME="${1:-}"
cd "$PROJECT_ROOT"

run_cargo_tests "$TEST_NAME"
