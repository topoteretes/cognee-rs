# 12 — Re-enable cross-SDK parity CI

> Wave 3 · Priority P1 · Track A · Release-blocking: strongly recommended ·
> Effort: 1–2d · Depends on: — ·
> Source: [release-readiness-plan.md](../release-readiness-plan.md) Phase 3 (T3.1–T3.4)

[← back to index](00-INDEX.md)

## Goal

`.github/workflows/http-parity.yml` is the only CI that verifies the project's headline
"drop-in replacement for Python cognee" promise, and it is **disabled** —
`workflow_dispatch`-only — because the Python alembic migration chain fails on a virgin
SQLite DB (the harness boots a fresh DB every run, Python uvicorn never reaches a
healthy `/health`, `wait_for_health.sh` times out, Phase-1 fails before pytest runs).

This task: (1) **diagnose and fix** the alembic-on-clean-SQLite failure, (2) **re-enable**
the workflow on `push`/`pull_request` for at least the no-LLM Phase-1 deterministic
checks, (3) **wire** the LLM-gated phases to the existing CI OpenAI secret (already
plumbed — confirm), and (4) **document** which suites are required vs optional for the
release gate.

This is investigative; the steps below are a concrete diagnostic sequence, not a single
edit.

## Background & why

The cross-SDK harness boots **both** servers side by side and runs pytest against both:

- Python uvicorn on `:8000`, Rust `cognee-http-server` on `:8001`
  (`e2e-cross-sdk/bin/start_servers.sh`).
- Each in an isolated tmpfs workspace (`/py`, `/rs`) — a **true virgin DB every run**.
- `start_servers.sh` calls `python -m cognee.run_migrations` **before** booting uvicorn,
  then waits on both `/health` endpoints (`harness/wait_for_health.sh`, 30 s timeout).
  Note: that module call is effectively a no-op (no `__main__` block). The real alembic
  run happens inside uvicorn's lifespan startup event (`cognee/api/client.py:86`).

The Python image is built from the **sibling `cognee/` checkout** copied into the Docker
build context (`Dockerfile` stage 2, `COPY cognee/cognee /build/cognee`), pinned in the
workflow to a specific SHA.

### Root cause (already diagnosed in the workflow header — re-verify)

The actual failure path (verified 2026-06-15):

1. `start_servers.sh` calls `python -m cognee.run_migrations` — this is a **no-op**; the
   module only defines async functions and has no `__main__` block.
2. uvicorn starts and its **lifespan** (`cognee/api/client.py:86`) calls
   `run_startup_migrations()`, which shells out to `alembic upgrade head`
   (`/tmp/cognee-python/cognee/run_migrations.py:43`).
3. The initial revision `8057ae7329c2_initial_migration.py` has a **no-op** `upgrade()`:
   ```python
   # /tmp/cognee-python/cognee/alembic/versions/8057ae7329c2_initial_migration.py:20-22
   def upgrade() -> None:
       pass
   ```
   On a virgin SQLite DB, no base tables are created. The next migration in the chain,
   `ab7e313804ae_permission_system_rework`, calls `insp.get_columns("acls", ...)` which
   throws `NoSuchTableError` → alembic exits nonzero → `MigrationError` is raised.
4. The lifespan's `except` block calls `db_engine.create_database()` (creates all tables
   from ORM models) then calls `run_startup_migrations()` a **second** time.
5. The second alembic run fails because the DB now has tables but no `alembic_version`
   entry → alembic tries to apply all migrations from scratch → "table already exists"
   → another `MigrationError` propagates out of the lifespan → uvicorn crashes →
   `wait_for_health.sh` times out → Phase-1 fails.

The workflow header notes the upstream fix lives on `origin/fix/db-migrations`
(commit `8ab385033`), which replaces the no-op with a comprehensive
`deadbeef0001_new_initial_schema.py` (~30 `create_table` calls). As of 2026-06-15
that branch has **not** merged to `dev`/`main` — Option A cannot proceed yet; use
Option B below, or re-check when upstream merges.

> Note: this Python alembic issue is **independent** of the Rust SeaORM migration
> squash (task 11). Both touch "migrations on a virgin DB" but in different SDKs.

## Prerequisites

```bash
git checkout main && git pull
git checkout -b task/12-cross-sdk-parity-ci
```

Read first:
- `.github/workflows/http-parity.yml` (whole file — the header documents the failure).
- `.github/workflows/ci.yml` lines ~170 and ~377 — the existing
  `OPENAI_TOKEN: ${{ secrets.OPENAI_KEY }}` wiring pattern to copy (the `test:` and
  `capi-check:` job `env:` blocks).
- `e2e-cross-sdk/bin/start_servers.sh` — migration + dual-server boot.
- `e2e-cross-sdk/harness/wait_for_health.sh` — health-poll/timeout.
- `e2e-cross-sdk/Dockerfile` — how Python source is copied (stage 2,
  `COPY cognee/cognee /build/cognee`) and the harness assembled (stage 3).
- `e2e-cross-sdk/docker-compose.yml` — services (`e2e-http-tests`, `e2e-telemetry`,
  `e2e-tests`, `telemetry-proxy`), `env_file: ../.env`, `context: ../..`.
- Python: `/tmp/cognee-python/cognee/run_migrations.py`,
  `/tmp/cognee-python/cognee/alembic/versions/8057ae7329c2_initial_migration.py`,
  and `ls /tmp/cognee-python/cognee/alembic/versions/`.

## Diagnostic sequence (do this first)

1. **Reproduce locally** to confirm the failure is still the alembic no-op and not
   something new. From the monorepo root (`~/dev/cognee`, parent of both repos):
   ```bash
   cd ~/dev/cognee/cognee-rust/e2e-cross-sdk
   touch ../.env                         # docker-compose env_file stub
   cargo generate-lockfile --manifest-path ../Cargo.toml   # Cargo.lock is gitignored
   KEEP_RUNNING=1 docker compose -f docker-compose.yml run --rm e2e-http-tests bash
   ```
   Inside the container, run the migration step by hand and read the error:
   ```bash
   # The start_servers.sh module call is a no-op; trigger the real alembic path directly:
   cd /py && python -m alembic -c /opt/python-venv/lib/python3.12/site-packages/cognee/alembic.ini upgrade head
   ```
   Expect a `NoSuchTableError` on the `acls` table thrown by
   `ab7e313804ae_permission_system_rework`. Capture the exact revision id and error.

2. **Confirm the no-op initial migration** in the pinned ref:
   ```bash
   grep -n "def upgrade\|pass\|create_table" \
     /tmp/cognee-python/cognee/alembic/versions/8057ae7329c2_initial_migration.py
   ```
   `upgrade()` should be `pass`.

3. **Check whether upstream has fixed it** on `dev`/`main` since the workflow header was
   written (the header predicts a `deadbeef0001_new_initial_schema.py`):
   ```bash
   # In a throwaway clone of the Python repo on the target branch:
   git -C /tmp/cognee-python ls-files 'cognee/alembic/versions/*new_initial_schema*'
   git -C /tmp/cognee-python log --oneline -5 -- cognee/alembic/versions/
   ```
   If a comprehensive initial migration now exists on `dev`/`main`, **Option A** below is
   the clean fix. If not, use **Option B**.

## Fix options for the alembic failure

> **Status as of 2026-06-15:** `fix/db-migrations` has **not** merged to `dev`/`main`.
> Option A is blocked until upstream merges. **Start with Option B** now; switch to A
> once upstream ships. Option B is the least harness-invasive path available today.

### Option A — bump the pinned Python ref to one with a real initial migration

1. Find a `dev`/`main` SHA whose `alembic upgrade head` succeeds on a virgin SQLite DB
   (verify with the diagnostic in step 1 against that ref).
2. In `http-parity.yml`, update the "Checkout cognee (Python sibling repo)" step's
   `ref:` (currently `b9014c1661bff1d1d8bc831f3d31bc5a965bcaa3`) to that SHA.
3. Re-run the local reproduction to confirm both `/health` endpoints come up.

   Risk: a newer Python ref may change wire shapes the harness asserts against — run the
   full Phase-1 suite locally and fix any drift before enabling in CI.

### Option B — make the harness bootstrap the schema independent of the no-op migration

Two sub-approaches; pick the one that keeps the harness honest (it must still exercise
Python's real runtime schema):

- **B1 (preferred): let SQLAlchemy create the base tables, then `alembic stamp head`.**
  Python's ORM models define the full schema; the no-op initial migration exists *because*
  upstream historically created tables via `Base.metadata.create_all` and used alembic
  only for deltas. The alembic failure happens inside uvicorn's lifespan: its try/except
  calls `create_database()` but then calls `run_startup_migrations()` a second time without
  stamping, so the second alembic run fails on "table already exists". The fix: pre-stamp
  the DB in `start_servers.sh` **before** uvicorn starts, so the lifespan's first alembic
  call finds `alembic_version` at head and applies nothing. Sketch:
  ```bash
  # In start_servers.sh, ADD BEFORE the uvicorn start block:
  (cd "$PY_WORKSPACE" && python - <<'PY'
  import asyncio
  from cognee.infrastructure.databases.relational import get_relational_engine
  async def main():
      engine = get_relational_engine()
      await engine.create_database()   # SqlAlchemyAdapter.create_database() — confirmed at line 548
  asyncio.run(main())
  PY
  ) || true
  # Stamp alembic to head so uvicorn's lifespan migration is a no-op delta.
  ALEMBIC_INI=/opt/python-venv/lib/python3.12/site-packages/cognee/alembic.ini
  (cd "$PY_WORKSPACE" && python -m alembic -c "$ALEMBIC_INI" stamp head 2>&1 || true)
  ```
  `create_database()` is verified at `SqlAlchemyAdapter.py:548`. The alembic.ini path
  above is correct for the harness container (stage 3 installs cognee into
  `/opt/python-venv/lib/python3.12/site-packages/`). The stamp ensures uvicorn's
  lifespan `run_startup_migrations()` finds `alembic_version` at head and applies
  nothing — uvicorn starts cleanly, health check passes.

- **B2 (last resort): patch the no-op migration at image-build time.** In the Dockerfile
  Python stage, after copying the source, overwrite `8057ae7329c2_initial_migration.py`'s
  `upgrade()` with the comprehensive version (or copy the upstream
  `deadbeef0001_new_initial_schema.py` if obtainable). This couples the harness to a
  specific revision graph and is brittle — prefer B1.

Whichever fixes it, the success criterion is identical: `wait_for_health.sh http://127.0.0.1:8000/health`
returns healthy (the `python -m cognee.run_migrations` call in start_servers.sh is a
no-op and can be removed — the real gating step is the uvicorn health check).

## Re-enable the workflow

Once the local reproduction shows both servers healthy and Phase-1 green:

1. **Restore push/PR triggers** in `http-parity.yml`. Replace the `workflow_dispatch`-only
   `on:` block (and delete the now-stale `TODO(http-parity)` header) with:
   ```yaml
   on:
     push:
       branches: [ main, master ]
     pull_request:
       branches: [ main, master ]
     workflow_dispatch:
       inputs:
         run_phase3:
           description: "Run phase-3 specialty tests (websocket, sync, permissions, visualize)"
           required: false
           default: "false"
           type: boolean
   ```
   Keep the `concurrency` group as-is.

2. **Keep Phase-1 unconditional** (it needs no LLM): the existing
   "Phase-1 — health, auth, datasets, add, search, forget, openapi, errors" step and the
   "Telemetry parity" step already run without `OPENAI_KEY`. Confirm they have no
   `if: env.HAS_OPENAI_KEY` gate so they run on every push/PR.

3. **The LLM-gated phases are already wired** to `secrets.OPENAI_KEY` via the
   `HAS_OPENAI_KEY` pattern (matching `ci.yml`). Confirm each LLM step keeps:
   ```yaml
   if: ${{ env.HAS_OPENAI_KEY == 'true' }}
   env:
     HAS_OPENAI_KEY: ${{ secrets.OPENAI_KEY != '' }}
     OPENAI_TOKEN: ${{ secrets.OPENAI_KEY }}
     OPENAI_URL: https://api.openai.com/v1
     OPENAI_MODEL: gpt-4o-mini
   ```
   This is the **same secret** `ci.yml` uses (`OPENAI_TOKEN: ${{ secrets.OPENAI_KEY }}`),
   so no new secret is needed. On forks without the secret these steps skip gracefully —
   that is intended; Phase-1 still gates the PR.

4. **Leave Phase-3 on `workflow_dispatch` only** (websocket/sync/permissions/visualize) —
   it is slower/flakier and already gated to manual runs.

## Document required vs optional suites (T3.4)

Add a short "CI gate" section to `e2e-cross-sdk/README.md` (create it if absent) or to
`docs/http-server/` stating the release gate:

| Suite | Trigger | LLM | Release gate |
|---|---|---|---|
| Phase-1 (health/auth/datasets/add/search/forget/openapi/errors) | push + PR | no | **required** |
| Telemetry parity | push + PR | no | **required** |
| Logging parity | push + PR (gated on secret presence) | no | recommended |
| Phase-2 (cognify/remember/recall/memify/improve/llm) | push + PR when `OPENAI_KEY` set | yes | recommended (best-effort on forks) |
| Provenance parity | push + PR when `OPENAI_KEY` set | yes | recommended |
| Phase-3 (websocket/sync/permissions/visualize) | `workflow_dispatch` only | mixed | optional/manual |

Reference this table from [00-INDEX.md](00-INDEX.md)'s "Minimum release gate" entry for
task 12 so the gate definition is unambiguous.

## Verification

```bash
# 1. Local repro: migration succeeds and both servers are healthy.
cd ~/dev/cognee/cognee-rust/e2e-cross-sdk
touch ../.env && cargo generate-lockfile --manifest-path ../Cargo.toml
docker compose -f docker-compose.yml run --rm e2e-http-tests \
  pytest -vs /harness/ -k "test_http_(health|auth|datasets|add|search|forget|openapi|errors|self)" --tb=short
# Expected: Phase-1 passes (no OPENAI needed).

# 2. Telemetry parity (no LLM).
docker compose -f docker-compose.yml run --rm e2e-telemetry
# Expected: pass.

# 3. (with a real key exported) LLM-gated phase.
OPENAI_KEY=sk-... \
docker compose -f docker-compose.yml run --rm e2e-http-tests \
  pytest -vs /harness/ -k "test_http_(cognify|remember|recall|memify|improve|llm)" --tb=short

# 4. Lint the workflow YAML (no syntax error).
python -c "import yaml,sys; yaml.safe_load(open('.github/workflows/http-parity.yml'))"
```

Expected outcomes:
- Inside the container, `wait_for_health.sh` reports both `:8000` and `:8001` healthy
  (the `python -m cognee.run_migrations` call is a no-op; the alembic path is exercised
  through uvicorn's lifespan).
- Phase-1 + telemetry parity pass with no OpenAI secret.
- The workflow triggers on push/PR (verify on the PR for this task — the `HTTP Parity`
  check should appear and Phase-1 should run/pass).

## Acceptance criteria

- [ ] Root cause re-confirmed (no-op `8057ae7329c2` initial migration; alembic fails in
      uvicorn's lifespan on a virgin SQLite DB) and the chosen fix (Option A bump-ref
      **or** Option B harness shim) makes `wait_for_health.sh http://127.0.0.1:8000/health`
      return healthy.
- [ ] `http-parity.yml` triggers on `push` and `pull_request` (plus retained
      `workflow_dispatch`); the stale `TODO(http-parity)` header block is removed/updated.
- [ ] Phase-1 deterministic checks + telemetry parity run with **no** OpenAI secret and
      pass.
- [ ] LLM-gated phases use `secrets.OPENAI_KEY` (same secret as `ci.yml`) and skip
      cleanly when absent.
- [ ] Phase-3 specialty tests remain `workflow_dispatch`-only.
- [ ] Required-vs-optional suite table documented and linked from the release gate.
- [ ] The `HTTP Parity` check shows green on this task's PR.

## Gotchas / do-not

- **Cargo.lock is gitignored** (workspace policy). The Dockerfile `COPY`s it, so CI must
  `cargo generate-lockfile` first — keep that step (it already exists in the workflow). Do
  **not** commit `Cargo.lock` to "fix" it (see user memory: no committed Cargo.lock).
- **Build cost:** this image compiles the full Rust release binary + lbug C++ via ccache
  and downloads ONNX models. Keep the Docker-layer cache, `free-disk-space`, and
  buildkit-cache-dance steps — removing them will OOM/timeout `ubuntu-latest`.
- **Pinned ref over `ref: dev`:** if using Option A, pin a SHA, never a branch name — the
  workflow header explains upstream churn must not silently break CI.
- **Don't loosen parity tolerances to make it pass.** The structural cognify comparison
  uses deliberate tolerances (node/edge counts within 50%, node-type Jaccard ≥ 0.3 — see
  `harness/test_cognify_structural.py`). If LLM-gated tests fail, investigate parity, do
  not widen tolerances to green the build.
- **Health timeout:** `wait_for_health.sh` is 30 s. If the fix makes startup slower (e.g.
  B1 creating all tables), bump the loop count rather than masking a real hang.
- **B2 brittleness:** patching a specific alembic revision file couples CI to the revision
  graph; prefer A or B1. If you must use B2, add a comment pinning the upstream revision
  it shadows so a future ref bump doesn't silently double-create tables.
- **Secret scope:** `secrets` is unavailable in step-level `if:`; the existing pattern
  routes it through `env.HAS_OPENAI_KEY` — preserve that indirection.

## Rollback

The workflow is additive/CI-only — reverting restores `workflow_dispatch`-only:
```bash
git checkout main -- .github/workflows/http-parity.yml
git checkout main -- e2e-cross-sdk/bin/start_servers.sh e2e-cross-sdk/Dockerfile  # if touched
```
No production code or on-disk format is affected by this task.
