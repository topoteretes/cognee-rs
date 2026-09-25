"""Phase-2 parity tests for POST /api/v1/recall (LLM-gated).

Requires: OPENAI_TOKEN or OPENAI_API_KEY in environment.

Per p8-e2e-parity.md Step 10: parameterize over auto-routed SearchType selection.
4 cases: factual question, summary question, temporal question, code-rule question.
Strict matcher on the *envelope*, structural compare on ``results[*].text``.
"""

from __future__ import annotations

import pytest

from conftest import requires_openai
from http_helpers import DEFAULT_IGNORE, assert_responses_match
from seed import seed_cognify, seed_dataset_with_text

pytestmark = [requires_openai]

_IGNORE = DEFAULT_IGNORE | {
    "$..tenant_id",
    "$..owner_id",
    "$..results[*].score",
    "$..results[*].id",
    "$..results[*].chunk_id",
    "$..session_id",
}

_SEED_TEXT = (
    "Python is a high-level programming language created by Guido van Rossum in 1991.  "
    "The Django framework was released in 2005 and follows the MTV pattern.  "
    "TensorFlow is an open-source machine learning library developed by Google.  "
    "Rule: Always write unit tests before submitting code to production."
)

_RECALL_CASES = [
    ("factual", "Who created Python?"),
    ("summary", "Give me a summary of the machine learning tools mentioned."),
    ("temporal", "When was Django released?"),
    ("coding_rules", "What are the coding rules mentioned?"),
]


@pytest.fixture
def recalled_seeded(authed_clients, unique_dataset_name):
    """Seed + cognify on both servers before a recall case.

    Function-scoped (not module): it depends on the function-scoped
    ``authed_clients`` / ``unique_dataset_name`` fixtures, and a module-scoped
    fixture may not depend on function-scoped ones (pytest ScopeMismatch). Each
    parametrized case seeds the same text into its own fresh dataset on *both*
    servers, so py/rs stay in lockstep and the parity assertion holds.
    """
    for side, client in authed_clients.items():
        r = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        ds_id = r.get("dataset_id") or r.get("id")
        if ds_id:
            seed_cognify(client, dataset_id=ds_id)
    return True


@pytest.mark.parametrize("case_name,query", _RECALL_CASES, ids=[c[0] for c in _RECALL_CASES])
def test_recall_parity(authed_clients, recalled_seeded, case_name, query):
    """POST /api/v1/recall returns equivalent envelope for both servers."""
    payload = {"query": query}
    py = authed_clients["py"].post("/api/v1/recall", json=payload, timeout=120.0)
    rs = authed_clients["rs"].post("/api/v1/recall", json=payload, timeout=120.0)

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/recall not yet implemented")

    assert py.status_code == rs.status_code, (
        f"recall status mismatch [{case_name}]: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )
    # Envelope structural match (result texts are LLM-derived; only status + shape checked)
    assert_responses_match(py, rs, ignore=_IGNORE | {"$..results[*].text"})

    # CLEAN-01 §5.4: response-key casing parity. POST /api/v1/recall returns a
    # `Vec<SearchResultDTO>` whose Python counterpart inherits `OutDTO`, so the
    # wire must be camelCase on both sides — no underscores in top-level keys.
    if py.status_code == 200 and rs.status_code == 200:
        for resp in (py, rs):
            try:
                body = resp.json()
            except ValueError:
                continue
            if isinstance(body, list):
                for item in body:
                    if isinstance(item, dict):
                        for key in item.keys():
                            assert "_" not in key, (
                                f"snake_case key found in /api/v1/recall response: {key} (full body: {body})"
                            )
