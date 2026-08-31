# Cross-SDK E2E Tests

Docker-based harness that verifies parity between the Python and Rust cognee
HTTP servers running side by side.

## Architecture

A 3-stage Dockerfile builds both CLIs (Rust release binary + Python venv) into
a single image. `bin/start_servers.sh` boots Python uvicorn on `:8000` and the
Rust `cognee-http-server` on `:8001`, each in an isolated tmpfs workspace, then
pytest runs against both.

## Running locally

```bash
cd cognee-rust/e2e-cross-sdk
touch ../.env                         # stub required by docker-compose env_file
cargo generate-lockfile --manifest-path ../Cargo.toml  # Cargo.lock is gitignored

# Phase-1 (no LLM):
docker compose -f docker-compose.yml run --rm e2e-http-tests \
  pytest -vs /harness/ -k "test_http_(health|auth|datasets|add|search|forget|openapi|errors|self)" --tb=short

# Telemetry parity (no LLM):
docker compose -f docker-compose.yml run --rm e2e-telemetry

# LLM-gated phases (requires OPENAI_KEY):
OPENAI_TOKEN=sk-... \
docker compose -f docker-compose.yml run --rm e2e-http-tests \
  pytest -vs /harness/ -k "test_http_(cognify|remember|recall|memify|improve|llm|hybrid)" --tb=short
```

## COGX golden archive

`harness/golden/cogx_archive/` is a COGX archive written by the Rust exporter
itself. `test_cogx_import_contract.py` runs Python cognee's real migration
loader over it, which gates the Rust→Python format contract on every PR — the
full roundtrip needs an LLM (you cannot export a graph you have not cognified)
and so only runs on key-gated lanes.

Regenerate it whenever the exporter's output changes; the same Rust test fails
if the committed copy drifts:

```bash
COGX_REGENERATE_GOLDEN=1 cargo test -p cognee-migration --test python_import_contract
```

## DB bootstrap (Option B1 fix)

The Python alembic initial migration (`8057ae7329c2_initial_migration.py`) is a
no-op `pass`. On a virgin tmpfs workspace this caused uvicorn's lifespan to fail
on subsequent migrations that assumed base tables already existed. `start_servers.sh`
now pre-bootstraps the schema before uvicorn starts:

1. `create_database()` — calls `Base.metadata.create_all` to create all ORM
   tables (`SqlAlchemyAdapter.py:548`).
2. `alembic stamp head` — records `alembic_version=<head>` so uvicorn's lifespan
   migration is a no-op delta.

Both steps fail loudly (nonzero exit, error to stderr) if they encounter a
problem, so a broken Python environment surfaces immediately rather than hanging
on the health timeout.

## CI gate

The `HTTP Parity` workflow (`.github/workflows/http-parity.yml`) runs on every
push and PR to `main`/`master`.

| Suite | Trigger | LLM | Release gate |
|---|---|---|---|
| Phase-1 (health/auth/datasets/add/search/forget/openapi/errors) | push + PR | no | **required** |
| Telemetry parity | push + PR | no | **required** |
| COGX import contract (Python reads a Rust archive) | push + PR | no | **required** |
| COGX roundtrip (Rust export → Python import) | push + PR when `OPENAI_KEY` set | yes | recommended |
| Logging parity | push + PR (when `OPENAI_KEY` present) | no | recommended |
| Phase-2 (cognify/remember/recall/memify/improve/llm/hybrid) | push + PR when `OPENAI_KEY` set | yes | recommended (best-effort on forks) |
| Provenance parity | push + PR when `OPENAI_KEY` set | yes | recommended |
| Phase-3 (websocket/sync/permissions/visualize) | `workflow_dispatch` only | mixed | optional/manual |

Phase-1 and Telemetry parity run unconditionally on every push/PR (no OpenAI
key required). LLM-gated suites use `secrets.OPENAI_KEY` (same secret as
`ci.yml`) and skip cleanly on forks without the key — Phase-1 still gates
every PR. Phase-3 is triggered manually via `workflow_dispatch` with
`run_phase3: true`.
