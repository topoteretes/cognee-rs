"""Pre-test seeding helpers for the HTTP parity harness.

Used by phase-1 and phase-2 test files to place data into both servers before
running diff assertions.  Per e2e-parity.md §8: any seed-time divergence fails
fast and clearly.

Exposed helpers:
- ``seed_dataset_with_text``  — POST /add with a text file; return the parsed JSON.
- ``seed_cognify``            — POST /cognify blocking; return the parsed JSON.
- ``seed_both``               — call both helpers against each client in a
                                ``both_clients`` dict and assert the seeded IDs
                                match (modulo legitimately differing fields).

Inline self-tests (pytest) at the bottom of this file use ``httpx.MockTransport``
to verify the multipart and JSON request shapes without a live server.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import httpx
import pytest

if TYPE_CHECKING:
    pass


# ─────────────────────────────────────────────────────────────────────────────
# Seeding helpers
# ─────────────────────────────────────────────────────────────────────────────


def seed_dataset_with_text(
    client: httpx.Client,
    *,
    name: str,
    text: str,
) -> dict:
    """POST ``/api/v1/add`` with a multipart body containing one text/plain part.

    Args:
        client: An authenticated ``httpx.Client`` pointing at one server.
        name:   Dataset name (use ``unique_dataset_name`` from conftest).
        text:   Text content to ingest.

    Returns:
        Parsed JSON response dict.

    Raises:
        AssertionError: if the server returns a non-200 status.
    """
    r = client.post(
        "/api/v1/add",
        files={"data": ("content.txt", text.encode(), "text/plain")},
        data={"dataset_name": name},
    )
    assert r.status_code == 200, (
        f"seed_dataset_with_text failed for dataset={name!r}: "
        f"status={r.status_code} body={r.text[:500]}"
    )
    return r.json()


def seed_cognify(
    client: httpx.Client,
    *,
    dataset_id: str,
) -> dict:
    """POST ``/api/v1/cognify`` synchronously and return the parsed JSON.

    Args:
        client:     An authenticated ``httpx.Client`` pointing at one server.
        dataset_id: UUID string of the dataset to cognify.

    Returns:
        Parsed JSON response dict.

    Raises:
        AssertionError: if the server returns a non-200 status.
    """
    r = client.post(
        "/api/v1/cognify",
        json={"datasets": [dataset_id], "run_in_background": False},
    )
    assert r.status_code == 200, (
        f"seed_cognify failed for dataset_id={dataset_id!r}: "
        f"status={r.status_code} body={r.text[:500]}"
    )
    return r.json()


def seed_both(
    both_clients: dict,
    *,
    name: str,
    text: str,
) -> dict[str, dict]:
    """Seed both servers with the same text and return per-side responses.

    Calls ``seed_dataset_with_text`` against ``both_clients["py"]`` and
    ``both_clients["rs"]``, then asserts that the ``content_hash`` fields match
    (content-addressed IDs must be identical for the same input).

    Args:
        both_clients: ``{"py": <httpx.Client>, "rs": <httpx.Client>}`` fixture.
        name:         Dataset name — should be unique per test.
        text:         Text to ingest.

    Returns:
        ``{"py": <response_dict>, "rs": <response_dict>}``
    """
    results: dict[str, dict] = {}
    for side, client in both_clients.items():
        results[side] = seed_dataset_with_text(client, name=name, text=text)

    # content_hash is deterministic (MD5 of content); both sides must agree.
    py_hash = results["py"].get("content_hash")
    rs_hash = results["rs"].get("content_hash")
    if py_hash is not None and rs_hash is not None:
        assert py_hash == rs_hash, (
            f"seed_both: content_hash mismatch — py={py_hash!r} rs={rs_hash!r}\n"
            f"py response: {results['py']}\n"
            f"rs response: {results['rs']}"
        )

    return results


# ─────────────────────────────────────────────────────────────────────────────
# Inline self-tests (pytest)
# ─────────────────────────────────────────────────────────────────────────────


class _MockTransport(httpx.MockTransport):
    """Minimal mock transport that captures the last request and returns a fixed response."""

    def __init__(self, status: int = 200, body: dict | None = None):
        self.last_request: httpx.Request | None = None
        self._status = status
        self._body = body or {}

    def handle_request(self, request: httpx.Request) -> httpx.Response:
        request.read()  # ensure .content is accessible after the call returns
        self.last_request = request
        import json as _json

        return httpx.Response(
            self._status,
            headers={"content-type": "application/json"},
            content=_json.dumps(self._body).encode(),
        )


def test_seed_dataset_posts_multipart():
    """seed_dataset_with_text sends a multipart POST to /api/v1/add."""
    transport = _MockTransport(200, {"content_hash": "abc123"})
    client = httpx.Client(base_url="http://testserver", transport=transport)
    result = seed_dataset_with_text(client, name="my_dataset", text="hello world")
    assert result == {"content_hash": "abc123"}
    assert transport.last_request is not None
    assert transport.last_request.url.path == "/api/v1/add"
    # Verify multipart body contains dataset_name
    body_bytes = transport.last_request.content
    assert b"my_dataset" in body_bytes
    assert b"hello world" in body_bytes


def test_seed_cognify_posts_json():
    """seed_cognify sends a JSON POST to /api/v1/cognify with run_in_background=false."""
    import json as _json

    transport = _MockTransport(200, {"status": "completed"})
    client = httpx.Client(base_url="http://testserver", transport=transport)
    result = seed_cognify(client, dataset_id="ds-uuid-001")
    assert result == {"status": "completed"}
    assert transport.last_request is not None
    assert transport.last_request.url.path == "/api/v1/cognify"
    sent = _json.loads(transport.last_request.content)
    assert sent["datasets"] == ["ds-uuid-001"]
    assert sent["run_in_background"] is False


def test_seed_dataset_raises_on_non_200():
    """seed_dataset_with_text raises AssertionError on non-200 response."""
    transport = _MockTransport(422, {"detail": "invalid"})
    client = httpx.Client(base_url="http://testserver", transport=transport)
    with pytest.raises(AssertionError, match="422"):
        seed_dataset_with_text(client, name="bad", text="x")
