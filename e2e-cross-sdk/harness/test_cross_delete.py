"""Cross-SDK delete tests: one SDK creates data, the other deletes it (Gap E2).

Since Python uses Kuzu/LanceDB and Rust uses Ladybug/Qdrant, cross-SDK
delete interop only works at the relational DB level.  We copy the SQLite
database from one workspace to another and verify deletion clears it.

The test in this file does *not* require an LLM (no cognify step) --- it
only exercises ``add`` + ``delete`` on the relational metadata layer.
"""

import shutil

from helpers import (
    open_db,
    query_data,
    query_datasets,
    query_dataset_data,
    python_db_path,
    rust_db_path,
    run_python_cli,
    run_rust_cli,
    write_rust_config,
    NLP_TEXT_FILE,
    DATASET_NAME,
)


def test_python_add_rust_delete_relational(tmp_path):
    """Python adds data, Rust deletes it via the shared relational DB.

    Steps:
      1. Python adds a text file (creates data + dataset rows in SQLite).
      2. Copy the Python SQLite DB into a Rust workspace.
      3. Configure Rust CLI to use the copied DB.
      4. Rust runs ``delete --all -f``.
      5. Assert the DB's data and dataset tables are empty.
    """
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()

    # ── Python: add ─────────────────────────────────────────────────────
    input_file = py_ws / "input.txt"
    input_file.write_text(NLP_TEXT_FILE.read_text())

    result = run_python_cli(
        py_ws, ["add", str(input_file), "-d", DATASET_NAME], check=False
    )
    assert result.returncode == 0, (
        f"Python add failed:\n{result.stdout}\n{result.stderr}"
    )

    # Verify Python actually wrote data
    py_db = python_db_path(py_ws)
    assert py_db.exists(), f"Python DB not found at {py_db}"
    conn = open_db(py_db)
    py_data = query_data(conn)
    py_datasets = query_datasets(conn)
    owner_id = str(py_datasets[0]["owner_id"])
    tenant_id = py_datasets[0].get("tenant_id")
    tenant_id_str = str(tenant_id) if tenant_id else None
    conn.close()

    assert len(py_data) >= 1, "Python produced no data rows"
    assert len(py_datasets) >= 1, "Python produced no dataset rows"

    # ── Copy Python DB to Rust workspace ────────────────────────────────
    rust_db = rust_ws / "cognee.db"
    shutil.copy2(str(py_db), str(rust_db))

    # Also copy file storage so Rust's delete can clean up stored files
    py_storage = py_ws / ".data_storage"
    rust_storage = rust_ws / ".data_storage"
    if py_storage.exists():
        shutil.copytree(str(py_storage), str(rust_storage))

    # Configure Rust to use the copied DB
    write_rust_config(
        rust_ws,
        user_id=owner_id,
        extra={"relational_db_url": f"sqlite:{rust_db}"},
    )

    # ── Rust: delete --all -f ───────────────────────────────────────────
    result = run_rust_cli(rust_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Rust delete on Python-created DB failed (exit {result.returncode}):\n"
        f"--- stdout ---\n{result.stdout}\n"
        f"--- stderr ---\n{result.stderr}"
    )

    # ── Assert the DB is empty ──────────────────────────────────────────
    conn = open_db(rust_db)
    data_rows = query_data(conn)
    dataset_rows = query_datasets(conn)
    junction_rows = query_dataset_data(conn)
    conn.close()

    assert len(data_rows) == 0, (
        f"Rust delete left {len(data_rows)} data row(s) in Python-created DB"
    )
    assert len(dataset_rows) == 0, (
        f"Rust delete left {len(dataset_rows)} dataset row(s) in Python-created DB"
    )
    assert len(junction_rows) == 0, (
        f"Rust delete left {len(junction_rows)} dataset_data row(s) in Python-created DB"
    )
