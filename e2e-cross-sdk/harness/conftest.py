"""Shared pytest fixtures for cross-SDK E2E tests.

The core flow:
1. Run Python ``add`` first to bootstrap its DB and default user.
2. Extract owner_id and tenant_id from the Python SQLite database.
3. Configure the Rust CLI to use those same IDs.
4. Run Rust ``add`` with ``--tenant-id`` so UUID5 inputs match Python exactly.

HTTP fixture hygiene notes
--------------------------
The ``/py`` and ``/rs`` tmpfs workspaces are wiped per ``docker compose run``
invocation but NOT between tests within a single run.  Tests must not rely on
a clean DB between test functions — use ``unique_dataset_name`` to avoid
cross-test contamination, and add explicit teardown when the test creates
persistent state (API keys, named datasets, etc.).

The Python-side DB migrations are run once at container start (via
``start_servers.sh``).  The Rust server runs its own migrations on first boot.
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


# ── Fixture: both SDKs have added + cognified the same text ─────────────────


@pytest.fixture
def both_cognified(tmp_path):
    """Run add + cognify on the same text in both SDKs.

    Requires an OpenAI API key (cognify invokes the LLM).  Returns
    ``(python_workspace, rust_workspace)``.
    """
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()

    # Write input file for both SDKs
    input_py = py_ws / "input.txt"
    input_py.write_text(NLP_TEXT_FILE.read_text())
    input_rust = rust_ws / "input.txt"
    input_rust.write_text(NLP_TEXT_FILE.read_text())

    # ── Python: add + cognify ────────────────────────────────────────────
    result = run_python_cli(py_ws, ["add", str(input_py), "-d", DATASET_NAME], check=False)
    assert result.returncode == 0, f"Python add failed:\n{result.stdout}\n{result.stderr}"

    # Extract user/tenant for Rust
    py_db = python_db_path(py_ws)
    conn = open_db(py_db)
    ds = query_datasets(conn)
    owner_id = str(ds[0]["owner_id"])
    tenant_id = ds[0].get("tenant_id")
    tenant_id_str = str(tenant_id) if tenant_id else None
    conn.close()

    result = run_python_cli(py_ws, ["cognify", "-d", DATASET_NAME], check=False)
    assert result.returncode == 0, f"Python cognify failed:\n{result.stdout}\n{result.stderr}"

    # ── Rust: add + cognify ──────────────────────────────────────────────
    write_rust_config(rust_ws, user_id=owner_id)

    rust_add_args = ["add", str(input_rust), "-d", DATASET_NAME]
    if tenant_id_str:
        rust_add_args.extend(["--tenant-id", tenant_id_str])

    result = run_rust_cli(rust_ws, rust_add_args, check=False)
    assert result.returncode == 0, f"Rust add failed:\n{result.stdout}\n{result.stderr}"

    result = run_rust_cli(rust_ws, ["cognify", "-d", DATASET_NAME], check=False)
    assert result.returncode == 0, f"Rust cognify failed:\n{result.stdout}\n{result.stderr}"

    return py_ws, rust_ws


# ─────────────────────────────────────────────────────────────────────────────
# HTTP parity fixtures
# ─────────────────────────────────────────────────────────────────────────────
# These fixtures are used exclusively by test_http_*.py files and drive two
# live HTTP servers (Python uvicorn on :8000, Rust cognee-http-server on :8001)
# that are started by the e2e-http-tests Compose service's entrypoint.
# ─────────────────────────────────────────────────────────────────────────────

import httpx
import uuid as _uuid

PY_BASE = "http://127.0.0.1:8000"
RS_BASE = "http://127.0.0.1:8001"


@pytest.fixture
def py_client():
    """httpx.Client pre-configured for the Python uvicorn server."""
    with httpx.Client(base_url=PY_BASE, timeout=60.0) as c:
        yield c


@pytest.fixture
def rs_client():
    """httpx.Client pre-configured for the Rust cognee-http-server."""
    with httpx.Client(base_url=RS_BASE, timeout=60.0) as c:
        yield c


@pytest.fixture
def both_clients(py_client, rs_client):
    """Dict with both clients keyed by 'py' and 'rs'."""
    return {"py": py_client, "rs": rs_client}


@pytest.fixture
def authed_clients(both_clients):
    """Register + login on both servers; return clients with auth cookies/headers set.

    Register uses JSON with ``email``; login uses OAuth2 form with ``username``
    (matching e2e-parity.md §4 and FastAPI-users behaviour).
    """
    creds = {"username": "test@example.com", "password": "test_password_123"}
    for name, c in both_clients.items():
        # Bootstrap user — ignore 409 / 422 "already exists" on re-runs.
        c.post(
            "/api/v1/auth/register",
            json={
                "email": creds["username"],
                "password": creds["password"],
                "is_verified": True,
            },
        )
        r = c.post("/api/v1/auth/login", data=creds)
        assert r.status_code == 200, f"{name} login failed: {r.text}"
    return both_clients


# ── Data-hygiene fixtures ──────────────────────────────────────────────────────


@pytest.fixture
def unique_dataset_name(request):
    """Return a function-scoped unique dataset name to avoid cross-test contamination."""
    return f"test_{request.node.name}_{_uuid.uuid4().hex[:8]}"


@pytest.fixture
def cleanup_api_keys(both_clients):
    """Collect issued API-key IDs and DELETE them at teardown.

    Usage::

        def test_create_key(authed_clients, cleanup_api_keys):
            r = authed_clients["py"].post("/api/v1/api-keys", json={"name": "k"})
            cleanup_api_keys["py"].append(r.json()["id"])
    """
    issued: dict = {"py": [], "rs": []}
    yield issued
    for side, ids in issued.items():
        c = both_clients[side]
        for key_id in ids:
            c.delete(f"/api/v1/api-keys/{key_id}")
