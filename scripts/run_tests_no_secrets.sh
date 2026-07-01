#!/usr/bin/env bash
# Run the workspace test suite WITHOUT any LLM secrets configured.
#
# The secret-dependent integration / E2E tests detect the absent
# OPENAI_* / LLM_* environment variables and skip themselves gracefully
# (see the `skipping: OPENAI_* not set` guards across crates/*/tests), so
# this invocation exercises every test that does NOT require a live LLM.
#
# Used by the community (fork-safe) CI workflow: pull requests from forks
# never receive repository secrets, so the keyed `run_tests_with_openai.sh`
# would hard-fail. This script gives community contributors fast feedback
# on the secret-free portion of the suite. Mirrors the upstream Python
# cognee `community_tests.yml` split.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

# Defensively clear any LLM credentials that might leak in from the runner
# environment so no test can make a live API call from this lane.
unset OPENAI_TOKEN OPENAI_API_KEY OPENAI_URL OPENAI_MODEL \
      LLM_API_KEY LLM_ENDPOINT LLM_MODEL 2>/dev/null || true

echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Cognee Workspace Tests — keyless (no secrets) lane${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo
echo -e "${YELLOW}No LLM credentials configured — LLM/integration tests will skip.${NC}"
echo

TEST_NAME="${1:-}"
cd "$PROJECT_ROOT"

run_cargo_tests "$TEST_NAME"
