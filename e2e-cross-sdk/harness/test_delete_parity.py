"""Cross-SDK parity tests for the ``delete`` operation (Gap E1).

Both SDKs independently run ``add -> cognify -> delete --all`` and verify
that the relational database, the graph store and the vector store are all
cleaned up afterwards.  Each SDK uses its
own workspace — no cross-SDK storage sharing is involved.

All tests require an OpenAI API key (cognify invokes the LLM).
"""

import pytest

from helpers import (
    open_db,
    query_data,
    query_datasets,
    query_dataset_data,
    python_graph,
    rust_graph,
    vector_rows,
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

    Checks: data, datasets, dataset memberships (``data.dataset_id``).  Nodes and edges may or may not
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
        f"{sdk_name} still has {len(junction_rows)} dataset membership(s) after delete --all"
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
    """After delete --all, both SDKs' graph and vector stores must be empty.

    Search cannot show this on its own: searching a dataset that no longer
    exists fails, and the search helpers turn a failed search into ``[]``,
    so an "empty search" passes whether or not the chunks are still stored.
    The test therefore:

      1. proves search works before the delete (non-empty CHUNKS results),
      2. deletes with both SDKs,
      3. reads each SDK's graph store and LanceDB tables directly and
         requires both to be empty.
    """
    py_ws, rust_ws = both_cognified
    question = "What is natural language processing?"

    # ── Precondition: search finds the chunks before the delete ──────────
    py_before = run_python_search(py_ws, question, query_type="CHUNKS", dataset=DATASET_NAME)
    rust_before = run_rust_search(rust_ws, question, query_type="CHUNKS", dataset=DATASET_NAME)
    assert py_before, "Python CHUNKS search found nothing before the delete"
    assert rust_before, "Rust CHUNKS search found nothing before the delete"
    assert python_graph(py_ws)[0], "Python's graph is empty before the delete"
    assert rust_graph(rust_ws)[0], "Rust's graph is empty before the delete"
    assert vector_rows(py_ws), "Python's vector store is empty before the delete"
    assert vector_rows(rust_ws), "Rust's vector store is empty before the delete"

    # ── Delete in both SDKs ───────────────────────────────────────────────
    result = run_python_cli(py_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Python delete failed:\n{result.stdout}\n{result.stderr}"
    )

    result = run_rust_cli(rust_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Rust delete failed:\n{result.stdout}\n{result.stderr}"
    )

    # ── Both stores must be empty ─────────────────────────────────────────
    for sdk, nodes, vectors in (
        ("Python", python_graph(py_ws)[0], vector_rows(py_ws)),
        ("Rust", rust_graph(rust_ws)[0], vector_rows(rust_ws)),
    ):
        assert not nodes, (
            f"{sdk} graph keeps {len(nodes)} node(s) after delete --all: "
            f"{sorted({n.get('type') or '?' for n in nodes})}"
        )
        assert not vectors, (
            f"{sdk} vector store keeps rows after delete --all: "
            f"{ {name: len(rows) for name, rows in vectors.items()} }"
        )
