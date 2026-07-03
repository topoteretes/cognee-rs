#!/usr/bin/env bash
# Shared utilities for cognee-rust test scripts. Source this file; do not execute directly.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'


# print_env — prints the key test environment variables
print_env() {
  echo -e "${BLUE}📝 Environment:${NC}"
  echo -e "   OPENAI_URL=${OPENAI_URL}"
  echo -e "   OPENAI_TOKEN=${OPENAI_TOKEN}"
  echo -e "   OPENAI_MODEL=${OPENAI_MODEL}"
  echo
}

# run_cargo_tests [test_name]
# Runs the workspace test suite. Uses `cargo nextest run` when available
# (faster compile scheduling, cleaner output) and falls back to `cargo test`
# otherwise. Tests run in PARALLEL: nextest executes each test in its own
# process, so the per-test isolation that the old serial `--no-capture` run
# provided is intrinsic — see .config/nextest.toml for the rationale. CI selects
# the tuned `ci` profile by exporting `NEXTEST_PROFILE=ci` (nextest reads it
# natively); local runs use the `default` profile. Set `NEXTEST_NO_CAPTURE=1`
# (or pass `--no-capture` manually) to debug a single test serially.
# Doctests are run separately because nextest does not execute them yet.
# Caller must cd to PROJECT_ROOT first.
run_cargo_tests() {
  local test_name="${1:-}"

  echo -e "${BLUE}🧪 Running workspace tests (including LLM/model tests)...${NC}"
  echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
  echo

  if command -v cargo-nextest >/dev/null 2>&1; then
    if [[ -n "$test_name" ]]; then
      cargo nextest run --workspace "$test_name"
    else
      cargo nextest run --workspace
    fi
    # nextest does not run doctests; exercise them separately.
    cargo test --workspace --doc
  else
    # `cargo test` (no nextest) shares one process per binary, so the LLM tests
    # that mutate process-global state still need single-threaded execution here.
    if [[ -n "$test_name" ]]; then
      cargo test --workspace "$test_name" -- --test-threads=1
    else
      cargo test --workspace -- --test-threads=1
    fi
  fi

  echo
  echo -e "${GREEN}✅ All tests passed.${NC}"
}
