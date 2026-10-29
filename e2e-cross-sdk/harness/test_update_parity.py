"""Cross-SDK parity for ``update()``: the document keeps its data id (P21).

Python's ``update()`` never deletes the document row. It resolves the id the
caller holds (``resolve_data_id``: the exact id, else the pre-fork
``legacy_id`` recorded on a row that the dataset-scoping migration forked),
drops the document's memory, re-adds **pinned** to the same id and
re-cognifies; an id that resolves to nothing is ``UpdateTargetNotFoundError``
(``cognee/api/v1/update/update.py:215-337``,
``cognee/modules/data/methods/resolve_data_id.py``,
``cognee/tasks/ingestion/ingest_data.py:377-387,517-581``). Rust used to add a
new row and soft-delete the old one (a new uuid4 each time); it now follows
Python (SDK-1039 P21).

* ``test_update_keeps_the_data_id`` -- add + cognify, ``update`` with new
  content: both SDKs list **one** row with the **same id**, the new content
  hash and the new stored content. Needs an LLM (update re-cognifies).
  Run once with the exact id and once with a legacy id.
* ``test_update_of_an_unknown_id_is_rejected`` -- an id that is neither a row
  id nor a legacy id fails in both, and nothing changes. No LLM.
* ``test_delete_by_a_legacy_id_removes_the_forked_row`` -- a legacy id also
  resolves for delete (``datasets.delete_data`` / ``forget --data-id``), and the
  row is gone in both. No LLM.

A legacy id is recorded the way the migration records it, by setting
``data.legacy_id`` on the row directly: producing a real pre-scoping database
needs the previous release's schema, and the migration itself is covered by
the Rust ``dataset_scoped_data_migration`` test.
"""

import hashlib
import uuid

import pytest
from conftest import requires_openai
from helpers import (
    DATASET_NAME,
    open_db,
    python_db_path,
    query_data,
    query_datasets,
    read_stored_file,
    run_python_script,
    run_rust_cli,
    rust_db_path,
)

NEW_TEXT = (
    "Dr. Ada Lovelace worked with Charles Babbage on the Analytical Engine in "
    "London. She wrote the first published algorithm intended for a machine."
)


def _only_row(ws, db_path):
    conn = open_db(db_path)
    try:
        rows = query_data(conn)
        datasets = query_datasets(conn)
    finally:
        conn.close()
    assert len(rows) == 1, f"expected one data row, got {len(rows)}"
    assert len(datasets) == 1, f"expected one dataset, got {len(datasets)}"
    return rows[0], datasets[0]


def _set_legacy_id(db_path, row_id, legacy_hex):
    """Record ``legacy_id`` on the row, as the dataset-scoping migration does."""
    conn = open_db(db_path)
    try:
        # Both SDKs store ids as 32 hex characters (query_rows normalises the
        # reads), so match on whichever spelling the column holds.
        cur = conn.execute(
            "UPDATE data SET legacy_id = ? WHERE replace(lower(id), '-', '') = ?",
            (legacy_hex, row_id),
        )
        assert cur.rowcount == 1, "the row to give a legacy id was not found"
        conn.commit()
    finally:
        conn.close()


def _python_update(py_ws, data_id, dataset_id, text):
    script = (
        "import asyncio, uuid\n"
        "async def main():\n"
        f"    result = await cognee.update(data_id=uuid.UUID({data_id!r}), "
        f"data={text!r}, dataset_id=uuid.UUID({dataset_id!r}))\n"
        "    print('RESULT', result)\n"
        "asyncio.run(main())\n"
    )
    return run_python_script(py_ws, script, check=False, timeout=600)


def _rust_update(rust_ws, data_id, text):
    return run_rust_cli(
        rust_ws,
        ["update", text, "--data-id", str(uuid.UUID(data_id)), "-d", DATASET_NAME],
        check=False,
    )


def _md5(text):
    return hashlib.md5(text.encode()).hexdigest()


@requires_openai
@pytest.mark.parametrize("by_legacy_id", [False, True], ids=["exact-id", "legacy-id"])
def test_update_keeps_the_data_id(both_cognified, by_legacy_id):
    py_ws, rust_ws = both_cognified
    py_db, rust_db = python_db_path(py_ws), rust_db_path(rust_ws)

    before = {}
    for sdk, ws, db in (("python", py_ws, py_db), ("rust", rust_ws, rust_db)):
        row, dataset = _only_row(ws, db)
        before[sdk] = (row, dataset)

    held = {}
    for sdk, db in (("python", py_db), ("rust", rust_db)):
        row, _ = before[sdk]
        if by_legacy_id:
            legacy = uuid.uuid4().hex
            _set_legacy_id(db, row["id"], legacy)
            held[sdk] = legacy
        else:
            held[sdk] = row["id"]

    result = _python_update(
        py_ws, held["python"], before["python"][1]["id"], NEW_TEXT
    )
    assert result.returncode == 0, (
        f"Python update failed:\n{result.stdout}\n{result.stderr}"
    )
    result = _rust_update(rust_ws, held["rust"], NEW_TEXT)
    assert result.returncode == 0, (
        f"Rust update failed:\n{result.stdout}\n{result.stderr}"
    )

    for sdk, ws, db in (("python", py_ws, py_db), ("rust", rust_ws, rust_db)):
        old_row, _ = before[sdk]
        row, _ = _only_row(ws, db)
        assert row["id"] == old_row["id"], (
            f"{sdk}: update changed the data id {old_row['id']} -> {row['id']}"
        )
        assert row["content_hash"] == _md5(NEW_TEXT), (
            f"{sdk}: the row does not hold the new content"
        )
        assert row["content_hash"] != old_row["content_hash"]
        assert row["owner_id"] == old_row["owner_id"]
        stored = read_stored_file(ws / ".data_storage", row["raw_data_location"])
        assert NEW_TEXT in stored.decode(), f"{sdk}: stored content is not the new text"
        if by_legacy_id:
            assert row.get("legacy_id") is not None, (
                f"{sdk}: the row lost its legacy id"
            )


def test_update_of_an_unknown_id_is_rejected(both_added):
    py_ws, rust_ws, _, _ = both_added
    py_db, rust_db = python_db_path(py_ws), rust_db_path(rust_ws)
    py_before, py_dataset = _only_row(py_ws, py_db)
    rust_before, _ = _only_row(rust_ws, rust_db)

    unknown = uuid.uuid4().hex
    result = _python_update(py_ws, unknown, py_dataset["id"], NEW_TEXT)
    assert result.returncode != 0, "Python update of an unknown id succeeded"
    assert "No document found to update" in (result.stdout + result.stderr), (
        f"unexpected Python failure:\n{result.stdout}\n{result.stderr}"
    )

    result = _rust_update(rust_ws, unknown, NEW_TEXT)
    assert result.returncode != 0, "Rust update of an unknown id succeeded"
    assert "No document found to update" in (result.stdout + result.stderr), (
        f"unexpected Rust failure:\n{result.stdout}\n{result.stderr}"
    )

    for sdk, ws, db, before in (
        ("python", py_ws, py_db, py_before),
        ("rust", rust_ws, rust_db, rust_before),
    ):
        row, _ = _only_row(ws, db)
        assert row["id"] == before["id"], f"{sdk}: a failed update changed the row"
        assert row["content_hash"] == before["content_hash"], (
            f"{sdk}: a failed update changed the content"
        )


def test_delete_by_a_legacy_id_removes_the_forked_row(both_added):
    py_ws, rust_ws, _, _ = both_added
    py_db, rust_db = python_db_path(py_ws), rust_db_path(rust_ws)
    py_row, py_dataset = _only_row(py_ws, py_db)
    rust_row, _ = _only_row(rust_ws, rust_db)

    legacy = uuid.uuid4()
    _set_legacy_id(py_db, py_row["id"], legacy.hex)
    _set_legacy_id(rust_db, rust_row["id"], legacy.hex)

    script = (
        "import asyncio, uuid\n"
        "from cognee.modules.users.methods import get_default_user\n"
        "async def main():\n"
        "    user = await get_default_user()\n"
        f"    receipt = await cognee.datasets.delete_data(dataset_id=uuid.UUID({py_dataset['id']!r}), "
        f"data_id=uuid.UUID({legacy.hex!r}), user=user)\n"
        "    print('RECEIPT', receipt)\n"
        "asyncio.run(main())\n"
    )
    result = run_python_script(py_ws, script, check=False)
    assert result.returncode == 0, (
        f"Python delete_data by a legacy id failed:\n{result.stdout}\n{result.stderr}"
    )
    assert str(uuid.UUID(py_row["id"])) in result.stdout, (
        "Python's receipt does not report the resolved data id:\n" + result.stdout
    )

    result = run_rust_cli(
        rust_ws,
        ["forget", "--data-id", str(legacy), "-d", DATASET_NAME],
        check=False,
    )
    assert result.returncode == 0, (
        f"Rust forget by a legacy id failed:\n{result.stdout}\n{result.stderr}"
    )

    for sdk, db in (("python", py_db), ("rust", rust_db)):
        conn = open_db(db)
        try:
            remaining = query_data(conn)
        finally:
            conn.close()
        assert remaining == [], (
            f"{sdk}: the row was not deleted by its legacy id ({len(remaining)} left)"
        )
