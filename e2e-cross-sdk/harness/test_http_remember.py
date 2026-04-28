"""Phase-2 parity tests for POST /api/v1/remember (LLM-gated).

Requires: OPENAI_TOKEN or OPENAI_API_KEY in environment.

Per p8-e2e-parity.md Step 10:
- Blocking remember on a cognified dataset.
- Remember with and without session_id.

LLM-derived fields use Jaccard structural compare.
"""

from __future__ import annotations

import uuid

import pytest

from conftest import requires_openai
from http_helpers import DEFAULT_IGNORE, assert_responses_match
from seed import seed_cognify, seed_dataset_with_text

pytestmark = [requires_openai]

_IGNORE = DEFAULT_IGNORE | {
    "$..pipeline_run_id",
    "$..started_at",
    "$..ended_at",
    "$..run_info",
    "$..session_id",
}

_SEED_TEXT = (
    "Isaac Newton formulated the laws of motion and universal gravitation.  "
    "Galileo Galilei pioneered the use of the telescope for astronomy.  "
    "Copernicus proposed the heliocentric model of the solar system."
)


def _seed_and_cognify(authed_clients, unique_dataset_name) -> dict[str, str | None]:
    """Seed text and cognify on both servers; return dataset IDs."""
    ds_ids: dict[str, str | None] = {}
    for side, client in authed_clients.items():
        r = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        ds_id = r.get("dataset_id") or r.get("id")
        ds_ids[side] = ds_id
        if ds_id:
            seed_cognify(client, dataset_id=ds_id)
    return ds_ids


def test_remember_blocking(authed_clients, unique_dataset_name):
    """POST /api/v1/remember blocking on a cognified dataset completes on both servers."""
    _seed_and_cognify(authed_clients, unique_dataset_name)

    payload = {
        "query": "What did Newton discover?",
        "run_in_background": False,
    }
    py = authed_clients["py"].post("/api/v1/remember", json=payload, timeout=300.0)
    rs = authed_clients["rs"].post("/api/v1/remember", json=payload, timeout=300.0)
    assert py.status_code == rs.status_code, (
        f"remember status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )


def test_remember_with_session_id(authed_clients, unique_dataset_name):
    """POST /api/v1/remember with an explicit session_id works on both servers."""
    _seed_and_cognify(authed_clients, unique_dataset_name)

    session_id = str(uuid.uuid4())
    payload = {
        "query": "Explain heliocentrism.",
        "session_id": session_id,
        "run_in_background": False,
    }
    py = authed_clients["py"].post("/api/v1/remember", json=payload, timeout=300.0)
    rs = authed_clients["rs"].post("/api/v1/remember", json=payload, timeout=300.0)
    assert py.status_code == rs.status_code, (
        f"remember-with-session status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_remember_without_session_id(authed_clients, unique_dataset_name):
    """POST /api/v1/remember without session_id assigns one and returns it."""
    _seed_and_cognify(authed_clients, unique_dataset_name)

    payload = {
        "query": "Tell me about Galileo.",
        "run_in_background": False,
    }
    py = authed_clients["py"].post("/api/v1/remember", json=payload, timeout=300.0)
    rs = authed_clients["rs"].post("/api/v1/remember", json=payload, timeout=300.0)
    assert py.status_code == rs.status_code, (
        f"remember-no-session status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert_responses_match(py, rs, ignore=_IGNORE)
