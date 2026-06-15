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

# ── Bootstrap Python DB schema before booting uvicorn (Option B1) ────────────
#
# Background: the initial alembic migration (8057ae7329c2_initial_migration.py)
# is a no-op `pass`.  On a virgin SQLite DB, alembic's first run in uvicorn's
# lifespan (cognee/api/client.py:86) finds the DB empty, tries subsequent
# migrations that assume base tables already exist (e.g.
# ab7e313804ae_permission_system_rework calls insp.get_columns("acls", ...)),
# and raises NoSuchTableError → MigrationError.  The lifespan's except block
# then calls create_database() and retries run_startup_migrations() a second
# time; that second alembic run finds tables but no alembic_version entry and
# fails again with "table already exists" → uvicorn crashes → /health never
# responds → wait_for_health.sh times out.
#
# Fix (B1): pre-populate the schema + stamp alembic to head BEFORE uvicorn
# starts so the lifespan migration is a no-op delta.
#   1. create_database() — SqlAlchemyAdapter.create_database() at line 548,
#      calls Base.metadata.create_all (line 572) which creates every ORM table.
#   2. alembic stamp head — records alembic_version=<head> without running any
#      migration SQL; on the next alembic upgrade head (inside uvicorn's
#      lifespan), alembic sees no pending revisions and exits cleanly.
#
# The `python -m cognee.run_migrations` call was previously a no-op (the
# module defines only async functions with no __main__ block) and is replaced
# by this two-step initialisation.

echo "[start_servers] Bootstrapping Python DB schema (create_database)..."
python - <<'PY' || { echo "[start_servers] ERROR: create_database() failed — aborting startup" >&2; exit 1; }
import asyncio
from cognee.infrastructure.databases.relational import get_relational_engine

async def main():
    engine = get_relational_engine()
    await engine.create_database()
    print("[start_servers] create_database() succeeded", flush=True)

asyncio.run(main())
PY

echo "[start_servers] Stamping alembic to head..."
ALEMBIC_INI=/opt/python-venv/lib/python3.12/site-packages/cognee/alembic.ini
if [ ! -f "$ALEMBIC_INI" ]; then
    echo "[start_servers] ERROR: alembic.ini not found at $ALEMBIC_INI — aborting startup" >&2
    exit 1
fi
(cd "$PY_WORKSPACE" && python -m alembic -c "$ALEMBIC_INI" stamp head 2>&1) \
    || { echo "[start_servers] ERROR: alembic stamp head failed — aborting startup" >&2; exit 1; }
echo "[start_servers] alembic stamp head succeeded."

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
