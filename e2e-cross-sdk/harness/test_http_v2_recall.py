"""HTTP API v2 parity tests for ``POST /api/v1/recall``.

Per [`docs/http-api-v2/tasks/e-04-recall-search.md`](../../docs/http-api-v2/tasks/e-04-recall-search.md)
§5 — verify that the v2 additions to the recall payload (`session_id`,
`scope`) and the resulting four-source fan-out match Python byte-for-byte.

Coverage:
1. ``test_session_id_passthrough`` — both backends accept and silently
   plumb ``session_id`` through (no 4xx).
2. ``test_scope_all_four_sources_match`` — both backends accept ``scope=all``
   and structurally diff the resulting flat list.
3. ``test_unknown_scope_returns_400`` — both backends reject
   ``scope="foo"`` with HTTP 400 and a validation envelope whose ``msg``
   contains the ``"Unknown recall scope(s)"`` substring. The exact
   envelope shape (``loc``/``type``) carries the documented v1-envelope
   gap and is not strictly compared (substring tolerance for ``msg``,
   skip ``type``).

The first two cases are LLM-gated — the underlying recall flow runs the
graph search via the orchestrator, which (for graph_completion) requires
an LLM. The third case (unknown-scope validation) runs without an LLM
because the rejection happens at JSON-deserialization time.
"""

from __future__ import annotations

import pytest

from conftest import requires_openai
from http_helpers import DEFAULT_IGNORE, assert_responses_match
from seed import seed_cognify, seed_dataset_with_text

_IGNORE = DEFAULT_IGNORE | {
    "$..tenant_id",
    "$..owner_id",
    "$..score",
    "$..id",
    "$..chunk_id",
    "$..session_id",
    "$..created_at",
    # Per-row text content is LLM-shaped and may differ between backends.
    "$..text",
    "$..answer",
    "$..question",
}

_SEED_TEXT = (
    "Python is a high-level programming language created by Guido van Rossum in 1991. "
    "The Django framework was released in 2005 and follows the MTV pattern. "
    "TensorFlow is an open-source machine learning library developed by Google."
)


@pytest.fixture(scope="module")
def recall_v2_seeded(authed_clients, unique_dataset_name):
    """Seed + cognify on both servers once per module."""
    for _side, client in authed_clients.items():
        r = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        ds_id = r.get("dataset_id") or r.get("id")
        if ds_id:
            try:
                seed_cognify(client, dataset_id=ds_id)
            except AssertionError:
                # If cognify is gated (no LLM, etc), the recall calls below
                # may return empty graph results — but the wire envelope is
                # still comparable.
                pass
    return True


@requires_openai
def test_session_id_passthrough(authed_clients, recall_v2_seeded):
    """Both backends accept ``session_id`` and return a 200 envelope.

    The wire shape is Python's flat list with ``_source`` per item.
    """
    payload = {"query": "Who created Python?", "session_id": "s1", "scope": "auto"}
    py = authed_clients["py"].post("/api/v1/recall", json=payload, timeout=120.0)
    rs = authed_clients["rs"].post("/api/v1/recall", json=payload, timeout=120.0)
    assert py.status_code == 200, f"py /recall failed: {py.status_code} {py.text[:300]}"
    assert rs.status_code == 200, f"rs /recall failed: {rs.status_code} {rs.text[:300]}"

    py_body = py.json()
    rs_body = rs.json()
    assert isinstance(py_body, list), f"py recall body must be list, got {type(py_body)}"
    assert isinstance(rs_body, list), f"rs recall body must be list, got {type(rs_body)}"

    # Every item must carry an `_source` tag.
    for side, body in (("py", py_body), ("rs", rs_body)):
        for i, row in enumerate(body):
            assert "_source" in row, f"{side}[{i}] missing `_source`: {row!r}"

    assert_responses_match(py, rs, ignore=_IGNORE)


@requires_openai
def test_scope_all_four_sources_match(authed_clients, recall_v2_seeded):
    """``scope=all`` runs the four-source fan-out on both backends.

    With session backends not seeded, the session/trace/graph_context
    sources contribute zero rows on both sides — so the comparison
    effectively asserts the graph-source wire shape is the same.
    """
    payload = {"query": "What is mentioned about programming?", "scope": "all"}
    py = authed_clients["py"].post("/api/v1/recall", json=payload, timeout=120.0)
    rs = authed_clients["rs"].post("/api/v1/recall", json=payload, timeout=120.0)
    assert py.status_code == 200, f"py /recall scope=all failed: {py.status_code} {py.text[:300]}"
    assert rs.status_code == 200, f"rs /recall scope=all failed: {rs.status_code} {rs.text[:300]}"

    py_body = py.json()
    rs_body = rs.json()
    assert isinstance(py_body, list), f"py /recall must return list: {type(py_body)}"
    assert isinstance(rs_body, list), f"rs /recall must return list: {type(rs_body)}"

    # Every item carries an `_source`. Allowed values mirror the four-source
    # canonical set (Python `recall.py:208/278/315/495-498`).
    allowed_sources = {"session", "trace", "graph_context", "graph"}
    for side, body in (("py", py_body), ("rs", rs_body)):
        for i, row in enumerate(body):
            src = row.get("_source")
            assert src in allowed_sources, (
                f"{side}[{i}] unexpected `_source`={src!r}, allowed={allowed_sources}"
            )

    assert_responses_match(py, rs, ignore=_IGNORE)


def test_unknown_scope_returns_400(authed_clients):
    """Both backends reject ``scope="foo"`` with HTTP 400.

    Substring-matches the ``msg`` against ``"Unknown recall scope(s)"``;
    the exact envelope shape (``loc``/``type``) carries the documented
    v1-envelope gap and is NOT compared.
    """
    payload = {"query": "x", "scope": "foo"}
    py = authed_clients["py"].post("/api/v1/recall", json=payload)
    rs = authed_clients["rs"].post("/api/v1/recall", json=payload)
    assert py.status_code == 400, (
        f"py expected 400, got {py.status_code}: {py.text[:300]}"
    )
    assert rs.status_code == 400, (
        f"rs expected 400, got {rs.status_code}: {rs.text[:300]}"
    )

    py_body = py.json()
    rs_body = rs.json()

    def _extract_msg(body):
        # Python and Rust both ship a `detail` array of `{loc, msg, type}`
        # entries. Tolerate slight shape drift — pull the first `msg` we
        # can find at any depth.
        if isinstance(body, dict):
            detail = body.get("detail")
            if isinstance(detail, list) and detail:
                first = detail[0]
                if isinstance(first, dict) and isinstance(first.get("msg"), str):
                    return first["msg"]
        return ""

    py_msg = _extract_msg(py_body)
    rs_msg = _extract_msg(rs_body)
    assert "Unknown recall scope(s)" in py_msg, (
        f"py validation msg must contain 'Unknown recall scope(s)': {py_body!r}"
    )
    assert "Unknown recall scope(s)" in rs_msg, (
        f"rs validation msg must contain 'Unknown recall scope(s)': {rs_body!r}"
    )
