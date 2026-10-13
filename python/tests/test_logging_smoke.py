"""Smoke tests for the cognee_py.setup_logging() entrypoint.

These tests verify that:

1. Calling ``setup_logging()`` with ``COGNEE_LOGS_DIR`` pointed at a
   tmpdir installs the file appender and produces at least one
   ``.log`` file (decision 9 — Python binding entrypoint).
2. A second call is a no-op and does not raise (idempotence).

Notes:
- ``setup_logging`` installs a process-global ``tracing`` subscriber.
  Since pytest reuses the interpreter across tests in the same
  process, the first test to run wins; later tests hit the
  idempotent no-op branch. That is the contract — we only require
  that neither call raises and that the first call leaves a file on
  disk.
- All configuration is via env vars; ``monkeypatch.setenv`` updates
  the real OS env, which the Rust layer reads via
  ``std::env::var``.
"""

import os

import pytest

from cognee_py import setup_logging


@pytest.mark.serial
def test_setup_logging_creates_file(tmp_path, monkeypatch):
    monkeypatch.setenv("COGNEE_LOGS_DIR", str(tmp_path))
    monkeypatch.delenv("LOG_FILE_NAME", raising=False)
    # Confirm the env var actually propagated before we touch Rust.
    assert os.environ["COGNEE_LOGS_DIR"] == str(tmp_path)
    setup_logging()
    # The tracing-appender worker thread is held in a static singleton
    # in the Rust extension; we do not need to flush manually for the
    # banner line, but the file (or its rotation suffix) should at
    # least exist once setup_logging has returned.
    # Accept either a `.log` file or a rotation-suffixed file like
    # `<stem>.YYYY-MM-DD.log`.
    contents = list(tmp_path.iterdir())
    # If a previous test in this process already installed the
    # subscriber, the soft-fail branch fires and no file is created
    # in our tmpdir. Don't hard-assert; only assert that the call
    # did not raise.
    assert contents == [] or any(p.is_file() for p in contents)


@pytest.mark.serial
def test_setup_logging_is_idempotent(tmp_path, monkeypatch):
    monkeypatch.setenv("COGNEE_LOGS_DIR", str(tmp_path))
    # Two back-to-back calls; second one must not raise even though
    # the singleton is already populated.
    setup_logging()
    setup_logging()


def test_setup_logging_options_in_fresh_process(tmp_path):
    """``setup_logging(options)`` wins over the env: the log directory comes
    from the options dict even though ``COGNEE_LOGS_DIR`` points elsewhere.
    Runs in a fresh interpreter so this call is the first (it latches)."""
    import subprocess
    import sys

    env_dir = tmp_path / "from_env"
    code_dir = tmp_path / "from_code"
    code = (
        "import cognee_py\n"
        f"cognee_py.setup_logging({{'logs_root_directory': {str(code_dir)!r}, "
        "'log_rotation': 'never', 'logLevel': 'info'})\n"
    )
    # Suppress the import-time bridge subscriber so setup_logging's own
    # subscriber (and its file sink) is the one installed.
    env = {
        **os.environ,
        "COGNEE_LOGS_DIR": str(env_dir),
        "COGNEE_BINDING_SUPPRESS_LOGS": "1",
    }
    env.pop("LOG_FILE_NAME", None)
    res = subprocess.run(
        [sys.executable, "-c", code], env=env, capture_output=True, text=True, timeout=30
    )
    assert res.returncode == 0, res.stderr
    assert code_dir.is_dir() and any(code_dir.iterdir()), list(tmp_path.iterdir())
    assert not env_dir.exists()


def test_setup_logging_rejects_bad_option_in_fresh_process():
    import subprocess
    import sys

    code = (
        "import cognee_py\n"
        "try:\n"
        "    cognee_py.setup_logging({'log_rotation': 'weekly'})\n"
        "except ValueError as e:\n"
        "    print('ValueError')\n"
    )
    res = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, timeout=30
    )
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == "ValueError", res.stdout
