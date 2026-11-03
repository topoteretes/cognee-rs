"""Cross-SDK parity tests for TEMPORAL search on a default cognify.

Both CLIs run plain ``add`` + ``cognify`` on the same biography text. There is
no temporal pipeline left to select in either SDK:

* Python deprecated ``temporal_cognify`` and ignores it
  (``cognee/api/v1/cognify/cognify.py``, ``TEMPORAL_COGNIFY_DEPRECATED``:
  "cognee 1.6.2 was the last release in which temporal_cognify=True built the
  event graph; since 1.6.3 the flag has no effect. cognify() now extracts dates
  as Timestamp nodes by default, and SearchType.TEMPORAL reranks by them").
* Rust removed the event-graph pipeline and the ``--temporal-cognify`` flag
  outright (SDK-1039, following the SDK-1115 rule of skipping deprecation
  shims).

TEMPORAL is hybrid retrieval reranked by the question's time window in both
SDKs (Python's ``TemporalHybridRetriever``, ported to Rust by SDK-1061), and the
``Timestamp`` nodes it reranks by are the ones default cognify writes. So the
tests check that both SDKs write those nodes and no event graph, and that
TEMPORAL answers on that graph. Answers are LLM output, so they are only
checked for being non-empty.

The former Event-graph tests (Event node counts, and that every Event has an
``at``/``during`` edge to a Timestamp) were deleted: Python builds no Event
nodes at all, so their premise no longer holds.
"""

import pytest

from helpers import (
    open_db,
    python_graph,
    run_python_cli,
    run_rust_cli,
    run_python_search,
    run_rust_search,
    rust_db_path,
    write_rust_config,
    query_nodes,
    query_nodes_by_type,
    TEST_DATA_DIR,
)
from conftest import requires_openai

TEMPORAL_DATASET = "temporal_e2e"
BIOGRAPHY_FILE = TEST_DATA_DIR / "biography_temporal.txt"

# The biography names well over ten distinct dates (1889 through 1978), but
# how many become Timestamp nodes is the extraction LLM's choice: two
# gpt-4o-mini runs gave Python 2 and 10, and Rust 2. The test asserts the
# contract (default cognify writes Timestamp nodes), not a count.
MIN_TIMESTAMPS = 1


# ── Fixtures ──────────────────────────────────────────────────────────────────


@pytest.fixture
def both_cognified(tmp_path):
    """Run add + default cognify on both SDKs with the same biography text."""
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()

    input_py = py_ws / "biography_temporal.txt"
    input_py.write_text(BIOGRAPHY_FILE.read_text())
    input_rust = rust_ws / "biography_temporal.txt"
    input_rust.write_text(BIOGRAPHY_FILE.read_text())

    # ── Python: add + default cognify ───────────────────────────────────
    result = run_python_cli(
        py_ws,
        ["add", str(input_py), "-d", TEMPORAL_DATASET],
        check=False,
    )
    assert result.returncode == 0, (
        f"Python add failed:\n{result.stdout}\n{result.stderr}"
    )

    result = run_python_cli(
        py_ws,
        ["cognify", "-d", TEMPORAL_DATASET],
        check=False,
    )
    assert result.returncode == 0, (
        f"Python cognify failed:\n{result.stdout}\n{result.stderr}"
    )

    # ── Rust: add + default cognify ─────────────────────────────────────
    write_rust_config(rust_ws)

    result = run_rust_cli(
        rust_ws,
        ["add", str(input_rust), "-d", TEMPORAL_DATASET],
        check=False,
    )
    assert result.returncode == 0, (
        f"Rust add failed:\n{result.stdout}\n{result.stderr}"
    )

    result = run_rust_cli(
        rust_ws,
        ["cognify", "-d", TEMPORAL_DATASET],
        check=False,
    )
    assert result.returncode == 0, (
        f"Rust cognify failed:\n{result.stdout}\n{result.stderr}"
    )

    return py_ws, rust_ws


def _graph_ids(nodes):
    """Distinct graph ids: the Rust ledger has one row per producing data item."""
    return {node["graph_id"] for node in nodes}


# ── Graph shape ────────────────────────────────────────────────────────────────


@requires_openai
def test_default_cognify_writes_timestamp_nodes_in_both_sdks(both_cognified):
    """Default cognify extracts the biography's dates as Timestamp nodes.

    These are the nodes TEMPORAL reranks by, in both SDKs.
    """
    py_ws, rust_ws = both_cognified

    py_nodes, _ = python_graph(py_ws)
    py_timestamps = _graph_ids(n for n in py_nodes if n.get("type") == "Timestamp")
    rust_timestamps = _graph_ids(
        query_nodes_by_type(open_db(rust_db_path(rust_ws)), "Timestamp")
    )

    counts = f"Python {len(py_timestamps)}, Rust {len(rust_timestamps)}"
    print(f"Timestamp nodes after default cognify: {counts}")
    assert len(py_timestamps) >= MIN_TIMESTAMPS, f"Python wrote no Timestamp node ({counts})"
    assert len(rust_timestamps) >= MIN_TIMESTAMPS, f"Rust wrote no Timestamp node ({counts})"


@requires_openai
def test_neither_sdk_builds_an_event_graph(both_cognified):
    """No Event or Interval nodes: the event-graph pipeline is gone in both SDKs."""
    py_ws, rust_ws = both_cognified

    py_nodes, _ = python_graph(py_ws)
    rust_nodes = query_nodes(open_db(rust_db_path(rust_ws)))

    for sdk, nodes in (("Python", py_nodes), ("Rust", rust_nodes)):
        types = {n.get("type") or n.get("node_type") for n in nodes}
        assert not types & {"Event", "Interval"}, (
            f"{sdk} wrote event-graph nodes: {sorted(t for t in types if t)}"
        )


# ── TEMPORAL search ───────────────────────────────────────────────────────────


@requires_openai
def test_temporal_search_returns_non_empty_results(both_cognified):
    """TEMPORAL search must return non-empty output on the Rust SDK."""
    _py_ws, rust_ws = both_cognified

    rust_results = run_rust_search(
        rust_ws,
        "What events happened?",
        query_type="TEMPORAL",
        check=False,
    )

    assert len(rust_results) > 0, "Rust TEMPORAL search returned empty results"


@requires_openai
def test_temporal_search_with_year_filter(both_cognified):
    """TEMPORAL search with a year in the question must return results on Rust.

    The year gives the window extraction a window, and default cognify's
    ``Timestamp`` nodes (1945 is named several times in the biography) are what
    the rerank looks the window up in.
    """
    _py_ws, rust_ws = both_cognified

    rust_results = run_rust_search(
        rust_ws,
        "What happened in 1945?",
        query_type="TEMPORAL",
        check=False,
    )

    # We do not compare exact text — LLM output is non-deterministic.
    assert len(rust_results) > 0, "Rust TEMPORAL year-filter search returned empty results"


@requires_openai
def test_temporal_search_parity_both_sdks_return_non_empty(both_cognified):
    """Both Python and Rust must return non-empty TEMPORAL search results."""
    py_ws, rust_ws = both_cognified

    python_results = run_python_search(
        py_ws,
        "What events happened?",
        query_type="TEMPORAL",
        check=False,
    )

    rust_results = run_rust_search(
        rust_ws,
        "What events happened?",
        query_type="TEMPORAL",
        check=False,
    )

    assert len(python_results) > 0, "Python TEMPORAL search returned empty results"
    assert len(rust_results) > 0, "Rust TEMPORAL search returned empty results"
