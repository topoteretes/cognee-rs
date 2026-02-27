#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OLLAMA_DIR="$PROJECT_ROOT/docker/ollama"

# ── Config ─────────────────────────────────────────────────────────────────────
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

prepare_env_and_configure_cli() {
  cleanup_demo_data
  mkdir -p "$DEMO_RUNTIME_DIR" "$DEMO_DATA_DIR" "$MODEL_DIR"

  log "🛠 Building release CLI (via cargo run on first invocation)"

  start_ollama

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
}

main() {
  require_cmd docker
  require_cmd curl
  require_cmd cargo

  prepare_env_and_configure_cli

  log "📝 Creating local demo text files"
  create_demo_documents

  run_demo_pipeline

  ok ""
  ok "✅ Demo completed successfully"
  ok "   Dataset: $DATASET_NAME"
  ok "   Ollama endpoint: $OLLAMA_OPENAI_BASE_URL"
  ok "   To stop Ollama: docker stop $OLLAMA_CONTAINER_NAME"
}

main "$@"
