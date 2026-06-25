"""Re-add after delete tests: UUID5 data_id preservation (Gap E5).

After deleting and re-adding the same content, the deterministic UUID5
data_id must match the original (content-addressed deduplication).

Tests include both same-SDK and cross-SDK re-add scenarios.  The cross-SDK
test copies the relational DB from Python to Rust, deletes via Rust, then
Python re-adds --- the data_id should still match.

These tests do **not** require an LLM --- only ``add`` + ``delete`` are
used (no cognify step).
"""

import shutil

from helpers import (
    open_db,
    query_data,
    query_datasets,
    python_db_path,
    rust_db_path,
    run_python_cli,
    run_rust_cli,
    write_rust_config,
    NLP_TEXT_FILE,
    DATASET_NAME,
)


def test_readd_after_own_delete_preserves_data_id(tmp_path):
    """Each SDK re-adds the same content after delete and gets the same data_id.

    Flow per SDK:
      1. Add text -> record data_id.
      2. Delete --all -f.
      3. Re-add the same text.
      4. Assert data_id matches the original.
    """
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()

    input_py = py_ws / "input.txt"
    input_py.write_text(NLP_TEXT_FILE.read_text())
    input_rust = rust_ws / "input.txt"
    input_rust.write_text(NLP_TEXT_FILE.read_text())

    # ── Python: add -> record ID -> delete -> re-add -> compare ─────────
    result = run_python_cli(
        py_ws, ["add", str(input_py), "-d", DATASET_NAME], check=False
    )
    assert result.returncode == 0, (
        f"Python initial add failed:\n{result.stdout}\n{result.stderr}"
    )

    py_db = python_db_path(py_ws)
    conn = open_db(py_db)
    py_data_before = query_data(conn)
    conn.close()
    assert len(py_data_before) == 1, (
        f"Expected 1 Python data row, got {len(py_data_before)}"
    )
    py_original_id = py_data_before[0]["id"]

    result = run_python_cli(py_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Python delete failed:\n{result.stdout}\n{result.stderr}"
    )

    # Verify data is gone
    conn = open_db(py_db)
    assert len(query_data(conn)) == 0, "Python data not deleted"
    conn.close()

    result = run_python_cli(
        py_ws, ["add", str(input_py), "-d", DATASET_NAME], check=False
    )
    assert result.returncode == 0, (
        f"Python re-add failed:\n{result.stdout}\n{result.stderr}"
    )

    conn = open_db(py_db)
    py_data_after = query_data(conn)
    conn.close()
    assert len(py_data_after) == 1, (
        f"Expected 1 Python data row after re-add, got {len(py_data_after)}"
    )
    py_readd_id = py_data_after[0]["id"]

    assert py_original_id == py_readd_id, (
        f"Python data_id changed after delete+re-add:\n"
        f"  Original: {py_original_id}\n"
        f"  Re-added: {py_readd_id}"
    )

    # ── Rust: add -> record ID -> delete -> re-add -> compare ───────────
    write_rust_config(rust_ws)

    result = run_rust_cli(
        rust_ws, ["add", str(input_rust), "-d", DATASET_NAME], check=False
    )
    assert result.returncode == 0, (
        f"Rust initial add failed:\n{result.stdout}\n{result.stderr}"
    )

    rust_db = rust_db_path(rust_ws)
    conn = open_db(rust_db)
    rust_data_before = query_data(conn)
    conn.close()
    assert len(rust_data_before) == 1, (
        f"Expected 1 Rust data row, got {len(rust_data_before)}"
    )
    rust_original_id = rust_data_before[0]["id"]

    result = run_rust_cli(
        rust_ws, ["delete", "--all", "-f"], check=False
    )
    assert result.returncode == 0, (
        f"Rust delete failed:\n{result.stdout}\n{result.stderr}"
    )

    conn = open_db(rust_db)
    assert len(query_data(conn)) == 0, "Rust data not deleted"
    conn.close()

    result = run_rust_cli(
        rust_ws, ["add", str(input_rust), "-d", DATASET_NAME], check=False
    )
    assert result.returncode == 0, (
        f"Rust re-add failed:\n{result.stdout}\n{result.stderr}"
    )

    conn = open_db(rust_db)
    rust_data_after = query_data(conn)
    conn.close()
    assert len(rust_data_after) == 1, (
        f"Expected 1 Rust data row after re-add, got {len(rust_data_after)}"
    )
    rust_readd_id = rust_data_after[0]["id"]

    assert rust_original_id == rust_readd_id, (
        f"Rust data_id changed after delete+re-add:\n"
        f"  Original: {rust_original_id}\n"
        f"  Re-added: {rust_readd_id}"
    )


def test_readd_after_cross_sdk_delete(tmp_path):
    """Python adds, Rust deletes (via shared DB), Python re-adds --- data_id preserved.

    Steps:
      1. Python adds text -> record data_id.
      2. Copy Python DB to Rust workspace.
      3. Rust deletes --all -f on the copied DB.
      4. Copy the cleaned DB back to a new Python workspace.
      5. Python re-adds the same text.
      6. Assert data_id matches the original.
    """
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()
    py_ws2 = tmp_path / "python_readd"
    py_ws2.mkdir()

    input_file = py_ws / "input.txt"
    input_file.write_text(NLP_TEXT_FILE.read_text())

    # ── Step 1: Python adds ──────────────────────────────────────────────
    result = run_python_cli(
        py_ws, ["add", str(input_file), "-d", DATASET_NAME], check=False
    )
    assert result.returncode == 0, (
        f"Python add failed:\n{result.stdout}\n{result.stderr}"
    )

    py_db = python_db_path(py_ws)
    conn = open_db(py_db)
    py_datasets = query_datasets(conn)
    original_data = query_data(conn)
    owner_id = str(py_datasets[0]["owner_id"])
    tenant_id = py_datasets[0].get("tenant_id")
    tenant_id_str = str(tenant_id) if tenant_id else None
    conn.close()

    assert len(original_data) == 1
    original_data_id = original_data[0]["id"]

    # ── Step 2: Copy DB to Rust ──────────────────────────────────────────
    rust_db = rust_ws / "cognee.db"
    shutil.copy2(str(py_db), str(rust_db))

    write_rust_config(
        rust_ws,
        user_id=owner_id,
        extra={"relational_db_url": f"sqlite:{rust_db}"},
    )

    # ── Step 3: Rust deletes ─────────────────────────────────────────────
    result = run_rust_cli(rust_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Rust delete failed:\n{result.stdout}\n{result.stderr}"
    )

    conn = open_db(rust_db)
    assert len(query_data(conn)) == 0, "Rust did not delete data"
    conn.close()

    # ── Step 4: Copy cleaned DB to new Python workspace ──────────────────
    # Place it where Python expects it: .cognee_system/databases/cognee_db
    py_sys2 = py_ws2 / ".cognee_system" / "databases"
    py_sys2.mkdir(parents=True, exist_ok=True)
    shutil.copy2(str(rust_db), str(py_sys2 / "cognee_db"))

    # ── Step 5: Python re-adds ───────────────────────────────────────────
    input_file2 = py_ws2 / "input.txt"
    input_file2.write_text(NLP_TEXT_FILE.read_text())

    result = run_python_cli(
        py_ws2, ["add", str(input_file2), "-d", DATASET_NAME], check=False
    )
    assert result.returncode == 0, (
        f"Python re-add failed:\n{result.stdout}\n{result.stderr}"
    )

    # ── Step 6: Assert data_id preserved ─────────────────────────────────
    py_db2 = python_db_path(py_ws2)
    conn = open_db(py_db2)
    readd_data = query_data(conn)
    conn.close()

    assert len(readd_data) >= 1, (
        f"Python re-add produced no data rows"
    )
    readd_data_id = readd_data[0]["id"]

    assert original_data_id == readd_data_id, (
        f"Data ID changed after cross-SDK delete+re-add:\n"
        f"  Original (Python add):   {original_data_id}\n"
        f"  Re-added (Python re-add after Rust delete): {readd_data_id}"
    )
