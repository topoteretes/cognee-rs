"""Verify the per-binding analytics policy for the PyO3 binding
(decision 11): Python defaults analytics OFF; opt-in via
``COGNEE_RUST_TELEMETRY=1``; ``COGNEE_HOST_SDK`` suppresses the opt-in.

``setup_telemetry_analytics`` installs a process-global latched flag.
Each scenario therefore runs in its own subprocess so the latch does
not leak between cases.
"""
import os
import subprocess
import sys

import pytest


def _run_in_subprocess(env_extra: dict) -> str:
    env = {**os.environ, **env_extra}
    # Belt-and-braces: the subprocess must not double-install the
    # default subscriber's heavier layers; keep OTLP env neutral.
    env.pop("OTEL_EXPORTER_OTLP_ENDPOINT", None)
    env.pop("COGNEE_TRACING_ENABLED", None)
    res = subprocess.run(
        [
            sys.executable,
            "-c",
            "import cognee_pipeline;"
            "print('armed=' + str(cognee_pipeline.setup_telemetry_analytics()))",
        ],
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert res.returncode == 0, f"stderr:\n{res.stderr}\nstdout:\n{res.stdout}"
    return res.stdout.strip()


@pytest.mark.serial
def test_default_is_off():
    out = _run_in_subprocess({"COGNEE_RUST_TELEMETRY": "", "COGNEE_HOST_SDK": ""})
    assert out == "armed=False", out


@pytest.mark.serial
def test_opt_in_arms():
    out = _run_in_subprocess({"COGNEE_RUST_TELEMETRY": "1", "COGNEE_HOST_SDK": ""})
    assert out == "armed=True", out


@pytest.mark.serial
def test_host_sdk_suppresses_opt_in():
    out = _run_in_subprocess({"COGNEE_RUST_TELEMETRY": "1", "COGNEE_HOST_SDK": "python"})
    assert out == "armed=False", out


@pytest.mark.serial
def test_true_value_also_arms():
    """The implementation also accepts ``true`` (case-insensitive) as
    an opt-in value — locked in via the policy doc."""
    out = _run_in_subprocess({"COGNEE_RUST_TELEMETRY": "true", "COGNEE_HOST_SDK": ""})
    assert out == "armed=True", out
