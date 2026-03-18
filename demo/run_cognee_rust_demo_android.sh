#!/usr/bin/env bash
#
# Run the cognee-rust demo on a connected Android device via ADB.
#
# The pipeline is identical to run_cognee_rust_demo.sh, but:
#   - cognee-cli executes on the device (via scripts/android-run.sh)
#   - Ollama runs in Docker on the host, reached from the device via ADB
#     reverse port forwarding (adb reverse tcp:PORT tcp:PORT)
#   - Embedding models are downloaded to the host then pushed to the device
#   - Demo text files are created on the host then pushed to the device
#
# Prerequisites:
#   - Android device connected via USB with USB debugging enabled
#   - Docker installed and running on the host
#   - adb available (ANDROID_SDK_ROOT set or adb on PATH)
#   - Android binary already deployed, or omit --skip-build to build+deploy automatically
#
# Usage:
#   ./demo/run_cognee_rust_demo_android.sh [--skip-build]
#
# Flags:
#   --skip-build   Skip building and deploying the Android binary.
#                  Use when artifacts are already on the device to save time.
#
# Environment overrides (all optional):
#   OLLAMA_PORT, OLLAMA_CONTAINER_NAME, OLLAMA_VOLUME_NAME,
#   MODEL_NAME, DATASET_NAME, RUST_LOG,
#   EMBED_MODEL_PATH, TOKENIZER_PATH

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OLLAMA_DIR="$PROJECT_ROOT/docker/ollama"

# ── Device paths (Android filesystem) ──────────────────────────────────────────
DEVICE_DIR="/data/local/tmp/cognee"
DEVICE_RUNTIME_DIR="${DEVICE_DIR}/runtime"
DEVICE_MODEL_DIR="${DEVICE_DIR}/models"
# DEMO_DATA_DIR is the device path — run_demo_pipeline passes it to
# 'run_cli add <paths>' which resolves on the device filesystem.
DEMO_DATA_DIR="${DEVICE_DIR}/demo_data"

# ── Host paths (build machine) ─────────────────────────────────────────────────
HOST_MODEL_DIR="$PROJECT_ROOT/target/models"
# HOST_DATA_DIR is where create_demo_documents writes files locally before push.
HOST_DATA_DIR="$PROJECT_ROOT/target/demo/data"

# ── Config ─────────────────────────────────────────────────────────────────────
OLLAMA_PORT="${OLLAMA_PORT:-11439}"
OLLAMA_CONTAINER_NAME="${OLLAMA_CONTAINER_NAME:-ollama-cognee-demo}"
OLLAMA_VOLUME_NAME="${OLLAMA_VOLUME_NAME:-ollama_cognee_demo_data}"
MODEL_NAME="${MODEL_NAME:-qwen3:4b}"
DATASET_NAME="${DATASET_NAME:-manhattan_project_demo}"

OLLAMA_OPENAI_BASE_URL="http://127.0.0.1:${OLLAMA_PORT}/v1"

EMBED_MODEL_PATH="${EMBED_MODEL_PATH:-$HOST_MODEL_DIR/BGE-Small-v1.5-model_quantized.onnx}"
TOKENIZER_PATH="${TOKENIZER_PATH:-$HOST_MODEL_DIR/bge-small-tokenizer.json}"

RUST_LOG="${RUST_LOG:-info,cognee_search=debug,ort=warn}"

# ── Parse flags ─────────────────────────────────────────────────────────────────
SKIP_BUILD=false
VIDEO_IDS=()
SEQUENCE_FILES=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build)
      SKIP_BUILD=true
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
      echo "Usage: $0 [--skip-build] [--video-ids <id>...] [--sequence-files <path>...]" >&2
      exit 1
      ;;
  esac
done

# ── Shared utilities ────────────────────────────────────────────────────────────
# shellcheck source=lib/demo_common.sh
source "${SCRIPT_DIR}/lib/demo_common.sh"

# ── Locate adb ─────────────────────────────────────────────────────────────────
ANDROID_SDK_ROOT="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"

if [[ -x "${ANDROID_SDK_ROOT}/platform-tools/adb" ]]; then
    ADB="${ANDROID_SDK_ROOT}/platform-tools/adb"
elif [[ -x "${ANDROID_SDK_ROOT}/Sdk/platform-tools/adb" ]]; then
    ADB="${ANDROID_SDK_ROOT}/Sdk/platform-tools/adb"
elif command -v adb &>/dev/null; then
    ADB="$(command -v adb)"
else
    ADB="${ANDROID_SDK_ROOT}/platform-tools/adb"  # will fail at validate_adb with a clear message
fi

validate_adb() {
  if [[ ! -x "${ADB}" ]]; then
    fail "❌ adb not found. Set ANDROID_SDK_ROOT or put adb on PATH."
  fi
  if ! "${ADB}" devices | grep -q "device$"; then
    fail "❌ No Android device connected. Connect a device and enable USB debugging."
  fi
}

# ── CLI runner (Android: via android-run.sh) ────────────────────────────────────
# --forward-port sets up 'adb reverse tcp:PORT tcp:PORT' before each invocation
# so device localhost:OLLAMA_PORT transparently reaches host Ollama.
run_cli() {
  "${PROJECT_ROOT}/scripts/android-run.sh" \
    --log "${RUST_LOG}" \
    --forward-port "${OLLAMA_PORT}" \
    cognee-cli -- "$@"
}

# ── Android-specific run_sequence_files ────────────────────────────────────────
# Override the shared run_sequence_files: expand env vars, push the resulting
# files to the device, then pass device-side paths to run_cli.
run_sequence_files() {
  local expanded_files=()
  local device_files=()
  local cleanup_files=()
  local device_seq_dir="${DEVICE_DIR}/sequences"

  "${ADB}" shell "mkdir -p ${device_seq_dir}"

  for template in "$@"; do
    if [[ ! -f "$template" ]]; then
      fail "Sequence file not found: $template"
    fi
    local base
    base="$(basename "$template")"
    local expanded="/tmp/cognee_seq_${$}_${base}"
    expand_sequence_file "$template" "$expanded"
    expanded_files+=("$expanded")
    cleanup_files+=("$expanded")

    # Push to device
    "${ADB}" push "$expanded" "${device_seq_dir}/${base}" > /dev/null
    device_files+=("${device_seq_dir}/${base}")
  done

  log "🚀 Running ${#device_files[@]} sequence file(s) via run-sequence"
  run_cli run-sequence "${device_files[@]}"

  rm -f "${cleanup_files[@]}"
}

# ── Android-specific configure_cli ─────────────────────────────────────────────
# All paths reference the device filesystem. The LLM endpoint uses
# localhost:OLLAMA_PORT which is forwarded to host Ollama via adb reverse.
configure_cli() {
  local db_path="${DEVICE_RUNTIME_DIR}/cognee_demo.db"
  local db_url="sqlite://${db_path}"
  local graph_path="${DEVICE_RUNTIME_DIR}/graph.ladybug"
  local vector_path="${DEVICE_RUNTIME_DIR}/vectors"
  local device_embed_model="${DEVICE_MODEL_DIR}/BGE-Small-v1.5-model_quantized.onnx"
  local device_tokenizer="${DEVICE_MODEL_DIR}/bge-small-tokenizer.json"

  run_cli config set default_dataset_name "${DATASET_NAME}"
  run_cli config set system_root_directory "${DEVICE_RUNTIME_DIR}/system"
  run_cli config set data_root_directory "${DEVICE_RUNTIME_DIR}/data_storage"
  run_cli config set cache_root_directory "${DEVICE_RUNTIME_DIR}/cache"
  run_cli config set logs_root_directory "${DEVICE_RUNTIME_DIR}/logs"

  run_cli config set relational_db_url "${db_url}"
  run_cli config set graph_database_provider "kuzu"
  run_cli config set graph_file_path "${graph_path}"

  run_cli config set vector_db_provider "qdrant"
  run_cli config set vector_db_url "${vector_path}"

  run_cli config set llm_provider "openai"
  run_cli config set llm_model "${MODEL_NAME}"
  run_cli config set llm_api_key "ollama"
  run_cli config set llm_endpoint "http://127.0.0.1:${OLLAMA_PORT}/v1"
  run_cli config set llm_max_retries 3
  run_cli config set llm_max_parallel_requests 4

  run_cli config set embedding_model_path "${device_embed_model}"
  run_cli config set embedding_tokenizer_path "${device_tokenizer}"
  run_cli config set embedding_model_name "BGE-Small-v1.5"
  run_cli config set embedding_dimensions 384
  run_cli config set embedding_max_sequence_length 512
  run_cli config set embedding_batch_size 16
}

prepare_env_and_configure_cli() {
  log "🧹 Cleaning previous demo data (host)"
  rm -rf "${HOST_DATA_DIR}"

  log "🧹 Cleaning previous demo data (device)"
  "${ADB}" shell "rm -rf ${DEVICE_DIR}/runtime ${DEVICE_DIR}/demo_data"

  log "📁 Creating device directories"
  "${ADB}" shell "mkdir -p ${DEVICE_RUNTIME_DIR}/vectors ${DEVICE_MODEL_DIR} ${DEVICE_DIR}/demo_data && touch ${DEVICE_RUNTIME_DIR}/cognee_demo.db"

  start_ollama

  log "⬇ Ensuring embedding model artifacts are present on host"
  download_if_missing \
    "${EMBED_MODEL_PATH}" \
    "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx"

  download_if_missing \
    "${TOKENIZER_PATH}" \
    "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/tokenizer.json"

  log "📤 Pushing embedding models to device"
  "${ADB}" push "${HOST_MODEL_DIR}/." "${DEVICE_MODEL_DIR}/"

  log "⚙ Resetting cognee-cli config on device"
  run_cli config reset --force

  log "⚙ Configuring cognee-cli on device"
  configure_cli

  wait_for_ollama_chat_api 40
}

main() {
  require_cmd docker
  require_cmd curl
  require_cmd adb

  validate_adb

  if [[ "${SKIP_BUILD}" == "false" ]]; then
    log "🔨 Building and deploying Android binary"
    "${PROJECT_ROOT}/scripts/android-build-and-deploy.sh" --deploy
  else
    warn "⚠ Skipping build (--skip-build). Assuming artifacts are already on device."
  fi

  prepare_env_and_configure_cli

  if [[ ${#VIDEO_IDS[@]} -gt 0 ]]; then
    log "🎬 Running video pipeline for: ${VIDEO_IDS[*]}"
    run_video_pipeline "${VIDEO_IDS[@]}"
  elif [[ ${#SEQUENCE_FILES[@]} -gt 0 ]]; then
    log "📋 Running custom sequence files: ${SEQUENCE_FILES[*]}"
    run_sequence_files "${SEQUENCE_FILES[@]}"
  else
    # create_demo_documents (from demo_common.sh) takes an explicit path so it
    # writes to the host filesystem; DEMO_DATA_DIR remains the device path for
    # the subsequent run_demo_pipeline call.
    log "📝 Creating demo text files on host"
    create_demo_documents "${HOST_DATA_DIR}"

    log "📤 Pushing demo text files to device"
    "${ADB}" push "${HOST_DATA_DIR}/." "${DEVICE_DIR}/demo_data/"

    # run_demo_pipeline uses DEMO_DATA_DIR (device path) for 'cognee-cli add'.
    run_demo_pipeline
  fi

  ok ""
  ok "✅ Android demo completed successfully"
  ok "   Dataset:          ${DATASET_NAME}"
  ok "   Device dir:       ${DEVICE_DIR}"
  ok "   Ollama (host):    ${OLLAMA_OPENAI_BASE_URL}"
  ok "   To stop Ollama:   docker stop ${OLLAMA_CONTAINER_NAME}"
}

main
