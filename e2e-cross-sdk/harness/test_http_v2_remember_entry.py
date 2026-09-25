"""HTTP API v2 parity tests for ``POST /api/v1/remember/entry``.

Per [`docs/http-api-v2/tasks/e-02-remember-entry.md`](../../docs/http-api-v2/tasks/e-02-remember-entry.md)
§5 — verify the typed-entry remember endpoint emits the same wire shape on
both the Python and Rust backends.

Coverage:
1. ``test_qa_entry_parity`` — POST a QA entry; assert both backends
   return ``status='session_stored'`` with the new ``entry_type='qa'`` and
   non-empty ``entry_id`` keys.
2. ``test_feedback_entry_qa_not_found_parity`` — POST a feedback entry
   referencing a non-existent ``qa_id``; assert both backends return
   ``status='errored'`` while still echoing the input ``qa_id`` as
   ``entry_id`` (Python parity at ``remember.py:307``).
3. ``test_missing_session_id_returns_4xx`` — both backends reject a
   payload without ``session_id`` with a 4xx (the exact envelope shape
   carries a documented v1-envelope gap and is not strictly compared).

These tests do NOT require an LLM — they only exercise the session-cache
dispatch which is purely deterministic.
"""

from __future__ import annotations

import uuid


def _post_entry(client, body, timeout=15.0):
    return client.post("/api/v1/remember/entry", json=body, timeout=timeout)


def test_qa_entry_parity(authed_clients):
    """A typed QA entry round-trips through the session cache on both
    backends and produces the documented ``RememberResult`` wire shape.
    """
    session_id = f"e2e-{uuid.uuid4()}"
    body = {
        "entry": {
            "type": "qa",
            "question": "What is parity?",
            "answer": "Equal wire shape across SDKs.",
        },
        "sessionId": session_id,
    }

    for side, client in authed_clients.items():
        resp = _post_entry(client, body)
        assert resp.status_code == 200, (
            f"{side} /remember/entry failed: {resp.status_code} {resp.text[:300]}"
        )
        payload = resp.json()
        assert payload.get("status") == "session_stored", (
            f"{side} expected status=session_stored, got {payload!r}"
        )
        assert payload.get("entry_type") == "qa", (
            f"{side} expected entry_type=qa, got {payload!r}"
        )
        entry_id = payload.get("entry_id")
        assert isinstance(entry_id, str) and entry_id, (
            f"{side} entry_id must be non-empty string, got {entry_id!r}"
        )
        assert payload.get("session_ids") == [session_id], (
            f"{side} session_ids should echo input, got {payload!r}"
        )


def test_feedback_entry_qa_not_found_parity(authed_clients):
    """A feedback entry referring to a non-existent ``qa_id`` produces
    ``status='errored'`` on both backends while still echoing the input
    ``qa_id`` as the response ``entry_id`` (Python parity remember.py:307).
    """
    session_id = f"e2e-fb-{uuid.uuid4()}"
    bogus_qa_id = f"missing-{uuid.uuid4()}"
    body = {
        "entry": {
            "type": "feedback",
            "qaId": bogus_qa_id,
            "feedbackText": "missing-qa parity probe",
        },
        "sessionId": session_id,
    }

    for side, client in authed_clients.items():
        resp = _post_entry(client, body)
        # Python returns 200 with status=errored — not a 4xx.
        assert resp.status_code == 200, (
            f"{side} unexpected status: {resp.status_code} {resp.text[:300]}"
        )
        payload = resp.json()
        assert payload.get("status") == "errored", (
            f"{side} expected errored, got {payload!r}"
        )
        assert payload.get("entry_type") == "feedback"
        assert payload.get("entry_id") == bogus_qa_id, (
            f"{side} entry_id must echo input qa_id, got {payload!r}"
        )
        assert isinstance(payload.get("error"), str) and payload["error"], (
            f"{side} error message must be set, got {payload!r}"
        )


def test_missing_session_id_returns_4xx(authed_clients):
    """Both backends reject a body without ``session_id`` with a 4xx.

    The exact envelope shape (``detail`` array vs scalar) carries the
    documented v1-envelope gap and is NOT strictly compared.
    """
    body = {
        "entry": {"type": "qa", "question": "x", "answer": "y"},
    }
    for side, client in authed_clients.items():
        resp = _post_entry(client, body)
        assert 400 <= resp.status_code < 500, (
            f"{side} expected 4xx, got {resp.status_code} {resp.text[:300]}"
        )
