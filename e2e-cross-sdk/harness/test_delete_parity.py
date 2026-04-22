"""Cross-SDK parity tests for the ``delete`` operation (Gap E1).

Both SDKs independently run ``add -> cognify -> delete --all`` and verify
that the relational database is cleaned up afterwards.  Each SDK uses its
own workspace — no cross-SDK storage sharing is involved.

All tests require an OpenAI API key (cognify invokes the LLM).
"""

import pytest

from helpers import (
    open_db,
    query_data,
    query_datasets,
    query_dataset_data,
    query_nodes,
    query_edges,
    python_db_path,
    rust_db_path,
    run_python_cli,
    run_rust_cli,
    run_rust_search,
    run_python_search,
    write_rust_config,
    NLP_TEXT_FILE,
    DATASET_NAME,
)
from conftest import requires_openai


# ── Helpers ─────────────────────────────────────────────────────────────────


def _assert_db_empty(db_path, sdk_name):
    """Assert that the core relational tables are empty after delete.

    Checks: data, datasets, dataset_data.  Nodes and edges may or may not
    be cleaned depending on the SDK's cascade behavior, so we only warn
    about those.
    """
    conn = open_db(db_path)
    data_rows = query_data(conn)
    dataset_rows = query_datasets(conn)
    junction_rows = query_dataset_data(conn)
    conn.close()

    assert len(data_rows) == 0, (
        f"{sdk_name} still has {len(data_rows)} data row(s) after delete --all"
    )
    assert len(dataset_rows) == 0, (
        f"{sdk_name} still has {len(dataset_rows)} dataset row(s) after delete --all"
    )
    assert len(junction_rows) == 0, (
        f"{sdk_name} still has {len(junction_rows)} dataset_data row(s) after delete --all"
    )


# ── Tests ───────────────────────────────────────────────────────────────────


@requires_openai
def test_delete_parity_both_sdks_clean_up(both_cognified):
    """Both SDKs must produce empty data/datasets tables after delete --all.

    Flow per SDK:
      1. ``both_cognified`` has already run ``add`` + ``cognify``.
      2. Run ``delete --all -f``.
      3. Assert the relational DB tables are empty.
    """
    py_ws, rust_ws = both_cognified

    # Verify pre-condition: both DBs have data
    py_db = python_db_path(py_ws)
    rust_db = rust_db_path(rust_ws)
    assert len(query_data(open_db(py_db))) > 0, "Python DB has no data before delete"
    assert len(query_data(open_db(rust_db))) > 0, "Rust DB has no data before delete"

    # ── Python: delete --all ──────────────────────────────────────────────
    result = run_python_cli(py_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Python delete --all failed (exit {result.returncode}):\n"
        f"--- stdout ---\n{result.stdout}\n"
        f"--- stderr ---\n{result.stderr}"
    )

    # ── Rust: delete --all -f ─────────────────────────────────────────────
    result = run_rust_cli(rust_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Rust delete --all failed (exit {result.returncode}):\n"
        f"--- stdout ---\n{result.stdout}\n"
        f"--- stderr ---\n{result.stderr}"
    )

    # ── Assert both DBs are empty ─────────────────────────────────────────
    _assert_db_empty(py_db, "Python")
    _assert_db_empty(rust_db, "Rust")


@requires_openai
def test_search_empty_after_delete(both_cognified):
    """After delete --all, CHUNKS search must return zero results in both SDKs.

    This tests the full cascading behavior: delete must remove not only
    relational metadata but also graph/vector data so that search finds
    nothing.
    """
    py_ws, rust_ws = both_cognified

    # ── Delete in both SDKs ───────────────────────────────────────────────
    result = run_python_cli(py_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Python delete failed:\n{result.stdout}\n{result.stderr}"
    )

    result = run_rust_cli(rust_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Rust delete failed:\n{result.stdout}\n{result.stderr}"
    )

    # ── Search: expect empty results ──────────────────────────────────────
    py_results = run_python_search(
        py_ws,
        "What is natural language processing?",
        query_type="CHUNKS",
        dataset=DATASET_NAME,
        check=False,
    )
    rust_results = run_rust_search(
        rust_ws,
        "What is natural language processing?",
        query_type="CHUNKS",
        dataset=DATASET_NAME,
        check=False,
    )

    assert len(py_results) == 0, (
        f"Python CHUNKS search returned {len(py_results)} result(s) after delete: "
        f"{py_results!r}"
    )
    assert len(rust_results) == 0, (
        f"Rust CHUNKS search returned {len(rust_results)} result(s) after delete: "
        f"{rust_results!r}"
    )
