"""Shared pytest fixtures for cross-SDK E2E tests.

The core flow:
1. Run Python ``add`` first to bootstrap its DB and default user.
2. Extract owner_id and tenant_id from the Python SQLite database.
3. Configure the Rust CLI to use those same IDs.
4. Run Rust ``add`` with ``--tenant-id`` so UUID5 inputs match Python exactly.
"""

import os
import pytest
from pathlib import Path

from helpers import (
    run_python_cli,
    run_rust_cli,
    write_rust_config,
    open_db,
    query_datasets,
    python_db_path,
    rust_db_path,
    NLP_TEXT_FILE,
    DATASET_NAME,
)


# ── Markers ──────────────────────────────────────────────────────────────────

requires_openai = pytest.mark.skipif(
    not (os.environ.get("OPENAI_API_KEY") or os.environ.get("OPENAI_TOKEN")),
    reason="OPENAI_API_KEY/OPENAI_TOKEN not set — skipping LLM-dependent test",
)


# ── Fixtures: workspaces ─────────────────────────────────────────────────────


@pytest.fixture
def python_workspace(tmp_path):
    """Create an isolated workspace directory for the Python CLI."""
    ws = tmp_path / "python"
    ws.mkdir()
    return ws


@pytest.fixture
def rust_workspace(tmp_path):
    """Create an isolated workspace directory for the Rust CLI."""
    ws = tmp_path / "rust"
    ws.mkdir()
    return ws


# ── Fixtures: Python add (runs first to bootstrap default user) ──────────────


@pytest.fixture
def python_add_result(python_workspace):
    """Run Python ``cognee-cli add`` on the NLP text and return (workspace, owner_id, tenant_id).

    This fixture triggers default-user creation in the Python DB, which we
    then extract so the Rust CLI can be configured to match.
    """
    # Write the test text to a file in the workspace so both CLIs
    # can reference it by path (avoids shell argument length / quoting issues).
    text = NLP_TEXT_FILE.read_text()
    input_file = python_workspace / "input.txt"
    input_file.write_text(text)

    result = run_python_cli(
        python_workspace,
        ["add", str(input_file), "-d", DATASET_NAME],
        check=False,
    )
    assert result.returncode == 0, (
        f"Python add failed (exit {result.returncode}):\n"
        f"--- stdout ---\n{result.stdout}\n"
        f"--- stderr ---\n{result.stderr}"
    )

    # Extract owner_id and tenant_id from the dataset row
    db_path = python_db_path(python_workspace)
    assert db_path.exists(), f"Python SQLite DB not found at {db_path}"

    conn = open_db(db_path)
    datasets = query_datasets(conn)
    assert len(datasets) >= 1, f"Expected at least 1 dataset, got {len(datasets)}"

    ds = datasets[0]
    owner_id = ds["owner_id"]
    tenant_id = ds.get("tenant_id")
    conn.close()

    return python_workspace, str(owner_id), str(tenant_id) if tenant_id else None


@pytest.fixture
def synced_rust_workspace(rust_workspace, python_add_result):
    """Configure the Rust workspace with the same user/tenant IDs as Python.

    Returns (workspace, owner_id, tenant_id).
    """
    _, owner_id, tenant_id = python_add_result

    extra = {}
    # If tenant_id is present, we don't need extra config — it will be passed
    # via --tenant-id on the CLI.

    write_rust_config(rust_workspace, user_id=owner_id, extra=extra)

    return rust_workspace, owner_id, tenant_id


# ── Fixture: both SDKs have added the same text ─────────────────────────────


@pytest.fixture
def both_added(python_add_result, synced_rust_workspace):
    """Run add on the same NLP text in both SDKs.

    Returns (python_workspace, rust_workspace, owner_id, tenant_id).
    """
    py_ws, owner_id, tenant_id = python_add_result
    rust_ws, _, _ = synced_rust_workspace

    # Write input file for Rust (Python already added via python_add_result)
    input_file = rust_ws / "input.txt"
    input_file.write_text(NLP_TEXT_FILE.read_text())

    # Build Rust add command
    rust_args = ["add", str(input_file), "-d", DATASET_NAME]
    if tenant_id:
        rust_args.extend(["--tenant-id", tenant_id])

    result = run_rust_cli(rust_ws, rust_args, check=False)
    assert result.returncode == 0, (
        f"Rust add failed (exit {result.returncode}):\n"
        f"--- stdout ---\n{result.stdout}\n"
        f"--- stderr ---\n{result.stderr}"
    )

    return py_ws, rust_ws, owner_id, tenant_id
