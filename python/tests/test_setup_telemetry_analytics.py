"""Verify the per-binding analytics policy for the PyO3 binding
(Python-SDK parity): Python defaults analytics **ON**; emission is
suppressed only by ``TELEMETRY_DISABLED``, ``ENV`` in {test, dev}, or
``COGNEE_HOST_SDK`` (the host-SDK deferral sentinel).

Analytics are armed automatically on import; ``setup_telemetry_analytics``
reports the effective state, and ``set_telemetry_enabled`` is the
process-wide programmatic switch that wins over the environment. The
environment is resolved once per process, so each scenario runs in its own
subprocess.
"""
import os
import subprocess
import sys

import pytest


def _run_in_subprocess(env_extra: dict, code: str | None = None) -> str:
    env = {**os.environ, **env_extra}
    # Belt-and-braces: the subprocess must not double-install the
    # default subscriber's heavier layers; keep OTLP env neutral.
    env.pop("OTEL_EXPORTER_OTLP_ENDPOINT", None)
    env.pop("COGNEE_TRACING_ENABLED", None)
    res = subprocess.run(
        [
            sys.executable,
            "-c",
            code
            or (
                "import cognee_py;"
                "print('armed=' + str(cognee_py.setup_telemetry_analytics()))"
            ),
        ],
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert res.returncode == 0, f"stderr:\n{res.stderr}\nstdout:\n{res.stdout}"
    return res.stdout.strip()


@pytest.mark.serial
def test_default_is_on():
    out = _run_in_subprocess(
        {"TELEMETRY_DISABLED": "", "ENV": "", "COGNEE_HOST_SDK": ""}
    )
    assert out == "armed=True", out


@pytest.mark.serial
def test_telemetry_disabled_suppresses():
    out = _run_in_subprocess({"TELEMETRY_DISABLED": "1", "COGNEE_HOST_SDK": ""})
    assert out == "armed=False", out


@pytest.mark.serial
def test_env_test_suppresses():
    out = _run_in_subprocess({"ENV": "test", "TELEMETRY_DISABLED": ""})
    assert out == "armed=False", out


@pytest.mark.serial
def test_env_dev_suppresses():
    out = _run_in_subprocess({"ENV": "dev", "TELEMETRY_DISABLED": ""})
    assert out == "armed=False", out


@pytest.mark.serial
def test_host_sdk_suppresses():
    out = _run_in_subprocess({"COGNEE_HOST_SDK": "python", "TELEMETRY_DISABLED": ""})
    assert out == "armed=False", out


_SWITCH = (
    "import cognee_py\n"
    "out = [cognee_py.is_telemetry_enabled()]\n"
    "cognee_py.set_telemetry_enabled(False)\n"
    "out += [cognee_py.is_telemetry_enabled(), cognee_py.setup_telemetry_analytics()]\n"
    "cognee_py.set_telemetry_enabled(True)\n"
    "out += [cognee_py.is_telemetry_enabled()]\n"
    "print(','.join(str(v) for v in out))\n"
)


@pytest.mark.serial
def test_programmatic_switch_from_default_on():
    out = _run_in_subprocess(
        {"TELEMETRY_DISABLED": "", "ENV": "", "COGNEE_HOST_SDK": ""}, _SWITCH
    )
    assert out == "True,False,False,True", out


@pytest.mark.serial
def test_programmatic_switch_wins_over_env_opt_out():
    """``set_telemetry_enabled(True)`` overrides ``TELEMETRY_DISABLED``."""
    out = _run_in_subprocess(
        {"TELEMETRY_DISABLED": "1", "ENV": "", "COGNEE_HOST_SDK": ""}, _SWITCH
    )
    assert out == "False,False,False,True", out


@pytest.mark.serial
def test_env_set_after_import_is_honoured():
    """An opt-out set after import (or by a ``.env`` loaded later) is honoured;
    the programmatic switch still wins over it."""
    code = (
        "import os, cognee_py\n"
        "out = [cognee_py.is_telemetry_enabled()]\n"
        "os.environ['TELEMETRY_DISABLED'] = '1'\n"
        "out.append(cognee_py.is_telemetry_enabled())\n"
        "cognee_py.set_telemetry_enabled(True)\n"
        "out.append(cognee_py.is_telemetry_enabled())\n"
        "print(','.join(str(v) for v in out))\n"
    )
    out = _run_in_subprocess(
        {"TELEMETRY_DISABLED": "", "ENV": "", "COGNEE_HOST_SDK": ""}, code
    )
    assert out == "True,False,True", out


@pytest.mark.serial
def test_dotenv_loaded_by_first_handle_is_honoured(tmp_path):
    """``TELEMETRY_DISABLED=1`` in a ``.env`` is loaded into the process env
    when the first handle resolves its settings — after import — and must
    still switch analytics off."""
    (tmp_path / ".env").write_text("TELEMETRY_DISABLED=1\n")
    code = (
        "import cognee_py\n"
        "before = cognee_py.is_telemetry_enabled()\n"
        "cognee_py.Cognee()\n"
        "print(f'{before},{cognee_py.is_telemetry_enabled()}')\n"
    )
    env = {
        **os.environ,
        "TELEMETRY_DISABLED": "",
        "ENV": "",
        "COGNEE_HOST_SDK": "",
        "COGNEE_SYSTEM_ROOT_DIRECTORY": str(tmp_path / "sys"),
        "COGNEE_DATA_ROOT_DIRECTORY": str(tmp_path / "data"),
    }
    # An empty TELEMETRY_DISABLED would shadow the .env value (dotenv never
    # overrides an existing variable), so it must be truly unset here.
    env.pop("TELEMETRY_DISABLED")
    res = subprocess.run(
        [sys.executable, "-c", code],
        env=env,
        cwd=tmp_path,
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert res.returncode == 0, f"stderr:\n{res.stderr}"
    assert res.stdout.strip().splitlines()[-1] == "True,False", res.stdout
