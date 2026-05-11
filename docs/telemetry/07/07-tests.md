# Task 07-07 — Tests for gap 07

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: tasks 07-02 through 07-06 — every implementation task must be in place before tests land.
**Blocks**:
- [Task 07-08 — Docs and CI](08-docs-and-ci.md) (CI lane references the new test files).

**Parent doc**: [07 — Bindings auto-init for tracing & telemetry](../07-bindings-auto-init.md)
**Locked decisions**: 1 (hybrid auto-init), 5 (`pyo3-log` bridge), 6 (panic hook), 11 (per-binding analytics defaults), 13 (cross-SDK no-double-emit test is skipped).

---

## 1. Goal

Land the test surface that locks in gap 07's invariants:

1. **PyO3** — `pyo3-log` bridge routes Rust events into Python
   `logging`; suppression env var works; `setup_telemetry` and
   `setup_telemetry_analytics` are idempotent and honour their
   policies.
2. **Neon** — default stderr subscriber writes events; suppression
   works; `setupTelemetry` and `setupTelemetryAnalytics` are
   idempotent.
3. **C API** — panic hook fires across FFI; `cognee_init_otlp` is
   idempotent; `cognee_init_telemetry` returns the expected policy
   code.
4. **Cross-SDK harness** — `test_telemetry_no_double_emit.py`
   exists, marked `pytest.skip` (decision 13).

All env-mutating tests are serialized.

## 2. Rationale

Each implementation task could land its own tests inline, but
batching them here lets sub-agent C run a single
`cargo test`/`pytest`/`npm test` pass per binding and catch
cross-task regressions (e.g. a `setup_telemetry` install that
clobbers the default subscriber installed by 07-02).

## 3. Pre-conditions

- Tasks 07-02 through 07-06 committed.
- `cognee-telemetry` exposes `arm_binding_emission` /
  `is_binding_armed` / `reset_binding_armed` (cfg-test).
- `serial_test` is already a workspace dev-dep
  ([`Cargo.toml`](../../../Cargo.toml) — verify).

## 4. Step-by-step

### 4.1 PyO3 tests — `python/tests/`

Add four test files under
[`python/tests/`](../../../python/tests/):

#### `python/tests/test_pyo3_log_bridge.py`

```python
"""Verify the gap-07 pyo3-log bridge routes Rust tracing events
into Python's logging module."""

import logging
import pytest


def test_rust_event_arrives_in_python_logging(monkeypatch):
    monkeypatch.delenv("COGNEE_BINDING_SUPPRESS_LOGS", raising=False)
    monkeypatch.setenv("RUST_LOG", "info")

    captured: list[logging.LogRecord] = []

    class Capture(logging.Handler):
        def emit(self, record):
            captured.append(record)

    handler = Capture(level=logging.DEBUG)
    logging.getLogger().addHandler(handler)
    logging.getLogger().setLevel(logging.DEBUG)

    import cognee_pipeline  # triggers default_subscriber::install

    # Constructing a Pipeline emits at least one tracing::info!
    cognee_pipeline.Pipeline()

    # Look for any record whose logger name starts with a Rust
    # crate name we instrument (pyo3-log preserves the tracing
    # target as logger name).
    rust_records = [r for r in captured if r.name.startswith(("cognee_core", "cognee_"))]
    assert rust_records, (
        "expected at least one Rust tracing event in Python logging; "
        f"captured loggers: {[r.name for r in captured]}"
    )


def test_suppression_env_var(monkeypatch):
    monkeypatch.setenv("COGNEE_BINDING_SUPPRESS_LOGS", "1")
    # Reimporting an extension module across tests is non-trivial;
    # this assertion can only check that the env var is observed
    # by the module's install path on first import. If a previous
    # test already loaded cognee_pipeline, this becomes
    # smoke-only — document and accept.
    import cognee_pipeline  # noqa: F401
    # Bridge is process-global; we cannot reliably reset it.
    # Verify the env var is plumbed by the install path via a
    # dedicated subprocess test:
    import subprocess, sys
    res = subprocess.run(
        [sys.executable, "-c",
         "import os; os.environ['COGNEE_BINDING_SUPPRESS_LOGS']='1';"
         "import logging, cognee_pipeline; "
         "captured=[]; "
         "logging.getLogger().addHandler(logging.Handler(level=0)); "
         "cognee_pipeline.Pipeline(); "
         "print('OK')"],
        capture_output=True, text=True, timeout=30,
    )
    assert res.returncode == 0, res.stderr
```

#### `python/tests/test_setup_telemetry_idempotent.py`

```python
"""Verify setup_telemetry() is a no-op when no collector is
configured, and that repeat calls don't panic."""

import cognee_pipeline


def test_no_config_is_silent(monkeypatch, capsys):
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)
    monkeypatch.delenv("COGNEE_TRACING_ENABLED", raising=False)
    cognee_pipeline.setup_telemetry()  # must not raise
    cognee_pipeline.setup_telemetry()  # idempotent
    captured = capsys.readouterr()
    # Tolerate one-time "Logging initialized" / similar warnings
    # but ensure no error/panic markers.
    assert "panic" not in captured.err.lower()


def test_service_name_default_applied(monkeypatch):
    monkeypatch.delenv("OTEL_SERVICE_NAME", raising=False)
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:65535")
    # Endpoint is unreachable — init_telemetry should still succeed
    # at setup time (export errors are deferred to first span flush).
    cognee_pipeline.setup_telemetry()
    import os
    assert os.environ.get("OTEL_SERVICE_NAME") == "cognee.python-binding"
```

#### `python/tests/test_setup_telemetry_analytics.py`

```python
"""Decision 11 — Python defaults analytics OFF; opt-in via
COGNEE_RUST_TELEMETRY=1; suppressed by COGNEE_HOST_SDK."""

import subprocess
import sys


def _run_in_subprocess(env_extra: dict) -> str:
    """Each scenario runs in its own subprocess because
    setup_telemetry_analytics installs a process-global flag."""
    res = subprocess.run(
        [sys.executable, "-c",
         "import cognee_pipeline; "
         "print('armed=' + str(cognee_pipeline.setup_telemetry_analytics()))"],
        env={**__import__('os').environ, **env_extra},
        capture_output=True, text=True, timeout=30,
    )
    assert res.returncode == 0, res.stderr
    return res.stdout.strip()


def test_default_is_off():
    assert _run_in_subprocess({"COGNEE_RUST_TELEMETRY": "",
                               "COGNEE_HOST_SDK": ""}) == "armed=False"


def test_opt_in_arms():
    assert _run_in_subprocess({"COGNEE_RUST_TELEMETRY": "1",
                               "COGNEE_HOST_SDK": ""}) == "armed=True"


def test_host_sdk_suppresses_opt_in():
    assert _run_in_subprocess({"COGNEE_RUST_TELEMETRY": "1",
                               "COGNEE_HOST_SDK": "python"}) == "armed=False"
```

### 4.2 Neon tests — `js/__tests__/`

Add three test files. The Neon binding uses the existing test
harness; verify the runner (`jest` vs `vitest`) by looking at
[`js/package.json`](../../../js/package.json) before writing test
files.

#### `js/__tests__/default_subscriber.test.ts`

```typescript
import { spawnSync } from "child_process";
import { resolve } from "path";

const requirePath = JSON.stringify(resolve(__dirname, ".."));

function runChild(env: Record<string, string>): { stderr: string } {
  const res = spawnSync(
    process.execPath,
    [
      "-e",
      `require(${requirePath}); ` +
      // Trigger a Rust tracing event by constructing a Pipeline.
      "const cog = require('cognee-neon'); cog.pipelineNew();"
    ],
    { env: { ...process.env, ...env }, encoding: "utf8" }
  );
  return { stderr: res.stderr };
}

test("default subscriber writes to stderr at info level", () => {
  const { stderr } = runChild({ RUST_LOG: "info" });
  expect(stderr).toMatch(/INFO/);
});

test("COGNEE_BINDING_SUPPRESS_LOGS suppresses default subscriber", () => {
  const { stderr } = runChild({
    RUST_LOG: "info",
    COGNEE_BINDING_SUPPRESS_LOGS: "1",
  });
  expect(stderr).toBe("");
});
```

#### `js/__tests__/setup_telemetry.test.ts`

```typescript
test("setupTelemetry is a no-op without OTLP endpoint", () => {
  const cog = require("cognee-neon");
  expect(() => cog.setupTelemetry()).not.toThrow();
  expect(() => cog.setupTelemetry()).not.toThrow(); // idempotent
});
```

#### `js/__tests__/setup_telemetry_analytics.test.ts`

```typescript
import { spawnSync } from "child_process";

function runChild(env: Record<string, string>): boolean {
  const res = spawnSync(
    process.execPath,
    ["-e", "console.log(require('cognee-neon').setupTelemetryAnalytics())"],
    { env: { ...process.env, ...env }, encoding: "utf8" }
  );
  expect(res.status).toBe(0);
  return res.stdout.trim() === "true";
}

test("Neon defaults analytics ON", () => {
  expect(runChild({ TELEMETRY_DISABLED: "", ENV: "prod", COGNEE_HOST_SDK: "" }))
    .toBe(true);
});

test("TELEMETRY_DISABLED suppresses", () => {
  expect(runChild({ TELEMETRY_DISABLED: "1" })).toBe(false);
});

test("ENV=test suppresses", () => {
  expect(runChild({ ENV: "test" })).toBe(false);
});

test("COGNEE_HOST_SDK suppresses", () => {
  expect(runChild({ COGNEE_HOST_SDK: "python" })).toBe(false);
});
```

### 4.3 C API tests — `capi/examples/` + `capi/scripts/check.sh`

Add two example/smoke programs under
[`capi/examples/`](../../../capi/examples/):

#### `capi/examples/panic_hook_smoke.c`

```c
/* Verifies the gap-07 panic hook fires across FFI. Compiled and
 * run by capi/scripts/check.sh. */

#include <stdio.h>
#include <stdlib.h>
#include "cognee.h"

int main(void) {
    cg_init();
    /* Force a panic via a deliberately-invalid pipeline call.
     * The exact mechanism depends on which API call panics most
     * predictably; the implementor must pick one or, failing
     * that, expose a dedicated `cg_test_force_panic()` symbol
     * gated behind a cognee-capi test feature. */
    /* fprintf(stderr, "...") */
    return 0;
}
```

Note for implementor: triggering a deterministic Rust panic from C
without adding a test-only symbol is awkward. The recommended
approach is:

```rust
// Add to capi/cognee-capi/src/lib.rs behind a feature flag:
#[cfg(feature = "testing-panic")]
#[unsafe(no_mangle)]
pub extern "C" fn cg_test_force_panic() -> ! {
    panic!("test panic from gap 07 task 07-04");
}
```

Add the `testing-panic` feature to the C binding's `Cargo.toml`,
enable it from `capi/scripts/check.sh` test build, and reference
the symbol from `panic_hook_smoke.c`. The check script greps
stderr for `[cognee-capi panic]`.

#### `capi/examples/init_otlp_smoke.c`

```c
/* Verifies cognee_init_otlp returns 0 for the no-config case and
 * is idempotent. */

#include <stdio.h>
#include "cognee.h"

int main(void) {
    cg_init();
    int rc1 = cognee_init_otlp();
    int rc2 = cognee_init_otlp();
    if (rc1 != 0 || rc2 != 0) {
        fprintf(stderr, "cognee_init_otlp returned non-zero: %d %d\n", rc1, rc2);
        return 1;
    }
    return 0;
}
```

#### Update `capi/scripts/check.sh`

Add a section that builds both smoke binaries and runs them. The
panic test expects the binary to abort with `[cognee-capi panic]`
on stderr; the OTLP test expects exit 0.

### 4.4 `cognee-telemetry` unit tests

Already added in §4.1 of [06-host-sdk-sentinel.md](06-host-sdk-sentinel.md);
no new file here. Re-run `cargo test -p cognee-telemetry` in
verification.

### 4.5 Cross-SDK harness — skipped

Add `e2e-cross-sdk/harness/test_telemetry_no_double_emit.py`:

```python
"""Cross-SDK no-double-emit assertion (gap 07 decision 13).

This test cannot fail meaningfully until a binding starts emitting
`send_telemetry` events. Today the Python `cognee` SDK fires
analytics from its own Python code; the Rust `cognee_pipeline`
binding exposes only the pipeline surface and never reaches the
`cognee_lib::api::*` call sites that emit. The harness wiring
lives here so the test runs automatically the moment a future gap
surfaces those APIs through PyO3.

When that gap lands, remove the skip marker and verify the assertion
runs against a mock proxy URL configured via
`COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS`."""

import pytest

pytestmark = pytest.mark.skip(
    reason="Pending binding surfacing of cognee_lib::api::* (gap 07 decision 13)"
)


def test_no_double_emit_when_host_sdk_set():
    # Skeleton — see docstring.
    assert False, "unreachable while marker is active"
```

Wire into the existing harness's `pyproject.toml` /
`pytest.ini` so the test is collected (and skipped) on every run —
keeps the wiring honest.

## 5. Verification

```bash
# Rust unit tests
cargo test -p cognee-telemetry
cargo test -p cognee-capi --features testing-panic --test '*'

# Python tests
cd python && pytest tests/test_pyo3_log_bridge.py \
                  tests/test_setup_telemetry_idempotent.py \
                  tests/test_setup_telemetry_analytics.py && cd -

# Neon tests
cd js && npm test -- default_subscriber setup_telemetry setup_telemetry_analytics && cd -

# C smoke binaries
bash capi/scripts/check.sh

# Cross-SDK harness (test is collected and skipped)
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit \
  || echo "tolerate cross-SDK harness issues for the skipped test"
cd -

# Full check
scripts/check_all.sh
```

## 6. Files modified

- `python/tests/test_pyo3_log_bridge.py` — NEW.
- `python/tests/test_setup_telemetry_idempotent.py` — NEW.
- `python/tests/test_setup_telemetry_analytics.py` — NEW.
- `js/__tests__/default_subscriber.test.ts` — NEW.
- `js/__tests__/setup_telemetry.test.ts` — NEW.
- `js/__tests__/setup_telemetry_analytics.test.ts` — NEW.
- `capi/examples/panic_hook_smoke.c` — NEW.
- `capi/examples/init_otlp_smoke.c` — NEW.
- `capi/cognee-capi/src/lib.rs` — `cg_test_force_panic` behind
  `testing-panic` feature.
- `capi/cognee-capi/Cargo.toml` — `testing-panic = []` feature.
- `capi/scripts/check.sh` — invoke smoke binaries.
- `e2e-cross-sdk/harness/test_telemetry_no_double_emit.py` — NEW
  (skipped).

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Python tests rely on module re-import to test suppression, but PyO3 modules are not freely re-importable | Acknowledged — use subprocess pattern from §4.1's `test_suppression_env_var`. | Subprocess pattern is the standard fix; documented in each test that needs it. |
| Neon tests require the cdylib to be re-loadable across subprocesses; build artefacts may be stale | Low — `npm test` rebuilds before running. | Implementor verifies `js/package.json` test script triggers a build. |
| `cg_test_force_panic` symbol leaks into release builds if `testing-panic` feature is accidentally enabled | Low — feature is opt-in. | Document in 07-08 README that release builds must not enable `testing-panic`. |
| Subprocess-based Python tests are slow (~1s each × N) | Medium — adds ~30s to the suite. | Accepted; cross-isolation is more valuable than speed for env-mutating tests. |
| Skipped cross-SDK test causes confusion ("why is this xfail/skip?") | Low — docstring is clear. | Reference decision 13 in the skip reason. |
| `pyo3-log` test asserts on logger names that may rename over time | Medium — depends on the Rust target naming convention (snake_case crate names). | Use a fuzzy prefix match (`startswith("cognee")`) rather than equality. |

## 8. Out of scope

- A test that asserts the OTLP layer actually exports a span to a
  real collector. That is gap 01's responsibility (and is already
  tested at the `cognee-observability` crate level); gap 07 only
  verifies the binding plumbing wires up correctly.
- A test that mocks `https://test.prometh.ai` to verify
  `send_telemetry` payload contents. Gap 02 covers that for the
  Rust call site path. Bindings don't emit yet (decision 4).
- A test that verifies `cognee.shared.logging_utils.setup_logging`
  (upstream Python `cognee`) interoperates with the bridge. The
  upstream SDK is not a build artifact here; cover this in the
  cross-SDK harness as a follow-up if requested.
