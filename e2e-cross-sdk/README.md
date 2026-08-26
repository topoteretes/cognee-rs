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

## What this harness does and does not prove

Read this before citing a green run as evidence of parity.

**Two lanes, different strengths.** The HTTP lane (`test_http_*`) compares
responses through `assert_responses_match`, which strips a tolerance set
(`http_helpers.py:DEFAULT_IGNORE`) that includes **every `id` field**. The CLI/DB
lane (`test_*_parity`, `test_cross_*`, `test_readd_*`) shells out to both CLIs and
compares SQLite state directly with exact equality on content hashes, UUIDs and
row counts — no ignore list in the path. The CLI/DB lane is the stronger evidence
and, until recently, no CI filter selected it.

**Known limits, in rough order of how much they weaken a green run:**

- **Auth-gated tests skip by default.** The OSS `cognee-http-server` ships no auth
  router, and `bin/start_servers.sh` launches that binary, so `authed_clients`
  skips — currently ~20 of 46 files. The run now prints a summary saying so; set
  `COGNEE_PARITY_REQUIRE_AUTH=1` to turn those skips into failures, or boot a
  closed `cognee-http-cloud` binary.
- **The two sides do not run the same vector backend.** `start_servers.sh` sets
  `VECTOR_DB_PROVIDER=mock` for Rust (the OSS server's only non-pgvector option,
  and the harness has no Postgres) while Python falls through to its `lancedb`
  default. **Any vector-specific result from this harness is therefore not a
  parity result.** Graph (`ladybug`), relational (`sqlite`) and embedding provider
  (`openai`) *are* matched. Fixing this needs either Postgres in the compose file
  so both sides can use pgvector, or a Python-side mock vector provider.
- **Per-test ignore sets can remove the thing under test.** `test_http_llm.py`
  ignores `$..output` and `$..response`; `test_http_v2_recall.py` ignores `$..text`,
  `$..answer` and `$..score`; `test_http_errors.py` ignores error messages. When
  adding a case, check its ignore set actually leaves your oracle visible.
- **"Both absent" scores as agreement.** Tests that accept 404-on-both convert
  *neither side implements this* into a pass. Prefer `xfail(strict=True)` keyed to
  a tracked gap.
- **Rust's identity is seeded from Python's database.** `conftest.py` reads
  `owner_id`/`tenant_id` out of Python's SQLite and passes them to the Rust CLI,
  so UUID5 agreement proves the hash function agrees on identical inputs — not
  that both sides derive the same inputs.
- **No OpenAPI comparison exists.** `harness/golden/openapi.python.json` is a
  placeholder with empty `paths`, and `test_http_openapi.py` is skipped.
- **LLM determinism is unwired.** Rust has `MOCK_LLM`, `crates/llm/src/mock/` and
  cassette replay (`COGNEE_RECORD_LLM` / `COGNEE_TEST_REPLAY`); nothing under
  `e2e-cross-sdk/` uses any of it. Python has no general LLM mock, and a shared
  fake cannot key on prompt text because the two SDKs' prompts differ.

A fuller accounting, with the divergences these limits were hiding, is in
[`docs/roadmap/python-parity-audit.md`](../docs/roadmap/python-parity-audit.md).

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
