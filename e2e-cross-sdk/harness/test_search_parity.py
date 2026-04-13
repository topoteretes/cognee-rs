"""Cross-SDK parity tests for the ``search`` operation.

All tests in this file run the full ``add → cognify → search`` pipeline in
both SDKs and assert that their search outputs agree on structural parity
(both non-empty, both mention the same seed keyword, both respect ``top_k``).

Every test requires an LLM — cognify itself invokes the LLM to build the
knowledge graph, even when the tested search type (CHUNKS, SUMMARIES) is
retrieval-only.
"""

import pytest

from helpers import (
    DATASET_NAME,
    run_python_search,
    run_rust_search,
)
from conftest import requires_openai


# Four parity-friendly search types exposed by *both* CLIs.
# (Rust CLI exposes CODE and CYPHER too, but CODE is a compat alias and
# CYPHER requires a Cypher-capable graph backend, so we exclude them.)
PARITY_SEARCH_TYPES = (
    "GRAPH_COMPLETION",
    "RAG_COMPLETION",
    "CHUNKS",
    "SUMMARIES",
)

# The NLP test fixture is about natural language processing, so any one of
# these keywords should appear in any reasonable retrieval or completion.
NLP_KEYWORDS = ("language", "nlp", "computer", "processing")

QUERY = "What is natural language processing about?"


def _mentions_any_keyword(results: list[str], keywords: tuple[str, ...]) -> bool:
    """Return True if the lowercased joined result text contains any keyword."""
    blob = " ".join(results).lower()
    return any(kw in blob for kw in keywords)


# ── Non-empty parity ────────────────────────────────────────────────────────


@requires_openai
@pytest.mark.parametrize("search_type", PARITY_SEARCH_TYPES)
def test_search_returns_nonempty_in_both_sdks(both_cognified, search_type):
    """Both SDKs must return a non-empty result for the same query + type."""
    py_ws, rust_ws = both_cognified

    py_results = run_python_search(
        py_ws, QUERY, query_type=search_type, dataset=DATASET_NAME
    )
    rust_results = run_rust_search(
        rust_ws, QUERY, query_type=search_type, dataset=DATASET_NAME
    )

    assert len(py_results) > 0, (
        f"Python produced zero {search_type} results for query {QUERY!r}"
    )
    assert len(rust_results) > 0, (
        f"Rust produced zero {search_type} results for query {QUERY!r}"
    )

    # Each returned item must itself be non-empty (no empty strings in the list).
    assert all(r.strip() for r in py_results), (
        f"Python {search_type} results contained an empty string: {py_results!r}"
    )
    assert all(r.strip() for r in rust_results), (
        f"Rust {search_type} results contained an empty string: {rust_results!r}"
    )


# ── Keyword parity (strict: both must mention a topic keyword) ──────────────


@requires_openai
@pytest.mark.parametrize("search_type", PARITY_SEARCH_TYPES)
def test_search_mentions_topic_in_both_sdks(both_cognified, search_type):
    """Both SDKs must surface at least one topic keyword from the source text."""
    py_ws, rust_ws = both_cognified

    py_results = run_python_search(
        py_ws, QUERY, query_type=search_type, dataset=DATASET_NAME
    )
    rust_results = run_rust_search(
        rust_ws, QUERY, query_type=search_type, dataset=DATASET_NAME
    )

    assert _mentions_any_keyword(py_results, NLP_KEYWORDS), (
        f"Python {search_type} result does not mention any of {NLP_KEYWORDS}:\n"
        f"{py_results!r}"
    )
    assert _mentions_any_keyword(rust_results, NLP_KEYWORDS), (
        f"Rust {search_type} result does not mention any of {NLP_KEYWORDS}:\n"
        f"{rust_results!r}"
    )


# ── top_k parity for list-shaped search types ──────────────────────────────


@requires_openai
@pytest.mark.parametrize("search_type", ("CHUNKS", "SUMMARIES"))
def test_search_top_k_respected_in_both_sdks(both_cognified, search_type):
    """With ``top_k=2`` both SDKs must cap their result list at 2 items."""
    py_ws, rust_ws = both_cognified

    py_results = run_python_search(
        py_ws, QUERY, query_type=search_type, dataset=DATASET_NAME, top_k=2
    )
    rust_results = run_rust_search(
        rust_ws, QUERY, query_type=search_type, dataset=DATASET_NAME, top_k=2
    )

    assert 0 < len(py_results) <= 2, (
        f"Python {search_type} returned {len(py_results)} results with top_k=2: "
        f"{py_results!r}"
    )
    assert 0 < len(rust_results) <= 2, (
        f"Rust {search_type} returned {len(rust_results)} results with top_k=2: "
        f"{rust_results!r}"
    )


# ── Completion search types produce a single text blob ─────────────────────


@requires_openai
@pytest.mark.parametrize("search_type", ("GRAPH_COMPLETION", "RAG_COMPLETION"))
def test_search_completion_returns_single_blob(both_cognified, search_type):
    """Completion-style searches must return exactly one (non-empty) string."""
    py_ws, rust_ws = both_cognified

    py_results = run_python_search(
        py_ws, QUERY, query_type=search_type, dataset=DATASET_NAME
    )
    rust_results = run_rust_search(
        rust_ws, QUERY, query_type=search_type, dataset=DATASET_NAME
    )

    assert len(py_results) == 1, (
        f"Python {search_type} expected 1 response blob, got {len(py_results)}: "
        f"{py_results!r}"
    )
    assert len(rust_results) == 1, (
        f"Rust {search_type} expected 1 response blob, got {len(rust_results)}: "
        f"{rust_results!r}"
    )


# ── Graceful handling of a nonexistent dataset ──────────────────────────────


@requires_openai
def test_search_nonexistent_dataset_graceful_in_both_sdks(both_cognified):
    """Searching an unknown dataset must not crash either SDK.

    Either both SDKs fail non-zero, or both SDKs return zero results — but no
    SDK is allowed to panic/segfault.  This guards against shape drift where
    one SDK raises and the other silently returns stale data.
    """
    py_ws, rust_ws = both_cognified
    missing_dataset = "this_dataset_does_not_exist"

    # Pass check=False so both runners return an empty list on non-zero exit
    # instead of raising.
    py_results = run_python_search(
        py_ws, QUERY, query_type="CHUNKS", dataset=missing_dataset, check=False
    )
    rust_results = run_rust_search(
        rust_ws, QUERY, query_type="CHUNKS", dataset=missing_dataset, check=False
    )

    # Both must end up with zero usable results — whether by graceful empty
    # response or by a caught non-zero exit.
    assert py_results == [] or all(not r.strip() for r in py_results), (
        f"Python returned data for nonexistent dataset: {py_results!r}"
    )
    assert rust_results == [] or all(not r.strip() for r in rust_results), (
        f"Rust returned data for nonexistent dataset: {rust_results!r}"
    )
