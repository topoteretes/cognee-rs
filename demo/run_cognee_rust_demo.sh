#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OLLAMA_DIR="$PROJECT_ROOT/docker/ollama"

OLLAMA_PORT="${OLLAMA_PORT:-11439}"
OLLAMA_CONTAINER_NAME="${OLLAMA_CONTAINER_NAME:-ollama-cognee-demo}"
OLLAMA_VOLUME_NAME="${OLLAMA_VOLUME_NAME:-ollama_cognee_demo_data}"
MODEL_NAME="${MODEL_NAME:-qwen3:4b}"
DATASET_NAME="${DATASET_NAME:-manhattan_project_demo}"

DEMO_RUNTIME_DIR="$PROJECT_ROOT/target/demo/runtime"
DEMO_DATA_DIR="$PROJECT_ROOT/target/demo/data"
MODEL_DIR="$PROJECT_ROOT/target/models"

EMBED_MODEL_PATH="${EMBED_MODEL_PATH:-$MODEL_DIR/BGE-Small-v1.5-model_quantized.onnx}"
TOKENIZER_PATH="${TOKENIZER_PATH:-$MODEL_DIR/bge-small-tokenizer.json}"

OLLAMA_OPENAI_BASE_URL="http://127.0.0.1:$OLLAMA_PORT/v1"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m'

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

require_cmd() {
  if ! command -v "$1" > /dev/null 2>&1; then
    fail "❌ Required command '$1' is not installed."
  fi
}

run_cli() {
  cargo run --release -p cognee-cli -- "$@"
}

cleanup_demo_data() {
  log "🧹 Cleaning previous demo data for an independent run"
  rm -rf "$DEMO_RUNTIME_DIR" "$DEMO_DATA_DIR"
}

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

configure_cli() {
  local db_path="$DEMO_RUNTIME_DIR/cognee_demo.db"
  local db_url="sqlite://$db_path"
  local graph_path="$DEMO_RUNTIME_DIR/graph.ladybug"
  local vector_path="$DEMO_RUNTIME_DIR/vectors"

  mkdir -p "$DEMO_RUNTIME_DIR" "$vector_path"
  : > "$db_path"

  run_cli config set default_dataset_name "$DATASET_NAME"
  run_cli config set system_root_directory "$DEMO_RUNTIME_DIR/system"
  run_cli config set data_root_directory "$DEMO_RUNTIME_DIR/data_storage"
  run_cli config set cache_root_directory "$DEMO_RUNTIME_DIR/cache"
  run_cli config set logs_root_directory "$DEMO_RUNTIME_DIR/logs"

  run_cli config set relational_db_url "$db_url"
  run_cli config set graph_database_provider "kuzu"
  run_cli config set graph_file_path "$graph_path"

  run_cli config set vector_db_provider "qdrant"
  run_cli config set vector_db_url "$vector_path"

  run_cli config set llm_provider "openai"
  run_cli config set llm_model "$MODEL_NAME"
  run_cli config set llm_api_key "ollama"
  run_cli config set llm_endpoint "$OLLAMA_OPENAI_BASE_URL"
  run_cli config set llm_max_retries 3
  run_cli config set llm_max_parallel_requests 4

  run_cli config set embedding_model_path "$EMBED_MODEL_PATH"
  run_cli config set embedding_tokenizer_path "$TOKENIZER_PATH"
  run_cli config set embedding_model_name "BGE-Small-v1.5"
  run_cli config set embedding_dimensions 384
  run_cli config set embedding_max_sequence_length 512
  run_cli config set embedding_batch_size 16
}

wait_for_ollama_chat_api() {
  local max_attempts="${1:-40}"

  log "⏳ Waiting for Ollama OpenAI chat endpoint: $OLLAMA_OPENAI_BASE_URL/chat/completions"

  for ((attempt=1; attempt<=max_attempts; attempt++)); do
    if curl -sS --max-time 20 "$OLLAMA_OPENAI_BASE_URL/chat/completions" \
      -H "Content-Type: application/json" \
      -d "{\"model\":\"$MODEL_NAME\",\"messages\":[{\"role\":\"user\",\"content\":\"ping\"}],\"temperature\":0,\"max_tokens\":4}" \
      > /dev/null 2>&1; then
      ok "✓ Ollama OpenAI chat endpoint is ready"
      return 0
    fi

    if (( attempt % 5 == 0 )); then
      warn "   still waiting for chat endpoint... ($attempt/$max_attempts)"
    fi
    sleep 2
  done

  warn "⚠ Ollama chat endpoint did not become ready in time"
  docker logs --tail 60 "$OLLAMA_CONTAINER_NAME" || true
  return 1
}

create_demo_documents() {
  mkdir -p "$DEMO_DATA_DIR"

  cat > "$DEMO_DATA_DIR/oppenheimer.txt" <<'TXT'
J. Robert Oppenheimer was the scientific director of the Manhattan Project's Los Alamos Laboratory.
He coordinated theoretical and experimental teams that designed and tested the first atomic bombs.
Oppenheimer worked with U.S. Army leadership and many physicists who had fled Europe.
TXT

  cat > "$DEMO_DATA_DIR/groves.txt" <<'TXT'
General Leslie Groves directed the Manhattan Engineer District for the U.S. Army Corps of Engineers.
Groves oversaw budget, logistics, security, and construction across major project sites.
He selected Oppenheimer to lead the scientific work at Los Alamos.
TXT

  cat > "$DEMO_DATA_DIR/laboratories.txt" <<'TXT'
Key Manhattan Project locations included Los Alamos in New Mexico, Oak Ridge in Tennessee, and Hanford in Washington.
Oak Ridge developed uranium enrichment processes, while Hanford produced plutonium.
The project integrated universities, government agencies, and industrial contractors.
TXT

  cat > "$DEMO_DATA_DIR/organizations.txt" <<'TXT'
The Manhattan Project involved the U.S. Army Corps of Engineers, the Office of Scientific Research and Development,
and research groups from institutions such as the University of California and the University of Chicago.
Scientists Enrico Fermi, Niels Bohr, and Richard Feynman were associated with project efforts.
TXT
}


run_search_queries() {
  log "🔎 Query 1: person-role relation"
  run_cli search "Who directed the scientific work at Los Alamos?" --datasets "$DATASET_NAME" --query-type GRAPH_COMPLETION --top-k 5 --output-format pretty

  log "🔎 Query 2: organizations"
  run_cli search "Which organizations were involved in the Manhattan Project?" --datasets "$DATASET_NAME" --query-type GRAPH_COMPLETION --top-k 5 --output-format pretty

  log "🔎 Query 3: site responsibilities"
  run_cli search "What were Oak Ridge and Hanford responsible for?" --datasets "$DATASET_NAME" --query-type RAG_COMPLETION --top-k 5 --output-format pretty

  log "🔎 Query 4: direct chunk retrieval"
  run_cli search "Leslie Groves responsibilities" --datasets "$DATASET_NAME" --query-type CHUNKS --top-k 5 --output-format pretty
}

main() {
  require_cmd docker
  require_cmd curl
  require_cmd cargo

  cleanup_demo_data
  mkdir -p "$DEMO_RUNTIME_DIR" "$DEMO_DATA_DIR" "$MODEL_DIR"

  log "🛠 Building release CLI (via cargo run on first invocation)"

  log "🐳 Starting Ollama on custom port $OLLAMA_PORT with model $MODEL_NAME"
  (
    cd "$OLLAMA_DIR"
    CONTAINER_NAME="$OLLAMA_CONTAINER_NAME" \
    PORT="$OLLAMA_PORT" \
    VOLUME_NAME="$OLLAMA_VOLUME_NAME" \
    MODEL_NAME="$MODEL_NAME" \
    MODEL_NAMES="$MODEL_NAME" \
    RECREATE_CONTAINER="0" \
    ./start.sh
  )

  log "⬇ Ensuring embedding model artifacts are present"
  download_if_missing \
    "$EMBED_MODEL_PATH" \
    "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx"

  download_if_missing \
    "$TOKENIZER_PATH" \
    "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/tokenizer.json"

  log "⚙ Configuring cognee-cli"
  run_cli config reset --force
  configure_cli

  wait_for_ollama_chat_api 40

  log "📝 Creating local demo text files"
  create_demo_documents

  log "➕ Adding local text files"
  run_cli add \
    "$DEMO_DATA_DIR/oppenheimer.txt" \
    "$DEMO_DATA_DIR/groves.txt" \
    "$DEMO_DATA_DIR/laboratories.txt" \
    "$DEMO_DATA_DIR/organizations.txt" \
    --dataset-name "$DATASET_NAME"

  log "🧠 Running cognify"
  COGNEE_DEBUG_LLM_REQUEST="${COGNEE_DEBUG_LLM_REQUEST:-0}" \
    run_cli cognify --datasets "$DATASET_NAME" --chunk-size 700 --llm-max-retries 3 --llm-max-parallel-requests 4 --verbose

  run_search_queries

  ok ""
  ok "✅ Demo completed successfully"
  ok "   Dataset: $DATASET_NAME"
  ok "   Ollama endpoint: $OLLAMA_OPENAI_BASE_URL"
  ok "   To stop Ollama: docker stop $OLLAMA_CONTAINER_NAME"
}

main "$@"
