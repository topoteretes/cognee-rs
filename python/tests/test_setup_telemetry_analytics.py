"""Verify the fail-closed per-binding analytics policy for PyO3.

Analytics default **OFF** and require ``COGNEE_PRODUCT_TELEMETRY_ENABLED``;
``TELEMETRY_DISABLED``, ``ENV`` and ``COGNEE_HOST_SDK`` remain suppressions.

Analytics are armed automatically on import; ``setup_telemetry_analytics``
reports the latched effective state. Each scenario therefore runs in its
own subprocess so the latch does not leak between cases.
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
            "import cognee_py;"
            "print('armed=' + str(cognee_py.setup_telemetry_analytics()))",
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
    out = _run_in_subprocess(
        {
            "COGNEE_PRODUCT_TELEMETRY_ENABLED": "",
            "TELEMETRY_DISABLED": "",
            "ENV": "",
            "COGNEE_HOST_SDK": "",
        }
    )
    assert out == "armed=False", out


@pytest.mark.serial
def test_explicit_opt_in_is_on():
    out = _run_in_subprocess(
        {
            "COGNEE_PRODUCT_TELEMETRY_ENABLED": "1",
            "TELEMETRY_DISABLED": "",
            "ENV": "",
            "COGNEE_HOST_SDK": "",
        }
    )
    assert out == "armed=True", out


@pytest.mark.serial
def test_telemetry_disabled_suppresses():
    out = _run_in_subprocess(
        {
            "COGNEE_PRODUCT_TELEMETRY_ENABLED": "1",
            "TELEMETRY_DISABLED": "1",
            "COGNEE_HOST_SDK": "",
        }
    )
    assert out == "armed=False", out


@pytest.mark.serial
def test_env_test_suppresses():
    out = _run_in_subprocess(
        {
            "COGNEE_PRODUCT_TELEMETRY_ENABLED": "1",
            "ENV": "test",
            "TELEMETRY_DISABLED": "",
        }
    )
    assert out == "armed=False", out


@pytest.mark.serial
def test_env_dev_suppresses():
    out = _run_in_subprocess(
        {
            "COGNEE_PRODUCT_TELEMETRY_ENABLED": "1",
            "ENV": "dev",
            "TELEMETRY_DISABLED": "",
        }
    )
    assert out == "armed=False", out


@pytest.mark.serial
def test_host_sdk_suppresses():
    out = _run_in_subprocess(
        {
            "COGNEE_PRODUCT_TELEMETRY_ENABLED": "1",
            "COGNEE_HOST_SDK": "python",
            "TELEMETRY_DISABLED": "",
        }
    )
    assert out == "armed=False", out
