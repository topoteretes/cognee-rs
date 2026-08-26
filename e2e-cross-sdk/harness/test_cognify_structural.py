"""Structural comparison of ``cognify`` output between Python and Rust SDKs.

All tests in this file require ``OPENAI_API_KEY`` and are **approximate** —
LLM-based graph extraction is non-deterministic, so we compare structural
properties (counts, type sets) rather than exact values.
"""

import os
import pytest

from helpers import (
    open_db,
    query_data,
    query_nodes,
    query_edges,
    python_db_path,
    rust_db_path,
)
from conftest import requires_openai


# ── Tests ────────────────────────────────────────────────────────────────────


# Maximum accepted relative divergence in node/edge counts between the two SDKs,
# as |py - rust| / mean. Overridable so a genuinely noisy signal can be widened
# deliberately, in one place, with the new value visible in CI config.
#
# This used to be enforced by `warnings.warn`, which meant the two
# "within_tolerance" tests below could only fail if a side produced literally
# zero — they asserted nothing about tolerance despite their names. If real-LLM
# runs prove this bound too tight, raise it here or mock the LLM
# (crates/llm/src/mock/ + COGNEE_TEST_REPLAY); do not return to warning.
COUNT_TOLERANCE = float(os.environ.get("COGNEE_PARITY_COUNT_TOLERANCE", "0.5"))


@requires_openai

def test_cognify_produces_nodes(both_cognified):
    """Both SDKs must produce at least one node after cognify."""
    py_ws, rust_ws = both_cognified

    py_nodes = query_nodes(open_db(python_db_path(py_ws)))
    rust_nodes = query_nodes(open_db(rust_db_path(rust_ws)))

    assert len(py_nodes) > 0, "Python produced zero nodes"
    assert len(rust_nodes) > 0, "Rust produced zero nodes"


@requires_openai

def test_cognify_produces_edges(both_cognified):
    """Both SDKs must produce at least one edge after cognify."""
    py_ws, rust_ws = both_cognified

    py_edges = query_edges(open_db(python_db_path(py_ws)))
    rust_edges = query_edges(open_db(rust_db_path(rust_ws)))

    assert len(py_edges) > 0, "Python produced zero edges"
    assert len(rust_edges) > 0, "Rust produced zero edges"


@requires_openai

def test_cognify_node_count_within_tolerance(both_cognified):
    """Node counts should be within 50% of each other.

    LLM extraction is non-deterministic, so we allow wide tolerance.
    """
    py_ws, rust_ws = both_cognified

    py_count = len(query_nodes(open_db(python_db_path(py_ws))))
    rust_count = len(query_nodes(open_db(rust_db_path(rust_ws))))

    avg = (py_count + rust_count) / 2
    diff = abs(py_count - rust_count)
    ratio = diff / avg if avg > 0 else 0

    assert py_count > 0, "Python produced zero nodes"
    assert rust_count > 0, "Rust produced zero nodes"
    assert ratio <= COUNT_TOLERANCE, (
        f"Node count divergence {ratio:.0%} exceeds the "
        f"{COUNT_TOLERANCE:.0%} tolerance: Python={py_count}, Rust={rust_count}"
    )


@requires_openai

def test_cognify_edge_count_within_tolerance(both_cognified):
    """Edge counts should be within 50% of each other."""
    py_ws, rust_ws = both_cognified

    py_count = len(query_edges(open_db(python_db_path(py_ws))))
    rust_count = len(query_edges(open_db(rust_db_path(rust_ws))))

    avg = (py_count + rust_count) / 2
    diff = abs(py_count - rust_count)
    ratio = diff / avg if avg > 0 else 0

    # LLM extraction is more non-deterministic for edges than for nodes
    # (relationship phrasing, merging). If COUNT_TOLERANCE proves too tight
    # here specifically, widen it there rather than dropping the assertion.
    assert py_count > 0, "Python produced zero edges"
    assert rust_count > 0, "Rust produced zero edges"
    assert ratio <= COUNT_TOLERANCE, (
        f"Edge count divergence {ratio:.0%} exceeds the "
        f"{COUNT_TOLERANCE:.0%} tolerance: Python={py_count}, Rust={rust_count}"
    )


@requires_openai

def test_cognify_node_types_overlap(both_cognified):
    """The sets of node types should overlap (Jaccard similarity > 0.3)."""
    py_ws, rust_ws = both_cognified

    py_types = {n["type"] for n in query_nodes(open_db(python_db_path(py_ws))) if n.get("type")}
    rust_types = {n["type"] for n in query_nodes(open_db(rust_db_path(rust_ws))) if n.get("type")}

    if not py_types and not rust_types:
        pytest.skip("Both SDKs produced zero typed nodes")

    intersection = py_types & rust_types
    union = py_types | rust_types
    jaccard = len(intersection) / len(union) if union else 0

    assert jaccard >= 0.3, (
        f"Node type Jaccard similarity too low ({jaccard:.2f}):\n"
        f"  Python types: {sorted(py_types)}\n"
        f"  Rust types:   {sorted(rust_types)}\n"
        f"  Overlap:      {sorted(intersection)}"
    )


@requires_openai

def test_cognify_updates_token_count(both_cognified):
    """Python should set token_count > 0 after cognify.

    Note: Rust currently does not write token_count back to the data table
    via the CLI path (tracked as a known gap).
    """
    py_ws, rust_ws = both_cognified

    py_data = query_data(open_db(python_db_path(py_ws)))

    assert len(py_data) >= 1

    py_tc = py_data[0].get("token_count", -1)
    assert py_tc > 0, f"Python token_count not updated: {py_tc}"
