# Task 02-10 — Cross-SDK identity parity (Python ↔ Rust)

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 02-07 — Callsite migration](07-callsite-migration.md) — Rust must emit *some* event for parity to be observable.
- [Task 02-09 — Integration tests](09-integration-tests.md) — establishes the mockito patterns reused here.

**Blocks**:
- [Task 02-12 — CI updates](12-ci-updates.md) (the new docker-compose service is gated on a CI lane).

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

Add a cross-SDK end-to-end test under
[`e2e-cross-sdk/`](../../../e2e-cross-sdk/) that:

1. Boots a single mock proxy (a `mockito`-equivalent Python service)
   inside the existing docker-compose harness.
2. Runs **Python `cognee.shared.utils.send_telemetry`** with a fixed
   `LLM_API_KEY`, fixed `HOME` (volume-mounted so `~/.cognee/.persistent_id`
   is shared), and a captured `TRACKING_ID`.
3. Runs **Rust `cognee_lib::telemetry::send_telemetry`** with the
   same fixed env.
4. Asserts:
   - `api_key_tracking_id` is identical between the two payloads
     (the parity-critical bit).
   - `persistent_id` is identical (because `HOME` is shared).
   - `anonymous_id` MAY differ (project-local file at
     `<cwd>/.anon_id` — Python and Rust have different working
     directories inside the container).
   - `event_name` matches whatever the test fires (we use
     `"cognee.forget"` because the Rust `forget.rs` call site is
     wired in [task 02-07](07-callsite-migration.md)).

## 2. Rationale

### Why Docker, not a host-level test

Identity parity hinges on the **PBKDF2 algorithm** producing
identical bytes. That's the byte-parity test in
[task 02-08](08-unit-tests.md). What this test adds is
*environment* parity: same `LLM_API_KEY`, same `~/.cognee/`,
same proxy. Docker is the cheapest way to pin those across two
SDKs.

The existing `e2e-cross-sdk/` harness already builds Python and
Rust CLIs into the same image (per the explore report). We extend
it with a fourth pytest test (`test_telemetry_parity.py`) that uses
the existing volume mounts and one new mock-proxy service.

### Why `mockito`-like service in Python

The Rust mockito tests at [task 02-09](09-integration-tests.md) bind
`mockito` inside the test process. Python equivalent is a tiny
`http.server` subclass that captures POST bodies — 30 lines, zero
deps.

We *could* use a real mockito server inside Docker, but a Python
`BaseHTTPRequestHandler` is simpler, faster, and matches the Python
test idioms already in the harness.

## 3. Pre-conditions

- Tasks 02-05 through 02-09 merged.
- `e2e-cross-sdk/` builds locally:
  ```
  cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit
  ```
- The harness already supports passing OPENAI keys via env (per the
  explore report) — same pattern works for `LLM_API_KEY`.

## 4. Step-by-step

### 4.1 Add the mock proxy service

Edit
[`e2e-cross-sdk/docker-compose.yml`](../../../e2e-cross-sdk/docker-compose.yml).
Add a new service:

```yaml
services:
  # ... existing services ...

  telemetry-proxy:
    image: python:3.11-slim
    working_dir: /app
    command: ["python", "/app/telemetry_proxy.py"]
    volumes:
      - ./harness/telemetry_proxy.py:/app/telemetry_proxy.py:ro
      - telemetry-captures:/captures
    expose:
      - "9090"
    healthcheck:
      test: ["CMD", "python", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:9090/_health').read()"]
      interval: 1s
      timeout: 1s
      retries: 30

  # The existing test runner needs the proxy to be reachable at a
  # stable hostname. Add a depends_on entry.
  test-runner:
    # ... existing config ...
    environment:
      - COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS=http://telemetry-proxy:9090
      - COGNEE_TELEMETRY_INTEGRATION_TEST=1
      - LLM_API_KEY=sk-test-fixture-cross-sdk
      - TRACKING_ID=fixed-anon-cross-sdk
    depends_on:
      telemetry-proxy:
        condition: service_healthy
    volumes:
      - cognee-home:/root/.cognee
      - telemetry-captures:/captures:ro

volumes:
  telemetry-captures:
  cognee-home:
```

The shared `cognee-home` volume mounts at `/root/.cognee` so the
Python `~/.cognee/.persistent_id` and the Rust
`$HOME/.cognee/.persistent_id` are the same file.

### 4.2 Implement the mock proxy

Create `e2e-cross-sdk/harness/telemetry_proxy.py`:

```python
#!/usr/bin/env python3
"""Tiny HTTP proxy that captures POST bodies to /captures/<event>.json.

For each request, append the JSON body to the file
/captures/all.jsonl (one record per line). The cross-SDK pytest test
reads this file and asserts cross-SDK identity parity.
"""
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

CAPTURE_DIR = Path("/captures")
CAPTURE_DIR.mkdir(parents=True, exist_ok=True)
JSONL = CAPTURE_DIR / "all.jsonl"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/_health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        ln = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(ln) if ln else b""
        try:
            obj = json.loads(body)
        except Exception:
            obj = {"_raw": body.decode("utf-8", errors="replace")}
        with JSONL.open("a") as f:
            f.write(json.dumps(obj) + "\n")
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"{}")

    def log_message(self, format, *args):
        # Quiet logs; the captures file is the source of truth.
        sys.stderr.write("proxy: " + (format % args) + "\n")


def main():
    port = int(os.environ.get("PORT", "9090"))
    HTTPServer(("0.0.0.0", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
```

### 4.3 Implement the pytest

Create `e2e-cross-sdk/harness/tests/test_telemetry_parity.py`:

```python
"""Cross-SDK identity parity for send_telemetry.

Asserts that Python and Rust SDKs, given the same LLM_API_KEY and
shared ~/.cognee/, produce identical `api_key_tracking_id` and
`persistent_id` on the wire.

Run via the existing harness:
    docker compose up --build --abort-on-container-exit
"""
import json
import os
import subprocess
import time
from pathlib import Path

CAPTURES = Path("/captures/all.jsonl")
PROXY_URL = os.environ["COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS"]


def _read_captures():
    if not CAPTURES.exists():
        return []
    return [json.loads(line) for line in CAPTURES.read_text().splitlines() if line]


def _wait_for_n_captures(n, timeout=15.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        records = _read_captures()
        if len(records) >= n:
            return records
        time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {n} captures; got {len(_read_captures())}")


def _python_emit():
    """Drive Python's send_telemetry directly inside this process."""
    # The harness has cognee installed in editable mode.
    from cognee.shared.utils import send_telemetry
    send_telemetry(
        "cognee.forget",
        user_id="cross-sdk-user",
        additional_properties={
            "target": "everything",
            "cognee_version": "cross-sdk-test",
        },
    )


def _rust_emit():
    """Drive Rust's send_telemetry by shelling the Rust CLI.

    The Rust CLI does not expose an explicit "emit telemetry" command;
    instead, we trigger the `cognee.forget` SDK function via a no-op
    forget call (e.g. `cognee delete --everything --dry-run` or
    similar). Inspect `e2e-cross-sdk/bin/` for the existing wrapper.
    """
    # The cross-SDK harness already has a Rust binary at /app/cognee-cli.
    # Adjust the args to whatever exercises forget.rs without touching
    # real data.
    subprocess.check_call(
        [
            "/app/cognee-cli",
            "delete",
            "--all",
            "--dry-run",
        ],
        env={**os.environ, "COGNEE_TELEMETRY_INTEGRATION_TEST": "1"},
        timeout=30,
    )


def test_cross_sdk_telemetry_identity_parity():
    # Clear any prior captures from previous test invocations.
    if CAPTURES.exists():
        CAPTURES.unlink()

    _python_emit()
    _rust_emit()

    records = _wait_for_n_captures(2, timeout=15.0)

    # Sanity: both records carry the same event name.
    py = next(r for r in records if r["properties"].get("sdk_runtime", "python") != "rust")
    rs = next(r for r in records if r["properties"].get("sdk_runtime") == "rust")

    assert py["event_name"] == "cognee.forget"
    assert rs["event_name"] == "cognee.forget"

    # api_key_tracking_id must be byte-identical.
    py_ak = py["user_properties"]["api_key_tracking_id"]
    rs_ak = rs["user_properties"]["api_key_tracking_id"]
    assert py_ak.startswith("ak_")
    assert rs_ak.startswith("ak_")
    assert py_ak == rs_ak, (
        f"api_key_tracking_id drift: python={py_ak!r}, rust={rs_ak!r}"
    )

    # persistent_id must be identical (shared ~/.cognee/.persistent_id).
    py_pid = py["user_properties"]["persistent_id"]
    rs_pid = rs["user_properties"]["persistent_id"]
    assert py_pid == rs_pid, (
        f"persistent_id drift: python={py_pid!r}, rust={rs_pid!r}"
    )

    # api_key_hash backward-compat alias must mirror api_key_tracking_id.
    assert py["user_properties"]["api_key_hash"] == py_ak
    assert rs["user_properties"]["api_key_hash"] == rs_ak

    # Rust adds sdk_runtime: "rust" (decision 2). Python may add
    # sdk_runtime later; tolerate either presence.
    assert rs["properties"]["sdk_runtime"] == "rust"
```

### 4.4 Wire the new test into the harness's pytest invocation

Inspect
[`e2e-cross-sdk/harness/`](../../../e2e-cross-sdk/harness/) for the
existing pytest entry point (per the explore report, the harness
has a `harness/tests/` layout). Add a marker:

```python
# In harness/tests/conftest.py, ensure the cross-SDK test runs in the
# same container as the existing tests. No new conftest entries needed
# unless the proxy hostname needs resolution.
```

### 4.5 Update the Dockerfile if needed

If the existing
[`e2e-cross-sdk/Dockerfile`](../../../e2e-cross-sdk/Dockerfile)
does not already include the Rust CLI binary in
`/app/cognee-cli`, ensure the second build stage copies the
release binary into that path. The harness's existing test
infrastructure already exercises `/app/cognee-cli` for `add`,
`cognify`, etc. — extend the args as needed.

### 4.6 Verify

```bash
cd e2e-cross-sdk
docker compose down -v
docker compose up --build --abort-on-container-exit \
  --exit-code-from test-runner
```

Expected: the new `test_telemetry_parity` test passes alongside
`test_add_parity`, `test_cross_read`, `test_cognify_structural`.

## 5. Verification

```bash
# 1. Rebuild and run the full cross-SDK suite.
cd e2e-cross-sdk
docker compose up --build --abort-on-container-exit \
  --exit-code-from test-runner

# 2. Inspect captures after a green run.
docker compose run --rm test-runner cat /captures/all.jsonl

# 3. Confirm the new service is healthy.
docker compose ps telemetry-proxy
# Expected: STATUS = (healthy)

# 4. Run only the new test for fast iteration.
docker compose run --rm test-runner pytest \
  harness/tests/test_telemetry_parity.py -v
```

## 6. Files modified

- `e2e-cross-sdk/docker-compose.yml` — add `telemetry-proxy` service +
  shared volumes + env vars on `test-runner`.
- `e2e-cross-sdk/harness/telemetry_proxy.py` — new file.
- `e2e-cross-sdk/harness/tests/test_telemetry_parity.py` — new file.
- `e2e-cross-sdk/Dockerfile` — possibly extend the Rust stage to
  copy `cognee-cli` to `/app/cognee-cli` if it doesn't already.
- (Optional) `e2e-cross-sdk/harness/conftest.py` — adjust if the
  pytest discovery needs to register the new file.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Python and Rust CLIs run in different working directories, so `<cwd>/.anon_id` differs | Expected — `anonymous_id` is intentionally project-local | The test asserts `persistent_id` and `api_key_tracking_id` parity, not `anonymous_id`. Documented in §1 above. |
| Rust CLI does not have a "delete dry-run that emits telemetry" path | Likely — the SDK fires telemetry only on actual `forget()` calls | Two options: (a) add a CLI subcommand `cognee telemetry emit-test --event cognee.forget` that calls `send_telemetry` directly. (b) drive the SDK from a small Rust harness binary added under `e2e-cross-sdk/bin/`. Recommendation: (b) — keeps production CLI surface clean. |
| Volume `cognee-home` persists across test runs and creates a stale `persistent_id` | Bug-shaped — expected: tests should use a fresh volume per run | The `docker compose down -v` step in §4.6 wipes volumes. Document the requirement in `e2e-cross-sdk/README.md`. |
| `telemetry-proxy` health check races with the test runner | Mitigated by `depends_on: condition: service_healthy` | If flakes appear, bump retries from 30 to 60. |
| Python `cognee` import in the harness breaks if a future Python release deprecates `send_telemetry` | Low — `cognee.shared.utils.send_telemetry` is part of Python's public surface | If renamed, update the import — the parity contract is stable across renames. |
| Test reads captures while the proxy is mid-write | `_wait_for_n_captures` polls; line-buffered writes via `\n`-terminated json | Acceptable race; tests poll for the exact count. |
| `LLM_API_KEY` set in container leaks to other test stages | Mitigated by per-service env scoping in `docker-compose.yml` | Other services don't read `LLM_API_KEY` unless they explicitly need it. |
| Rust binary doesn't honour `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS` because the override is `#[cfg(test)]` only | Real risk — the override is gated by `cfg(test)` AND `COGNEE_TELEMETRY_INTEGRATION_TEST` env (per [task 02-05](05-client-dispatch-and-optout.md)). The `cfg(test)` branch is **not** active in `cargo build --release` | We need a release-build path for the override. Either: (a) gate the override on `COGNEE_TELEMETRY_INTEGRATION_TEST` *only*, no `cfg(test)`. (b) Build the Rust binary in debug mode for the cross-SDK test. Recommendation: (a) — safer for release artefacts, easier for tests. Update [task 02-05](05-client-dispatch-and-optout.md) `proxy_url()` to drop the `cfg(test)` guard before this task lands. |

## 8. Out of scope

- Running this test in the main `lib-tests.yml` GitHub Actions
  workflow — the Docker image is large and the harness is already
  on a separate workflow (`http-parity.yml` per the explore report).
  [Task 02-12](12-ci-updates.md) decides whether to add a new
  `telemetry-parity.yml` lane or fold into an existing one.
- Live-proxy smoke tests — see [task 02-11](11-user-docs.md) for the
  manual recipe.
- OTLP cross-SDK tests — that's a separate gap-01 follow-up.
