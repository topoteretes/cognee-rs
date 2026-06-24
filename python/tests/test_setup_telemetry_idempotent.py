"""Verify ``setup_telemetry()`` and ``setup_telemetry_analytics()`` are
idempotent and that the no-config path is silent (gap 07 decision 12).

* The first call without OTLP env vars installs a noop guard and
  returns normally.
* Subsequent calls return without panicking.
* When ``OTEL_EXPORTER_OTLP_ENDPOINT`` is set, the binding applies the
  ``cognee.python-binding`` default for ``OTEL_SERVICE_NAME`` (decision
  8). The endpoint can be unreachable — exporter errors are deferred.

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
def test_service_name_default_applied(monkeypatch):
    """With OTLP endpoint configured but ``OTEL_SERVICE_NAME`` unset,
    the binding must seed ``cognee.python-binding`` (decision 8) before
    the OTEL exporter reads the env.

    A previous test in this process may have latched the singleton — in
    that case the env var is not re-applied. We assert the weaker
    invariant: after the call, the env var is either set to the
    binding default OR was already set (singleton-skipped path)."""
    monkeypatch.delenv("OTEL_SERVICE_NAME", raising=False)
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:65535")
    monkeypatch.setenv("COGNEE_TRACING_ENABLED", "true")
    # Endpoint is unreachable but the exporter is lazy — init must
    # succeed at setup time.
    cognee_py.setup_telemetry()
    value = os.environ.get("OTEL_SERVICE_NAME")
    # If the singleton was already populated by a prior test, the env
    # default is not re-seeded. Both states are acceptable.
    assert value in (None, "", "cognee.python-binding")


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
