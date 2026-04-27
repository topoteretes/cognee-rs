# Implementation: P8 — Cross-SDK HTTP parity harness

## 1. Goal

Land the **HTTP-level** cross-SDK parity harness: a new `e2e-http-tests` Docker Compose service that runs the Python uvicorn server (`cognee.api.client:app`) on `:8000` and the Rust `cognee-http-server` binary on `:8001` *inside the same container*, with isolated per-server tmpfs workspaces, then drives both with `httpx` clients from a pytest harness that asserts JSON-response equality (modulo allowlisted volatile fields). Reuses the existing `e2e-cross-sdk/` 3-stage Dockerfile, conftest, and Compose file — the existing CLI-driven `e2e-tests` service stays untouched. The phase ends with phase-1 tests (no LLM) running green on a clean checkout, phase-2 (LLM-gated) running green when `OPENAI_KEY` is configured, and a CI workflow that gates PRs touching `crates/http-server/` or `cognee/api/`.

## 2. References (read these before starting)

- Spec: [e2e-parity.md](../e2e-parity.md) — full harness design (architecture, container additions, fixtures, test inventory, diff strategy, pre-test seeding, CI integration). **Normative.**
- Phase summary: [plan.md §4 P8](../plan.md#4-implementation-phases) and [§7 Q5 (OpenAPI normalizer)](../plan.md#7-open-questions).
- Binary spec: [architecture.md §17](../architecture.md#17-binary-cognee-http-server) — `cognee-http-server` build flags (`--features bin`), CLI args, env-var fallbacks (`HTTP_API_HOST`, `HTTP_API_PORT`, `CORS_ALLOWED_ORIGINS`, `ENV`).
- Per-router contracts (the normative spec the diff checks against): all 30 docs in [routers/](../routers/). The harness reads these to derive per-test `ignore=` extensions.
- Existing harness: [`e2e-cross-sdk/Dockerfile`](../../../e2e-cross-sdk/Dockerfile), [`docker-compose.yml`](../../../e2e-cross-sdk/docker-compose.yml), [`harness/conftest.py`](../../../e2e-cross-sdk/harness/conftest.py), [`harness/test_add_parity.py`](../../../e2e-cross-sdk/harness/test_add_parity.py) — the conventions to extend.
- Status template + invariants: [implementation/README.md](README.md).
- Auth fixtures used by `authed_clients`: [auth.md §8](../auth.md#8-endpoints).
- Pipeline-run wire shape used by `test_http_cognify`/`remember`/`improve`/`memify`: [pipelines.md §3](../pipelines.md#3-status-taxonomy-and-wire-mapping).
- WebSocket frame shape used by `test_http_websocket`: [websocket.md §5](../websocket.md#5-frame-format).

## 3. Prerequisites

The harness can land **incrementally** alongside server work — the phase-1 test set only needs P0–P2 (`/health`, auth, datasets, `/add`, `/forget`, `/openapi.json`, error envelope) to be wire-callable. Phase-2 (LLM-gated) needs P3+P4 (`/cognify`, `/remember`, `/memify`, `/improve`, `/search`, `/recall`, `/llm`). Phase-3 (specialty) needs P3 (websocket), P5 (permissions), P4 (visualize), P6 (sync). See [e2e-parity.md §5](../e2e-parity.md#5-test-inventory) for the full phase split.

This phase doc covers all three phases of test rollout in one place; ship phase-1 in the PR that closes P2, phase-2 in the PR that closes P4, phase-3 in the PR that closes P7. Each phase ships independently — the Compose service and pytest harness skeleton land once, then test files arrive in waves.

For the **first** PR that lands the harness skeleton (Steps 1–6), only P0 needs to be in place: enough for the Rust server to boot and serve `/health`.

## 4. Step-by-step

The 14 steps below split into three execution waves matching the test-rollout phases in [e2e-parity.md §5](../e2e-parity.md#5-test-inventory):

- **Wave A (skeleton + phase-1)** — Steps 1–9, 12, 13, 14. Lands the dual-server container, the pytest fixtures, the diff helper, the OpenAPI structural diff, the synthetic-divergence self-test, the always-on phase-1 test set, and the CI workflow. **PR boundary**: this wave closes alongside P2 (write path) so the phase-1 endpoints are wire-callable end-to-end.
- **Wave B (phase-2)** — Step 10. Adds the LLM-gated tests. **PR boundary**: closes alongside P4 (read path) so `/cognify`, `/recall`, `/llm` are wire-callable.
- **Wave C (phase-3)** — Step 11. Adds the specialty tests (websocket, sync, permissions, visualize). **PR boundary**: closes alongside P6 (observability + sync) and P5 (permissions); visualize follows P4.

Each wave updates the P8 status row in [implementation/README.md](README.md) (`Draft → In Progress → Done` flips per wave).

### Step 1: Extend the Dockerfile to also build `cognee-http-server`

- **File(s)**: `e2e-cross-sdk/Dockerfile`.
- **Action**: In Stage 1 (`rust-builder`), add a second `cargo build` invocation that builds the new binary in release mode with the `bin` feature enabled, and copy the resulting binary alongside `cognee-cli-rust`. The new line lives immediately after the existing `cargo build --release --package cognee-cli` step and reuses the same cargo cache mounts. In Stage 3 (`harness`), add a `COPY --from=rust-builder` line that places `cognee-http-server` into `/usr/local/bin/`. Do not change Stage 2 (Python builder).
- **Spec reference**: [e2e-parity.md §3](../e2e-parity.md#3-container-additions). Binary name + features: [architecture.md §17](../architecture.md#17-binary-cognee-http-server).
- **Verify**: `docker build -f cognee-rust/e2e-cross-sdk/Dockerfile .` from the monorepo root succeeds. Inside a `docker compose run --rm --entrypoint=bash e2e-http-tests`, `cognee-http-server --help` prints clap help.

### Step 2: Add the `wait_for_health.sh` helper

- **File(s)**: `e2e-cross-sdk/harness/wait_for_health.sh` (new), made executable.
- **Action**: Tiny bash helper that polls a URL with `curl -fsS --max-time 1` in a `for i in $(seq 1 60); do ... done` loop, sleeping 0.5s between attempts; exits 0 when a `200` is observed, exits 1 with a diagnostic message after 30 s. Single argument: the URL. Used by the entrypoint in Step 3.
- **Spec reference**: [e2e-parity.md §3](../e2e-parity.md#3-container-additions).
- **Verify**: `bash harness/wait_for_health.sh http://127.0.0.1:9999` returns non-zero within 30 s; against an actual `python -m http.server` it returns 0.

### Step 3: Add the dual-server entrypoint

- **File(s)**: `e2e-cross-sdk/bin/start_servers.sh` (new), made executable. Also create the `e2e-cross-sdk/bin/` directory.
- **Action**: Bash script that:
  1. Sets `PY_WORKSPACE=/py` and `RS_WORKSPACE=/rs`, creates both directories.
  2. `cd "$PY_WORKSPACE"` then runs `python -m cognee.run_migrations` once so the Python SQLite DB is migrated before uvicorn boots (per [e2e-parity.md §11](../e2e-parity.md#11-test-data-hygiene)).
  3. Starts uvicorn in the background: `(cd "$PY_WORKSPACE" && exec uvicorn cognee.api.client:app --host 127.0.0.1 --port 8000 --log-level warning) &` capturing `PY_PID=$!`.
  4. Starts the Rust server in the background: `(cd "$RS_WORKSPACE" && HTTP_API_HOST=127.0.0.1 HTTP_API_PORT=8001 ENV=test exec cognee-http-server) &` capturing `RS_PID=$!`. Forward the same env vars Python honours (`OPENAI_TOKEN`, `OPENAI_URL`, `OPENAI_MODEL`, `LLM_API_KEY`, `EMBEDDING_PROVIDER`, etc.) by exporting them at the top of the script — they are inherited from the Compose `env_file`.
  5. Waits on both `/health` endpoints via `harness/wait_for_health.sh`.
  6. `trap "kill $PY_PID $RS_PID 2>/dev/null || true" EXIT` and `exec "$@"` so Compose can pass `pytest -vs /harness/test_http_*` (or any override) as the command.
  7. If `KEEP_RUNNING=1` is set, replaces the final `exec "$@"` with `tail -f /dev/null` so a developer can `docker compose exec` into the container and curl either server by hand. See [e2e-parity.md §10](../e2e-parity.md#10-local-development-workflow).
- **Spec reference**: [e2e-parity.md §3](../e2e-parity.md#3-container-additions).
- **Verify**: `docker compose run --rm --entrypoint=/usr/local/bin/start_servers.sh e2e-http-tests bash -c 'curl -fsS http://127.0.0.1:8000/health && curl -fsS http://127.0.0.1:8001/health'` returns both health bodies.

### Step 4: Wire the entrypoint into the Dockerfile

- **File(s)**: `e2e-cross-sdk/Dockerfile`.
- **Action**: In Stage 3, add `COPY cognee-rust/e2e-cross-sdk/bin/ /usr/local/bin/` (after the existing `harness/` copy) and `RUN chmod +x /usr/local/bin/start_servers.sh /harness/wait_for_health.sh`. The `harness/` `COPY` already places `wait_for_health.sh` inside `/harness/` — keep it there for parity with how other helpers (`helpers.py`, `seed.py`, `http_helpers.py`) live next to the tests; the entrypoint script lives in `/usr/local/bin/` so it's on `$PATH` for the Compose `entrypoint:` field. **Do not** change the Stage 3 `CMD` — the new `e2e-http-tests` Compose service overrides it.
- **Spec reference**: [e2e-parity.md §3](../e2e-parity.md#3-container-additions).
- **Verify**: Same image build still succeeds; `docker run --rm --entrypoint=ls <image> /usr/local/bin/start_servers.sh /harness/wait_for_health.sh` lists both.

### Step 5: Add the `e2e-http-tests` Compose service

- **File(s)**: `e2e-cross-sdk/docker-compose.yml`.
- **Action**: Append a second service `e2e-http-tests` that reuses the same `build:` block as `e2e-tests`, sets `entrypoint: ["/usr/local/bin/start_servers.sh"]`, sets `command: ["pytest", "-vs", "/harness/", "-k", "test_http_"]`, and adds two tmpfs mounts: `/py` and `/rs`. Preserve the existing `env_file: [../.env]` line and the existing `e2e-tests` service unchanged (the CLI-parity tests must still run under their old `pytest -vs /harness/` default).
- **Spec reference**: [e2e-parity.md §3](../e2e-parity.md#3-container-additions) and [§9](../e2e-parity.md#9-ci-integration).
- **Verify**: `docker compose -f e2e-cross-sdk/docker-compose.yml config` lists both services; `docker compose run --rm e2e-http-tests pytest --collect-only -q /harness/ -k 'test_http_'` collects zero tests (no test files yet) and exits 5 (pytest's "no tests collected" code) — **not** an error.

### Step 6: Extend `harness/conftest.py` with HTTP fixtures

- **File(s)**: `e2e-cross-sdk/harness/conftest.py`.
- **Action**: Append (do not replace) the following block after the existing CLI-parity fixtures:
  ```python
  import httpx, uuid
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
      creds = {"email": "test@example.com", "password": "test_password_123"}
      for name, c in both_clients.items():
          c.post("/api/v1/auth/register", json={**creds, "is_verified": True})
          r = c.post("/api/v1/auth/login", data=creds)
          assert r.status_code == 200, f"{name} login failed: {r.text}"
      return both_clients
  ```
  Use the same `requires_openai` marker that already lives in this file for the LLM-gated tests; do not redefine it. Add `httpx>=0.27` to `harness/requirements.txt` (currently only `pytest>=8.0`).
- **Spec reference**: [e2e-parity.md §4](../e2e-parity.md#4-pytest-fixtures), [§7](../e2e-parity.md#7-authentication-strategy-in-tests).
- **Verify**: `docker compose run --rm e2e-http-tests pytest --collect-only -q /harness/conftest.py` does not error.

### Step 7: Add `harness/http_helpers.py` with the diff helper

- **File(s)**: `e2e-cross-sdk/harness/http_helpers.py` (new).
- **Action**: Implement `assert_responses_match(py, rs, *, ignore=())` and the `strip_paths(json_value, paths)` walker that strips JSONPath-like patterns (support `$.key`, `$..key`, `$.list[*].key`). Plus the `DEFAULT_IGNORE` constant per [e2e-parity.md §6.2](../e2e-parity.md#62-what-we-strip--ignore):
  ```python
  DEFAULT_IGNORE = frozenset({
      "$..created_at", "$..updated_at", "$..pipeline_run_id",
      "$..run_info.duration_ms", "$..access_token", "$..token_type",
      "$..session.id", "$..run_id", "$..id",
  })
  ```
  Helper signature: `def assert_responses_match(py, rs, *, ignore=DEFAULT_IGNORE):` — diff status code first (must match exactly), then `Content-Type` header, then body. JSON bodies compared with `strip_paths` applied to both sides; HTML bodies (status 200, `Content-Type: text/html`) compared after stripping a `<!--JSON_ISLAND_START-->...<!--JSON_ISLAND_END-->` region; binary bodies compared by SHA-256. Failure messages include both bodies and a `jsondiff`-style summary so CI logs pinpoint the diverging field per [e2e-parity.md §9](../e2e-parity.md#9-ci-integration). Add inline `pytest.fail(...)` rather than bare `assert` so the diff is the test message, not a stack trace. **Do not** depend on third-party `jsondiff` — implement a minimal differ inline (sets of keys, type compare, recursive). Add `jsondiff>=2.0` to `requirements.txt` if a richer diff helps debugging — optional.
- **Spec reference**: [e2e-parity.md §6](../e2e-parity.md#6-diff-strategy).
- **Verify**: Inline pytest tests in the same file (`def test_strip_paths_dollar_dot(): ...`, `def test_assert_match_passes_for_equal_dicts(): ...`, `def test_assert_match_fails_on_extra_key(): ...`) cover the strip walker and the assertion's failure message shape.

### Step 8: Add `harness/seed.py` with seeding helpers

- **File(s)**: `e2e-cross-sdk/harness/seed.py` (new).
- **Action**: Implement the two helpers from [e2e-parity.md §8](../e2e-parity.md#8-pre-test-seeding):
  - `seed_dataset_with_text(client, *, name, text) -> dict` — POSTs to `/api/v1/add` with a multipart body containing one `text/plain` part named `data` and a form field `dataset_name=<name>`. Asserts `200`. Returns the parsed JSON.
  - `seed_cognify(client, *, dataset_id) -> dict` — POSTs `/api/v1/cognify` with `{"datasets": [dataset_id], "run_in_background": false}`, asserts `200`, returns parsed JSON.
  Both helpers call `assert r.status_code == 200, r.text` and return `r.json()`. They take a `client` (either `py_client` or `rs_client`) so each test can seed both servers symmetrically. Per-test convenience: a `seed_both(both_clients, *, name, text)` wrapper that calls each client and asserts the *seeded* IDs match modulo defaulting (so seed-time divergence fails fast — see [e2e-parity.md §8](../e2e-parity.md#8-pre-test-seeding) "Any seed-time divergence fails fast and clearly").
- **Spec reference**: [e2e-parity.md §8](../e2e-parity.md#8-pre-test-seeding).
- **Verify**: Inline test that mocks the two clients with `httpx.MockTransport` and asserts the helpers post the right multipart shape and JSON body.

### Step 9: Phase-1 test files (no LLM required)

- **File(s)**: under `e2e-cross-sdk/harness/`, eight new files:
  - `test_http_health.py`
  - `test_http_auth.py`
  - `test_http_datasets.py`
  - `test_http_add.py`
  - `test_http_search.py` (skip LLM-heavy `SearchType`s; cover `Chunks`, `Summaries`, `ChunksLexical`)
  - `test_http_forget.py`
  - `test_http_openapi.py` (see Step 12 for the structural diff)
  - `test_http_errors.py`
- **Action**: Each file follows this skeleton:
  ```python
  from http_helpers import assert_responses_match, DEFAULT_IGNORE

  def test_<area>_<case>(authed_clients):
      payload = {...}
      py = authed_clients["py"].post("/api/v1/<endpoint>", json=payload)
      rs = authed_clients["rs"].post("/api/v1/<endpoint>", json=payload)
      assert_responses_match(py, rs, ignore=DEFAULT_IGNORE | {"$..tenant_id"})
  ```
  Aim for **3–10 cases per file**, parameterized where the input matrix is dense. Per-file scope and ignore-extension guidance:
  - **`test_http_health.py`** — 4 cases: `GET /health` HEALTHY, `GET /health/detailed` HEALTHY, both endpoints during a forced-UNHEALTHY mode (set `COGNEE_TEST_FORCE_UNHEALTHY=1`). No auth required. Ignore extension: `{"$..version"}` (release versions differ between SDKs).
  - **`test_http_auth.py`** — 6 cases: register, login, `/me`, logout, `/me` post-logout (must 401), and the JWT cross-server canary `test_jwt_cross_compat`. Per [e2e-parity.md §7](../e2e-parity.md#7-authentication-strategy-in-tests), the canary is the one test that does **not** call `assert_responses_match`: issue a token on Python, present it as `Authorization: Bearer ...` to Rust's `/api/v1/auth/me`, assert `200`; reverse the direction; assert the returned `email` matches. If this canary fails, every other auth-using test is suspect — flag it as a fail-fast in the test order. Ignore extension for the others: `DEFAULT_IGNORE` covers `access_token` and `id`.
  - **`test_http_datasets.py`** — 7 cases: list-empty, create, list-after-create, get-by-id, status-by-name, delete, get-deleted (must 404). Ignore extension: `{"$..tenant_id", "$..owner_id"}` (independent default tenants per server).
  - **`test_http_add.py`** — 5 cases: text upload, multi-file upload, URL ingestion, deduplication (post twice, assert second response signals dedup the same way), validation error on missing file. Ignore extension: `{"$..tenant_id", "$..data_id", "$..dataset_id", "$..raw_data_location"}` (file paths under `/py` vs `/rs`); the `content_hash` field is **not** ignored — it must match.
  - **`test_http_search.py`** — parametrize over `SearchType` ∈ `{Chunks, Summaries, ChunksLexical}` only at phase-1 (LLM-heavy types ride in phase-2). Run after `seed_dataset_with_text` + `seed_cognify` against both servers. Ignore extension: `{"$..tenant_id", "$..owner_id", "$..results[*].score"}` (cosine scores can differ in the last decimal).
  - **`test_http_forget.py`** — 4 cases: forget by `data_id`, forget by `dataset`, forget by `everything: true`, forget non-existent (must 404 on both). Ignore extension: `DEFAULT_IGNORE`.
  - **`test_http_openapi.py`** — handled in Step 12.
  - **`test_http_errors.py`** — 5 cases: missing required body field (validation `400`), bad JWT (`401` — assert `detail.code == LOGIN_BAD_CREDENTIALS`), missing auth (`401`), unsupported method on a known path (`405`), `/teapot` debug route if present (`418`). Asserts only the `detail[*].type`/`loc` *codes* match per [e2e-parity.md §6.4](../e2e-parity.md#64-whats-a-soft-mismatch-logged-not-failed) — the human-readable `msg` is allowed to differ; pre-strip `detail[*].msg` before the diff.
- **Spec reference**: [e2e-parity.md §5 (phase-1)](../e2e-parity.md#5-test-inventory), [§7](../e2e-parity.md#7-authentication-strategy-in-tests). Endpoint contracts: per-router specs in [routers/](../routers/).
- **Verify**: `docker compose run --rm e2e-http-tests pytest -vs /harness/ -k 'test_http_(health|auth|datasets|add|search|forget|openapi|errors)' --tb=short` runs all phase-1 files green on a clean checkout (P0–P2 landed). Failures must point at the diverging field via the `assert_responses_match` message.

### Step 10: Phase-2 test files (LLM-gated)

- **File(s)**: six new files under `e2e-cross-sdk/harness/`:
  - `test_http_cognify.py`
  - `test_http_remember.py`
  - `test_http_recall.py`
  - `test_http_memify.py`
  - `test_http_improve.py`
  - `test_http_llm.py`
- **Action**: Each test module starts with `pytestmark = [requires_openai]` (the marker already in `conftest.py`). All four pipeline tests (`cognify`/`remember`/`memify`/`improve`) use `seed.seed_dataset_with_text` against both servers, then POST the pipeline endpoint with `run_in_background=false` so the call blocks until the pipeline run terminates and the response carries the final pipeline-run dict. The diff must strip everything in [pipelines.md §3](../pipelines.md#3-status-taxonomy-and-wire-mapping) that's inherently non-deterministic (`pipeline_run_id`, `started_at`, `ended_at`, `run_info.duration_ms`, `payload.entities[*].id`). Per-file scope:
  - **`test_http_cognify.py`** — 3 cases: blocking on a single dataset, blocking on multiple datasets, error path (cognify on non-existent dataset → both must `404`). Ignore extension: `{"$..pipeline_run_id", "$..started_at", "$..ended_at", "$..payload.entities[*].id", "$..payload.relationships[*].id", "$..run_info"}`. **Structural equality only** for `payload.entities` and `payload.relationships` — the LLM is non-deterministic; assert the *names* present in both responses overlap by ≥ 50% (Jaccard) per the precedent in `test_cognify_structural.py`. Use a per-test custom matcher rather than `assert_responses_match` for the LLM-output regions; everything else (status code, top-level shape, run-info keys) goes through the strict matcher.
  - **`test_http_remember.py`** — 3 cases: blocking remember on a cognified dataset, with-and-without `session_id`. Same Jaccard-style structural compare for the LLM-derived fields.
  - **`test_http_recall.py`** — parameterize over the auto-routed `SearchType` selection (the recall endpoint picks the type from the question shape). 4 cases minimum: factual question, summary question, temporal question, code-rule question. Strict matcher on the *envelope*, structural compare on `results[*].text`.
  - **`test_http_memify.py`** — 2 cases: memify on a cognified dataset, memify on an empty dataset (graceful no-op).
  - **`test_http_improve.py`** — 2 cases: improve with sessions present, improve with no sessions (per the [pipelines.md §2 library-refactor prereq](../pipelines.md#2-library-refactor-prerequisite) the no-session path is a synchronous return).
  - **`test_http_llm.py`** — 4 cases: `POST /llm/custom-prompt` with a deterministic prompt (`temperature=0`, fixed seed); `POST /llm/infer-schema` with a fixed input. Strict matcher on status + envelope; the `output` field allows ±1 token via a per-test fuzzy compare.
  **3–6 cases per file** is the budget — these are slow (30–90 s each).
- **Spec reference**: [e2e-parity.md §5 (phase-2)](../e2e-parity.md#5-test-inventory), [pipelines.md §3](../pipelines.md#3-status-taxonomy-and-wire-mapping).
- **Verify**: With `OPENAI_TOKEN` set in `cognee-rust/.env`, `docker compose run --rm e2e-http-tests pytest -vs /harness/ -k 'test_http_(cognify|remember|recall|memify|improve|llm)'` runs green. Each test takes 30–90 s; the suite as a whole stays under 10 min.

### Step 11: Phase-3 test files (specialty)

- **File(s)**: four new files under `e2e-cross-sdk/harness/`:
  - `test_http_websocket.py`
  - `test_http_sync.py`
  - `test_http_permissions.py`
  - `test_http_visualize.py`
- **Action**: Per-file scope:
  - **`test_http_websocket.py`** — uses `httpx_ws` (or the `websockets` package) to connect to `/api/v1/cognify/subscribe/{pipeline_run_id}` *after* a `seed_cognify(..., run_in_background=true)` issues a run-id; it reads frames until terminal-close and asserts the JSON shape of each frame matches per [websocket.md §5](../websocket.md#5-frame-format) (`{pipeline_run_id, status, payload}`). The harness collects the full frame sequence on each side and asserts (a) the close code is `1000` on both sides, (b) the terminal frame's `status` is `PipelineRunCompleted` on both sides, (c) the *set* of intermediate `status` values matches, and (d) the count delta is within `±2` per [e2e-parity.md §12 Q5](../e2e-parity.md#12-open-questions). The cookie-only auth handshake from [websocket.md](../websocket.md) means the test must reuse the cookie jar from `authed_clients`. 2 cases: happy-path complete, error-path (cognify on bad dataset → `Errored` status frame, no close).
  - **`test_http_sync.py`** — exercises `POST /sync` and `GET /sync/status` per [routers/sync.md](../routers/sync.md). 3 cases: sync triggered, status polled, sync on empty workspace. Ignore extension: `{"$..last_run_at", "$..duration_ms"}`.
  - **`test_http_permissions.py`** — walks the 13-endpoint permission-API surface in CRUD order; principals / roles / tenants are created on each server independently and the test asserts the *shape* of responses, not concrete IDs. ~13 cases (one per endpoint), ordered so each later test depends on earlier seeds. Ignore extension: `{"$..tenant_id", "$..principal_id", "$..role_id", "$..created_at"}`.
  - **`test_http_visualize.py`** — calls `GET /api/v1/visualize?dataset_id={...}` after `seed_cognify`, strips the JSON-island region from both HTML bodies (delimited by `<!--JSON_ISLAND_START-->...<!--JSON_ISLAND_END-->` per [routers/visualize.md](../routers/visualize.md)), and bytewise-diffs the remaining HTML scaffold. The JSON-island contents are diffed separately with a structural compare (entity-name set Jaccard ≥ 0.5 — the graph layout is non-deterministic but the node/edge set should overlap heavily).
  Add `httpx-ws>=0.6` (or `websockets>=12`) to `requirements.txt`.
- **Spec reference**: [e2e-parity.md §5 (phase-3)](../e2e-parity.md#5-test-inventory), [websocket.md](../websocket.md), [routers/sync.md](../routers/sync.md), [routers/permissions.md](../routers/permissions.md), [routers/visualize.md](../routers/visualize.md).
- **Verify**: With LLM secrets present (visualize and websocket need a cognified dataset), `docker compose run --rm e2e-http-tests pytest -vs /harness/ -k 'test_http_(websocket|sync|permissions|visualize)'` runs green.

### Step 12: OpenAPI structural diff + golden file

- **File(s)**: `e2e-cross-sdk/harness/test_http_openapi.py` (already created in Step 9 as a stub — this step fills it in).
- **Action**: The test fetches `/openapi.json` from both servers (no auth required per [routers/health.md](../routers/health.md) and [architecture.md §13](../architecture.md#13-openapi-generation--utoipa)) and runs four diffs:
  1. **Path-set diff**: `set(py["paths"].keys())` vs `set(rs["paths"].keys())`. The diff is reported as `paths_only_in_py` and `paths_only_in_rs`. An empty diff is the success criterion.
  2. **Method-set diff per shared path**: for each path present on both sides, the set of HTTP methods registered must match.
  3. **Security-scheme diff**: `set(py["components"]["securitySchemes"].keys())` must equal the Rust side. Names must match — `BearerAuth` and `ApiKeyAuth` per [architecture.md §13](../architecture.md#13-openapi-generation--utoipa).
  4. **Top-level `components.schemas` shape diff**: the *names* of registered schemas must match. Per-schema field-set diff is a follow-up; this step only diffs key sets.
  Apply a **normalizer** before each diff per [plan.md §7 Q5](../plan.md#7-open-questions): rewrite path parameter names so `{dataset_id}` and `{datasetId}` collapse to a canonical form; sort the security-scheme list. The normalizer's allowlist (which transformations are valid) must be approved before the test is non-skipped — until approval, mark the test `pytest.mark.skip(reason="normalizer allowlist pending — plan.md §7 Q5")` so the file lands but does not enforce. Once approved, remove the skip in a follow-up commit.
  Also commit a `harness/golden/openapi.python.json` snapshot of the Python `/openapi.json` so future Python-side additions are visible in PR diffs (the test does NOT assert on this file — it's an aid for reviewers).
- **Spec reference**: [e2e-parity.md §5 / §12 Q3](../e2e-parity.md#12-open-questions), [plan.md §7 Q5](../plan.md#7-open-questions).
- **Verify**: When the normalizer skip is removed, the test passes on a clean P0–P7 checkout. **Self-test**: a synthetic divergence (drop one path from the Rust router) is caught by the structural diff and produces a CI-friendly error message — covered by Step 14.

### Step 13: CI workflow

- **File(s)**: `.github/workflows/http-parity.yml` (new).
- **Action**: GitHub Actions workflow that:
  - Triggers on `push` to `main` and on `pull_request` paths-filter `crates/http-server/**`, `cognee/api/**`, `e2e-cross-sdk/**`, plus `workflow_dispatch`.
  - Single job `http-parity` on `ubuntu-latest`, with `concurrency: { group: http-parity-${{ github.ref }}, cancel-in-progress: true }`.
  - Steps: `actions/checkout@v4` (with `submodules: recursive` so the Python `cognee/` submodule is present), then `docker compose -f cognee-rust/e2e-cross-sdk/docker-compose.yml run --rm e2e-http-tests pytest -vs /harness/ -k 'test_http_(health|auth|datasets|add|search|forget|openapi|errors)' --tb=short` for the always-on phase-1 set.
  - A second step gated on `${{ secrets.OPENAI_KEY != '' }}` runs the phase-2 set with `pytest -vs /harness/ -k 'test_http_(cognify|remember|recall|memify|improve|llm)'`. Pass the secret as `env: OPENAI_TOKEN: ${{ secrets.OPENAI_KEY }}` plus `OPENAI_URL`/`OPENAI_MODEL` per [e2e-parity.md §9](../e2e-parity.md#9-ci-integration).
  - Phase-3 stays manual (`workflow_dispatch`) until the websocket frame-yield delta is locked in (per [e2e-parity.md §12 Q5](../e2e-parity.md#12-open-questions)).
  - Cache the Docker layer cache via `actions/cache@v4` keyed on `e2e-cross-sdk/Dockerfile` + `cognee-rust/Cargo.lock` so cold-build minutes stay bounded.
- **Spec reference**: [e2e-parity.md §9](../e2e-parity.md#9-ci-integration).
- **Verify**: Open a PR that touches `crates/http-server/src/routers/health.rs` and watch `http-parity` fire and turn green. Confirm phase-2 is correctly skipped on a forked-PR run (no secret) and runs on a same-repo PR.

### Step 13b: Test-data hygiene fixtures

- **File(s)**: `e2e-cross-sdk/harness/conftest.py` (further extension).
- **Action**: Add two session-scoped fixtures to keep cross-test contamination from confusing diffs:
  - `unique_dataset_name(request)` (function-scoped) — yields a per-test dataset name like `f"test_{request.node.name}_{uuid.uuid4().hex[:8]}"`. Every test that creates a dataset uses this fixture so test-runs don't collide. Per [e2e-parity.md §11](../e2e-parity.md#11-test-data-hygiene).
  - `cleanup_api_keys(both_clients)` (function-scoped, autouse=False) — used by `test_http_api_keys.py` to delete created keys at teardown. Records the issued keys in a list and DELETEs each at fixture exit.
  Document in a comment block at the top of `conftest.py` that the `/py` and `/rs` tmpfs workspaces are wiped per `docker compose run` invocation but **not** between tests within a single run — tests must rely on `unique_dataset_name` plus their own teardown rather than assume a clean DB. The Python-side migrations are run once at container start (Step 3); the Rust-side server runs its migrations on first boot.
- **Spec reference**: [e2e-parity.md §11](../e2e-parity.md#11-test-data-hygiene).
- **Verify**: `pytest --collect-only` lists the fixtures; a quick `pytest -vs harness/test_http_datasets.py::test_create` run shows distinct dataset names across re-runs.

### Step 14: Synthetic-divergence regression test for the harness itself

- **File(s)**: `e2e-cross-sdk/harness/test_http_self.py` (new).
- **Action**: A meta-test that exercises `assert_responses_match` and the OpenAPI structural diff against **synthetic** payloads (no live server). It feeds the helpers two crafted `httpx.Response` objects with a known divergence (extra key on one side, type mismatch, status mismatch, OpenAPI path missing) and asserts that the helper's failure message names the diverging field/path. This is the *parity test for the parity test* called out in §6 acceptance criteria — it ensures that if the diff helpers regress, CI catches it before a real divergence slips through. Five cases is enough.
- **Spec reference**: [e2e-parity.md §6](../e2e-parity.md#6-diff-strategy).
- **Verify**: `pytest -vs harness/test_http_self.py` passes; mutating `assert_responses_match` to skip status-code comparison breaks one of the five cases.

## 5. Tests

The deliverable IS tests. Each phase's set runs independently:

- **Phase-1** (no LLM): `test_http_health.py`, `test_http_auth.py`, `test_http_datasets.py`, `test_http_add.py`, `test_http_search.py` (Chunks/Summaries/ChunksLexical only — LLM-heavy types skipped here), `test_http_forget.py`, `test_http_openapi.py`, `test_http_errors.py`. Run with `pytest -k "test_http_(health|auth|datasets|add|search|forget|openapi|errors)"`.
- **Phase-2** (needs `OPENAI_KEY`): `test_http_cognify.py`, `test_http_remember.py`, `test_http_recall.py`, `test_http_memify.py`, `test_http_improve.py`, `test_http_llm.py`. Run with `pytest -k "test_http_(cognify|remember|recall|memify|improve|llm)"`.
- **Phase-3** (specialty): `test_http_websocket.py`, `test_http_sync.py`, `test_http_permissions.py`, `test_http_visualize.py`. Run with `pytest -k "test_http_(websocket|sync|permissions|visualize)"`.
- **Self-tests**: `test_http_self.py` — meta-tests for the diff helpers.
- **Inline tests** in `harness/http_helpers.py` and `harness/seed.py` — exercise the strip-paths walker, the failure-message shape, and the seeding helpers' multipart shape.

The remaining test files from the [e2e-parity.md §5](../e2e-parity.md#5-test-inventory) inventory (`test_http_update`, `test_http_delete`, `test_http_ontologies`, `test_http_settings`, `test_http_configuration`, `test_http_users`, `test_http_api_keys`, `test_http_activity`, `test_http_cors`) are **not part of P8's hard scope** — they ride on the same skeleton landed here and can be added incrementally per the table in [e2e-parity.md §5](../e2e-parity.md#5-test-inventory). Land them as routers stabilize.

## 6. Acceptance criteria

Per-wave criteria — each wave is one PR (see §4 preamble for the wave/PR mapping):

**Wave A (skeleton + phase-1):**

- [ ] `docker compose -f e2e-cross-sdk/docker-compose.yml run --rm e2e-http-tests` runs the phase-1 set successfully on a clean checkout, **with no `OPENAI_KEY` configured**.
- [ ] CI workflow `.github/workflows/http-parity.yml` exists and runs green on PRs touching `crates/http-server/`, `cognee/api/`, or `e2e-cross-sdk/`.
- [ ] The OpenAPI structural-diff test catches a known synthetic divergence (regression test for the parity test itself) — covered by `test_http_self.py`.
- [ ] The synthetic-divergence meta-test (`test_http_self.py`) is included in phase-1's selector and passes.
- [ ] `start_servers.sh` boots both servers, both `/health` checks pass within 30 s; `KEEP_RUNNING=1` mode keeps the container up so a developer can `docker compose exec` and curl either server by hand.
- [ ] The existing `e2e-tests` Compose service (CLI-driven parity) is **untouched** — `docker compose run --rm e2e-tests` still passes.
- [ ] `cognee-cli` does not gain a transitive dep on `cognee-http-server` (parity invariant from [architecture.md §3](../architecture.md#3-crate-topology) — confirm `cargo tree -p cognee-cli | grep cognee-http-server` returns empty).
- [ ] OpenAPI normalizer allowlist approved per [plan.md §7 Q5](../plan.md#7-open-questions); `test_http_openapi`'s skip-marker removed.
- [ ] Status row for **P8** in [implementation/README.md](README.md) flips **Draft → In Progress** in the PR that closes Wave A.
- [ ] Status note in [routers/README.md](../routers/README.md) referencing `e2e-cross-sdk/harness/test_http_<area>.py` for each Wave-A `Done` router so per-router parity coverage is traceable.
- [ ] `scripts/check_all.sh` still passes (no Rust changes in this phase).

**Wave B (phase-2):**

- [ ] Phase-2 set runs successfully when `OPENAI_KEY` (mapped to `OPENAI_TOKEN`) is configured in `cognee-rust/.env`. Local repro: `docker compose run --rm e2e-http-tests pytest -vs /harness/ -k 'test_http_(cognify|remember|recall|memify|improve|llm)'`.
- [ ] CI workflow's phase-2 step is gated on `${{ secrets.OPENAI_KEY != '' }}` and is observed running green on a same-repo PR; observed correctly skipped on a forked-PR run.
- [ ] Wave-B `Done` routers' rows in [routers/README.md](../routers/README.md) reference their `test_http_<area>.py` file.

**Wave C (phase-3):**

- [ ] Phase-3 set runs successfully manually via `workflow_dispatch` (or local `docker compose run --rm e2e-http-tests pytest -vs -k 'test_http_(websocket|sync|permissions|visualize)'`).
- [ ] WebSocket frame-yield delta locked at `±2` per [e2e-parity.md §12 Q5](../e2e-parity.md#12-open-questions); recorded as a constant `WS_YIELD_TOLERANCE` in `harness/http_helpers.py`.
- [ ] Status row for **P8** in [implementation/README.md](README.md) flips **In Progress → Done**.

## 7. Files touched

New (under `e2e-cross-sdk/`):

- `bin/start_servers.sh`
- `harness/wait_for_health.sh`
- `harness/http_helpers.py`
- `harness/seed.py`
- `harness/test_http_health.py`
- `harness/test_http_auth.py`
- `harness/test_http_datasets.py`
- `harness/test_http_add.py`
- `harness/test_http_search.py`
- `harness/test_http_forget.py`
- `harness/test_http_openapi.py`
- `harness/test_http_errors.py`
- `harness/test_http_cognify.py`
- `harness/test_http_remember.py`
- `harness/test_http_recall.py`
- `harness/test_http_memify.py`
- `harness/test_http_improve.py`
- `harness/test_http_llm.py`
- `harness/test_http_websocket.py`
- `harness/test_http_sync.py`
- `harness/test_http_permissions.py`
- `harness/test_http_visualize.py`
- `harness/test_http_self.py`
- `harness/golden/openapi.python.json` (committed reference snapshot — informational, not asserted)

New (under repo root):

- `.github/workflows/http-parity.yml`

Modified:

- `e2e-cross-sdk/Dockerfile` — adds `cognee-http-server` Stage-1 build + Stage-3 copy, plus `bin/` copy and chmod.
- `e2e-cross-sdk/docker-compose.yml` — adds `e2e-http-tests` service (does NOT change `e2e-tests`).
- `e2e-cross-sdk/harness/conftest.py` — appends HTTP fixtures (`py_client`, `rs_client`, `both_clients`, `authed_clients`) without touching the existing CLI-parity fixtures.
- `e2e-cross-sdk/harness/requirements.txt` — adds `httpx>=0.27` and (for phase-3) `httpx-ws>=0.6`.
- `docs/http-server/implementation/README.md` — flip P8 status row across the three phase-1 / phase-2 / phase-3 PRs.
- `docs/http-server/routers/README.md` — annotate per-router rows with the `test_http_<area>.py` file that covers their parity, as those tests land.

Out of scope (do NOT touch in this phase):

- `crates/http-server/` Rust code — P8 is harness-only; any server bug uncovered by the harness is fixed in the owning P0–P7 phase.
- The existing CLI-driven `e2e-tests` Compose service or its tests (`test_add_parity.py`, `test_cognify_structural.py`, `test_cross_read.py`, etc.) — they continue to run unchanged.
- Performance benchmarking, TLS testing, multi-replica WebSocket fan-out — explicit non-goals per [e2e-parity.md §1](../e2e-parity.md#1-goals--non-goals) and [§12](../e2e-parity.md#12-open-questions).
- The remaining harness test files from [e2e-parity.md §5](../e2e-parity.md#5-test-inventory) (`test_http_update`, `test_http_delete`, `test_http_ontologies`, `test_http_settings`, `test_http_configuration`, `test_http_users`, `test_http_api_keys`, `test_http_activity`, `test_http_cors`) — incremental follow-ups, not gating P8.
