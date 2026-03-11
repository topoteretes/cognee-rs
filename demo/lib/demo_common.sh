#!/usr/bin/env bash
# demo/lib/demo_common.sh — Shared utilities for cognee-rust demo scripts.
#
# Source this file; do NOT execute it directly.
#
# Caller-defined variables consumed by functions in this library:
#
#   DATASET_NAME          — name of the cognee dataset
#   DEMO_DATA_DIR         — path used by run_demo_pipeline when calling
#                           'run_cli add <paths>'.  For the host demo this is
#                           a host filesystem path; for the Android demo it is
#                           a device filesystem path (e.g. /data/local/tmp/cognee/demo_data).
#   OLLAMA_DIR            — directory containing docker/ollama/start.sh
#   OLLAMA_PORT           — TCP port Ollama listens on
#   OLLAMA_CONTAINER_NAME — Docker container name
#   OLLAMA_VOLUME_NAME    — Docker volume name
#   OLLAMA_OPENAI_BASE_URL — full base URL, e.g. http://127.0.0.1:$OLLAMA_PORT/v1
#   MODEL_NAME            — Ollama model name, e.g. qwen3:4b
#
# Caller-defined functions consumed by run_demo_pipeline:
#   run_cli [args...]     — executes the cognee-cli binary with given arguments

# ── Colors ─────────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m'

# ── Logging helpers ────────────────────────────────────────────────────────────
log() {
  echo -e "${BLUE}$*${NC}"
}

ok() {
  echo -e "${GREEN}$*${NC}"
}

warn() {
  echo -e "${YELLOW}$*${NC}"
}

fail() {
  echo -e "${RED}$*${NC}"
  exit 1
}

# require_cmd <cmd>
# Exits with an error if <cmd> is not found on PATH.
require_cmd() {
  if ! command -v "$1" > /dev/null 2>&1; then
    fail "❌ Required command '$1' is not installed."
  fi
}

# download_if_missing <local_path> <url>
# Creates parent directories and downloads the file only if absent.
download_if_missing() {
  local path="$1"
  local url="$2"

  if [[ -f "$path" ]]; then
    ok "✓ Already exists: $path"
    return 0
  fi

  mkdir -p "$(dirname "$path")"
  log "⬇ Downloading $(basename "$path")"
  curl -fL "$url" -o "$path"
}

# start_ollama
# Starts the Ollama Docker container via docker/ollama/start.sh.
# Reads: OLLAMA_DIR, OLLAMA_PORT, OLLAMA_CONTAINER_NAME, OLLAMA_VOLUME_NAME, MODEL_NAME
start_ollama() {
  log "🐳 Starting Ollama on port ${OLLAMA_PORT} with model ${MODEL_NAME}"
  (
    cd "${OLLAMA_DIR}"
    CONTAINER_NAME="${OLLAMA_CONTAINER_NAME}" \
    PORT="${OLLAMA_PORT}" \
    VOLUME_NAME="${OLLAMA_VOLUME_NAME}" \
    MODEL_NAME="${MODEL_NAME}" \
    MODEL_NAMES="${MODEL_NAME}" \
    RECREATE_CONTAINER="0" \
    ./start.sh
  )
}

# wait_for_ollama_chat_api [max_attempts]
# Polls OLLAMA_OPENAI_BASE_URL/chat/completions until ready or timeout.
# Reads: OLLAMA_OPENAI_BASE_URL, MODEL_NAME, OLLAMA_CONTAINER_NAME
wait_for_ollama_chat_api() {
  local max_attempts="${1:-40}"

  log "⏳ Waiting for Ollama OpenAI chat endpoint: ${OLLAMA_OPENAI_BASE_URL}/chat/completions"

  for ((attempt=1; attempt<=max_attempts; attempt++)); do
    if curl -sS --max-time 20 "${OLLAMA_OPENAI_BASE_URL}/chat/completions" \
      -H "Content-Type: application/json" \
      -d "{\"model\":\"${MODEL_NAME}\",\"messages\":[{\"role\":\"user\",\"content\":\"ping\"}],\"temperature\":0,\"max_tokens\":4}" \
      > /dev/null 2>&1; then
      ok "✓ Ollama OpenAI chat endpoint is ready"
      return 0
    fi

    if (( attempt % 5 == 0 )); then
      warn "   still waiting for chat endpoint... (${attempt}/${max_attempts})"
    fi
    sleep 2
  done

  warn "⚠ Ollama chat endpoint did not become ready in time"
  docker logs --tail 60 "${OLLAMA_CONTAINER_NAME}" || true
  return 1
}

# create_demo_documents [target_dir]
# Writes the 4 Manhattan Project demo .txt files to target_dir.
# Falls back to $DEMO_DATA_DIR if no argument is provided.
create_demo_documents() {
  local target_dir="${1:-${DEMO_DATA_DIR}}"
  mkdir -p "${target_dir}"

  cat > "${target_dir}/oppenheimer.txt" <<'TXT'
J. Robert Oppenheimer was the scientific director of the Manhattan Project's Los Alamos Laboratory.
He coordinated theoretical and experimental teams that designed and tested the first atomic bombs.
Oppenheimer worked with U.S. Army leadership and many physicists who had fled Europe.
TXT

  cat > "${target_dir}/groves.txt" <<'TXT'
General Leslie Groves directed the Manhattan Engineer District for the U.S. Army Corps of Engineers.
Groves oversaw budget, logistics, security, and construction across major project sites.
He selected Oppenheimer to lead the scientific work at Los Alamos.
TXT

  cat > "${target_dir}/laboratories.txt" <<'TXT'
Key Manhattan Project locations included Los Alamos in New Mexico, Oak Ridge in Tennessee, and Hanford in Washington.
Oak Ridge developed uranium enrichment processes, while Hanford produced plutonium.
The project integrated universities, government agencies, and industrial contractors.
TXT

  cat > "${target_dir}/organizations.txt" <<'TXT'
The Manhattan Project involved the U.S. Army Corps of Engineers, the Office of Scientific Research and Development,
and research groups from institutions such as the University of California and the University of Chicago.
Scientists Enrico Fermi, Niels Bohr, and Richard Feynman were associated with project efforts.
TXT
}

# expand_sequence_file <template_json> <output_json>
# Expands shell variables (DEMO_DATA_DIR, DATASET_NAME, etc.) in a JSON
# template and writes the result to output_json.
expand_sequence_file() {
  local template="$1"
  local output="$2"
  mkdir -p "$(dirname "$output")"
  # envsubst only sees exported variables
  export DEMO_DATA_DIR DATASET_NAME
  envsubst < "$template" > "$output"
}

# run_demo_pipeline
# Runs the full add + cognify + 4 search queries pipeline via run-sequence.
# Reads:  DEMO_DATA_DIR, DATASET_NAME, DEMO_SEQUENCE_TEMPLATE (optional)
# Calls:  run_cli (must be defined by the sourcing script)
run_demo_pipeline() {
  local script_dir
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  local template="${DEMO_SEQUENCE_TEMPLATE:-${script_dir}/sequences/demo_pipeline.json}"
  local expanded="/tmp/cognee_demo_sequence_$$.json"

  log "📋 Expanding sequence template: ${template}"
  expand_sequence_file "$template" "$expanded"

  log "🚀 Running demo pipeline via run-sequence"
  run_cli run-sequence "$expanded"

  rm -f "$expanded"
}
