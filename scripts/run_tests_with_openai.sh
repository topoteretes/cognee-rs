#!/usr/bin/env bash
# Run the full test suite against the OpenAI public API.
#
# Required env vars:
#   OPENAI_KEY  — OpenAI API key (set as a GitHub Actions secret or locally)
#
# Optional env vars:
#   OPENAI_MODEL              — model to use (default: gpt-4o-mini)
#   COGNEE_TEST_MODEL_DIR     — directory for embedding model cache
#   COGNEE_E2E_EMBED_MODEL_PATH / COGNEE_E2E_TOKENIZER_PATH — full path overrides

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Cognee Workspace Tests (OpenAI)${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo

if [[ -z "${OPENAI_KEY:-}" ]]; then
  echo -e "${RED}❌ OPENAI_KEY is not set. Export it or configure it as a GitHub Actions secret.${NC}"
  exit 1
fi

export OPENAI_URL="https://api.openai.com/v1"
export OPENAI_TOKEN="$OPENAI_KEY"
export OPENAI_MODEL="${OPENAI_MODEL:-gpt-4o-mini}"

MODEL_DIR="${COGNEE_TEST_MODEL_DIR:-$PROJECT_ROOT/target/models}"
setup_embedding_models "$MODEL_DIR"

print_env

TEST_NAME="${1:-}"
cd "$PROJECT_ROOT"

run_cargo_tests "$TEST_NAME"
