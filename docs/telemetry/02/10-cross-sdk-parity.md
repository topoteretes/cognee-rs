# Task 02-10 — Cross-SDK identity parity (Python ↔ Rust)

**Status**: implemented in commit 9762c2b (note: harness lives at e2e-cross-sdk/telemetry-emit + bin/telemetry_proxy.py + harness/test_telemetry_parity.py with a new e2e-telemetry compose service; harness Cargo.toml enables cognee-lib's qdrant feature only to silence a pre-existing unused-binding lint in component_manager.rs:140 — clean up separately. Final docker compose up --build verification deferred to CI task 02-12).
**Owner**: _unassigned_
**Depends on**:
- [Task 02-07 — Callsite migration](07-callsite-migration.md) — Rust must emit *some* event for parity to be observable.
- [Task 02-09 — Integration tests](09-integration-tests.md) — establishes the mockito patterns reused here.

**Blocks**:
- [Task 02-12 — CI updates](12-ci-updates.md) (the new docker-compose service is gated on a CI lane).

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

> **Note on review history**: this is the **second consecutive A-pass** apply-fixes
> revision of this sub-doc. The previous A-pass identified seven issues
> (fictional service names, wrong binary paths, wrong test directory layout,
> wrong proxy script path, an unnecessary Dockerfile change, an already-mitigated
> risk, and an unverified Rust telemetry entrypoint). All seven fixes have been
> applied in this revision; verification by Sub-agent B does not require
> running Docker (deferred to CI per task 02-12).

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
Rust CLIs into the same image. The Rust release binary lives at
`/usr/local/bin/cognee-cli-rust` and the Python CLI lives at
`/usr/local/bin/cognee-cli-python` (both confirmed in the existing
`Dockerfile` lines 103 and 117 respectively). We extend the
harness with an additional pytest module
(`harness/test_telemetry_parity.py`) and one new docker-compose
service.

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
- The harness already supports passing `OPENAI_TOKEN` / `OPENAI_MODEL`
  via `cognee-rust/.env` (see existing `e2e-tests` service in
  `docker-compose.yml`); the same `env_file` pattern works for
  `LLM_API_KEY` if added to `.env`.

## 4. Step-by-step

### 4.1 Add the mock proxy service + telemetry parity service

Edit
[`e2e-cross-sdk/docker-compose.yml`](../../../e2e-cross-sdk/docker-compose.yml).
Add two new services alongside the existing `e2e-tests` and
`e2e-http-tests`:

```yaml
services:
  # ... existing e2e-tests and e2e-http-tests services unchanged ...

  telemetry-proxy:
    image: python:3.11-slim
    working_dir: /app
    command: ["python", "/app/telemetry_proxy.py"]
    volumes:
      - ./bin/telemetry_proxy.py:/app/telemetry_proxy.py:ro
      - telemetry-captures:/captures
    expose:
      - "9090"
    healthcheck:
      test: ["CMD", "python", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:9090/_health').read()"]
      interval: 1s
      timeout: 1s
      retries: 30

  e2e-telemetry:
    build:
      context: ../..                              # /home/dmytro/dev/cognee/ (monorepo root)
      dockerfile: cognee-rust/e2e-cross-sdk/Dockerfile
    env_file:
      - ../.env
    environment:
      - LLM_MODEL=${OPENAI_MODEL:-gpt-4o-mini}
      - COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS=http://telemetry-proxy:9090
      - COGNEE_TELEMETRY_INTEGRATION_TEST=1
      - LLM_API_KEY=sk-test-fixture-cross-sdk
      - TRACKING_ID=fixed-anon-cross-sdk
    tmpfs:
      - /workspace
    depends_on:
      telemetry-proxy:
        condition: service_healthy
    volumes:
      - cognee-home:/root/.cognee
      - telemetry-captures:/captures:ro
    command: ["pytest", "-vs", "/harness/test_telemetry_parity.py"]

volumes:
  telemetry-captures:
  cognee-home:
```

The shared `cognee-home` volume mounts at `/root/.cognee` so the
Python `~/.cognee/.persistent_id` and the Rust
`$HOME/.cognee/.persistent_id` point at the same file.

### 4.2 Implement the mock proxy

Create `e2e-cross-sdk/bin/telemetry_proxy.py` (the existing
harness convention places executable helpers under `bin/` — see
`bin/start_servers.sh`):

```python
#!/usr/bin/env python3
"""Tiny HTTP proxy that captures POST bodies to /captures/all.jsonl.

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

### 4.3 Add a Rust telemetry driver binary

The Rust CLI does **not** currently route any subcommand to a
`send_telemetry` call (verified by grepping
`crates/cli/src/` — no `send_telemetry` references; the only
`forget`-related code path is in `commands/delete.rs` and does not
emit telemetry yet). Even after [task 02-07](07-callsite-migration.md)
wires `forget.rs`, the CLI `delete --dry-run` may not exercise that
path with the cross-SDK fixture data.

To keep the production CLI surface clean, **add a dedicated
harness-only Rust binary** at `e2e-cross-sdk/bin/telemetry_emit.rs`
(packaged as a tiny crate in `e2e-cross-sdk/`, OR added as an extra
`[[bin]]` target in the existing `cognee-cli` crate behind a
`harness` feature flag — pick the simpler path during implementation).
The binary calls
`cognee_lib::telemetry::send_telemetry("cognee.forget", ...)`
directly with fixed args:

```rust
// e2e-cross-sdk/bin/telemetry_emit.rs (illustrative)
use cognee_lib::telemetry;

#[tokio::main]
async fn main() {
    telemetry::send_telemetry(
        "cognee.forget",
        "cross-sdk-user",
        std::collections::HashMap::from([
            ("target".to_string(), "everything".into()),
            ("cognee_version".to_string(), "cross-sdk-test".into()),
        ]),
    ).await;
}
```

Wire it into the harness Dockerfile (existing `rust-builder`
stage) so the binary is copied to `/usr/local/bin/cognee-telemetry-emit`
in the final image. **Note**: this is the only Dockerfile change
this task needs; the main `cognee-cli-rust` binary is already at
`/usr/local/bin/cognee-cli-rust` (Dockerfile line 103) and the
Python CLI is at `/usr/local/bin/cognee-cli-python` (line 117).

### 4.4 Implement the pytest

Create `e2e-cross-sdk/harness/test_telemetry_parity.py` (tests
live directly under `harness/`, not in a `tests/` subdir — see
all the existing `test_*.py` files in `harness/`):

```python
"""Cross-SDK identity parity for send_telemetry.

Asserts that Python and Rust SDKs, given the same LLM_API_KEY and
shared ~/.cognee/, produce identical `api_key_tracking_id` and
`persistent_id` on the wire.

Run via the e2e-telemetry compose service.
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
    """Drive Python's send_telemetry directly inside this process.

    The harness image installs the Python `cognee` package into
    /opt/python-venv (Dockerfile line 115). Either import in-process
    via the venv's python interpreter, or invoke the installed CLI at
    /usr/local/bin/cognee-cli-python. In-process import is preferred
    because it avoids a CLI subcommand dependency.
    """
    # Use the venv interpreter so cognee imports resolve correctly.
    subprocess.check_call(
        [
            "/opt/python-venv/bin/python",
            "-c",
            (
                "from cognee.shared.utils import send_telemetry;"
                "send_telemetry('cognee.forget', user_id='cross-sdk-user',"
                " additional_properties={'target':'everything',"
                " 'cognee_version':'cross-sdk-test'})"
            ),
        ],
        env={**os.environ},
        timeout=30,
    )


def _rust_emit():
    """Drive Rust's send_telemetry via the harness-only emit binary.

    See §4.3: a dedicated /usr/local/bin/cognee-telemetry-emit binary
    calls cognee_lib::telemetry::send_telemetry directly with fixed
    args. We do NOT use cognee-cli-rust because no production
    subcommand is guaranteed to route to send_telemetry.
    """
    subprocess.check_call(
        ["/usr/local/bin/cognee-telemetry-emit"],
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

### 4.5 No `cognee-cli-rust` Dockerfile change required

The existing
[`e2e-cross-sdk/Dockerfile`](../../../e2e-cross-sdk/Dockerfile)
already copies the Rust release binary to
`/usr/local/bin/cognee-cli-rust` (line 103) and the Python CLI to
`/usr/local/bin/cognee-cli-python` (line 117). **Neither needs to
change for this task.**

The only Dockerfile addition is for the new harness-only
`cognee-telemetry-emit` binary from §4.3 — extend the
`rust-builder` stage's `cargo build` invocation to also build that
binary, and add a `COPY --from=rust-builder` line into the final
stage to land it at `/usr/local/bin/cognee-telemetry-emit`.

### 4.6 Verify (Docker required)

> **Verification deferred to CI (task 02-12).** The commands below
> require `docker compose up --build`, which takes several minutes.
> Sub-agent B will implement the files (compose entry, proxy
> script, harness emit binary, pytest, Dockerfile diff) **without**
> invoking Docker. Final green run lives in task 02-12's CI lane.

```bash
cd e2e-cross-sdk
docker compose down -v
docker compose up --build --abort-on-container-exit \
  --exit-code-from e2e-telemetry
```

Expected: the new `test_telemetry_parity` test passes alongside
`test_add_parity`, `test_cross_read`, `test_cognify_structural`
(which run under the existing `e2e-tests` service).

## 5. Verification

> **Reminder**: all commands in this section require Docker; defer
> to CI per §4.6.

```bash
# 1. Rebuild and run the new telemetry parity service.
cd e2e-cross-sdk
docker compose up --build --abort-on-container-exit \
  --exit-code-from e2e-telemetry

# 2. Inspect captures after a green run.
docker compose run --rm e2e-telemetry cat /captures/all.jsonl

# 3. Confirm the new service is healthy.
docker compose ps telemetry-proxy
# Expected: STATUS = (healthy)

# 4. Run only the new test for fast iteration.
docker compose run --rm e2e-telemetry pytest \
  /harness/test_telemetry_parity.py -v
```

## 6. Files modified

- `e2e-cross-sdk/docker-compose.yml` — add `telemetry-proxy` and
  `e2e-telemetry` services + named volumes (`telemetry-captures`,
  `cognee-home`).
- `e2e-cross-sdk/bin/telemetry_proxy.py` — new file (mock proxy).
- `e2e-cross-sdk/harness/test_telemetry_parity.py` — new file
  (cross-SDK pytest, flat under `harness/` per existing convention).
- `e2e-cross-sdk/bin/telemetry_emit.rs` (or equivalent extra
  `[[bin]]` target gated on a `harness` feature in `cognee-cli`) —
  new harness-only Rust binary that calls
  `cognee_lib::telemetry::send_telemetry` directly.
- `e2e-cross-sdk/Dockerfile` — extend the `rust-builder` stage to
  build `cognee-telemetry-emit` and copy it to
  `/usr/local/bin/cognee-telemetry-emit` in the final image. The
  existing `cognee-cli-rust` and `cognee-cli-python` lines are
  unchanged.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Python and Rust drivers run in different working directories, so `<cwd>/.anon_id` differs | Expected — `anonymous_id` is intentionally project-local | The test asserts `persistent_id` and `api_key_tracking_id` parity, not `anonymous_id`. Documented in §1 above. |
| Rust CLI does not have a "delete dry-run that emits telemetry" path | Confirmed by grep — no `send_telemetry` references anywhere in `crates/cli/src/` | Resolved in §4.3: a dedicated `e2e-cross-sdk/bin/telemetry_emit.rs` harness binary calls `send_telemetry` directly with fixed args. Keeps production CLI surface clean. |
| Volume `cognee-home` persists across test runs and creates a stale `persistent_id` | Bug-shaped — expected: tests should use a fresh volume per run | The `docker compose down -v` step in §4.6 wipes volumes. Document the requirement in `e2e-cross-sdk/README.md`. |
| `telemetry-proxy` health check races with the test runner | Mitigated by `depends_on: condition: service_healthy` | If flakes appear, bump retries from 30 to 60. |
| Python `cognee` import in the harness breaks if a future Python release deprecates `send_telemetry` | Low — `cognee.shared.utils.send_telemetry` is part of Python's public surface | If renamed, update the import — the parity contract is stable across renames. |
| Test reads captures while the proxy is mid-write | `_wait_for_n_captures` polls; line-buffered writes via `\n`-terminated json | Acceptable race; tests poll for the exact count. |
| `LLM_API_KEY` set in container leaks to other test stages | Mitigated by per-service `environment:` scoping in `docker-compose.yml` | `e2e-tests` and `e2e-http-tests` do not read `LLM_API_KEY` and do not declare it. |
| Rust release binary might ignore `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS` because the override is `#[cfg(test)]` only | **Already mitigated by [task 02-05](05-client-dispatch-and-optout.md).** The current `proxy_url()` at `crates/telemetry/src/env.rs:53-71` honours the override in non-test builds whenever `COGNEE_TELEMETRY_INTEGRATION_TEST=1`. No retroactive edit required. | None. Verified directly in source. |

## 8. Out of scope

- Running this test in the main `lib-tests.yml` GitHub Actions
  workflow — the Docker image is large and the harness is on a
  separate workflow lane.
  [Task 02-12](12-ci-updates.md) decides whether to add a new
  `telemetry-parity.yml` lane or fold into an existing
  cross-SDK lane.
- Live-proxy smoke tests — see [task 02-11](11-user-docs.md) for the
  manual recipe.
- OTLP cross-SDK tests — that's a separate gap-01 follow-up.
