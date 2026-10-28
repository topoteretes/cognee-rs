"""Re-add after delete tests (Gap E5).

Data ids are a random ``uuid4`` in both SDKs: Python's ``ingest_data`` mints
``uuid4()`` for a new row and dedups by ``(content_hash, dataset_id, owner_id,
tenant_id)`` via ``identify_many``
(``cognee/tasks/ingestion/ingest_data.py:364-389``). Once delete has removed
the row there is nothing to dedup against, so a re-add creates a **new** row
with a **new** id. These tests therefore check that both SDKs behave that way
— exactly one row, the same content hash, a fresh uuid4 — instead of the old
premise that the id survives (SDK-1039 D7).

Tests include both same-SDK and cross-SDK re-add scenarios.  The cross-SDK
test copies the relational DB from Python to Rust, deletes via Rust, then
Python re-adds into the database Rust cleaned.

These tests do **not** require an LLM --- only ``add`` + ``delete`` are
used (no cognify step).
"""

import shutil
import uuid

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


def _assert_fresh_readd(sdk, before, after):
    """``after`` is one re-added row: same content, a new uuid4 id."""
    assert len(after) == 1, f"Expected 1 {sdk} data row after re-add, got {len(after)}"
    assert after[0]["content_hash"] == before["content_hash"], (
        f"{sdk}: re-added row holds different content"
    )
    assert uuid.UUID(str(after[0]["id"])).version == 4, (
        f"{sdk}: re-added data id {after[0]['id']} is not a uuid4"
    )
    assert after[0]["id"] != before["id"], (
        f"{sdk}: re-add after delete reused the deleted row's id {before['id']}; "
        f"Python mints a fresh uuid4 (ingest_data.py:364-389)"
    )


def test_readd_after_own_delete_mints_a_fresh_data_id(tmp_path):
    """Each SDK re-adds the same content after delete and gets a new row.

    Flow per SDK:
      1. Add text -> record the row.
      2. Delete --all -f.
      3. Re-add the same text.
      4. Assert one row with the same content and a fresh uuid4 id, as Python
         does (SDK-1039 D7: the old test asserted the id survived).
    """
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()

    input_py = py_ws / "input.txt"
    input_py.write_text(NLP_TEXT_FILE.read_text())
    input_rust = rust_ws / "input.txt"
    input_rust.write_text(NLP_TEXT_FILE.read_text())

    # ── Python: add -> delete -> re-add -> check ────────────────────────
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
    _assert_fresh_readd("Python", py_data_before[0], py_data_after)

    # ── Rust: add -> delete -> re-add -> check ──────────────────────────
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
    _assert_fresh_readd("Rust", rust_data_before[0], rust_data_after)


def test_readd_after_cross_sdk_delete(tmp_path):
    """Python adds, Rust deletes (via shared DB), Python re-adds into it.

    Steps:
      1. Python adds text -> record the row.
      2. Copy Python DB to Rust workspace.
      3. Rust deletes --all -f on the copied DB (it must find Python's rows
         through ``data.dataset_id``).
      4. Copy the cleaned DB back to a new Python workspace.
      5. Python re-adds the same text.
      6. Assert one row with the same content and a fresh uuid4 id: Python
         mints a new id once the row is gone (SDK-1039 D7; the old test
         asserted the id survived).
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

    # ── Step 2: Copy DB to Rust ──────────────────────────────────────────
    rust_db = rust_ws / "cognee.db"
    shutil.copy2(str(py_db), str(rust_db))

    write_rust_config(
        rust_ws,
        user_id=owner_id,
        extra={"relational_db_url": f"sqlite:{rust_db}"},
    )

    # ── Step 3: Rust deletes ─────────────────────────────────────────────
    # `delete --all` deletes the acting user's datasets in its tenant (Python's
    # `datasets.delete_all`), so pass the tenant the rows were written with.
    delete_args = ["delete", "--all", "-f"]
    if tenant_id_str:
        delete_args.extend(["--tenant-id", tenant_id_str])
    result = run_rust_cli(rust_ws, delete_args, check=False)
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

    # ── Step 6: Assert one fresh row ─────────────────────────────────────
    py_db2 = python_db_path(py_ws2)
    conn = open_db(py_db2)
    readd_data = query_data(conn)
    conn.close()

    _assert_fresh_readd("Python (after Rust delete)", original_data[0], readd_data)
