"""Cross-SDK tests for the ``memify`` pipeline + ``TRIPLET_COMPLETION`` search.

After Phase 9 Step 7, Rust's triplet embeddable text format matches Python's
canonical form ("{src}-›{rel}-›{tgt}"), so a triplet-completion search after
memify returns comparable results in both SDKs.

Python and Rust do not share a vector DB in the docker harness (Python uses
LanceDB, Rust uses embedded Qdrant), so this test follows the
parallel-pipelines approach: each SDK independently runs
``add -> cognify -> memify`` and then issues a ``TRIPLET_COMPLETION`` search.
We assert non-empty, topic-relevant results in each — parity in behavior
rather than shared storage.

All tests require an OpenAI API key (cognify invokes the LLM).
"""

import pytest

from helpers import (
    DATASET_NAME,
    run_python_memify,
    run_python_search,
    run_rust_cli,
    run_rust_search,
)
from conftest import requires_openai


# Topic keywords present in the NLP fixture text. At least one should appear
# in a reasonable TRIPLET_COMPLETION result after memify indexes the graph.
NLP_KEYWORDS = ("language", "nlp", "computer", "processing")

QUERY = "What is natural language processing about?"


def _mentions_any_keyword(results: list[str], keywords: tuple[str, ...]) -> bool:
    """Return True if the lowercased joined result text contains any keyword."""
    blob = " ".join(results).lower()
    return any(kw in blob for kw in keywords)


@requires_openai
def test_triplet_completion_after_memify(both_cognified):
    """After memify, TRIPLET_COMPLETION search returns topical results in both SDKs.

    Flow per SDK (parallel, no shared storage):
      1. ``both_cognified`` has already run ``add`` + ``cognify``.
      2. Run ``memify`` to populate the ``Triplet``/``text`` vector collection
         from the existing knowledge graph.
      3. Query ``TRIPLET_COMPLETION`` with an NLP-topic question.
      4. Assert exit 0 on memify and non-empty, keyword-bearing results.
    """
    py_ws, rust_ws = both_cognified

    # ── Python: memify then TRIPLET_COMPLETION search ─────────────────────
    py_result = run_python_memify(py_ws, DATASET_NAME, check=False)
    assert py_result.returncode == 0, (
        f"Python memify failed (exit {py_result.returncode}):\n"
        f"--- stdout ---\n{py_result.stdout}\n"
        f"--- stderr ---\n{py_result.stderr}"
    )

    py_search_results = run_python_search(
        py_ws,
        QUERY,
        query_type="TRIPLET_COMPLETION",
        dataset=DATASET_NAME,
    )

    # ── Rust: memify then TRIPLET_COMPLETION search ───────────────────────
    rust_result = run_rust_cli(rust_ws, ["memify", "-d", DATASET_NAME], check=False)
    assert rust_result.returncode == 0, (
        f"Rust memify failed (exit {rust_result.returncode}):\n"
        f"--- stdout ---\n{rust_result.stdout}\n"
        f"--- stderr ---\n{rust_result.stderr}"
    )

    rust_search_results = run_rust_search(
        rust_ws,
        QUERY,
        query_type="TRIPLET_COMPLETION",
        dataset=DATASET_NAME,
    )

    # ── Assertions: non-empty + topical in each SDK ───────────────────────
    assert len(py_search_results) > 0, (
        f"Python TRIPLET_COMPLETION produced zero results for {QUERY!r}"
    )
    assert len(rust_search_results) > 0, (
        f"Rust TRIPLET_COMPLETION produced zero results for {QUERY!r}"
    )

    assert all(r.strip() for r in py_search_results), (
        f"Python TRIPLET_COMPLETION results contained an empty string: "
        f"{py_search_results!r}"
    )
    assert all(r.strip() for r in rust_search_results), (
        f"Rust TRIPLET_COMPLETION results contained an empty string: "
        f"{rust_search_results!r}"
    )

    # Tolerance-based relevance: at least one of the topic keywords from the
    # fixture text must appear in each SDK's top-K result text.
    assert _mentions_any_keyword(py_search_results, NLP_KEYWORDS), (
        f"Python TRIPLET_COMPLETION does not mention any of {NLP_KEYWORDS}:\n"
        f"{py_search_results!r}"
    )
    assert _mentions_any_keyword(rust_search_results, NLP_KEYWORDS), (
        f"Rust TRIPLET_COMPLETION does not mention any of {NLP_KEYWORDS}:\n"
        f"{rust_search_results!r}"
    )
