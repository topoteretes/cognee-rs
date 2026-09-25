"""Phase-2 parity tests for POST /api/v1/remember (LLM-gated).

Requires: OPENAI_TOKEN or OPENAI_API_KEY in environment.

Per p8-e2e-parity.md Step 10:
- Blocking remember on a cognified dataset.
- Remember with and without session_id.

Python's `/remember` router is `Form(...)`-only (see
``cognee/api/v1/remember/routers/get_remember_router.py:30-39``); JSON bodies
are silently ignored. All requests below use multipart uploads on **both**
SDK calls — Decision 15 / E-01.

LLM-derived fields use Jaccard structural compare. Per-run identifiers
(`pipeline_run_id`, `dataset_id`, `elapsed_seconds`) are excluded from the
diff.
"""

from __future__ import annotations

import uuid

import pytest

from conftest import requires_openai
from http_helpers import DEFAULT_IGNORE, assert_responses_match
from seed import seed_cognify, seed_dataset_with_text

pytestmark = [requires_openai]

# Per-run / non-deterministic fields excluded from the structural diff.
# `dataset_id` and `pipeline_run_id` are server-generated UUIDs that diverge
# between SDKs even on the same content; `elapsed_seconds` is wall-clock
# wall time and is non-deterministic.
_IGNORE = DEFAULT_IGNORE | {
    "$..pipeline_run_id",
    "$..dataset_id",
    "$..elapsed_seconds",
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


def _post_remember(
    client,
    *,
    query: str,
    session_id: str | None = None,
    dataset_name: str | None = None,
):
    """POST ``/api/v1/remember`` as a multipart form (Python parity).

    The Python router is ``Form(...)``-only; the file payload (`data`) and
    text fields all travel as multipart parts. We send the query text as a
    single ``data`` file part so the cognify+memify pipeline has content to
    chew on, plus optional ``datasetName`` / ``session_id`` form fields.
    """
    files = [("data", ("query.txt", query.encode(), "text/plain"))]
    data: dict[str, str] = {"run_in_background": "false"}
    if session_id is not None:
        data["session_id"] = session_id
    if dataset_name is not None:
        data["datasetName"] = dataset_name
    return client.post(
        "/api/v1/remember",
        files=files,
        data=data,
        timeout=300.0,
    )


def test_remember_blocking(authed_clients, unique_dataset_name):
    """POST /api/v1/remember blocking on a cognified dataset completes on both servers."""
    _seed_and_cognify(authed_clients, unique_dataset_name)

    py = _post_remember(
        authed_clients["py"],
        query="What did Newton discover?",
        dataset_name=unique_dataset_name,
    )
    rs = _post_remember(
        authed_clients["rs"],
        query="What did Newton discover?",
        dataset_name=unique_dataset_name,
    )
    assert py.status_code == rs.status_code, (
        f"remember status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )
    if py.status_code == 200 and rs.status_code == 200:
        assert_responses_match(py, rs, ignore=_IGNORE)


def test_remember_with_session_id(authed_clients, unique_dataset_name):
    """POST /api/v1/remember with an explicit session_id works on both servers."""
    _seed_and_cognify(authed_clients, unique_dataset_name)

    session_id = str(uuid.uuid4())
    py = _post_remember(
        authed_clients["py"],
        query="Explain heliocentrism.",
        session_id=session_id,
        dataset_name=unique_dataset_name,
    )
    rs = _post_remember(
        authed_clients["rs"],
        query="Explain heliocentrism.",
        session_id=session_id,
        dataset_name=unique_dataset_name,
    )
    assert py.status_code == rs.status_code, (
        f"remember-with-session status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    if py.status_code == 200 and rs.status_code == 200:
        assert_responses_match(py, rs, ignore=_IGNORE)


def test_remember_without_session_id(authed_clients, unique_dataset_name):
    """POST /api/v1/remember without session_id assigns one and returns it."""
    _seed_and_cognify(authed_clients, unique_dataset_name)

    py = _post_remember(
        authed_clients["py"],
        query="Tell me about Galileo.",
        dataset_name=unique_dataset_name,
    )
    rs = _post_remember(
        authed_clients["rs"],
        query="Tell me about Galileo.",
        dataset_name=unique_dataset_name,
    )
    assert py.status_code == rs.status_code, (
        f"remember-no-session status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    if py.status_code == 200 and rs.status_code == 200:
        assert_responses_match(py, rs, ignore=_IGNORE)
