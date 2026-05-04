#!/usr/bin/env bash
# start_servers.sh — dual-server entrypoint for the e2e-http-tests Compose service.
#
# Boots Python uvicorn on :8000 and the Rust cognee-http-server on :8001,
# each in its own isolated workspace (/py and /rs), waits for both /health
# endpoints to respond, then execs whatever command was passed as arguments
# (typically: pytest -vs /harness/ -k 'test_http_').
#
# Set KEEP_RUNNING=1 to keep both servers up for interactive debugging
# (replaces the final exec with tail -f /dev/null so docker exec works).

set -euo pipefail

# ── Workspace directories ────────────────────────────────────────────────────
export PY_WORKSPACE=/py
export RS_WORKSPACE=/rs
mkdir -p "$PY_WORKSPACE" "$RS_WORKSPACE"

# ── Forward env vars honoured by both servers ────────────────────────────────
# These are inherited from the Compose env_file (.env) and passed through.
export OPENAI_TOKEN="${OPENAI_TOKEN:-}"
export OPENAI_URL="${OPENAI_URL:-}"
export OPENAI_MODEL="${OPENAI_MODEL:-gpt-4o-mini}"
export LLM_API_KEY="${LLM_API_KEY:-${OPENAI_TOKEN:-}}"
export LLM_API_ENDPOINT="${LLM_API_ENDPOINT:-${OPENAI_URL:-}}"
export LLM_MODEL="${LLM_MODEL:-${OPENAI_MODEL:-gpt-4o-mini}}"
export EMBEDDING_PROVIDER="${EMBEDDING_PROVIDER:-}"
export EMBEDDING_MODEL="${EMBEDDING_MODEL:-}"
export EMBEDDING_ENDPOINT="${EMBEDDING_ENDPOINT:-}"
export EMBEDDING_API_KEY="${EMBEDDING_API_KEY:-}"
export MOCK_EMBEDDING="${MOCK_EMBEDDING:-}"
export COGNEE_E2E_EMBED_MODEL_PATH="${COGNEE_E2E_EMBED_MODEL_PATH:-/opt/models/BGE-Small-v1.5-model_quantized.onnx}"
export COGNEE_E2E_TOKENIZER_PATH="${COGNEE_E2E_TOKENIZER_PATH:-/opt/models/bge-small-tokenizer.json}"

# ── Python cognee storage roots ──────────────────────────────────────────────
# BaseConfig.{data,system,cache}_root_directory default to paths resolved via
# Path(__file__).parent of the installed cognee package — i.e. inside
# /opt/python-venv/lib/python3.12/site-packages/cognee/, which is read-only.
# Point them at the per-run tmpfs PY_WORKSPACE so the SQLite migration can
# actually create its DB file and so cleanup is automatic between runs.
export DATA_ROOT_DIRECTORY="${DATA_ROOT_DIRECTORY:-$PY_WORKSPACE/.data_storage}"
export SYSTEM_ROOT_DIRECTORY="${SYSTEM_ROOT_DIRECTORY:-$PY_WORKSPACE/.cognee_system}"
export CACHE_ROOT_DIRECTORY="${CACHE_ROOT_DIRECTORY:-$PY_WORKSPACE/.cognee_cache}"
mkdir -p "$DATA_ROOT_DIRECTORY" "$SYSTEM_ROOT_DIRECTORY/databases" "$CACHE_ROOT_DIRECTORY"

# ── Run Python DB migrations once before booting uvicorn ────────────────────
echo "[start_servers] Running Python DB migrations..."
(cd "$PY_WORKSPACE" && python -m cognee.run_migrations 2>&1 || true)

# ── Start Python uvicorn on :8000 ────────────────────────────────────────────
echo "[start_servers] Starting Python uvicorn on :8000..."
(cd "$PY_WORKSPACE" && \
 exec uvicorn cognee.api.client:app \
      --host 127.0.0.1 --port 8000 \
      --log-level warning) &
PY_PID=$!

# ── Start Rust HTTP server on :8001 ──────────────────────────────────────────
echo "[start_servers] Starting Rust cognee-http-server on :8001..."
(cd "$RS_WORKSPACE" && \
 HTTP_API_HOST=127.0.0.1 \
 HTTP_API_PORT=8001 \
 ENV=test \
 exec cognee-http-server) &
RS_PID=$!

# ── Graceful shutdown on EXIT ─────────────────────────────────────────────────
trap 'kill "$PY_PID" "$RS_PID" 2>/dev/null || true' EXIT

# ── Wait for both servers to be healthy ──────────────────────────────────────
echo "[start_servers] Waiting for Python server health..."
/harness/wait_for_health.sh http://127.0.0.1:8000/health
echo "[start_servers] Waiting for Rust server health..."
/harness/wait_for_health.sh http://127.0.0.1:8001/health

echo "[start_servers] Both servers are healthy."

# ── Run command or keep alive for interactive debugging ───────────────────────
if [ "${KEEP_RUNNING:-0}" = "1" ]; then
    echo "[start_servers] KEEP_RUNNING=1 — keeping servers alive. Use docker exec to interact."
    tail -f /dev/null
else
    exec "$@"
fi
