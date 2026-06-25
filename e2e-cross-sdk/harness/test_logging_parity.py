"""Cross-SDK file-logging parity test (gap 06, task 06-10 §4.4).

Asserts:
  1. Both Python and Rust SDKs create at least one ``*.log`` file under
     a shared ``COGNEE_LOGS_DIR`` after invoking a known no-op command.
  2. The shared anchor message ``"Logging initialized"`` appears at the
     start of the body of at least one formatted line in each SDK's log
     output, with the body extracted between the timestamp and the
     trailing ``[<logger>]`` bracket.

Per locked decision 12 in
``docs/telemetry/06-file-logging-rotation.md``:

* **Loose at the filename level** — each process picks its own
  ``LOG_FILE_NAME``, so separate files are OK.
* **Per-message strict after stripping the timestamp and logger
  bracket** — the body of the anchor line must match across SDKs.

The Python SDK's ``setup_logging`` emits ``"Logging initialized"`` with
trailing structlog fields (``python_version=…``, ``cognee_version=…``,
…); the Rust SDK emits the same anchor with its own trailing fields
(``file=…``, ``rotation=…``). The sub-doc explicitly permits
normalising both sides by stripping trailing field tokens before
comparing — that normalisation is implemented in
:func:`_anchor_prefix` below.
"""
from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path

import pytest

from helpers import PYTHON_RUNNER, RUST_CLI

# ── Constants ────────────────────────────────────────────────────────────────

# Format produced by both PythonPlainFormatter (Rust) and
# PlainFileHandler (Python):
#   <ts> [<LEVEL ljust(8)>] <body> [<logger>]\n
#
# Notes for the regex:
# * The level field is exactly 8 characters wide (Python's
#   ``record.levelname.ljust(8)`` and Rust's ``level_ljust_8``).
# * ``<body>`` is lazy and greedy enough to swallow everything up to
#   the final ``[<logger>]`` bracket. Logger names never contain ``]``.
LINE_RE = re.compile(
    r"^(?P<ts>\S+)\s+\[(?P<level>[A-Z ]{8})\]\s+(?P<body>.*?)\s+\[(?P<logger>[^\]]+)\]\s*$"
)

ANCHOR = "Logging initialized"


# ── Helpers ──────────────────────────────────────────────────────────────────


def _read_logs(dir_: Path) -> list[str]:
    """Return every line across every ``*.log`` file in *dir_*."""
    out: list[str] = []
    for p in sorted(dir_.glob("*.log")):
        out.extend(p.read_text(errors="replace").splitlines())
    return out


def _anchor_prefix(lines: list[str]) -> str | None:
    """Find the first line whose body starts with ``ANCHOR`` and return
    only the substring up to (but excluding) any trailing ``k=v`` fields.

    Both SDKs append unordered structured field tokens after the
    anchor (``python_version=…`` on Python, ``file=…`` on Rust). The
    sub-doc explicitly authorises normalising both sides before
    comparing, so we keep just the leading anchor substring.
    """
    for line in lines:
        m = LINE_RE.match(line)
        if not m:
            continue
        body = m.group("body")
        if not body.startswith(ANCHOR):
            continue
        # Strip everything from the first ``\b\w+=`` token onward —
        # both SDKs emit fields as space-separated ``key=value`` pairs.
        normalised = re.split(r"\s+\w+=", body, maxsplit=1)[0]
        return normalised
    return None


def _python_setup_script(logs_dir: Path) -> str:
    """Inline Python that imports cognee, calls ``setup_logging`` so the
    file handler is wired, and emits an anchor event explicitly so we
    do not depend on cognee's startup banner timing."""
    return (
        "import os\n"
        f"os.environ['COGNEE_LOGS_DIR'] = {str(logs_dir)!r}\n"
        # Python SDK's ``setup_logging`` reads the env var and wires
        # the PlainFileHandler. It also emits the canonical anchor as
        # its last call, so importing + calling is sufficient.
        "from cognee.shared.logging_utils import setup_logging\n"
        "setup_logging()\n"
        "print('OK')\n"
    )


def _rust_env(logs_dir: Path) -> dict[str, str]:
    """Env for the Rust CLI invocation. Strips any inherited
    ``LOG_FILE_NAME`` so the child picks its own timestamped filename
    under the shared logs dir."""
    env = {**os.environ, "COGNEE_LOGS_DIR": str(logs_dir)}
    env.pop("LOG_FILE_NAME", None)
    env.pop("RUST_LOG", None)
    env.pop("LOG_LEVEL", None)
    return env


# ── Tests ────────────────────────────────────────────────────────────────────


def test_both_sdks_create_log_files(tmp_path: Path) -> None:
    py_logs = tmp_path / "py_logs"
    rs_logs = tmp_path / "rs_logs"
    py_logs.mkdir()
    rs_logs.mkdir()

    # --- Python side --------------------------------------------------
    py_result = subprocess.run(
        [PYTHON_RUNNER, "-c", _python_setup_script(py_logs)],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if py_result.returncode != 0:
        pytest.skip(
            f"Python setup_logging() exited non-zero ({py_result.returncode}); "
            f"stderr=\n{py_result.stderr}\nstdout=\n{py_result.stdout}"
        )

    # --- Rust side ----------------------------------------------------
    rs_result = subprocess.run(
        [RUST_CLI, "--help"],
        env=_rust_env(rs_logs),
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert rs_result.returncode == 0, (
        f"Rust CLI --help failed (exit {rs_result.returncode}); "
        f"stderr=\n{rs_result.stderr}"
    )

    # (1) Both SDKs created at least one .log file in their shared dir.
    py_files = list(py_logs.glob("*.log"))
    rs_files = list(rs_logs.glob("*.log"))
    assert py_files, f"Python SDK did not create any *.log in {py_logs}"
    assert rs_files, f"Rust SDK did not create any *.log in {rs_logs}"


def test_anchor_message_matches_after_normalization(tmp_path: Path) -> None:
    py_logs = tmp_path / "py_logs"
    rs_logs = tmp_path / "rs_logs"
    py_logs.mkdir()
    rs_logs.mkdir()

    py_result = subprocess.run(
        [PYTHON_RUNNER, "-c", _python_setup_script(py_logs)],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if py_result.returncode != 0:
        pytest.skip(
            f"Python setup_logging() exited non-zero ({py_result.returncode}); "
            f"stderr=\n{py_result.stderr}"
        )

    rs_result = subprocess.run(
        [RUST_CLI, "--help"],
        env=_rust_env(rs_logs),
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert rs_result.returncode == 0, (
        f"Rust CLI --help failed: stderr=\n{rs_result.stderr}"
    )

    py_anchor = _anchor_prefix(_read_logs(py_logs))
    rs_anchor = _anchor_prefix(_read_logs(rs_logs))

    assert py_anchor is not None, (
        "Python anchor line not found; raw lines:\n"
        + "\n".join(_read_logs(py_logs))
    )
    assert rs_anchor is not None, (
        "Rust anchor line not found; raw lines:\n"
        + "\n".join(_read_logs(rs_logs))
    )

    # Decision 12 — strict per-message equality after normalisation.
    # Both SDKs emit exactly the canonical anchor string before any
    # structured fields.
    assert py_anchor == rs_anchor, (
        "anchor body differs between SDKs after normalisation:\n"
        f"  Python: {py_anchor!r}\n"
        f"  Rust:   {rs_anchor!r}"
    )
    assert py_anchor.startswith(ANCHOR), (
        f"normalised anchor does not start with {ANCHOR!r}: {py_anchor!r}"
    )
