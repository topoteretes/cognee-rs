"""Cross-SDK parity for the ``pipeline_runs`` table (gap 08, decision 8).

Verifies that Python and Rust write the same lifecycle to ``pipeline_runs``
after an ``add`` + ``cognify`` run:

1. Schema parity — both SDKs expose a ``pipeline_runs`` table with the
   same column set (after gap 08-01's nullability migration).
2. ``dataset_id`` is nullable in both schemas.
3. Cognify writes ``STARTED → COMPLETED`` (one row per transition, sharing a
   ``pipeline_run_id``) on both sides, and no ``INITIATED`` row: Python stopped
   writing ``DATASET_PROCESSING_INITIATED`` in ``dc0b4ad6a`` ("remove the
   INITIATED reset row and the pipeline cache gate"); the enum value survives
   only for rows that already carry it.
4. ``run_info`` follows Python's contract since ``a88402500`` (COG-6089, "Stop
   storing data items in run_info"; Python's own pin is
   ``cognee/tests/unit/modules/pipelines/test_run_info_has_no_data.py``):
     - no row carries a ``data`` key;
     - STARTED → ``{}`` or ``{"recovery_lock": …}``, plus ``progress`` once the
       run ticks (``log_pipeline_run_start.py:27``,
       ``log_pipeline_run_progress.py``). Both SDKs write ``recovery_lock``
       (the run's ownership marker for startup recovery, SDK-1039 P22: Python
       ``run_tasks.py:96`` / ``run_ownership.py``, Rust
       ``cognee_core::RunOwnership``); ``progress`` is Python-only, Rust has no
       in-row progress ticks;
     - COMPLETED → ``{}`` (``log_pipeline_run_complete.py``). Rust may add its
       documented ``cognify_failures`` key on a run that completed with files
       outstanding;
     - ERRORED → ``{"error": …}`` (``log_pipeline_run_error.py``).

The shape checks assert key sets, not values.

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


# The run_info keys each status may carry, per SDK. Python-only keys are
# tolerated on the Python side only; Rust-only keys on the Rust side only.
# `recovery_lock` is on both sides since the ownership-marker port (the premise
# "Rust has no recovery lock" held only until then).
STARTED_KEYS = {"Python": {"recovery_lock", "progress"}, "Rust": {"recovery_lock"}}
COMPLETED_KEYS = {"Python": set(), "Rust": {"cognify_failures"}}
ERRORED_KEYS = {"Python": {"error"}, "Rust": {"error"}}


@requires_openai
def test_cognify_writes_started_completed_lifecycle(both_cognified):
    """Each SDK's cognify run writes STARTED → COMPLETED, and no INITIATED row."""
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

        initiated = [r for r in rows if r["status"] == "DATASET_PROCESSING_INITIATED"]
        assert not initiated, (
            f"{label}: no SDK writes INITIATED any more (Python dc0b4ad6a); got "
            f"{[(r.get('pipeline_name'), r.get('pipeline_run_id')) for r in initiated]}"
        )

        cognify_rows = [
            r
            for r in rows
            if r.get("pipeline_name") == "cognify_pipeline"
        ]
        assert cognify_rows, f"{label}: no cognify_pipeline rows present"

        run_ids = {r["pipeline_run_id"] for r in cognify_rows if r.get("pipeline_run_id")}
        assert run_ids, f"{label}: cognify rows missing pipeline_run_id"

        # For at least one cognify run, the trail is STARTED then COMPLETED
        # (rows are in created_at order).
        lifecycle_found = False
        for rid in run_ids:
            statuses = _statuses_for_run(cognify_rows, rid)
            if (
                "DATASET_PROCESSING_STARTED" in statuses
                and "DATASET_PROCESSING_COMPLETED" in statuses
                and statuses.index("DATASET_PROCESSING_STARTED")
                < statuses.index("DATASET_PROCESSING_COMPLETED")
            ):
                lifecycle_found = True
                break
        assert lifecycle_found, (
            f"{label}: no cognify_pipeline run carried STARTED then COMPLETED. "
            f"statuses observed: "
            f"{[(rid, _statuses_for_run(cognify_rows, rid)) for rid in run_ids]}"
        )


@requires_openai
def test_run_info_shape_parity(both_cognified):
    """``run_info`` follows Python's post-COG-6089 contract on both sides.

    No ``data`` key anywhere; STARTED ⊆ ``{recovery_lock, progress}``
    (Rust: ``{recovery_lock}``); COMPLETED is ``{}`` (Rust may add ``cognify_failures``);
    ERRORED is ``{"error": …}``.
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
            where = f"{label}: {r.get('pipeline_name')} {status}"
            run_info_raw = r.get("run_info")
            if run_info_raw is None:
                pytest.fail(f"{where} row has NULL run_info")

            # SQLite stores JSON as text — both SDKs serialise to bytes
            # then read back as text.
            if isinstance(run_info_raw, (bytes, bytearray)):
                run_info_text = run_info_raw.decode("utf-8")
            else:
                run_info_text = str(run_info_raw)

            try:
                payload = json.loads(run_info_text)
            except json.JSONDecodeError as e:
                pytest.fail(f"{where} row has non-JSON run_info: {run_info_text!r} ({e})")

            assert isinstance(payload, dict), f"{where} run_info must be a JSON object"
            keys = set(payload)
            assert "data" not in keys, (
                f"{where} run_info must not carry `data` (Python a88402500, COG-6089); "
                f"got keys={sorted(keys)}"
            )

            if status == "DATASET_PROCESSING_STARTED":
                assert keys <= STARTED_KEYS[label], (
                    f"{where} run_info keys must be within "
                    f"{sorted(STARTED_KEYS[label])}; got {sorted(keys)}"
                )
            elif status == "DATASET_PROCESSING_COMPLETED":
                assert keys <= COMPLETED_KEYS[label], (
                    f"{where} run_info keys must be within "
                    f"{sorted(COMPLETED_KEYS[label])}; got {sorted(keys)}"
                )
            elif status == "DATASET_PROCESSING_ERRORED":
                assert keys == ERRORED_KEYS[label], (
                    f"{where} run_info must be exactly `{{\"error\": …}}`; "
                    f"got {sorted(keys)}"
                )
            else:
                pytest.fail(f"{where}: unexpected status (no SDK writes INITIATED)")
