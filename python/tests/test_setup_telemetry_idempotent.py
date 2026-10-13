"""Verify ``setup_telemetry()`` and ``setup_telemetry_analytics()`` are
idempotent and that the no-config path is silent (gap 07 decision 12).

* The first call without OTLP env vars installs a noop guard and
  returns normally.
* Subsequent calls return without panicking.
* When ``OTEL_EXPORTER_OTLP_ENDPOINT`` is set, the binding uses the
  ``cognee.python-binding`` default service name (decision 8) as a
  *default* in its config — it no longer writes ``OTEL_SERVICE_NAME`` into
  the process environment. The endpoint can be unreachable — exporter
  errors are deferred.

These tests must run in the same process to actually exercise the
singleton; ``monkeypatch`` mutates the live env and ``pytest`` ensures
the changes are unwound between tests.
"""
import os

import pytest

import cognee_py


@pytest.mark.serial
def test_no_config_is_silent(monkeypatch, capsys):
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)
    monkeypatch.delenv("COGNEE_TRACING_ENABLED", raising=False)
    # Must not raise.
    cognee_py.setup_telemetry()
    # Idempotent — second call must also not raise.
    cognee_py.setup_telemetry()
    captured = capsys.readouterr()
    # We tolerate any informational/banner output but reject panic
    # markers from a misbehaving install.
    assert "panic" not in captured.err.lower()


@pytest.mark.serial
def test_service_name_default_does_not_mutate_env(monkeypatch):
    """With an OTLP endpoint configured but ``OTEL_SERVICE_NAME`` unset, the
    binding default service name (decision 8) is applied as a config default:
    the process environment is left untouched.

    A previous test in this process may have latched the singleton; either
    way the env var must stay unset."""
    monkeypatch.delenv("OTEL_SERVICE_NAME", raising=False)
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:65535")
    monkeypatch.setenv("COGNEE_TRACING_ENABLED", "true")
    # Endpoint is unreachable but the exporter is lazy — init must
    # succeed at setup time.
    cognee_py.setup_telemetry({"otel_span_processor": "batch"})
    assert os.environ.get("OTEL_SERVICE_NAME") is None


def test_setup_telemetry_rejects_unknown_option_in_fresh_process():
    """An unknown option key raises ``ValueError`` (fresh process, so the
    first-call latch cannot short-circuit the validation)."""
    import subprocess
    import sys

    code = (
        "import cognee_py\n"
        "try:\n"
        "    cognee_py.setup_telemetry({'otel_nope': 'x'})\n"
        "except ValueError as e:\n"
        "    print('ValueError', 'unknown OpenTelemetry setting' in str(e))\n"
    )
    res = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, timeout=30
    )
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == "ValueError True", res.stdout


@pytest.mark.serial
def test_setup_telemetry_analytics_is_idempotent(monkeypatch):
    """``setup_telemetry_analytics()`` returns the latched decision and
    must not raise on repeat invocation (decision 12)."""
    # Make the env shape deterministic. We do not assert ``armed`` —
    # a prior test may have latched the opposite. We only assert the
    # function never raises and that two consecutive calls return the
    # same value.
    monkeypatch.delenv("COGNEE_RUST_TELEMETRY", raising=False)
    monkeypatch.delenv("COGNEE_HOST_SDK", raising=False)
    first = cognee_py.setup_telemetry_analytics()
    second = cognee_py.setup_telemetry_analytics()
    assert first == second
    assert isinstance(first, bool)
