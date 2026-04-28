# HTTP Server — Cross-SDK Parity Harness

Specification for the **HTTP-level** parity tests between the Python and Rust HTTP servers. Reuses and extends the existing Docker harness in [`e2e-cross-sdk/`](../../e2e-cross-sdk/), which today runs CLI-vs-CLI parity tests (`test_add_parity.py`, `test_cognify_structural.py`, etc.). The harness defined here adds a **uvicorn ↔ `cognee-http-server`** test mode that hits both servers over HTTP and diffs responses.

Companion docs: [plan.md](plan.md), [architecture.md](architecture.md). Per-router contracts are in `routers/*.md` and serve as the *normative spec* the harness checks against.

## 1. Goals & non-goals

### Goals

- **Wire-level parity**: identical input → identical output JSON (modulo timestamps, UUIDs, and other inherently non-deterministic fields). Catches drift the moment it happens.
- **Reuse the existing infrastructure**: same Dockerfile pattern, same `tmpfs` workspace, same pytest runner. The only new piece is a way to run two servers side by side.
- **Catch divergence early in CI**: the HTTP suite runs on every PR. Drift fails fast.
- **Catch drift on both sides**: an unintentional change in Python *or* Rust that breaks compatibility is caught equally. The suite is symmetric.
- **Fixture-driven, not snapshot-driven where it matters**: structural diffs (key sets, types, ordering) over byte-equal snapshots, with explicit allowlists for fields that legitimately differ.

### Non-goals

- **Exhaustive performance benchmarking**: we don't compare latency or throughput. Add a separate `bench/` if/when needed.
- **Database-level diffing**: covered by the existing CLI-driven parity tests (`test_add_parity.py`). The HTTP suite focuses on what the wire emits.
- **Full WebSocket frame replay**: covered at unit level in `crates/http-server/tests/ws/`. The cross-SDK harness only sanity-checks that a connect → terminal-frame → close cycle works on both sides.
- **Rate-limiting / load-test parity**: phase 2 if needed.

## 2. Architecture

```
                    ┌─────────────────────────────────────┐
                    │   docker compose up                 │
                    │   (cognee-rust/e2e-cross-sdk/)      │
                    └────────────────┬────────────────────┘
                                     │
                                     ▼
            ┌──────────────────────────────────────────────────┐
            │  e2e-tests container (3-stage Dockerfile)        │
            │                                                  │
            │   ┌───────────────────────┐  ┌────────────────┐  │
            │   │  Python uvicorn       │  │  Rust          │  │
            │   │  cognee.api.client    │  │  cognee-http-  │  │
            │   │  on :8000             │  │  server :8001  │  │
            │   │  (workspace = /py)    │  │  (workspace =  │  │
            │   │                       │  │   /rs)         │  │
            │   └───────────┬───────────┘  └───────┬────────┘  │
            │               │                      │           │
            │               └──────────┬───────────┘           │
            │                          ▼                       │
            │             ┌────────────────────────┐           │
            │             │  pytest harness        │           │
            │             │  /harness/test_http_*  │           │
            │             │  (httpx clients)       │           │
            │             └────────────────────────┘           │
            └──────────────────────────────────────────────────┘
```

Both servers run inside the same container and listen on different ports. The pytest harness has an `httpx` client per server (`py_client` and `rs_client` fixtures), and most tests run the same request against both then assert structural equality of the responses.

### 2.1 Why two servers in one container

- Same OS/kernel/clock — eliminates per-host divergence.
- Same `tmpfs` mount — eliminates filesystem-driver differences.
- One `docker-compose.yml` service → simplest CI wiring.
- Per-server isolation through `WORKSPACE_PY=/py` and `WORKSPACE_RS=/rs` so file-system writes don't collide.

The existing CLI-parity tests use this exact pattern (separate workspaces); we reuse it. See [`e2e-cross-sdk/Dockerfile`](../../e2e-cross-sdk/Dockerfile).

## 3. Container additions

The Dockerfile already builds:

1. **Stage 1**: Rust release binary (`cognee-cli`).
2. **Stage 2**: Python venv with `cognee` editable-installed.
3. **Stage 3**: Combined runtime with both binaries on `$PATH` and a pytest harness.

We add:

- **Stage 1 build target**: also produce `cognee-http-server` (the new binary from [architecture.md §17](architecture.md#17-binary-cognee-http-server)). One extra `cargo build -p cognee-http-server --features bin --release`.
- **Stage 3 entrypoint script** (`bin/start_servers.sh`):
  ```bash
  #!/usr/bin/env bash
  set -euo pipefail

  # Workspace dirs (per server) — keep DBs, file storage, graph dirs isolated.
  export PY_WORKSPACE=/py
  export RS_WORKSPACE=/rs
  mkdir -p "$PY_WORKSPACE" "$RS_WORKSPACE"

  # Bring up Python uvicorn on :8000
  (cd "$PY_WORKSPACE" && \
   exec uvicorn cognee.api.client:app --host 127.0.0.1 --port 8000 \
        --log-level warning) &
  PY_PID=$!

  # Bring up Rust server on :8001
  (cd "$RS_WORKSPACE" && \
   HTTP_API_HOST=127.0.0.1 HTTP_API_PORT=8001 \
   exec cognee-http-server) &
  RS_PID=$!

  # Wait for both to be ready, then exec pytest
  /harness/wait_for_health.sh http://127.0.0.1:8000/health
  /harness/wait_for_health.sh http://127.0.0.1:8001/health

  trap "kill $PY_PID $RS_PID" EXIT
  exec pytest -vs /harness/test_http_*
  ```
- **`docker-compose.yml`** gets a second service `e2e-http-tests` that runs the new entrypoint. The existing `e2e-tests` service stays unchanged.

## 4. pytest fixtures

Defined in `e2e-cross-sdk/harness/conftest.py` (extending the existing file):

```python
import httpx
import pytest

PY_BASE = "http://127.0.0.1:8000"
RS_BASE = "http://127.0.0.1:8001"

@pytest.fixture
def py_client():
    with httpx.Client(base_url=PY_BASE, timeout=60.0) as c:
        yield c

@pytest.fixture
def rs_client():
    with httpx.Client(base_url=RS_BASE, timeout=60.0) as c:
        yield c

@pytest.fixture
def both_clients(py_client, rs_client):
    return {"py": py_client, "rs": rs_client}

@pytest.fixture
def authed_clients(both_clients):
    """Login on both servers as the same user; return clients with cookies set."""
    creds = {"username": "test@example.com", "password": "test_password_123"}
    for name, client in both_clients.items():
        # Bootstrap user on first run via /auth/register; ignore "already exists".
        client.post("/api/v1/auth/register", json={**creds, "is_verified": True})
        r = client.post("/api/v1/auth/login", data=creds)
        assert r.status_code == 200, f"{name} login failed: {r.text}"
    return both_clients
```

A small helper, `assert_responses_match(py_resp, rs_resp, *, ignore=())`, lives in `harness/http_helpers.py`:

```python
def assert_responses_match(py, rs, *, ignore=()):
    """Strict status-code + structural JSON match. Fields in `ignore`
    (paths like 'data[*].created_at' or 'pipeline_run_id') are stripped
    before comparison."""
    assert py.status_code == rs.status_code, (
        f"status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py body: {py.text}\nrs body: {rs.text}"
    )
    py_json = strip_paths(py.json(), ignore)
    rs_json = strip_paths(rs.json(), ignore)
    assert py_json == rs_json, ...
```

## 5. Test inventory

One file per area. Each test is a parameterized run over the request matrix; failures pinpoint the diverging endpoint.

| File | What it covers | Dependencies |
|---|---|---|
| `test_http_health.py` | `GET /`, `GET /health`, `GET /health/detailed` | None |
| `test_http_auth.py` | `/auth/register`, `/auth/login`, `/auth/me`, `/auth/logout`, JWT verification (issue with one server, present at the other), API-key roundtrip | DB ready |
| `test_http_datasets.py` | `GET /datasets`, `POST /datasets`, `GET /datasets/{id}`, `GET /datasets/status?dataset=`, `DELETE /datasets/{id}`, `/{id}/data`, `/{id}/data/{did}/raw` | LLM not required |
| `test_http_add.py` | `POST /add` with text + URL + multi-file uploads; assert `data_id`/`dataset_id`/`content_hash` match | LLM not required |
| `test_http_update.py` | `PATCH /update` round-trip | LLM not required |
| `test_http_search.py` | `GET /search`, `POST /search` for each of the 9 implemented `SearchType`s | OpenAI |
| `test_http_recall.py` | `POST /recall` parity (auto-routed `SearchType` selection) | OpenAI |
| `test_http_cognify.py` | `POST /cognify` blocking; structural compare on the returned pipeline-run dict | OpenAI |
| `test_http_remember.py` | `POST /remember` blocking | OpenAI |
| `test_http_memify.py` | `POST /memify` blocking | OpenAI |
| `test_http_improve.py` | `POST /improve` blocking | OpenAI |
| `test_http_forget.py` | `POST /forget` (`data_id` / `dataset` / `everything`) | None |
| `test_http_delete.py` | Deprecated `DELETE /delete?...` | None |
| `test_http_ontologies.py` | `POST /ontologies`, `GET /ontologies` | None |
| `test_http_visualize.py` | `GET /visualize?dataset_id=` returns HTML; bytewise diff after stripping the JSON-island and dataset id | LLM not required (after cognify) |
| `test_http_settings.py` | `GET /settings`, `POST /settings` | None |
| `test_http_configuration.py` | `POST /configuration/store_user_configuration`, `GET /configuration/get_user_configuration/{id}` | None |
| `test_http_permissions.py` | Tenant + role + ACL CRUD | None |
| `test_http_users.py` | `GET /users/{id}`, `PATCH /users/me`, `POST /users/get-user-id` | None |
| `test_http_api_keys.py` | `POST /api-keys`, `GET /api-keys`, `DELETE /api-keys/{id}` | None |
| `test_http_activity.py` | `GET /activity/pipeline-runs`, `GET /activity/users`, `GET /activity/agents` | None |
| `test_http_sync.py` | `POST /sync`, `GET /sync/status` | None |
| `test_http_llm.py` | `POST /llm/custom-prompt`, `POST /llm/infer-schema` | OpenAI |
| `test_http_websocket.py` | Connect to `/cognify/subscribe/{id}` after starting a background cognify; assert the JSON shape of frames matches; assert close code 1000 | OpenAI |
| `test_http_openapi.py` | `GET /openapi.json` from both servers; **structural** diff (path set, method set, security scheme set, top-level components) — not byte equality | None |
| `test_http_cors.py` | Preflight OPTIONS; allowed origins / methods / headers match | None |
| `test_http_errors.py` | Validation errors (`400 {"detail": [...], "body": ...}`), `LOGIN_BAD_CREDENTIALS`, `418 fallback`, missing-auth `401` | None |

**Phase 1 selection** (gate the PR that lands the server): `test_http_health`, `test_http_auth`, `test_http_datasets`, `test_http_add`, `test_http_search`, `test_http_forget`, `test_http_openapi`, `test_http_errors`. These are all OpenAI-optional or quick.

**Phase 2** (LLM-gated): `cognify`, `remember`, `recall`, `memify`, `improve`, `llm`.

**Phase 3** (specialty): `websocket`, `sync`, `permissions`, `visualize`.

## 6. Diff strategy

### 6.1 What we compare

- HTTP **status code**: must match exactly.
- HTTP **headers**: targeted comparisons. `Content-Type` must match. `Content-Length` not compared (gzip / framing differences). `Set-Cookie` compared after stripping volatile attributes (expiration timestamp).
- Response **body**:
  - JSON responses: structural diff on the parsed JSON, not the raw bytes. Order of object keys is unstable across runtimes; we compare canonicalized JSON.
  - HTML responses (`/visualize`): strip the embedded JSON island and compare the rest as bytes.
  - Binary responses (`/datasets/{id}/data/{did}/raw`): bytewise SHA-256 equality.

### 6.2 What we strip / ignore

A central `IGNORE` map in `harness/http_helpers.py`:

```python
DEFAULT_IGNORE = {
    "$.created_at", "$.updated_at",
    "$..created_at", "$..updated_at",
    "$..pipeline_run_id",        # deterministic in theory but seed differs across SDKs
    "$..run_info.duration_ms",
    "$..access_token",           # JWTs differ by `iat`
    "$..token_type",             # always "bearer"; trivially equal but skip to allow cookie-only
    "$..session.id",
    "$..run_id",
}
```

Per-test extensions ride alongside:

```python
def test_post_add(authed_clients, sample_text):
    py = authed_clients["py"].post("/api/v1/add", files={...}, data={...})
    rs = authed_clients["rs"].post("/api/v1/add", files={...}, data={...})
    assert_responses_match(
        py, rs,
        ignore=DEFAULT_IGNORE | {"$..tenant_id"},   # default-tenant id differs across runs
    )
```

### 6.3 What's a hard mismatch

- Status code differs.
- Top-level key set differs.
- A key present in Python is missing in Rust (or vice versa).
- A field's type differs (`str` vs `int`, `null` vs `[]`).

### 6.4 What's a soft mismatch (logged, not failed)

- Field order in arrays where the API doesn't promise ordering (e.g. `datasets` list returned without `ORDER BY`).
- Cosmetic differences in error messages — only the `detail` *code* must match; the human-readable message is allowed to differ. The Python-parity error codes are documented in [auth.md §8](auth.md#8-endpoints) and `routers/*.md`.

## 7. Authentication strategy in tests

- **Single shared user**: every parity test logs in (or registers + logs in) as `test@example.com`. Both servers see the same email; they have *different* `users.id` UUIDs (independent DBs), but `assert_responses_match` strips ID fields from comparison.
- **Single shared API key**: `test_http_api_keys.py` exercises POST then re-uses the issued key for subsequent calls *within the same suite run*. Cross-server key reuse is not tested — the keys are independently generated.
- **JWT-cross-server compatibility**: `test_http_auth.py::test_jwt_cross_compat` is the canary that proves the secret/audience contract holds. Issue a token with Python, present it to Rust; assert `/me` returns 200. Then the reverse. If this fails, *every other auth-using test is suspect*.

## 8. Pre-test seeding

Some tests need state in place before hitting the HTTP layer.

`harness/seed.py` exposes helpers:

```python
def seed_dataset_with_text(client: httpx.Client, *, name: str, text: str) -> dict:
    """POST /add with a text part; return the parsed response."""

def seed_cognify(client: httpx.Client, *, dataset_id: str) -> dict:
    """POST /cognify, blocking; return the parsed response."""
```

Tests call these against both servers in their setup so the cross-server diff begins from equivalent seeded states. Any seed-time divergence fails fast and clearly.

## 9. CI integration

In `.github/workflows/`:

```yaml
http-parity:
  runs-on: ubuntu-latest
  needs: [lib-tests, lint]
  steps:
    - uses: actions/checkout@v4
    - run: docker compose -f cognee-rust/e2e-cross-sdk/docker-compose.yml \
             run --rm e2e-http-tests
  env:
    OPENAI_TOKEN: ${{ secrets.OPENAI_KEY }}
    OPENAI_URL:   https://api.openai.com/v1
    OPENAI_MODEL: gpt-4o-mini
```

Phase-1 tests run without `OPENAI_TOKEN`. The phase-2 / phase-3 jobs are gated on the secret being present (the existing `lib-tests.yml` pattern handles this).

Failure mode: pytest produces a machine-readable diff per failed test (the structured assertion error from `assert_responses_match`) so the CI logs pinpoint exactly which response field diverged.

## 10. Local development workflow

```bash
cd cognee-rust/e2e-cross-sdk
docker compose run --rm e2e-http-tests           # full suite
docker compose run --rm e2e-http-tests \
  pytest -vs /harness/test_http_add.py           # single file
docker compose run --rm e2e-http-tests \
  pytest -vs -k "test_register"                  # by name
```

A `--keep-running` mode (in the entrypoint) leaves both servers up so a developer can shell into the container and `httpx`/`curl` against either by hand.

## 11. Test-data hygiene

- Every test uses fresh dataset names with a `uuid4()` suffix to avoid cross-test contamination.
- The `tmpfs` workspace is wiped per `docker compose run` invocation.
- API keys created in `test_http_api_keys.py` are deleted at teardown.
- The Rust-side relational DB is recreated from migrations on every container start.
- The Python-side venv runs `python -m cognee.run_migrations` at container start so its DB matches.

## 12. Open questions

1. **Single-container vs two-container layout**: keeping both servers in one container is simpler but couples their lifecycles. If parallelization across servers becomes a bottleneck, split into two services that share a network. Defer.
2. **Shared workspace vs per-server**: per-server today. If we want to test "Python writes, Rust reads" via *direct shared workspace* (skipping HTTP), the existing CLI parity tests already cover that — keep this suite HTTP-only.
3. ~~**Snapshot vs structural diffs**~~ **Resolved (P8)**: `harness/golden/openapi.python.json` is committed as an informational reference snapshot; the structural-diff test does not assert on this file but reviewers can diff it to detect Python-side additions. Per-endpoint snapshots remain a follow-up.
4. **Time-bound runs**: phase-2 tests with LLM calls can take 60s+. Consider a `--quick` mode that mocks the LLM via `MOCK_EMBEDDING=true` + a cached LLM response fixture — separate proposal.
5. ~~**WebSocket binary diff**~~ **Resolved (P8)**: `WS_YIELD_TOLERANCE = 2` is the accepted yield-count delta, encoded as a named constant in `harness/http_helpers.py`. The WS test asserts both frame shape and that the frame-count difference between the two servers does not exceed this value.
6. **TLS**: the suite runs over plain HTTP. Production deployments terminate TLS at a proxy; this suite doesn't. If a TLS-specific behavior bug ever appears, add a fixture with `httpx-tls` and self-signed certs.

## 13. References

- Existing CLI-driven cross-SDK harness: [`e2e-cross-sdk/`](../../e2e-cross-sdk/).
- Existing CLI parity test patterns:
  - [`test_add_parity.py`](../../e2e-cross-sdk/harness/test_add_parity.py)
  - [`test_cognify_structural.py`](../../e2e-cross-sdk/harness/test_cognify_structural.py)
  - [`test_cross_read.py`](../../e2e-cross-sdk/harness/test_cross_read.py)
- Python server: [`cognee/api/client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py).
- Rust server entry point: `cognee-http-server` (specified in [architecture.md §17](architecture.md#17-binary-cognee-http-server)).
- Per-router contracts (the normative spec the harness checks against): `routers/*.md`.
- Auth fixtures: [auth.md §8](auth.md#8-endpoints).
- WebSocket frame format: [websocket.md §5](websocket.md#5-frame-format).
- Pipeline-run contract: [pipelines.md §3](pipelines.md#3-status-taxonomy-and-wire-mapping).
