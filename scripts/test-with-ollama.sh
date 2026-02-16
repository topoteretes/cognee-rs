#!/bin/bash
# Test script that starts Ollama Docker container and runs all workspace tests
#
# Usage:
#   ./scripts/test-with-ollama.sh [test-name]
#
# Examples:
#   ./scripts/test-with-ollama.sh                              # Run all workspace tests
#   ./scripts/test-with-ollama.sh test_entity_extraction       # Run specific test by name
#   ./scripts/test-with-ollama.sh test_fact_extraction_batch   # Run specific test by name

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DOCKER_DIR="$PROJECT_ROOT/docker/ollama"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Cognee Workspace Tests with Ollama${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo

# Check if Docker is running
if ! docker info > /dev/null 2>&1; then
  echo -e "${RED}❌ Docker is not running. Please start Docker and try again.${NC}"
  exit 1
fi

# Check if Ollama container exists and is running
CONTAINER_EXISTS=$(docker ps -a --filter "name=ollama" --format "{{.Names}}" | grep -c "^ollama$" || true)
CONTAINER_RUNNING=$(docker ps --filter "name=ollama" --format "{{.Names}}" | grep -c "^ollama$" || true)

CLEANUP_CONTAINER=false

if [ "$CONTAINER_RUNNING" -eq 1 ]; then
  echo -e "${GREEN}✓ Ollama container is already running${NC}"
else
  if [ "$CONTAINER_EXISTS" -eq 1 ]; then
    echo -e "${YELLOW}▶ Starting existing Ollama container...${NC}"
    docker start ollama > /dev/null
  else
    echo -e "${YELLOW}🐳 Starting new Ollama container...${NC}"
    cd "$DOCKER_DIR"
    ./start.sh > /dev/null 2>&1 || {
      echo -e "${RED}❌ Failed to start Ollama container${NC}"
      exit 1
    }
    cd "$PROJECT_ROOT"
    CLEANUP_CONTAINER=true
  fi

  # Wait for Ollama to be ready
  echo -e "${YELLOW}⏳ Waiting for Ollama to be ready...${NC}"
  for i in {1..30}; do
    if curl -s http://localhost:11435/api/tags > /dev/null 2>&1; then
      echo -e "${GREEN}✓ Ollama is ready!${NC}"
      break
    fi
    if [ $i -eq 30 ]; then
      echo -e "${RED}❌ Timeout waiting for Ollama${NC}"
      exit 1
    fi
    sleep 2
  done

  # Check if model is available
  echo -e "${YELLOW}🔍 Checking for model llama3.2:3b...${NC}"
  if ! docker exec ollama ollama list 2>/dev/null | grep -q "llama3.2:3b"; then
    echo -e "${RED}❌ Model llama3.2:3b not found. Please wait for the model to download.${NC}"
    echo -e "${YELLOW}   Check progress with: docker logs ollama -f${NC}"
    exit 1
  fi
  echo -e "${GREEN}✓ Model is available${NC}"
fi

echo

# Set environment variables for tests
export OPENAI_URL="http://localhost:11435/v1"
export OPENAI_TOKEN="not-needed"

echo -e "${BLUE}📝 Environment:${NC}"
echo -e "   OPENAI_URL=${OPENAI_URL}"
echo -e "   OPENAI_TOKEN=${OPENAI_TOKEN}"
echo

# Run tests
echo -e "${BLUE}🧪 Running all workspace tests...${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo

TEST_NAME="${1:-}"

cd "$PROJECT_ROOT"

if [ -n "$TEST_NAME" ]; then
  # Run specific test by name across all packages
  echo -e "${YELLOW}Running test: $TEST_NAME${NC}"
  echo
  cargo test --workspace "$TEST_NAME" -- --nocapture --test-threads=1
  TEST_EXIT_CODE=$?
else
  # Run all tests in the workspace (unit tests + integration tests)
  echo -e "${BLUE}Running all unit and integration tests in workspace...${NC}"
  echo
  cargo test --workspace -- --nocapture --test-threads=1
  TEST_EXIT_CODE=$?
fi

echo
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"

if [ $TEST_EXIT_CODE -eq 0 ]; then
  echo -e "${GREEN}✅ All tests passed!${NC}"
else
  echo -e "${RED}❌ Some tests failed (exit code: $TEST_EXIT_CODE)${NC}"
fi

echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo

# Cleanup instructions
if [ "$CLEANUP_CONTAINER" = true ]; then
  echo -e "${YELLOW}💡 Container management:${NC}"
  echo -e "   To stop:   docker stop ollama"
  echo -e "   To remove: docker rm ollama"
  echo -e "   To view logs: docker logs ollama -f"
else
  echo -e "${YELLOW}💡 Ollama container is still running${NC}"
  echo -e "   To view logs: docker logs ollama -f"
  echo -e "   To stop: docker stop ollama"
fi

exit $TEST_EXIT_CODE
