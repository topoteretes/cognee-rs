"""Verify the gap-07 pyo3-log bridge routes Rust tracing events into
Python's ``logging`` module.

The PyO3 binding installs (in ``#[pymodule] fn _native``) a
``tracing_subscriber::Registry`` with a custom layer that forwards each
event into Python's ``logging`` module via the ``pyo3-log`` bridge
(see ``python/src/default_subscriber.rs``). These tests lock in that
behaviour against accidental clobbering by ``setup_telemetry`` or
``setup_logging``.

Decisions exercised:

* **5** — ``pyo3-log`` is the canonical Python event sink.
* **1**  — the default subscriber is installed on module import and is
  suppressible via ``COGNEE_BINDING_SUPPRESS_LOGS``.

Implementation notes
--------------------
``pyo3-log`` caches per-logger enablement at first event. The cache
is seeded from the Python root logger's effective level *at install
time*, so a Python handler attached later in the same process may
not start receiving Rust events without an explicit cache reset.
The conftest in this directory imports ``cognee_pipeline`` eagerly,
which races us — every assertion here therefore runs in a fresh
subprocess where we control the order:

  1. ``logging.basicConfig(level=DEBUG)`` BEFORE the import so the
     default Python root level lets INFO/DEBUG through from the start.
  2. ``import cognee_pipeline`` installs the bridge (or skips it when
     suppressed).
  3. Execute a pipeline to fire ``tracing::info_span!``/``warn!`` etc.
  4. Print a summary of captured records back to the parent.
"""
import os
import subprocess
import sys
import textwrap

import pytest


def _run(env_extra: dict, expect_zero: bool = True) -> subprocess.CompletedProcess:
    env = {**os.environ, **env_extra}
    # Make sure analytics + OTLP env are inert so the subprocess does
    # nothing surprising.
    env.pop("COGNEE_RUST_TELEMETRY", None)
    env.pop("OTEL_EXPORTER_OTLP_ENDPOINT", None)
    env.pop("COGNEE_TRACING_ENABLED", None)

    script = textwrap.dedent(
        """
        import logging

        captured = []

        class Capture(logging.Handler):
            def emit(self, record):
                captured.append((record.name, record.levelname))

        # Order matters: configure the root logger BEFORE importing
        # cognee_pipeline so the pyo3-log cache sees a permissive
        # effective level at install time.
        root = logging.getLogger()
        root.setLevel(logging.DEBUG)
        root.addHandler(Capture(level=logging.DEBUG))

        import cognee_pipeline

        ctx = cognee_pipeline.TaskContext.mock()
        pipeline = cognee_pipeline.Pipeline("bridge-smoke")
        pipeline.add_task(lambda x: x + 1, name="inc")
        pipeline.execute_sync([1], ctx)

        # Print one line per record: <logger_name>\\t<levelname>.
        for name, levelname in captured:
            print(f"REC\\t{name}\\t{levelname}")
        print(f"TOTAL\\t{len(captured)}")
        """
    )
    res = subprocess.run(
        [sys.executable, "-c", script],
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if expect_zero:
        assert res.returncode == 0, (
            f"subprocess exited {res.returncode}\n"
            f"stderr:\n{res.stderr}\nstdout:\n{res.stdout}"
        )
    return res


@pytest.mark.serial
def test_rust_event_arrives_in_python_logging():
    """With the bridge installed, executing a pipeline must yield at
    least one Python ``LogRecord`` whose logger name matches a Rust
    crate target (the bridge preserves ``metadata.target()`` as the
    logger name).

    ``RUST_LOG`` deliberately silences ``sqlx`` query-level events —
    they fire on a backoff timer that can starve the connection pool
    before the bridge cache warms up in a fresh subprocess. The
    surviving cognee/sea-orm targets still produce many events per
    pipeline run."""
    res = _run({
        "RUST_LOG": "info,sqlx=warn",
        "COGNEE_BINDING_SUPPRESS_LOGS": "",
    })
    lines = [ln for ln in res.stdout.splitlines() if ln.startswith("REC\t")]
    # Any record with an underscore in its name is almost certainly a
    # Rust crate target (snake_case). Accept any non-zero count of
    # such records.
    rust = [ln for ln in lines if "_" in ln.split("\t")[1]]
    assert rust, (
        "expected at least one Rust-target record forwarded via pyo3-log; "
        f"captured records:\n{res.stdout}\nstderr:\n{res.stderr}"
    )


@pytest.mark.serial
def test_suppression_env_var_is_observed_in_subprocess():
    """``COGNEE_BINDING_SUPPRESS_LOGS=1`` set BEFORE the first
    ``cognee_pipeline`` import keeps the bridge silent — zero records
    must be forwarded."""
    res = _run({
        "RUST_LOG": "info,sqlx=warn",
        "COGNEE_BINDING_SUPPRESS_LOGS": "1",
    })
    lines = [ln for ln in res.stdout.splitlines() if ln.startswith("REC\t")]
    rust = [ln for ln in lines if "_" in ln.split("\t")[1]]
    assert not rust, (
        "expected zero Rust events forwarded into Python logging when the "
        f"bridge is suppressed; captured:\n{res.stdout}"
    )
