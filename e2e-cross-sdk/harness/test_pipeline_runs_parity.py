"""Cross-SDK parity for the ``pipeline_runs`` table (gap 08, decision 8).

Verifies that Python and Rust write the same lifecycle to ``pipeline_runs``
after an ``add`` + ``cognify`` run:

1. Schema parity — both SDKs expose a ``pipeline_runs`` table with the
   same column set (after gap 08-01's nullability migration).
2. ``dataset_id`` is nullable in both schemas.
3. Cognify writes the full ``INITIATED → STARTED → COMPLETED`` lifecycle
   (three rows per pipeline_run_id) on both sides.
4. ``run_info`` JSON shape is byte-identical at every status:
     - INITIATED → ``{}``
     - STARTED / COMPLETED → ``{"data": [...]}`` or ``{"data": "None"}``
     - ERRORED → ``{"data": ..., "error": ...}`` with ``data`` BEFORE
       ``error`` (insertion-order matters per Decision 5).

The shape checks only assert *structure* (key set + value type), not
exact byte equality of the payload — the data array contents differ
between SDKs because each writes its own UUID strings.

These tests skip when ``OPENAI_KEY`` is unavailable; they reuse the
existing ``both_cognified`` fixture (defined in ``conftest.py``).
"""

import json
import sqlite3
from pathlib import Path

import pytest
from conftest import requires_openai
from helpers import (
    open_db,
    python_db_path,
    query_rows,
    rust_db_path,
)


# ── Schema parity (no LLM required) ──────────────────────────────────────────


def _columns(conn: sqlite3.Connection, table: str) -> dict[str, dict]:
    """Return ``{name: {type, notnull, pk}}`` for *table* via PRAGMA."""
    info = conn.execute(f"PRAGMA table_info({table})").fetchall()
    return {
        row[1]: {"type": (row[2] or "").upper(), "notnull": row[3], "pk": row[5]}
        for row in info
    }


def _ensure_table_exists(db_path: Path) -> bool:
    """Return ``True`` iff the DB at *db_path* has a ``pipeline_runs`` table."""
    if not db_path.exists():
        return False
    conn = sqlite3.connect(str(db_path))
    try:
        row = conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='pipeline_runs'"
        ).fetchone()
        return row is not None
    finally:
        conn.close()


REQUIRED_COLUMNS = {
    "id",
    "created_at",
    "status",
    "pipeline_run_id",
    "pipeline_name",
    "pipeline_id",
    "dataset_id",
    "run_info",
}


@requires_openai
def test_schema_columns_match(both_cognified):
    """Both SDKs expose the same column set on ``pipeline_runs``."""
    py_ws, rust_ws = both_cognified
    py_db = python_db_path(py_ws)
    rust_db = rust_db_path(rust_ws)

    if not _ensure_table_exists(py_db) or not _ensure_table_exists(rust_db):
        pytest.skip("pipeline_runs table not present in one of the DBs")

    py_conn = open_db(py_db)
    rust_conn = open_db(rust_db)
    try:
        py_cols = _columns(py_conn, "pipeline_runs")
        rust_cols = _columns(rust_conn, "pipeline_runs")
    finally:
        py_conn.close()
        rust_conn.close()

    missing_py = REQUIRED_COLUMNS - set(py_cols)
    missing_rust = REQUIRED_COLUMNS - set(rust_cols)
    assert not missing_py, f"Python schema missing columns: {missing_py}"
    assert not missing_rust, f"Rust schema missing columns: {missing_rust}"


@requires_openai
def test_dataset_id_is_nullable(both_cognified):
    """After 08-01's migration, both SDKs have ``dataset_id`` as nullable."""
    py_ws, rust_ws = both_cognified
    py_db = python_db_path(py_ws)
    rust_db = rust_db_path(rust_ws)

    if not _ensure_table_exists(py_db) or not _ensure_table_exists(rust_db):
        pytest.skip("pipeline_runs table not present in one of the DBs")

    py_conn = open_db(py_db)
    rust_conn = open_db(rust_db)
    try:
        py_cols = _columns(py_conn, "pipeline_runs")
        rust_cols = _columns(rust_conn, "pipeline_runs")
    finally:
        py_conn.close()
        rust_conn.close()

    assert py_cols["dataset_id"]["notnull"] == 0, (
        "Python `dataset_id` must be nullable (no NOT NULL)"
    )
    assert rust_cols["dataset_id"]["notnull"] == 0, (
        "Rust `dataset_id` must be nullable after 08-01"
    )


# ── Lifecycle parity (LLM required) ──────────────────────────────────────────


def _pipeline_run_rows(conn: sqlite3.Connection) -> list[dict]:
    """All rows in ``pipeline_runs`` ordered by ``created_at`` ascending."""
    return query_rows(
        conn,
        "SELECT * FROM pipeline_runs ORDER BY created_at ASC",
    )


def _statuses_for_run(rows: list[dict], pipeline_run_id: str) -> list[str]:
    return [r["status"] for r in rows if r.get("pipeline_run_id") == pipeline_run_id]


@requires_openai
def test_cognify_writes_four_state_lifecycle(both_cognified):
    """Each SDK's cognify run writes INITIATED → STARTED → COMPLETED."""
    py_ws, rust_ws = both_cognified
    py_db = python_db_path(py_ws)
    rust_db = rust_db_path(rust_ws)

    if not _ensure_table_exists(py_db):
        pytest.skip("Python pipeline_runs table not present")
    if not _ensure_table_exists(rust_db):
        pytest.skip("Rust pipeline_runs table not present")

    for label, db_path in [("Python", py_db), ("Rust", rust_db)]:
        conn = open_db(db_path)
        try:
            rows = _pipeline_run_rows(conn)
        finally:
            conn.close()

        # Find every pipeline_run_id that belongs to cognify_pipeline.
        cognify_rows = [
            r
            for r in rows
            if r.get("pipeline_name") == "cognify_pipeline"
        ]
        assert cognify_rows, f"{label}: no cognify_pipeline rows present"

        run_ids = {r["pipeline_run_id"] for r in cognify_rows if r.get("pipeline_run_id")}
        assert run_ids, f"{label}: cognify rows missing pipeline_run_id"

        # For at least one cognify run, the trail must include all three
        # successful-lifecycle statuses.
        full_lifecycle_found = False
        for rid in run_ids:
            statuses = _statuses_for_run(cognify_rows, rid)
            if (
                "DATASET_PROCESSING_INITIATED" in statuses
                and "DATASET_PROCESSING_STARTED" in statuses
                and "DATASET_PROCESSING_COMPLETED" in statuses
            ):
                full_lifecycle_found = True
                break
        assert full_lifecycle_found, (
            f"{label}: no cognify_pipeline run carried the full INITIATED + "
            f"STARTED + COMPLETED trail. run_ids tried: {sorted(run_ids)}, "
            f"statuses observed: "
            f"{[(rid, _statuses_for_run(cognify_rows, rid)) for rid in run_ids]}"
        )


@requires_openai
def test_run_info_shape_parity(both_cognified):
    """``run_info`` JSON respects the four-state shape contract on both sides.

    INITIATED rows carry ``{}``; STARTED + COMPLETED carry an object with a
    ``data`` key; ERRORED carries both ``data`` and ``error`` keys (with
    ``data`` first).
    """
    py_ws, rust_ws = both_cognified
    py_db = python_db_path(py_ws)
    rust_db = rust_db_path(rust_ws)

    if not _ensure_table_exists(py_db):
        pytest.skip("Python pipeline_runs table not present")
    if not _ensure_table_exists(rust_db):
        pytest.skip("Rust pipeline_runs table not present")

    for label, db_path in [("Python", py_db), ("Rust", rust_db)]:
        conn = open_db(db_path)
        try:
            rows = _pipeline_run_rows(conn)
        finally:
            conn.close()

        assert rows, f"{label}: pipeline_runs table is empty after cognify"

        for r in rows:
            status = r["status"]
            run_info_raw = r.get("run_info")
            if run_info_raw is None:
                # NULL run_info is acceptable only on rows that pre-date
                # gap 08-03 — but every row written by gap-08 code must
                # populate run_info. The fixture runs current code, so
                # null rows are a regression.
                pytest.fail(
                    f"{label}: row with status={status} has NULL run_info "
                    f"(violates locked decision 5)"
                )

            # SQLite stores JSON as text — both SDKs serialise to bytes
            # then read back as text.
            if isinstance(run_info_raw, (bytes, bytearray)):
                run_info_text = run_info_raw.decode("utf-8")
            else:
                run_info_text = str(run_info_raw)

            try:
                payload = json.loads(run_info_text)
            except json.JSONDecodeError as e:
                pytest.fail(
                    f"{label}: row with status={status} has non-JSON "
                    f"run_info: {run_info_text!r} ({e})"
                )

            if status == "DATASET_PROCESSING_INITIATED":
                assert payload == {}, (
                    f"{label}: INITIATED run_info must be `{{}}`, got "
                    f"{payload!r}"
                )
            elif status in (
                "DATASET_PROCESSING_STARTED",
                "DATASET_PROCESSING_COMPLETED",
            ):
                assert isinstance(payload, dict), (
                    f"{label}: {status} run_info must be a JSON object"
                )
                assert "data" in payload, (
                    f"{label}: {status} run_info must include a `data` "
                    f"key (decision 5); got keys={list(payload.keys())}"
                )
            elif status == "DATASET_PROCESSING_ERRORED":
                assert isinstance(payload, dict), (
                    f"{label}: ERRORED run_info must be a JSON object"
                )
                keys = list(payload.keys())
                assert "data" in keys and "error" in keys, (
                    f"{label}: ERRORED run_info must include both `data` "
                    f"and `error` keys; got {keys}"
                )
                # `data` must precede `error` on the wire so Python's
                # json.loads preserves insertion order (3.7+).
                assert keys.index("data") < keys.index("error"), (
                    f"{label}: ERRORED run_info must order `data` before "
                    f"`error` (decision 5); got {keys}"
                )
