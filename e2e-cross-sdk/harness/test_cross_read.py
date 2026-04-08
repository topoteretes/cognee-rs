"""Cross-read tests: one SDK reads data produced by the other.

These verify schema-level compatibility — that the SQLite database written
by one SDK can be opened, queried, and extended by the other without errors.
"""

import shutil
import pytest

from helpers import (
    open_db,
    query_data,
    query_datasets,
    query_nodes,
    query_edges,
    python_db_path,
    rust_db_path,
    run_python_cli,
    run_rust_cli,
    write_rust_config,
    NLP_TEXT_FILE,
    QC_TEXT_FILE,
    DATASET_NAME,
)
from conftest import requires_openai


def _write_input(ws, filename, source_file):
    """Write test text to a file in the workspace, return its path."""
    p = ws / filename
    p.write_text(source_file.read_text())
    return p


# ── Rust reads Python's DB ───────────────────────────────────────────────────


def test_rust_reads_python_add_output(tmp_path):
    """Rust can open a Python-created SQLite DB and add new data to it."""
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()

    input1 = _write_input(py_ws, "input1.txt", NLP_TEXT_FILE)
    input2 = _write_input(rust_ws, "input2.txt", QC_TEXT_FILE)

    # Python: add
    result = run_python_cli(py_ws, ["add", str(input1), "-d", DATASET_NAME], check=False)
    assert result.returncode == 0, f"Python add failed:\n{result.stdout}\n{result.stderr}"

    # Extract user info
    py_db = python_db_path(py_ws)
    conn = open_db(py_db)
    ds = query_datasets(conn)
    owner_id = str(ds[0]["owner_id"])
    tenant_id = ds[0].get("tenant_id")
    tenant_id_str = str(tenant_id) if tenant_id else None
    conn.close()

    # Copy Python's DB to Rust workspace
    rust_db = rust_ws / "cognee.db"
    shutil.copy2(str(py_db), str(rust_db))

    # Configure Rust to use the copied DB
    write_rust_config(
        rust_ws,
        user_id=owner_id,
        extra={"relational_db_url": f"sqlite:{rust_db}"},
    )

    # Rust: add a different file to the Python-created DB
    rust_args = ["add", str(input2), "-d", DATASET_NAME]
    if tenant_id_str:
        rust_args.extend(["--tenant-id", tenant_id_str])

    result = run_rust_cli(rust_ws, rust_args, check=False)
    assert result.returncode == 0, (
        f"Rust failed to add to Python-created DB:\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )

    # Verify both data rows exist
    conn = open_db(rust_db)
    data = query_data(conn)
    conn.close()

    assert len(data) == 2, (
        f"Expected 2 data rows in cross-read DB, got {len(data)}.\n"
        f"Rows: {[d['name'] for d in data]}"
    )


# ── Python reads Rust's DB ───────────────────────────────────────────────────


def test_python_reads_rust_add_output(tmp_path):
    """Verify that the Rust SQLite schema is compatible with Python's expectations.

    Steps:
    1. Run Python add first (bootstraps auth tables, gives us a user ID).
    2. Run Rust add on different text.
    3. Verify the Rust DB has the columns Python expects.
    """
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()

    input1 = _write_input(py_ws, "input1.txt", NLP_TEXT_FILE)
    input2 = _write_input(rust_ws, "input2.txt", QC_TEXT_FILE)

    # Bootstrap Python DB (creates auth tables)
    result = run_python_cli(py_ws, ["add", str(input1), "-d", "bootstrap"], check=False)
    assert result.returncode == 0, f"Python bootstrap add failed:\n{result.stdout}\n{result.stderr}"

    # Extract user info
    py_db = python_db_path(py_ws)
    conn = open_db(py_db)
    ds = query_datasets(conn)
    owner_id = str(ds[0]["owner_id"])
    tenant_id = ds[0].get("tenant_id")
    tenant_id_str = str(tenant_id) if tenant_id else None
    conn.close()

    # Rust: add to its own DB
    write_rust_config(rust_ws, user_id=owner_id)
    rust_args = ["add", str(input2), "-d", DATASET_NAME]
    if tenant_id_str:
        rust_args.extend(["--tenant-id", tenant_id_str])
    result = run_rust_cli(rust_ws, rust_args, check=False)
    assert result.returncode == 0, f"Rust add failed:\n{result.stdout}\n{result.stderr}"

    # Verify the Rust DB has the columns Python expects
    rust_db = rust_db_path(rust_ws)
    conn = open_db(rust_db)
    rust_data = query_data(conn)
    rust_datasets = query_datasets(conn)
    conn.close()

    assert len(rust_data) >= 1, "Rust DB has no data rows"
    assert len(rust_datasets) >= 1, "Rust DB has no dataset rows"

    required_columns = {"id", "name", "content_hash", "mime_type", "extension", "owner_id"}
    actual_columns = set(rust_data[0].keys())
    missing = required_columns - actual_columns
    assert not missing, (
        f"Rust DB missing columns expected by Python: {missing}\n"
        f"Available: {sorted(actual_columns)}"
    )


# ── Python adds, Rust cognifies ──────────────────────────────────────────────


@requires_openai
def test_python_adds_rust_cognifies(tmp_path):
    """Rust cognify can process data ingested by Python."""
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()

    input_file = _write_input(py_ws, "input.txt", NLP_TEXT_FILE)

    # Python: add
    result = run_python_cli(py_ws, ["add", str(input_file), "-d", DATASET_NAME], check=False)
    assert result.returncode == 0, f"Python add failed:\n{result.stdout}\n{result.stderr}"

    # Extract user info
    py_db = python_db_path(py_ws)
    conn = open_db(py_db)
    ds = query_datasets(conn)
    owner_id = str(ds[0]["owner_id"])
    tenant_id = ds[0].get("tenant_id")
    tenant_id_str = str(tenant_id) if tenant_id else None
    conn.close()

    # Copy Python's DB and file storage to Rust workspace
    rust_db = rust_ws / "cognee.db"
    shutil.copy2(str(py_db), str(rust_db))

    py_storage = py_ws / ".data_storage"
    rust_storage = rust_ws / ".data_storage"
    if py_storage.exists():
        shutil.copytree(str(py_storage), str(rust_storage))

    # Configure Rust
    write_rust_config(
        rust_ws,
        user_id=owner_id,
        extra={"relational_db_url": f"sqlite:{rust_db}"},
    )

    # Rust: cognify the Python-ingested data
    result = run_rust_cli(rust_ws, ["cognify", "-d", DATASET_NAME], check=False)
    assert result.returncode == 0, (
        f"Rust cognify on Python data failed:\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )

    # Verify nodes and edges were created
    conn = open_db(rust_db)
    nodes = query_nodes(conn)
    edges = query_edges(conn)
    conn.close()

    assert len(nodes) > 0, "Rust cognify produced zero nodes from Python data"
    assert len(edges) > 0, "Rust cognify produced zero edges from Python data"
