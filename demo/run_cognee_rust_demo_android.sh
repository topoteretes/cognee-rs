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
#   ./demo/run_cognee_rust_demo_android.sh [--skip-build] [--llm-backend ollama|litert]
#
# Flags:
#   --skip-build   Skip building and deploying the Android binary.
#                  Use when artifacts are already on the device to save time.
#   --llm-backend  Select LLM backend: ollama or litert.
#   --litert-model-local   Host path to LiteRT model to push when needed.
#   --litert-model-device  Device path used by cognee as llm_model.
#   --litert-backend       LiteRT backend value (cpu, gpu, custom).
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
LLM_BACKEND="${LLM_BACKEND:-litert}"

# Defaults aligned with cognee-litert-lm benchmark_android.sh
LITERT_MODEL_LOCAL="${LITERT_MODEL_LOCAL:-$HOME/.litert-lm/models/gemma3-1b-it-int4.litertlm}"
LITERT_MODEL_DEVICE="${LITERT_MODEL_DEVICE:-${DEVICE_MODEL_DIR}/gemma3-1b-it-int4.litertlm}"
LITERT_BACKEND="${LITERT_BACKEND:-cpu}"
LITERT_MODEL_DEVICE_EXPLICIT=0

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
    --llm-backend)
      shift
      [[ $# -gt 0 ]] || { echo "ERROR: --llm-backend requires a value" >&2; exit 1; }
      LLM_BACKEND="$1"
      shift
      ;;
    --litert-model-local)
      shift
      [[ $# -gt 0 ]] || { echo "ERROR: --litert-model-local requires a value" >&2; exit 1; }
      LITERT_MODEL_LOCAL="$1"
      shift
      ;;
    --litert-model-device)
      shift
      [[ $# -gt 0 ]] || { echo "ERROR: --litert-model-device requires a value" >&2; exit 1; }
      LITERT_MODEL_DEVICE="$1"
      LITERT_MODEL_DEVICE_EXPLICIT=1
      shift
      ;;
    --litert-backend)
      shift
      [[ $# -gt 0 ]] || { echo "ERROR: --litert-backend requires a value" >&2; exit 1; }
      LITERT_BACKEND="$1"
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
      echo "Usage: $0 [--skip-build] [--llm-backend ollama|litert] [--litert-model-local <path>] [--litert-model-device <device-path>] [--litert-backend cpu|gpu|<custom>] [--video-ids <id>...] [--sequence-files <path>...]" >&2
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
run_cli() {
  local cmd=(
    "${PROJECT_ROOT}/scripts/android-run.sh"
    --log "${RUST_LOG}"
    --device-dir "${DEVICE_DIR}"
  )

  if [[ "${LLM_BACKEND}" == "ollama" ]]; then
    # Device localhost reaches host Ollama only in ollama mode.
    cmd+=(--forward-port "${OLLAMA_PORT}")
  fi

  cmd+=(cognee-cli -- "$@")
  "${cmd[@]}"
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

# ── Android-specific run_demo_pipeline ────────────────────────────────────────
# Override shared run_demo_pipeline so the expanded sequence file exists on the
# device filesystem before invoking run-sequence.
run_demo_pipeline() {
  local template="${DEMO_SEQUENCE_TEMPLATE:-${SCRIPT_DIR}/sequences/demo_pipeline.json}"
  local expanded_host="/tmp/cognee_demo_sequence_${$}.json"
  local device_seq_dir="${DEVICE_DIR}/sequences"
  local device_sequence="${device_seq_dir}/demo_pipeline_${$}.json"

  log "📋 Expanding sequence template: ${template}"
  expand_sequence_file "${template}" "${expanded_host}"

  "${ADB}" shell "mkdir -p ${device_seq_dir}"
  "${ADB}" push "${expanded_host}" "${device_sequence}" > /dev/null

  log "🚀 Running demo pipeline via run-sequence"
  run_cli run-sequence "${device_sequence}"

  rm -f "${expanded_host}"
  "${ADB}" shell "rm -f ${device_sequence}" > /dev/null || true
}

ensure_device_permissions_before_launch() {
  log "🔐 Normalizing ownership/permissions under ${DEVICE_DIR}"
  # Some Android shells may not permit chown/chgrp to arbitrary IDs; keep best-effort.
  "${ADB}" shell "chown -R 777:777 ${DEVICE_DIR} >/dev/null 2>&1 || true; chmod -R 777 ${DEVICE_DIR} >/dev/null 2>&1 || true; chmod +x ${DEVICE_DIR}/run_demo.sh >/dev/null 2>&1 || true; chmod +x ${DEVICE_DIR}/bin/* >/dev/null 2>&1 || true"
}

install_device_demo_runner() {
  local template="${DEMO_SEQUENCE_TEMPLATE:-${SCRIPT_DIR}/sequences/demo_pipeline.json}"
  local expanded_host="/tmp/cognee_demo_sequence_device_${$}.json"
  local host_runner="/tmp/cognee_run_demo_device_${$}.sh"
  local device_seq_dir="${DEVICE_DIR}/sequences"
  local device_sequence="${device_seq_dir}/demo_pipeline_device.json"
  local device_runner="${DEVICE_DIR}/run_demo.sh"
  local db_path="${DEVICE_RUNTIME_DIR}/cognee_demo.db"
  local db_url="sqlite://${db_path}"
  local graph_path="${DEVICE_RUNTIME_DIR}/graph.ladybug"
  local vector_path="${DEVICE_RUNTIME_DIR}/vectors"
  local device_embed_model="${DEVICE_MODEL_DIR}/BGE-Small-v1.5-model_quantized.onnx"
  local device_tokenizer="${DEVICE_MODEL_DIR}/bge-small-tokenizer.json"
  local litert_model_basename
  litert_model_basename="$(basename "${LITERT_MODEL_DEVICE}")"

  log "🧩 Installing device self-run demo script: ${device_runner}"

  expand_sequence_file "${template}" "${expanded_host}"
  "${ADB}" shell "mkdir -p ${device_seq_dir}"
  "${ADB}" push "${expanded_host}" "${device_sequence}" > /dev/null
  rm -f "${expanded_host}"

  cat > "${host_runner}" <<EOF
#!/usr/bin/sh
set -eux

SCRIPT_DIR="\$(CDPATH= cd -- "\$(dirname -- "\$0")" && pwd)"
cd "\${SCRIPT_DIR}"

DEVICE_DIR="."
DEVICE_RUNTIME_DIR="./runtime"
DEVICE_MODEL_DIR="./models"
DEMO_DATA_DIR="./demo_data"
DB_URL="sqlite://./runtime/cognee_demo.db"
GRAPH_PATH="./runtime/graph.ladybug"
VECTOR_PATH="./runtime/vectors"
EMBED_MODEL="./models/BGE-Small-v1.5-model_quantized.onnx"
TOKENIZER_PATH="./models/bge-small-tokenizer.json"
DATASET_NAME="${DATASET_NAME}"
LLM_BACKEND="${LLM_BACKEND}"
LITERT_MODEL_DEVICE="./models/${litert_model_basename}"
LITERT_BACKEND="${LITERT_BACKEND}"
OLLAMA_OPENAI_BASE_URL="http://127.0.0.1:${OLLAMA_PORT}/v1"
MODEL_NAME="${MODEL_NAME}"
DEVICE_SEQUENCE="./sequences/demo_pipeline_device.json"

mkdir -p "\${DEVICE_RUNTIME_DIR}/vectors" "\${DEVICE_MODEL_DIR}" "\${DEMO_DATA_DIR}" "\${DEVICE_DIR}/tmp"
touch "\${DEVICE_RUNTIME_DIR}/cognee_demo.db"

cat > "\${DEMO_DATA_DIR}/oppenheimer.txt" <<'TXT'
J. Robert Oppenheimer was the scientific director of the Manhattan Project's Los Alamos Laboratory.
He coordinated theoretical and experimental teams that designed and tested the first atomic bombs.
Oppenheimer worked with U.S. Army leadership and many physicists who had fled Europe.
TXT

cat > "\${DEMO_DATA_DIR}/groves.txt" <<'TXT'
General Leslie Groves directed the Manhattan Engineer District for the U.S. Army Corps of Engineers.
Groves oversaw budget, logistics, security, and construction across major project sites.
He selected Oppenheimer to lead the scientific work at Los Alamos.
TXT

cat > "\${DEMO_DATA_DIR}/laboratories.txt" <<'TXT'
Key Manhattan Project locations included Los Alamos in New Mexico, Oak Ridge in Tennessee, and Hanford in Washington.
Oak Ridge developed uranium enrichment processes, while Hanford produced plutonium.
The project integrated universities, government agencies, and industrial contractors.
TXT

cat > "\${DEMO_DATA_DIR}/organizations.txt" <<'TXT'
The Manhattan Project involved the U.S. Army Corps of Engineers, the Office of Scientific Research and Development,
and research groups from institutions such as the University of California and the University of Chicago.
Scientists Enrico Fermi, Niels Bohr, and Richard Feynman were associated with project efforts.
TXT

chown -R 777:777 "\${DEVICE_DIR}" >/dev/null 2>&1 || true
chmod -R 777 "\${DEVICE_DIR}" || true
chmod +x "\${DEVICE_DIR}/run_demo.sh" >/dev/null 2>&1 || true
chmod +x "\${DEVICE_DIR}/bin/"* >/dev/null 2>&1 || true

cd "\${DEVICE_DIR}"
export HOME="\${DEVICE_DIR}"
export TMPDIR="\${DEVICE_DIR}/tmp"
export LLVM_PROFILE_FILE="\${DEVICE_DIR}/default.profraw"
export PATH="\${DEVICE_DIR}/bin:\${PATH}"
export LD_LIBRARY_PATH="\${DEVICE_DIR}/lib"
export ORT_DYLIB_PATH="\${DEVICE_DIR}/lib/libonnxruntime.so"
export RUST_LOG="${RUST_LOG}"

cognee-cli config reset --force
cognee-cli config set default_dataset_name "\${DATASET_NAME}"
cognee-cli config set system_root_directory "\${DEVICE_RUNTIME_DIR}/system"
cognee-cli config set data_root_directory "\${DEVICE_RUNTIME_DIR}/data_storage"
cognee-cli config set cache_root_directory "\${DEVICE_RUNTIME_DIR}/cache"
cognee-cli config set logs_root_directory "\${DEVICE_RUNTIME_DIR}/logs"
cognee-cli config set relational_db_url "\${DB_URL}"
cognee-cli config set graph_database_provider "kuzu"
cognee-cli config set graph_file_path "\${GRAPH_PATH}"
cognee-cli config set vector_db_provider "qdrant"
cognee-cli config set vector_db_url "\${VECTOR_PATH}"

if [ "\${LLM_BACKEND}" = "litert" ]; then
  cognee-cli config set llm_provider "litert"
  cognee-cli config set llm_model "\${LITERT_MODEL_DEVICE}"
  cognee-cli config set llm_api_key ""
  cognee-cli config set llm_endpoint "\${LITERT_BACKEND}"
  cognee-cli config set llm_max_retries 1
  cognee-cli config set llm_max_parallel_requests 1
else
  cognee-cli config set llm_provider "openai"
  cognee-cli config set llm_model "\${MODEL_NAME}"
  cognee-cli config set llm_api_key "ollama"
  cognee-cli config set llm_endpoint "\${OLLAMA_OPENAI_BASE_URL}"
  cognee-cli config set llm_max_retries 3
  cognee-cli config set llm_max_parallel_requests 4
fi

cognee-cli config set embedding_model_path "\${EMBED_MODEL}"
cognee-cli config set embedding_tokenizer_path "\${TOKENIZER_PATH}"
cognee-cli config set embedding_model_name "BGE-Small-v1.5"
cognee-cli config set embedding_dimensions 384
cognee-cli config set embedding_max_sequence_length 512
cognee-cli config set embedding_batch_size 16

cognee-cli run-sequence "\${DEVICE_SEQUENCE}"
EOF

  "${ADB}" push "${host_runner}" "${device_runner}" > /dev/null
  rm -f "${host_runner}"

  "${ADB}" shell "chmod 777 ${device_runner} && chmod +x ${device_runner} && chmod -R 777 ${DEVICE_DIR} && chmod +x ${DEVICE_DIR}/bin/* >/dev/null 2>&1 || true"

  ok "✓ Device runner installed: ${device_runner}"
  ok "  Run with: adb shell ${device_runner}"
}

# ── Android-specific configure_cli ─────────────────────────────────────────────
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

  case "${LLM_BACKEND}" in
    ollama)
      run_cli config set llm_provider "openai"
      run_cli config set llm_model "${MODEL_NAME}"
      run_cli config set llm_api_key "ollama"
      run_cli config set llm_endpoint "http://127.0.0.1:${OLLAMA_PORT}/v1"
      run_cli config set llm_max_retries 3
      run_cli config set llm_max_parallel_requests 4
      ;;
    litert)
      run_cli config set llm_provider "litert"
      run_cli config set llm_model "${LITERT_MODEL_DEVICE}"
      run_cli config set llm_api_key ""
      run_cli config set llm_endpoint "${LITERT_BACKEND}"
      run_cli config set llm_max_retries 1
      run_cli config set llm_max_parallel_requests 1
      ;;
    *)
      fail "❌ Unsupported --llm-backend '${LLM_BACKEND}'. Supported: ollama, litert"
      ;;
  esac

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

  if [[ "${LLM_BACKEND}" == "ollama" ]]; then
    start_ollama
  else
    # In LiteRT mode, ensure model exists on device (push only if missing).
    if [[ ! -f "${LITERT_MODEL_LOCAL}" ]]; then
      fail "❌ LiteRT model not found on host: ${LITERT_MODEL_LOCAL}"
    fi
    if ! "${ADB}" shell "test -f ${LITERT_MODEL_DEVICE}" 2>/dev/null; then
      log "📤 Pushing LiteRT model to device: ${LITERT_MODEL_DEVICE}"
      "${ADB}" push "${LITERT_MODEL_LOCAL}" "${LITERT_MODEL_DEVICE}" > /dev/null
    else
      log "✓ LiteRT model already present on device: ${LITERT_MODEL_DEVICE}"
    fi
  fi

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

  if [[ "${LLM_BACKEND}" == "ollama" ]]; then
    wait_for_ollama_chat_api 40
  fi

  install_device_demo_runner
}

validate_llm_mode() {
  case "${LLM_BACKEND}" in
    ollama)
      return 0
      ;;
    litert)
      return 0
      ;;
    *)
      fail "❌ Unsupported --llm-backend '${LLM_BACKEND}'. Supported: ollama, litert"
      ;;
  esac
}

main() {
  require_cmd curl
  require_cmd adb

  validate_llm_mode

  if [[ "${LLM_BACKEND}" == "ollama" ]]; then
    require_cmd docker
  fi

  validate_adb

  if [[ "${SKIP_BUILD}" == "false" ]]; then
    log "🔨 Building and deploying Android binary"
    local build_args=(--deploy)
    if [[ "${LLM_BACKEND}" == "litert" ]]; then
      build_args+=(--litert)
    fi
    "${PROJECT_ROOT}/scripts/android-build-and-deploy.sh" "${build_args[@]}"
  else
    warn "⚠ Skipping build (--skip-build). Assuming artifacts are already on device."
  fi

  prepare_env_and_configure_cli
  ensure_device_permissions_before_launch

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
  ok "   LLM backend:      ${LLM_BACKEND}"
  if [[ "${LLM_BACKEND}" == "ollama" ]]; then
    ok "   Ollama (host):    ${OLLAMA_OPENAI_BASE_URL}"
    ok "   To stop Ollama:   docker stop ${OLLAMA_CONTAINER_NAME}"
  else
    ok "   LiteRT model:     ${LITERT_MODEL_DEVICE}"
    ok "   LiteRT backend:   ${LITERT_BACKEND}"
  fi
}

main
