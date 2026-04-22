"""Memify + delete integration test (Gap E4).

Extends the memify pipeline test pattern: after ``add -> cognify -> memify``,
a TRIPLET_COMPLETION search returns non-empty results; after ``delete --all``,
the same search returns empty results.

Each SDK runs independently in its own workspace (no shared graph/vector
backends).

All tests require an OpenAI API key (cognify invokes the LLM).
"""

import pytest

from helpers import (
    DATASET_NAME,
    run_python_cli,
    run_python_memify,
    run_python_search,
    run_rust_cli,
    run_rust_search,
)
from conftest import requires_openai

NLP_KEYWORDS = ("language", "nlp", "computer", "processing")
QUERY = "What is natural language processing about?"


def _mentions_any_keyword(results: list[str], keywords: tuple[str, ...]) -> bool:
    """Return True if the lowercased joined result text contains any keyword."""
    blob = " ".join(results).lower()
    return any(kw in blob for kw in keywords)


@requires_openai
def test_memify_then_delete_cleans_search(both_cognified):
    """After memify, search returns results; after delete --all, search is empty.

    Flow per SDK:
      1. ``both_cognified`` has already run ``add`` + ``cognify``.
      2. Run ``memify`` to populate the Triplet vector collection.
      3. Search TRIPLET_COMPLETION --- assert non-empty, topical results.
      4. Run ``delete --all -f``.
      5. Search TRIPLET_COMPLETION again --- assert zero results.
    """
    py_ws, rust_ws = both_cognified

    # ── Step 2: memify ───────────────────────────────────────────────────
    py_result = run_python_memify(py_ws, DATASET_NAME, check=False)
    assert py_result.returncode == 0, (
        f"Python memify failed (exit {py_result.returncode}):\n"
        f"--- stdout ---\n{py_result.stdout}\n"
        f"--- stderr ---\n{py_result.stderr}"
    )

    rust_result = run_rust_cli(
        rust_ws, ["memify", "-d", DATASET_NAME], check=False
    )
    assert rust_result.returncode == 0, (
        f"Rust memify failed (exit {rust_result.returncode}):\n"
        f"--- stdout ---\n{rust_result.stdout}\n"
        f"--- stderr ---\n{rust_result.stderr}"
    )

    # ── Step 3: search before delete (expect non-empty) ──────────────────
    py_before = run_python_search(
        py_ws,
        QUERY,
        query_type="TRIPLET_COMPLETION",
        dataset=DATASET_NAME,
        check=False,
    )
    rust_before = run_rust_search(
        rust_ws,
        QUERY,
        query_type="TRIPLET_COMPLETION",
        dataset=DATASET_NAME,
        check=False,
    )

    assert len(py_before) > 0, (
        f"Python TRIPLET_COMPLETION returned zero results before delete"
    )
    assert len(rust_before) > 0, (
        f"Rust TRIPLET_COMPLETION returned zero results before delete"
    )

    assert _mentions_any_keyword(py_before, NLP_KEYWORDS), (
        f"Python TRIPLET_COMPLETION pre-delete does not mention {NLP_KEYWORDS}:\n"
        f"{py_before!r}"
    )
    assert _mentions_any_keyword(rust_before, NLP_KEYWORDS), (
        f"Rust TRIPLET_COMPLETION pre-delete does not mention {NLP_KEYWORDS}:\n"
        f"{rust_before!r}"
    )

    # ── Step 4: delete --all ─────────────────────────────────────────────
    result = run_python_cli(py_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Python delete failed:\n{result.stdout}\n{result.stderr}"
    )

    result = run_rust_cli(rust_ws, ["delete", "--all", "-f"], check=False)
    assert result.returncode == 0, (
        f"Rust delete failed:\n{result.stdout}\n{result.stderr}"
    )

    # ── Step 5: search after delete (expect empty) ───────────────────────
    py_after = run_python_search(
        py_ws,
        QUERY,
        query_type="TRIPLET_COMPLETION",
        dataset=DATASET_NAME,
        check=False,
    )
    rust_after = run_rust_search(
        rust_ws,
        QUERY,
        query_type="TRIPLET_COMPLETION",
        dataset=DATASET_NAME,
        check=False,
    )

    assert len(py_after) == 0, (
        f"Python TRIPLET_COMPLETION returned {len(py_after)} result(s) after "
        f"delete --all: {py_after!r}"
    )
    assert len(rust_after) == 0, (
        f"Rust TRIPLET_COMPLETION returned {len(rust_after)} result(s) after "
        f"delete --all: {rust_after!r}"
    )
