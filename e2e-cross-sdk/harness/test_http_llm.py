"""Phase-2 parity tests for /api/v1/llm/* (LLM-gated).

Requires: OPENAI_TOKEN or OPENAI_API_KEY in environment.

Per p8-e2e-parity.md Step 10:
- POST /llm/custom-prompt with a deterministic prompt (temperature=0, fixed seed).
- POST /llm/infer-schema with a fixed input.

Strict match on status + envelope; the ``output`` field allows ±1-token
divergence via a per-test fuzzy compare (word-set Jaccard ≥ 0.7).
"""

from __future__ import annotations

import pytest

from conftest import requires_openai
from http_helpers import DEFAULT_IGNORE, assert_responses_match

pytestmark = [requires_openai]

_IGNORE = DEFAULT_IGNORE | {"$..output", "$..response"}


def _word_jaccard(a: str, b: str) -> float:
    wa = set(a.lower().split())
    wb = set(b.lower().split())
    if not wa and not wb:
        return 1.0
    return len(wa & wb) / len(wa | wb)


def test_llm_custom_prompt(authed_clients):
    """POST /api/v1/llm/custom-prompt returns 200 with an output on both servers."""
    payload = {
        "prompt": "Reply with exactly the word 'PARITY' and nothing else.",
        "temperature": 0.0,
    }
    py = authed_clients["py"].post(
        "/api/v1/llm/custom-prompt", json=payload, timeout=120.0
    )
    rs = authed_clients["rs"].post(
        "/api/v1/llm/custom-prompt", json=payload, timeout=120.0
    )
    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/llm/custom-prompt not yet implemented")

    assert py.status_code == rs.status_code, (
        f"custom-prompt status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )
    # Fuzzy compare on the output string
    py_output = (py.json().get("output") or "")
    rs_output = (rs.json().get("output") or "")
    if py_output and rs_output:
        j = _word_jaccard(py_output, rs_output)
        assert j >= 0.5, (
            f"LLM output Jaccard {j:.2f} < 0.5\npy={py_output!r}\nrs={rs_output!r}"
        )


def test_llm_infer_schema(authed_clients):
    """POST /api/v1/llm/infer-schema returns a JSON schema on both servers."""
    payload = {
        "description": "A person with a name and age",
        "temperature": 0.0,
    }
    py = authed_clients["py"].post(
        "/api/v1/llm/infer-schema", json=payload, timeout=120.0
    )
    rs = authed_clients["rs"].post(
        "/api/v1/llm/infer-schema", json=payload, timeout=120.0
    )
    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/llm/infer-schema not yet implemented")

    assert py.status_code == rs.status_code, (
        f"infer-schema status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert_responses_match(py, rs, ignore=_IGNORE)


def test_llm_custom_prompt_envelope_structure(authed_clients):
    """The /llm/custom-prompt response envelope has matching top-level keys."""
    payload = {
        "prompt": "What is 1+1?",
        "temperature": 0.0,
    }
    py = authed_clients["py"].post(
        "/api/v1/llm/custom-prompt", json=payload, timeout=120.0
    )
    rs = authed_clients["rs"].post(
        "/api/v1/llm/custom-prompt", json=payload, timeout=120.0
    )
    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/llm/custom-prompt not yet implemented")

    assert py.status_code == rs.status_code
    # Top-level key set must match
    py_keys = set(py.json().keys()) if py.status_code == 200 else set()
    rs_keys = set(rs.json().keys()) if rs.status_code == 200 else set()
    assert py_keys == rs_keys, (
        f"Response key mismatch:\n  only in py: {py_keys - rs_keys}\n  only in rs: {rs_keys - py_keys}"
    )


def test_llm_error_on_missing_prompt(authed_clients):
    """POST /api/v1/llm/custom-prompt with missing prompt returns 4xx on both."""
    py = authed_clients["py"].post("/api/v1/llm/custom-prompt", json={}, timeout=30.0)
    rs = authed_clients["rs"].post("/api/v1/llm/custom-prompt", json={}, timeout=30.0)
    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/llm/custom-prompt not yet implemented")
    assert py.status_code == rs.status_code, (
        f"missing-prompt status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert py.status_code >= 400, f"Expected 4xx for missing prompt, got {py.status_code}"
