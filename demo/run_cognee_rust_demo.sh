#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Load project-root .env (OPENAI_URL / OPENAI_TOKEN / OPENAI_MODEL) if present,
# without clobbering anything already set in the environment.
if [[ -f "$PROJECT_ROOT/.env" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$PROJECT_ROOT/.env"
  set +a
fi

# ── Config ─────────────────────────────────────────────────────────────────────
DATASET_NAME="${DATASET_NAME:-manhattan_project_demo}"
# Default to OpenAI (works on any host with an OPENAI_URL/OPENAI_TOKEN in .env).
# `ollama` remains available via --llm-backend.
LLM_BACKEND="${LLM_BACKEND:-openai}"

# OpenAI backend config (read from .env / environment).
OPENAI_MODEL="${OPENAI_MODEL:-gpt-4o-mini}"

# Ollama backend config (only used with --llm-backend ollama; requires an
# externally-running Ollama, see start_ollama in lib/demo_common.sh).
OLLAMA_DIR="${OLLAMA_DIR:-$PROJECT_ROOT/docker/ollama}"
OLLAMA_PORT="${OLLAMA_PORT:-11439}"
OLLAMA_CONTAINER_NAME="${OLLAMA_CONTAINER_NAME:-ollama-cognee-demo}"
OLLAMA_VOLUME_NAME="${OLLAMA_VOLUME_NAME:-ollama_cognee_demo_data}"
MODEL_NAME="${MODEL_NAME:-qwen3:4b}"

# ── Parse flags ──────────────────────────────────────────────────────────────────
VIDEO_IDS=()
SEQUENCE_FILES=()
LLM_BACKEND_EXPLICIT=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --llm-backend)
      shift
      [[ $# -gt 0 ]] || { echo "ERROR: --llm-backend requires a value" >&2; exit 1; }
      LLM_BACKEND="$1"
      LLM_BACKEND_EXPLICIT=1
      shift
      ;;
    --video-ids)
      shift
      while [[ $# -gt 0 && "$1" != --* ]]; do
        VIDEO_IDS+=("$1")
        shift
      done
      ;;
    --sequence-files)
      shift
      while [[ $# -gt 0 && "$1" != --* ]]; do
        SEQUENCE_FILES+=("$1")
        shift
      done
      ;;
    *)
      echo "ERROR: Unknown argument: $1" >&2
      echo "Usage: $0 [--llm-backend openai|ollama] [--video-ids <id>...] [--sequence-files <path>...]" >&2
      exit 1
      ;;
  esac
done

DEMO_RUNTIME_DIR="$PROJECT_ROOT/target/demo/runtime"
DEMO_DATA_DIR="$PROJECT_ROOT/target/demo/data"
MODEL_DIR="$PROJECT_ROOT/target/models"

EMBED_MODEL_PATH="${EMBED_MODEL_PATH:-$MODEL_DIR/BGE-Small-v1.5-model_quantized.onnx}"
TOKENIZER_PATH="${TOKENIZER_PATH:-$MODEL_DIR/bge-small-tokenizer.json}"

OLLAMA_OPENAI_BASE_URL="http://127.0.0.1:$OLLAMA_PORT/v1"

RUST_LOG="${RUST_LOG:-info,cognee_search=debug,ort=warn}"

# ── Shared utilities ────────────────────────────────────────────────────────────
# shellcheck source=lib/demo_common.sh
source "${SCRIPT_DIR}/lib/demo_common.sh"

# ── CLI runner (host: cargo run) ────────────────────────────────────────────────
run_cli() {
  cargo run --release -p cognee-cli -- "$@"
}

# ── Host-specific helpers ───────────────────────────────────────────────────────
cleanup_demo_data() {
  log "🧹 Cleaning previous demo data for an independent run"
  rm -rf "$DEMO_RUNTIME_DIR" "$DEMO_DATA_DIR"
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

  run_cli config set vector_db_provider "brute-force"
  run_cli config set vector_db_url "$vector_path"

  case "$LLM_BACKEND" in
    openai)
      run_cli config set llm_provider "openai"
      run_cli config set llm_model "$OPENAI_MODEL"
      run_cli config set llm_api_key "$OPENAI_TOKEN"
      run_cli config set llm_endpoint "$OPENAI_URL"
      run_cli config set llm_max_retries 3
      run_cli config set llm_max_parallel_requests 4
      ;;
    ollama)
      run_cli config set llm_provider "openai"
      run_cli config set llm_model "$MODEL_NAME"
      run_cli config set llm_api_key "ollama"
      run_cli config set llm_endpoint "$OLLAMA_OPENAI_BASE_URL"
      run_cli config set llm_max_retries 3
      run_cli config set llm_max_parallel_requests 4
      ;;
    *)
      fail "❌ Unsupported --llm-backend '$LLM_BACKEND'. Supported: openai, ollama"
      ;;
  esac

  run_cli config set embedding_model_path "$EMBED_MODEL_PATH"
  run_cli config set embedding_tokenizer_path "$TOKENIZER_PATH"
  run_cli config set embedding_model_name "BGE-Small-v1.5"
  run_cli config set embedding_dimensions 384
  run_cli config set embedding_max_sequence_length 512
  run_cli config set embedding_batch_size 16
}

prepare_env_and_configure_cli() {
  cleanup_demo_data
  mkdir -p "$DEMO_RUNTIME_DIR" "$DEMO_DATA_DIR" "$MODEL_DIR"

  log "🛠 Building release CLI (via cargo run on first invocation)"

  if [[ "$LLM_BACKEND" == "ollama" ]]; then
    start_ollama
  fi

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

  if [[ "$LLM_BACKEND" == "ollama" ]]; then
    wait_for_ollama_chat_api 40
  fi
}

validate_llm_mode() {
  case "$LLM_BACKEND" in
    openai)
      if [[ -z "${OPENAI_URL:-}" || -z "${OPENAI_TOKEN:-}" ]]; then
        fail "❌ OpenAI backend needs OPENAI_URL and OPENAI_TOKEN (set them in $PROJECT_ROOT/.env or the environment)."
      fi
      return 0
      ;;
    ollama)
      return 0
      ;;
    *)
      fail "❌ Unsupported --llm-backend '$LLM_BACKEND'. Supported: openai, ollama"
      ;;
  esac
}

main() {
  require_cmd curl
  require_cmd cargo

  validate_llm_mode

  if [[ "$LLM_BACKEND" == "ollama" ]]; then
    require_cmd docker
  fi

  prepare_env_and_configure_cli

  if [[ ${#VIDEO_IDS[@]} -gt 0 ]]; then
    log "🎬 Running video pipeline for: ${VIDEO_IDS[*]}"
    run_video_pipeline "${VIDEO_IDS[@]}"
  elif [[ ${#SEQUENCE_FILES[@]} -gt 0 ]]; then
    log "📋 Running custom sequence files: ${SEQUENCE_FILES[*]}"
    run_sequence_files "${SEQUENCE_FILES[@]}"
  else
    log "📝 Creating local demo text files"
    create_demo_documents
    run_demo_pipeline
  fi

  ok ""
  ok "✅ Demo completed successfully"
  ok "   Dataset: $DATASET_NAME"
  ok "   LLM backend: $LLM_BACKEND"
  case "$LLM_BACKEND" in
    openai)
      ok "   OpenAI endpoint: $OPENAI_URL"
      ok "   OpenAI model: $OPENAI_MODEL"
      ;;
    ollama)
      ok "   Ollama endpoint: $OLLAMA_OPENAI_BASE_URL"
      ok "   To stop Ollama: docker stop $OLLAMA_CONTAINER_NAME"
      ;;
  esac
}

main
